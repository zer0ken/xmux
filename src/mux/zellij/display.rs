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
