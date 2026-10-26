//! The herdr display driver: one client per session, because no external command can
//! retarget a named client.

use crate::driver::{DriverCtx, MuxDriver};
use crate::model::Selection;

/// herdr display orchestration through one client per session.
///
/// The client can move itself to another session on the same machine through a saved
/// machine; following that move re-keys the attachment to that session before the nav
/// moves, so the selection arriving here finds the client already there and keeps it.
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
        let attach = host.mux.display_attach_plan(&sel.session);
        let records = crate::driver::attach_records_client(host.transport.as_ref());
        let transport = host.transport.clone_box();
        let id = ctx
            .request_attach_with_id(sel, |id, key, instance_name| {
                if records {
                    let record = crate::mux::display_tty_key(key, instance_name, id);
                    transport.exec_argv(true, &crate::mux::recording_attach(&attach, &record))
                } else {
                    transport.exec_argv(true, &attach)
                }
            })
            .expect("the selected host exists");
        tracing::info!(addr = %key, id, count = ctx.registry.len(), "attach_created");
        crate::driver::log_display_inventory!(ctx, sel.session, pre_mismatch);
        true
    }
}
