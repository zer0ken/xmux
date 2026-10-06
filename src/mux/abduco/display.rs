//! The abduco display driver: a per-session mux (one server per session) displayed
//! through ONE per-host PTY that is REATTACHED whenever the selected session changes.
//! `Abduco::driver` constructs it, so mux selection lives in the abduco implementation, not a
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

/// Per-session mux (abduco): one server per session, displayed through ONE per-host
/// PTY that is REATTACHED whenever the selected session changes (`abduco -a <name>`
/// attaches to that session's own server). `Abduco::driver` constructs it for a
/// `PerSession` host.
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
        let live = ctx.registry.contains(&key);
        let (pre_mismatch, command) = {
            let Some(host) = ctx.hosts.get_mut(&sel.host) else {
                return false;
            };
            let pre_mismatch = host.display.shows(&key) != Some(sel.session.as_str());
            host.display.clear(&key);
            let mux_argv = host.mux.attach_plan(&sel.session);
            let command = host.transport.exec_argv(true, &mux_argv);
            (pre_mismatch, command)
        };

        // REATTACH, always: the only way to move abduco's display. The stale attachment
        // is KEPT in the registry so its grid stays on screen until the fresh client
        // paints or reaches its bounded wait (stale-while-revalidate). At first display
        // there is nothing to keep, so Ready installs immediately.
        let reason = if live { "reshow" } else { "no-live-client" };
        tracing::info!(
            host = %sel.host,
            model = "per-session",
            decision = "reattach",
            reason,
            session = %sel.session,
            "display_show"
        );
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
