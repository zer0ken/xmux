//! The herdr display driver: one client per host. No external command can retarget a
//! named client, and every client of the user on one machine follows one saved-machine
//! selection, so a second client would move the first.

use crate::driver::{DriverCtx, MuxDriver};
use crate::model::Selection;

/// herdr display orchestration: one client per host, because every herdr client of the
/// user on one machine follows one saved-machine selection.
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
        // Every herdr client of the user on one machine follows one saved-machine
        // selection, so a second client moves the first. The host keeps one client: the
        // others end, and their last frames stay on screen until the new client paints.
        let others: Vec<String> = ctx
            .registry
            .addresses()
            .into_iter()
            .filter(|other| crate::driver::host_of_key(other) == sel.host && *other != key)
            .collect();
        for other in &others {
            if let Some(id) = ctx.registry.get(other).map(|att| att.id()) {
                ctx.registry.reap(id);
            }
            ctx.registry.remove_pending(other);
        }
        let host = ctx
            .hosts
            .get_mut(&sel.host)
            .expect("the selected host exists");
        for other in &others {
            host.display.clear(other);
            host.clear_display_tty(other);
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::registry::AttachRegistry;

    /// A second session of a herdr host ends the first session's client, since herdr
    /// clients on one machine share one saved-machine selection; the old frame stays on
    /// screen until the new client paints.
    #[tokio::test(flavor = "current_thread")]
    async fn herdr_driver_keeps_one_client_per_host() {
        let mut hosts = crate::model::Hosts::default();
        hosts.insert(crate::model::Host::new(
            crate::transport::ssh("box".into(), String::new(), "linux".into()),
            crate::mux::for_binary("herdr").unwrap(),
        ));
        let mut registry = AttachRegistry::new();
        let (first, ended) = crate::display::attachment::fake_attachment_with_teardown_flag(5);
        registry.insert("box/one", first);
        hosts
            .get_mut("box")
            .unwrap()
            .display
            .set_shows("box/one", "one");
        let mgr = crate::link::HostManager::new(tokio::sync::mpsc::unbounded_channel().0);
        let (ptx, _prx) = tokio::sync::mpsc::unbounded_channel();
        let worker = crate::display::DisplayWorker::with_spawner(
            ptx,
            Box::new(|_argv, _cols, _rows, id, _events, _env_clear| {
                Ok(crate::display::attachment::fake_attachment(id))
            }),
        );
        let mut attach_seq = 0u64;
        let (cap_tx, _cap_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut ctx = DriverCtx {
            registry: &mut registry,
            hosts: &mut hosts,
            instance_name: "test",
            mgr: &mgr,
            worker: &worker,
            pty_tx: &cap_tx,
            attach_seq: &mut attach_seq,
            viewport: (31, 25),
        };
        let sel = Selection {
            host: "box".into(),
            session: "two".into(),
        };
        assert!(HerdrDriver.show(&sel, &mut ctx));
        assert!(ended.load(std::sync::atomic::Ordering::SeqCst));
        assert!(!registry.contains("box/one"));
        assert!(registry.grid("box/one").is_some(), "the old frame stays");
        let h = hosts.get("box").unwrap();
        assert_eq!(h.display.shows("box/one"), None);
        assert!(h.display.in_flight_contains("box/two"));
    }
}
