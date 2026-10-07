//! The herdr display driver: each session change xmux makes uses a fresh client
//! attachment because no external command can retarget a named client.

use crate::driver::{DriverCtx, MuxDriver};
use crate::model::Selection;

/// herdr display orchestration through one per-host PTY, reattached whenever a
/// session is selected that the live client is not already on.
///
/// The client can move itself to another session on the same machine through a saved
/// machine, and following that move records the client's own report as what the
/// attachment shows before the nav moves. The selection arriving here then finds the
/// client already there, and the client the user just moved is kept.
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
        let Some(host) = ctx.hosts.get(&sel.host) else {
            return false;
        };
        let pre_mismatch = host.display.shows(&key) != Some(sel.session.as_str());
        if live && !pre_mismatch {
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
        let (attach, records, transport) = {
            let host = ctx
                .hosts
                .get_mut(&sel.host)
                .expect("the selected host exists");
            host.display.clear(&key);
            (
                host.mux.attach_plan(&sel.session),
                crate::driver::attach_records_client(host.transport.as_ref()),
                host.transport.clone_box(),
            )
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
        // A shell-run attach records the client's pid, which is what the host-side
        // query for where the client went names it by.
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

    fn sync(&mut self, id: &str, sessions: &[crate::session::Session], ctx: &mut DriverCtx) {
        if sessions.is_empty() {
            ctx.registry.remove(id);
            if let Some(host) = ctx.hosts.get_mut(id) {
                host.display.clear(id);
            }
        }
    }
}
