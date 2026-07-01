//! The mux DRIVER boundary: the supervisor passes INTENT (display this
//! session+window) and reads back a grid; HOW (attach / switch-client / reattach
//! / select-window) lives behind `MuxDriver`. `DriverCtx` injects the
//! supervisor-owned spawn capability + registry so the driver owns the DECISION
//! and per-host display STATE while the PTY infrastructure stays in the loop.
//!
//! The per-mux drivers (`TmuxDriver`, `PsmuxDriver`) live in their mux family
//! (`crate::mux::{tmux, psmux}`) and OWN the display decision. Each backend
//! constructs its own driver via [`Backend::driver`](crate::mux::Backend::driver),
//! so [`driver_for`] is a thin mux-agnostic wrapper (`host.mux.driver()`) that names no
//! concrete mux type. Each driver is zero-sized — the per-host display STATE stays in
//! `host.display`/`AttachRegistry`, borrowed through `DriverCtx`, so the boundary moved
//! the decision without relocating the state (a later step inverts that ownership).

use std::sync::{Arc, Mutex};

use crate::cockpit::{run_lowered, Selection};
use crate::display::grid::Grid;
use crate::display::registry::AttachRegistry;
use crate::display::DisplayWorker;
use crate::host::HostManager;
use crate::model::{Host, Hosts};

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
/// spawner, the attachment registry it fills, the transport-aware hosts, the source
/// config, the open control channel (via `mgr`), the view size, and the attach seq.
/// Attach argv is composed from each host's own `mux`/`transport` (the two axes), so a
/// driver reads `hosts`, not `env`, to build one. The driver owns the DECISION +
/// per-host display state; these stay supervisor-owned.
pub struct DriverCtx<'a> {
    pub registry: &'a mut AttachRegistry,
    pub hosts: &'a mut Hosts,
    pub worker: &'a DisplayWorker,
    pub mgr: &'a HostManager,
    pub env: &'a crate::env::Env,
    /// The off-loop event sink (a clone of the loop's `PtyEvent` channel). A driver may
    /// spawn a read-only probe that feeds a `PtyEvent` back to the loop — e.g. the psmux
    /// driver captures its display client's tty with an off-loop `list-clients` probe.
    pub pty_tx: &'a tokio::sync::mpsc::UnboundedSender<crate::display::attachment::PtyEvent>,
    pub attach_seq: &'a mut u64,
    pub cols: u16,
    pub body_rows: u16,
    pub tree_width: u16,
}

/// One mux driver per host: intent in, screen out.
pub trait MuxDriver {
    /// The mux identity this driver speaks for, for diagnostics + driver selection tests.
    fn kind(&self) -> &str;
    /// Make the selected session live and landed on its window. Returns true when the
    /// selection has a session to show (so the caller can confirm the display truth).
    fn show(&mut self, sel: &Selection, ctx: &mut DriverCtx) -> bool;
    /// The grid the supervisor renders for the selection, if a live attach exists.
    fn grid(&self, sel: &Selection, ctx: &DriverCtx) -> Option<Arc<Mutex<Grid>>>;
    /// Forward input bytes to the selected session's attachment.
    fn input(&mut self, sel: &Selection, bytes: Vec<u8>, ctx: &DriverCtx);
    /// Reconcile the host's display terminal with its current `sessions` (an inventory
    /// update — a remote `%`-event refresh or a local poll). Shared keeps ONE PTY per
    /// host: warm it on the first session, reap it when the host has no sessions.
    /// PerSession is selected on demand: only reap the host PTY when no sessions remain.
    fn sync(&mut self, source: &str, sessions: &[crate::session::Session], ctx: &mut DriverCtx);
}

/// The host's mux driver — the DECISION is a Backend method (`host.mux.driver()`), not a
/// `match` at the call site. Each backend constructs its OWN driver, so mux selection
/// lives in the mux family (`crate::mux::{tmux, psmux}`), never a central match here.
/// Drivers are zero-sized, so a fresh value per call is free; the per-host state lives in
/// `host.display`/`AttachRegistry` (via `DriverCtx`).
pub fn driver_for(host: &Host) -> Box<dyn MuxDriver> {
    host.mux.driver()
}

