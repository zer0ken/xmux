//! The abduco display driver: a per-session mux (one server per session) shown through
//! one client per session, kept while xmux runs. `Abduco::driver` constructs it, so mux selection lives in the abduco implementation, not a
//! central match.
//!
//! abduco cannot move an attached client to another session. Its whole option surface
//! is `-a -A -c -l -n -e -f -p -q -r -v`, and none of those is a switch verb: `-e` only
//! names the detach key, and pressing that key ends the attachment and leaves every
//! session running, so the client is gone rather than pointed somewhere else. There is
//! therefore no session change to follow: the nav follow that tmux's
//! `%client-session-changed` drives has nothing to fire on here, and the nav selection
//! standing still after a detach is the correct answer, not a missed update. A
//! session-follow path would carry a notification abduco cannot send about a move abduco
//! cannot make.

use crate::driver::{DriverCtx, MuxDriver};
use crate::model::Selection;

/// Per-session mux (abduco): one server per session, each shown through its own
/// `abduco -a <name>` client. `Abduco::driver` constructs it for a `PerSession` host.
pub struct AbducoDriver;

impl MuxDriver for AbducoDriver {
    fn kind(&self) -> &str {
        "abduco"
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
