//! The herdr display driver: each selection uses a fresh client attachment because
//! no external command can retarget a named client.

use crate::driver::{DriverCtx, MuxDriver};
use crate::model::Selection;

/// herdr display orchestration through one per-host PTY, reattached whenever a
/// session is selected.
pub struct HerdrDriver;

impl MuxDriver for HerdrDriver {
    fn kind(&self) -> &str {
        "herdr"
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
        if sessions.is_empty() {
            ctx.registry.remove(id);
            if let Some(host) = ctx.hosts.get_mut(id) {
                host.display.clear(id);
            }
        }
    }
}
