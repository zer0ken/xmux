//! The psmux display driver: a per-session mux (one server per session) shown through
//! one client per session, kept while xmux runs.
//! `Psmux::driver` constructs it, so mux selection lives in the psmux implementation, not a
//! central match.

use crate::driver::{DriverCtx, MuxDriver};
use crate::model::Selection;

/// Per-session mux (psmux): one server per session, each shown through its own
/// `-f NUL attach -t <name>` client, which routes to that session's own server.
/// `Psmux::driver` constructs it for a `PerSession` host.
pub struct PsmuxDriver;

impl MuxDriver for PsmuxDriver {
    fn kind(&self) -> &str {
        "psmux"
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::registry::AttachRegistry;
    use crate::model::Selection;

    /// The worker, channels, and counters a `show` or `sync` runs against, with a
    /// spawner that answers every attach with a fake. Headless: no live psmux.
    fn run_driver<R>(
        hosts: &mut crate::model::Hosts,
        registry: &mut AttachRegistry,
        f: impl FnOnce(&mut PsmuxDriver, &mut DriverCtx) -> R,
    ) -> R {
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
            registry,
            hosts,
            instance_name: "test",
            mgr: &mgr,
            worker: &worker,
            pty_tx: &cap_tx,
            attach_seq: &mut attach_seq,
            viewport: (31, 25),
        };
        f(&mut PsmuxDriver, &mut ctx)
    }

    /// A local psmux host holding one live client, attachment `id`, on session `shows`.
    fn host_with_live_client(shows: &str, id: u64) -> (crate::model::Hosts, AttachRegistry) {
        let mut hosts = crate::model::Hosts::default();
        hosts.insert(crate::model::Host::new(
            crate::transport::local(None),
            crate::mux::for_binary("psmux").unwrap(),
        ));
        let key = crate::driver::session_display_key("local", shows);
        hosts
            .get_mut("local")
            .unwrap()
            .display
            .set_shows(&key, shows);
        let mut registry = AttachRegistry::new();
        registry.insert(&key, crate::display::attachment::fake_attachment(id));
        (hosts, registry)
    }

    fn sel(session: &str) -> Selection {
        Selection {
            host: "local".into(),
            session: session.into(),
        }
    }

    /// A session whose client xmux holds is shown without attaching anything, so a
    /// client psmux already put there is never torn down and respawned.
    #[tokio::test(flavor = "current_thread")]
    async fn psmux_driver_show_of_a_held_session_attaches_nothing() {
        let (mut hosts, mut registry) = host_with_live_client("vfy-ps-b", 42);

        assert!(
            run_driver(&mut hosts, &mut registry, |d, ctx| d
                .show(&sel("vfy-ps-b"), ctx)),
            "a held session is still a show, so the attach gate settles"
        );
        assert!(hosts.get("local").unwrap().display.in_flight_is_empty());
        assert_eq!(
            registry.get("local/vfy-ps-b").map(|a| a.id()),
            Some(42),
            "the held client stays on screen rather than being replaced"
        );
    }

    /// Each session gets its own client: a session change the nav drives attaches the
    /// new session under its own key, by session name, and leaves the first client open.
    /// A recorded client tty grants no in-place switch.
    #[tokio::test(flavor = "current_thread")]
    async fn psmux_driver_show_opens_one_connection_per_session() {
        let (mut hosts, mut registry) = host_with_live_client("vfy-ps-b", 42);
        hosts
            .get_mut("local")
            .unwrap()
            .record_display_tty("local/vfy-ps-b", Some("/dev/pts/3".into()));

        assert!(run_driver(&mut hosts, &mut registry, |d, ctx| d
            .show(&sel("vfy-ps-a"), ctx)));
        let h = hosts.get("local").unwrap();
        assert!(
            h.display.in_flight_contains("local/vfy-ps-a"),
            "another session is reached by a fresh attach under its own key"
        );
        assert_eq!(h.display.shows("local/vfy-ps-a"), Some("vfy-ps-a"));
        assert_eq!(
            registry.get("local/vfy-ps-b").map(|a| a.id()),
            Some(42),
            "the first session keeps its client"
        );
    }

    /// `sync` never warms (per-session attaches are opened on demand by `show`), and it
    /// ends only the connection of a session the inventory no longer lists.
    #[tokio::test(flavor = "current_thread")]
    async fn psmux_driver_sync_does_not_warm_and_ends_only_unlisted_sessions() {
        let (mut hosts, mut registry) = host_with_live_client("work", 7);
        let sessions = vec![
            crate::driver::tests::sess("local", "work"),
            crate::driver::tests::sess("local", "other"),
        ];
        run_driver(&mut hosts, &mut registry, |d, ctx| {
            d.sync("local", &sessions, ctx)
        });
        assert!(
            registry.contains("local/work"),
            "a listed session keeps its client"
        );
        assert!(
            hosts.get("local").unwrap().display.in_flight_is_empty(),
            "psmux sync never requests a warm spawn"
        );

        run_driver(&mut hosts, &mut registry, |d, ctx| {
            d.sync("local", &[], ctx)
        });
        assert!(
            !registry.contains("local/work"),
            "an empty psmux inventory ends the client"
        );
    }
}
