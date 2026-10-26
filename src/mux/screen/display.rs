//! The screen display driver: one `screen -x <name>` client per session, kept while
//! xmux runs. `Screen::driver` constructs it, so mux selection lives in the screen
//! implementation, not a central match.
//!
//! There is no in-place client switch to make: screen has no `switch-client` equivalent
//! and a client cannot be named from outside the session it is in. The view on screen is
//! kept until a session's first client is ready, so it never blanks between the two.
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

/// screen: one daemon per session, each shown through its own display client.
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
        let pre_mismatch = host.display.shows(&key) != Some(sel.session.as_str());
        if crate::driver::holds_connection(ctx, sel) {
            tracing::info!(
                host = %sel.host,
                decision = "warm",
                reason = "already-on",
                session = %sel.session,
                "display_show"
            );
            crate::driver::log_display_inventory!(ctx, sel.session, pre_mismatch);
            return true;
        }
        tracing::info!(
            host = %sel.host,
            decision = "attach",
            reason = "no-live-client",
            session = %sel.session,
            "display_show"
        );
        let mux_argv = host.mux.display_attach_plan(&sel.session);
        let command = host.transport.exec_argv(true, &mux_argv);
        let id = ctx
            .request_attach(sel, command)
            .expect("the selected host exists");
        tracing::info!(addr = %key, id, count = ctx.registry.len(), "attach_created");
        crate::driver::log_display_inventory!(ctx, sel.session, pre_mismatch);
        true
    }
}
