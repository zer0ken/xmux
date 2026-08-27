//! The zellij display driver: a per-session mux displayed through ONE per-host PTY
//! that is REATTACHED whenever the selected session changes. `Zellij::driver`
//! constructs it, so mux selection lives in the zellij family, not a central match.
//!
//! There is no in-place client switch to make. zellij's `switch-session` moves
//! whichever client RUNS it, and a client cannot be named from outside the session it
//! is in, so xmux has no way to aim a switch at its own display client the way it aims
//! tmux's and psmux's. Every session change is therefore a fresh attach, and the stale
//! attachment is kept until the new one is ready so the view never blanks between the
//! two.

use std::sync::{Arc, Mutex};

use crate::app::runtime::{host_selection_key, request_attach, terminal_view_size};
use crate::display::grid::Grid;
use crate::driver::{lower_select_window, DriverCtx, MuxDriver};
use crate::model::Selection;

/// Per-session mux (zellij): one server per session, displayed through ONE per-host
/// PTY that is REATTACHED whenever the selected session changes.
pub struct ZellijDriver;

impl MuxDriver for ZellijDriver {
    fn kind(&self) -> &str {
        "zellij"
    }

    fn show(&mut self, sel: &Selection, ctx: &mut DriverCtx) -> bool {
        if sel.is_empty() {
            return false;
        }
        let (cols, rows) = terminal_view_size(ctx.cols, ctx.body_rows, ctx.nav);
        let control = ctx.mgr.get(&sel.source);
        let Some(host) = ctx.hosts.get_mut(&sel.source) else {
            return false;
        };
        let key = host_selection_key(host);
        let live = ctx.registry.contains(&key);
        let already_on = host.display.shows(&key) == Some(sel.session.as_str());
        let pre_mismatch = !already_on;

        if live && already_on {
            // The live attachment already shows this session, so only a window row can
            // need moving: `go-to-tab` on the session's own server, no teardown.
            tracing::info!(
                host = %sel.source,
                model = "per-session",
                decision = "warm",
                reason = "already-on",
                session = %sel.session,
                "display_show"
            );
            if let Some(win) = sel.window {
                lower_select_window(host, control, &sel.session, win);
            }
            crate::driver::log_display_inventory!(ctx, sel.session, pre_mismatch);
            return true;
        }

        // REATTACH: the only way to move zellij's display. The stale attachment is KEPT
        // in the registry (not removed) so its grid stays on screen until DisplayReady
        // swaps in the new one and tears the stale one down (stale-while-revalidate).
        // At first display there is nothing to keep, so the view is blank until Ready.
        let reason = if live {
            "other-session"
        } else {
            "no-live-client"
        };
        tracing::info!(
            host = %sel.source,
            model = "per-session",
            decision = "reattach",
            reason,
            session = %sel.session,
            "display_show"
        );
        host.display.clear(&key);
        let mux_argv = host.mux.attach_plan(&sel.session);
        let (cmd, args) = host.transport.exec_argv(true, &mux_argv);
        let mut argv = vec![cmd];
        argv.extend(args);
        let id = request_attach(
            ctx.registry,
            ctx.worker,
            &mut host.display,
            ctx.attach_seq,
            &key,
            argv,
            (cols, rows),
        );
        tracing::info!(addr = %key, id, count = ctx.registry.len(), "attach_created");
        host.display.set_shows(&key, &sel.session);

        if let Some(win) = sel.window {
            lower_select_window(host, control, &sel.session, win);
        }
        crate::driver::log_display_inventory!(ctx, sel.session, pre_mismatch);
        true
    }

    fn grid(&self, sel: &Selection, ctx: &DriverCtx) -> Option<Arc<Mutex<Grid>>> {
        ctx.registry
            .grid(&crate::app::runtime::display_key(ctx.hosts, sel))
    }

    fn input(&mut self, sel: &Selection, bytes: Vec<u8>, ctx: &DriverCtx) {
        ctx.registry
            .input(&crate::app::runtime::display_key(ctx.hosts, sel), bytes);
    }

