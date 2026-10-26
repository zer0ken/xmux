//! The zellij display driver: one client per session, kept while xmux runs.
//! `Zellij::driver` constructs it, so mux selection lives in the zellij implementation,
//! not a central match.
//!
//! There is no in-place client switch to make. zellij's `switch-session` moves
//! whichever client RUNS it, and a client cannot be named from outside the session it
//! is in. A session's first attach keeps the view on screen until its paint gate opens,
//! either after settled output or the bounded wait.

use crate::driver::{DriverCtx, MuxDriver};
use crate::model::Selection;

/// Per-session mux (zellij): one server per session, each shown through its own client.
///
/// Following a client switch re-keys the attachment to the client's own report BEFORE
/// the nav moves, so the selection arriving here finds the client already there, and
/// the client the user just moved is never torn down to reach the session it is in.
pub struct ZellijDriver;

impl MuxDriver for ZellijDriver {
    fn kind(&self) -> &str {
        "zellij"
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::registry::AttachRegistry;
    use crate::model::Selection;

    /// A headless zellij host with one live display attachment already on record: the
    /// registry holds attachment `id` under the session's key, and the host's display
    /// bookkeeping says that attachment shows `shows`. No live zellij is involved; the
    /// spawner is a fake, so an attach shows up as a request and nothing else.
    fn host_with_live_client(shows: &str, id: u64) -> (crate::model::Hosts, AttachRegistry) {
        let mut hosts = crate::model::Hosts::default();
        hosts.insert(crate::model::Host::new(
            crate::transport::local(None),
            crate::mux::for_binary("zellij").unwrap(),
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

    /// Runs the driver's `show` for `session` against those facts and answers what it
    /// returned, keeping the ctx borrow inside so the caller can read the hosts back.
    fn show_session(
        hosts: &mut crate::model::Hosts,
        registry: &mut AttachRegistry,
        session: &str,
    ) -> bool {
        let (ptx, _prx) = tokio::sync::mpsc::unbounded_channel();
        let worker = crate::display::DisplayWorker::with_spawner(
            ptx,
            Box::new(|_argv, _cols, _rows, id, _events, _env_clear| {
                Ok(crate::display::attachment::fake_attachment(id))
            }),
        );
        let mut attach_seq = 0u64;
        let (cap_tx, _cap_rx) = tokio::sync::mpsc::unbounded_channel();
        let sel = Selection {
            host: "local".into(),
            session: session.into(),
        };
        let mgr = crate::link::HostManager::new(tokio::sync::mpsc::unbounded_channel().0);
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
        ZellijDriver.show(&sel, &mut ctx)
    }

    /// zellij moved xmux's own client with `switch-session`, and the follow re-keyed it
    /// to that session before moving the nav, so by the time the selection reaches `show`
    /// xmux already holds the session's connection. The decision must then be WARM: an
    /// attach here would spawn a second `zellij attach` for a session xmux's own client
    /// is already in.
    #[tokio::test(flavor = "current_thread")]
    async fn a_followed_switch_warms_the_client_the_mux_already_moved() {
        let (mut hosts, mut registry) = host_with_live_client("vfy-ze-b", 42);
        let shown = show_session(&mut hosts, &mut registry, "vfy-ze-b");

        assert!(shown, "a selection with a session has something to show");
        let h = hosts.get("local").unwrap();
        assert!(
            !h.display.in_flight_contains("local/vfy-ze-b"),
            "the client is already there, so nothing is reattached"
        );
        assert_eq!(
            registry.get("local/vfy-ze-b").map(|a| a.id()),
            Some(42),
            "the same client stays on screen: it was never respawned"
        );
        assert_eq!(h.display.shows("local/vfy-ze-b"), Some("vfy-ze-b"));
    }

    /// The detach recovery's driver decision: the display client is GONE (its
    /// attachment EOF'd and was reaped) while the display belief still names the
    /// selected session. The warm branch rests on a LIVE client, never on the belief
    /// alone, so a dead one is reached by a fresh attach and never held up as if it
    /// were still showing the session.
    #[tokio::test(flavor = "current_thread")]
    async fn a_dead_display_client_is_reattached_not_warmed() {
        let (mut hosts, mut registry) = host_with_live_client("vfy-ze-b", 42);
        registry.remove("local/vfy-ze-b"); // the client's attachment EOF'd and was reaped
        let shown = show_session(&mut hosts, &mut registry, "vfy-ze-b");

        assert!(shown);
        let h = hosts.get("local").unwrap();
        assert!(
            h.display.in_flight_contains("local/vfy-ze-b"),
            "a dead client is reached by a fresh attach, not held as warm"
        );
        assert_eq!(h.display.shows("local/vfy-ze-b"), Some("vfy-ze-b"));
    }

    /// The other half: a session change the NAV drives to a session xmux does not hold
    /// is a fresh attach under that session's own key, so following a mux-side switch
    /// cannot wedge the next one, and the first session keeps its client.
    #[tokio::test(flavor = "current_thread")]
    async fn a_nav_driven_switch_away_opens_its_own_connection() {
        let (mut hosts, mut registry) = host_with_live_client("vfy-ze-b", 42);
        let shown = show_session(&mut hosts, &mut registry, "vfy-ze-a");

        assert!(shown);
        let h = hosts.get("local").unwrap();
        assert!(
            h.display.in_flight_contains("local/vfy-ze-a"),
            "another session is reached by a fresh attach under its own key"
        );
        assert_eq!(h.display.shows("local/vfy-ze-a"), Some("vfy-ze-a"));
        assert_eq!(
            registry.get("local/vfy-ze-b").map(|a| a.id()),
            Some(42),
            "the first session keeps its client"
        );
    }
}