/// Moves the session's active window server-side (the real attached client follows).
/// Over the host's open `-CC` connection if any (no fresh ssh handshake), else a lowered
/// select-window subprocess. Shared by both drivers' window-row handling.
pub(crate) fn lower_select_window(
    host: &Host,
    control: Option<&crate::host::HostClient>,
    session: &str,
    win: i64,
) {
    let target = crate::mux::window_target(session, win);
    if let Some(client) = control {
        client.select_window_on(&target);
    } else {
        let mux_argv = host.mux.select_window_plan(&target);
        let (cmd, args) = host.transport.exec_argv(false, &mux_argv);
        let mut argv = vec![cmd];
        argv.extend(args);
        run_lowered(crate::model::LoweredSwitch::Local(argv));
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::cockpit::Selection;

    /// A minimal `Env` with one local `cmd.exe` `Source` per alias (in both `srcs` and
    /// `by_alias`), used to construct a `DriverCtx` in the driver tests (this module's
    /// and the per-mux drivers' in `crate::mux::{tmux, psmux}`).
    pub(crate) fn fake_env(aliases: &[&str]) -> crate::env::Env {
        let srcs: Vec<crate::source::Source> = aliases
            .iter()
            .map(|a| crate::source::Source {
                alias: (*a).into(),
                binary: "cmd.exe".into(),
                remote: false,
                control_path: String::new(),
                os: "windows".into(),
                socket: None,
                runner: None,
            })
            .collect();
        let by_alias = srcs.iter().map(|s| (s.alias.clone(), s.clone())).collect();
        crate::env::Env {
            cfg: crate::config::Config::default(),
            cfg_warnings: Vec::new(),
            srcs,
            by_alias,
            local_bin: "cmd.exe".into(),
            ui_prefix: "C-g".into(),
            xmux_dir: std::path::PathBuf::from("."),
        }
    }

    pub(crate) fn sess(source: &str, name: &str) -> crate::session::Session {
        crate::session::Session {
            source: source.into(),
            name: name.into(),
            windows: 1,
            attached: false,
            last_attached: 0,
        }
    }

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
    fn drivers_are_object_safe() {
        // The whole point: a Box<dyn MuxDriver> must compile. If the trait gains a
        // non-dispatchable method this stops compiling. Obtained via the production
        // path (`Backend::driver()` through `driver_for`) so this seam names no
        // concrete driver type — those live in `crate::mux::{tmux, psmux}`.
        let tmux_host = crate::model::Host::new(
            crate::model::Transport::Local { socket: None },
            crate::mux::for_binary("tmux"),
        );
        let psmux_host = crate::model::Host::new(
            crate::model::Transport::Local { socket: None },
            crate::mux::for_binary("psmux"),
        );
        let _t: Box<dyn MuxDriver> = driver_for(&tmux_host);
        let _p: Box<dyn MuxDriver> = driver_for(&psmux_host);
    }

    /// The decision is a Backend method, not a `match` in the cockpit: a Shared host is
    /// driven by the tmux driver, a PerSession host by the psmux driver. This is
    /// `driver_for` delegating to `host.mux.driver()` — each backend builds its own.
    #[test]
    fn driver_for_picks_the_mux_specific_driver_by_backend() {
        let tmux_host = crate::model::Host::new(
            crate::model::Transport::Ssh {
                alias: "jup".into(),
                control_path: String::new(),
                os: "linux".into(),
            },
            crate::mux::for_binary("tmux"),
        );
        let psmux_host = crate::model::Host::new(
            crate::model::Transport::Local { socket: None },
            crate::mux::for_binary("psmux"),
        );
        assert_eq!(driver_for(&tmux_host).kind(), "tmux");
        assert_eq!(driver_for(&psmux_host).kind(), "psmux");
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
            crate::mux::for_binary("psmux"),
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
            Box::new(|_argv, _cols, _rows, id, _events| {
                Ok(crate::display::attachment::fake_attachment(id))
            }),
        );
        let mut registry = AttachRegistry::new();
        registry.insert("local", crate::display::attachment::fake_attachment(99));
        let mut attach_seq = 0u64;
        let mgr = HostManager::new(tokio::sync::mpsc::unbounded_channel().0);
        let env = fake_env(&["local"]);
        let (cap_tx, _cap_rx) = tokio::sync::mpsc::unbounded_channel();

        let sel = Selection {
            source: "local".into(),
            session: "target".into(),
            window: None,
        };

        // Through the Backend dispatch (driver_for → host.mux.driver()) + the concrete
        // driver — the same path the cockpit takes — so this pins the whole boundary.
        let mut driver = driver_for(hosts.get("local").unwrap());
        let shown = {
            let mut ctx = DriverCtx {
                registry: &mut registry,
                hosts: &mut hosts,
                worker: &worker,
                mgr: &mgr,
                env: &env,
                pty_tx: &cap_tx,
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
            registry.contains("local"),
            "the stale attachment is HELD (kept on screen) while the fresh reattach is \
             requested; the swap + teardown happens at DisplayReady (stale-while-revalidate)"
        );
    }
}
