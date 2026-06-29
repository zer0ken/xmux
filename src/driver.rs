//! The mux DRIVER boundary: the supervisor passes INTENT (display this
//! session+window) and reads back a grid; HOW (attach / switch-client / reattach
//! / select-window) lives behind `MuxDriver`. `DriverCtx` injects the
//! supervisor-owned spawn capability + registry so the driver owns the DECISION
//! and per-host display STATE while the PTY infrastructure stays in the loop.
//!
//! `SeamDriver` is the behavior-preserving adapter: it holds NO per-host state and
//! delegates straight to the existing free functions, so introducing the boundary
//! changes no behavior. tmux/psmux-specific drivers that own the decision come later.

use std::sync::{Arc, Mutex};

use crate::cockpit::Selection;
use crate::display::DisplayWorker;
use crate::host::HostManager;
use crate::model::Hosts;
use crate::proxy::registry::AttachRegistry;
use crate::proxy::screen::Grid;

/// A supervisor INTENT: show this session (and optionally land on a window). The
/// generic shape the supervisor knows; the driver maps it onto mux mechanics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub session: String,
    pub window: Option<i64>,
}

impl Target {
    pub fn from_selection(sel: &Selection) -> Self {
        Target {
            session: sel.session.clone(),
            window: sel.window,
        }
    }
    pub fn into_selection(&self, source: &str) -> Selection {
        Selection {
            source: source.to_string(),
            session: self.session.clone(),
            window: self.window,
        }
    }
}

/// The generic capabilities the supervisor injects into a driver call: the off-loop
/// spawner, the attachment registry it fills, the transport-aware hosts, the open
/// control channel (if any), the view size, and the attach seq. The driver owns the
/// DECISION + per-host display state; these stay supervisor-owned.
pub struct DriverCtx<'a> {
    pub registry: &'a mut AttachRegistry,
    pub hosts: &'a mut Hosts,
    pub worker: &'a DisplayWorker,
    pub mgr: &'a HostManager,
    pub attach_seq: &'a mut u64,
    pub cols: u16,
    pub body_rows: u16,
    pub tree_width: u16,
}

/// One mux driver per host: intent in, screen out.
pub trait MuxDriver {
    /// Make the selected session live and landed on its window. Returns true when the
    /// selection has a session to show (so the caller can confirm the display truth).
    fn show(&mut self, sel: &Selection, ctx: &mut DriverCtx) -> bool;
    /// The grid the supervisor renders for the selection, if a live attach exists.
    fn grid(&self, sel: &Selection, ctx: &DriverCtx) -> Option<Arc<Mutex<Grid>>>;
    /// Forward input bytes to the selected session's attachment.
    fn input(&mut self, sel: &Selection, bytes: Vec<u8>, ctx: &DriverCtx);
}

/// The behavior-preserving adapter: delegates to the existing free functions with the
/// same arguments, so the boundary changes no behavior. Holds no per-host state.
pub struct SeamDriver;

impl MuxDriver for SeamDriver {
    fn show(&mut self, sel: &Selection, ctx: &mut DriverCtx) -> bool {
        crate::cockpit::select_attach(
            ctx.registry,
            ctx.hosts,
            sel,
            ctx.worker,
            ctx.attach_seq,
            ctx.cols,
            ctx.body_rows,
            ctx.tree_width,
            ctx.mgr,
        )
    }
    fn grid(&self, sel: &Selection, ctx: &DriverCtx) -> Option<Arc<Mutex<Grid>>> {
        ctx.registry
            .grid(&crate::cockpit::display_key(ctx.hosts, sel))
    }
    fn input(&mut self, sel: &Selection, bytes: Vec<u8>, ctx: &DriverCtx) {
        ctx.registry
            .input(&crate::cockpit::display_key(ctx.hosts, sel), bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cockpit::Selection;

    #[test]
    fn target_round_trips_through_selection() {
        let sel = Selection {
            source: "jup".into(),
            session: "api".into(),
            window: Some(2),
        };
        let t = Target::from_selection(&sel);
        assert_eq!(t.session, "api");
        assert_eq!(t.window, Some(2));
        assert_eq!(t.into_selection("jup"), sel);
    }

    #[test]
    fn seam_driver_is_object_safe() {
        // The whole point: a Box<dyn MuxDriver> must compile. If the trait gains a
        // non-dispatchable method this stops compiling.
        let _d: Box<dyn MuxDriver> = Box::new(SeamDriver);
    }

    /// Through the driver boundary, a psmux selection still REPLACES the single
    /// host-keyed display attachment (the per-session reattach). This pins the seam's
    /// faithfulness independently of `select_attach` keeping its current name/shape, so
    /// a future driver that owns the decision must preserve the same observable effect
    /// (the 4a5f053 per-session attach behavior). Headless: a fake spawner, no live psmux.
    #[tokio::test(flavor = "current_thread")]
    async fn seam_show_replaces_the_psmux_display_attachment() {
        let mut hosts = crate::model::Hosts::default();
        hosts.insert(crate::model::Host::new(
            crate::model::Transport::Local { socket: None },
            crate::backend::for_binary("psmux"),
        ));
        // A stale attachment + bookkeeping for a different session: show() must drop it
        // and reattach for the selected session (psmux is one PTY per host, reattached).
        hosts
            .get_mut("local")
            .unwrap()
            .display
            .set_shows("local", "old");

        let (ptx, _prx) = tokio::sync::mpsc::unbounded_channel();
        let worker = crate::display::DisplayWorker::with_spawner(
            ptx,
            Box::new(|_argv, _cols, _rows, id, _events| Ok(crate::proxy::run::fake_attachment(id))),
        );
        let mut registry = AttachRegistry::new();
        registry.insert("local", crate::proxy::run::fake_attachment(99));
        let mut attach_seq = 0u64;
        let mgr = HostManager::new(tokio::sync::mpsc::unbounded_channel().0);

        let sel = Selection {
            source: "local".into(),
            session: "target".into(),
            window: None,
        };

        let mut driver: Box<dyn MuxDriver> = Box::new(SeamDriver);
        let shown = {
            let mut ctx = DriverCtx {
                registry: &mut registry,
                hosts: &mut hosts,
                worker: &worker,
                mgr: &mgr,
                attach_seq: &mut attach_seq,
                cols: 80,
                body_rows: 24,
                tree_width: crate::ui::switcher::TREE_WIDTH,
            };
            driver.show(&sel, &mut ctx)
        };

        assert!(shown, "a selection with a session has something to show");
        let h = hosts.get("local").unwrap();
        assert_eq!(
            h.display.shows("local"),
            Some("target"),
            "show records the newly-selected session on the host key"
        );
        assert!(
            h.display.in_flight.contains_key("local"),
            "show requests a fresh per-session reattach"
        );
        assert!(
            !registry.contains("local"),
            "the stale display attachment is removed before reattach"
        );
    }
}