    fn sync(&mut self, source: &str, sessions: &[crate::session::Session], ctx: &mut DriverCtx) {
        // Per-session attaches are selected on demand by `show`, not pre-warmed: sync
        // only tears down the host PTY when the host has no sessions left.
        if sessions.is_empty() {
            ctx.registry.remove(source);
            if let Some(host) = ctx.hosts.get_mut(source) {
                host.display.clear(source);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::registry::AttachRegistry;
    use crate::link::HostManager;
    use crate::model::Selection;

    /// A headless zellij host with one live display attachment already on record: the
    /// registry holds attachment `id` under the host key, and the host's display
    /// bookkeeping says that attachment shows `shows`. No live zellij is involved; the
    /// spawner is a fake, so a reattach shows up as a request and nothing else.
    fn host_with_live_client(shows: &str, id: u64) -> (crate::model::Hosts, AttachRegistry) {
        let mut hosts = crate::model::Hosts::default();
        hosts.insert(crate::model::Host::new(
            crate::transport::local(None),
            crate::mux::for_binary("zellij"),
        ));
        hosts
            .get_mut("local")
            .unwrap()
            .display
            .set_shows("local", shows);
        let mut registry = AttachRegistry::new();
        registry.insert("local", crate::display::attachment::fake_attachment(id));
        (hosts, registry)
    }

    /// Runs the driver's `show` for `session` against those facts and answers what it
    /// returned, keeping the ctx borrow inside so the caller can read the hosts back.
    fn show_session(
        hosts: &mut crate::model::Hosts,
        registry: &mut AttachRegistry,
        session: &str,
    ) -> bool {
        let (ptx, _prx) = tokio::sync::mpsc::unbounded_channel();
        let worker = crate::display::DisplayWorker::with_spawner(
            ptx,
            Box::new(|_argv, _cols, _rows, id, _events, _env_clear| {
                Ok(crate::display::attachment::fake_attachment(id))
            }),
        );
        let mut attach_seq = 0u64;
        let mgr = HostManager::new(tokio::sync::mpsc::unbounded_channel().0);
        let (cap_tx, _cap_rx) = tokio::sync::mpsc::unbounded_channel();
        let sel = Selection {
            source: "local".into(),
            session: session.into(),
            window: None,
        };
        let mut ctx = DriverCtx {
            registry,
            hosts,
            worker: &worker,
            mgr: &mgr,
            pty_tx: &cap_tx,
            attach_seq: &mut attach_seq,
            cols: 80,
            body_rows: 24,
            nav: crate::ui::switcher::NavSize::visible(crate::ui::switcher::NAV_WIDTH),
        };
        ZellijDriver.show(&sel, &mut ctx)
    }

    /// zellij moved xmux's own client with `switch-session`, and the follow recorded that
    /// before moving the nav, so by the time the selection reaches `show` the bookkeeping
    /// already names the session the client is on. The decision must then be WARM: a
    /// reattach here would spawn a second `zellij attach` for a session xmux's own client
    /// is already in, and tear down the client the user just moved to reach it.
    ///
    /// This is why the zellij driver needs no reattach guard of its own. Its warm branch
    /// already rests on the display belief, and the follow is what keeps that belief
    /// equal to the live client's own report.
    #[tokio::test(flavor = "current_thread")]
    async fn a_followed_switch_warms_the_client_the_mux_already_moved() {
        let (mut hosts, mut registry) = host_with_live_client("vfy-ze-b", 42);
        let shown = show_session(&mut hosts, &mut registry, "vfy-ze-b");

        assert!(shown, "a selection with a session has something to show");
        let h = hosts.get("local").unwrap();
        assert!(
            !h.display.in_flight_contains("local"),
            "the client is already there, so nothing is reattached"
        );
        assert_eq!(
            registry.get("local").map(|a| a.id()),
            Some(42),
            "the same client stays on screen: it was never respawned"
        );
        assert_eq!(h.display.shows("local"), Some("vfy-ze-b"));
    }

    /// The other half: a session change the NAV drives is still a reattach, so following
    /// a mux-side switch cannot wedge the next one. zellij can aim no switch at its own
    /// client from outside the session it is in, so a fresh attach is the only way to
    /// reach another session, and the stale attachment is held on screen until it lands.
    #[tokio::test(flavor = "current_thread")]
    async fn a_nav_driven_switch_away_still_reattaches() {
        let (mut hosts, mut registry) = host_with_live_client("vfy-ze-b", 42);
        let shown = show_session(&mut hosts, &mut registry, "vfy-ze-a");

        assert!(shown);
        let h = hosts.get("local").unwrap();
        assert!(
            h.display.in_flight_contains("local"),
            "another session is reached by a fresh attach and nothing else"
        );
        assert_eq!(
            h.display.shows("local"),
            Some("vfy-ze-a"),
            "the newly-selected session is what the host key now shows"
        );
        assert_eq!(
            registry.get("local").map(|a| a.id()),
            Some(42),
            "the stale attachment is HELD until DisplayReady swaps the fresh one in"
        );
    }
}
