//! The tuios display driver: one daemon owns every session, but no external command
//! can retarget a particular client, so each session is shown through its own client.

use crate::driver::{DriverCtx, MuxDriver};
use crate::model::Selection;

/// tuios display orchestration through one client per session.
pub struct TuiosDriver;

impl MuxDriver for TuiosDriver {
    fn kind(&self) -> &str {
        "tuios"
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
