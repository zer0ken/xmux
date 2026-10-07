//! The screen display driver: a per-session mux displayed through ONE per-host PTY that
//! is REATTACHED whenever the selected session changes. `Screen::driver` constructs it,
//! so mux selection lives in the screen implementation, not a central match.
//!
//! There is no in-place client switch to make: screen has no `switch-client` equivalent
//! and a client cannot be named from outside the session it is in, so every session
//! change is a fresh `screen -x <name>` attach. The stale attachment is kept until the
//! new one is ready so the view never blanks between the two.
//!
//! There is no session change to follow either. Inside a session `C-a d` detaches the
//! client, and detaching is the only move screen offers it, so a client reaches another
//! session only by ending and a new one starting from outside. tmux's
//! `%client-session-changed` follow therefore has no screen counterpart to wire, and a
//! nav selection that stays where the user put it is the right answer, not a missed
//! event. This rests on screen 4.09.00 and 4.9.1, where `select` and `other` under
//! `C-a` are WINDOW commands and `sessionname` only renames; 5.x is unverified.

use crate::driver::{DriverCtx, MuxDriver};
use crate::model::Selection;

/// screen: one daemon per session, displayed through ONE per-host PTY that is REATTACHED
/// whenever the selected session changes.
pub struct ScreenDriver;

impl MuxDriver for ScreenDriver {
    fn kind(&self) -> &str {
        "screen"
    }

    fn show(&mut self, sel: &Selection, ctx: &mut DriverCtx) -> bool {
        if sel.is_empty() {
            return false;
        }
        let key = ctx.display_key(sel);
        let Some(host) = ctx.hosts.get(&sel.host) else {
            return false;
        };
        let live = ctx.registry.contains(&key);
        let already_on = host.display.shows(&key) == Some(sel.session.as_str());
        let pre_mismatch = !already_on;

        if live && already_on {
            // The live attachment already shows this session; nothing to move, no teardown.
            tracing::info!(
                host = %sel.host,
                model = "per-session",
                decision = "warm",
                reason = "already-on",
                session = %sel.session,
                "display_show"
            );
            crate::driver::log_display_inventory!(ctx, sel.session, pre_mismatch);
            return true;
        }

        // REATTACH: the only way to move screen's display. The stale attachment is KEPT
        // in the registry so its grid stays on screen until the fresh client paints or
        // reaches its bounded wait (stale-while-revalidate).
        let reason = if live {
            "other-session"
        } else {
            "no-live-client"
        };
        tracing::info!(
            host = %sel.host,
            model = "per-session",
            decision = "reattach",
            reason,
            session = %sel.session,
            "display_show"
        );
        let command = {
            let host = ctx
                .hosts
                .get_mut(&sel.host)
                .expect("the selected host exists");
            host.display.clear(&key);
            let mux_argv = host.mux.display_attach_plan(&sel.session);
            host.transport.exec_argv(true, &mux_argv)
        };
        let id = ctx
            .request_attach(sel, command)
            .expect("the selected host exists");
        tracing::info!(addr = %key, id, count = ctx.registry.len(), "attach_created");
        crate::driver::log_display_inventory!(ctx, sel.session, pre_mismatch);
        true
    }

    fn sync(&mut self, id: &str, sessions: &[crate::session::Session], ctx: &mut DriverCtx) {
        // Per-session attaches are selected on demand by `show`, not pre-warmed: sync
        // only tears down the host PTY when the host has no sessions left.
        if sessions.is_empty() {
            ctx.registry.remove(id);
            if let Some(host) = ctx.hosts.get_mut(id) {
                host.display.clear(id);
            }
        }
    }
}
