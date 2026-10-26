//! The tmux display driver: one display client per session, each kept attached while
//! xmux runs, so a switch to a session xmux already holds only changes which grid is on
//! view. `Tmux::driver` constructs it, so mux selection lives in the tmux
//! implementation, not a central match.

use crate::driver::{DriverCtx, MuxDriver};
use crate::model::Selection;

/// tmux: one display client per session. A remote attach records its own tty before it
/// execs the client, so the control connection's notices about a client can be matched
/// to the attachment they are about. `Tmux::driver` constructs it.
pub struct TmuxDriver;

impl MuxDriver for TmuxDriver {
    fn kind(&self) -> &str {
        "tmux"
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
        // A remote attach records its own tty before exec, so a control notice naming
        // a client can be matched to this attachment; a local attach stays bare.
        let mux_argv = host.mux.display_attach_plan(&sel.session);
        let command = host.transport.exec_argv(true, &mux_argv);
        let runs_through_shell = host.transport.runs_through_shell();
        let id = ctx
            .request_attach_with_id(sel, |id, key, instance_name| {
                let tty_key = super::display_tty_key(key, instance_name, id);
                with_display_tty_record(command, runs_through_shell, &tty_key)
            })
            .expect("the selected host exists");
        tracing::info!(addr = %key, id, count = ctx.registry.len(), "attach_created");
        crate::driver::log_display_inventory!(ctx, sel.session, pre_mismatch);
        true
    }
}

/// Folds the tmux record prefix into a shell-based attach's command (the last argv
/// element), so the attach shell records its OWN tty before exec'ing the attach - the
/// value the control connection reads back to tell xmux's own display client from the
/// user's own attached clients. An attach that does not run through a machine shell has
/// nowhere to run the snippet, so it is returned unchanged: folded into a local attach,
/// the prefix would corrupt the argv's session-name argument. The transport's
/// `runs_through_shell` answer gates the record here, in the driver, so the mux itself
/// stays transport-blind.
fn with_display_tty_record(
    command: crate::transport::CommandSpec,
    runs_through_shell: bool,
    host_key: &str,
) -> crate::transport::CommandSpec {
    command.map_argv(|mut argv| {
        if !runs_through_shell {
            return argv;
        }
        let prefix = super::record_prefix(host_key);
        if let Some(last) = argv.last_mut() {
            *last = format!("{prefix}{last}");
        }
        argv
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::registry::AttachRegistry;
    use crate::model::Selection;

    /// A REMOTE shared attach gets the mux's record prefix folded into its remote
    /// command (the last argv element), so the attach shell records its OWN tty for a
    /// later in-place switch - identifying xmux's display client and not the user's.
    #[test]
    fn remote_shared_attach_records_its_display_tty() {
        let host = crate::model::Host::new(
            crate::transport::ssh("jup".into(), String::new(), "linux".into()),
            crate::mux::for_binary("tmux").unwrap(),
        );
        let argv = vec![
            "ssh".to_string(),
            "jup".to_string(),
            "tmux attach -t api".to_string(),
        ];
        let out = with_display_tty_record(
            crate::transport::CommandSpec::from_argv(argv),
            host.transport.runs_through_shell(),
            "jup",
        );
        let last = out.last().unwrap();
        assert!(last.starts_with("tty >"), "records its tty first: {out:?}");
        assert!(
            last.contains("tmux attach -t api"),
            "then runs the attach: {out:?}"
        );
    }

    /// A LOCAL attach has no shell to run the record snippet, so it is left bare -
    /// prepending the snippet would corrupt the local argv's session-name argument.
    #[test]
    fn local_shared_attach_is_not_prefixed() {
        let host = crate::model::Host::new(
            crate::transport::local(None),
            crate::mux::for_binary("tmux").unwrap(),
        );
        let argv = vec![
            "tmux".to_string(),
            "attach".to_string(),
            "-t".to_string(),
            "api".to_string(),
        ];
        let out = with_display_tty_record(
            crate::transport::CommandSpec::from_argv(argv.clone()),
            host.transport.runs_through_shell(),
            "local",
        );
        assert_eq!(out, argv, "local attach is untouched");
    }

    /// The worker, channels, and counters a `show` or `sync` runs against, with a
    /// spawner that answers every attach with a fake.
    fn run_driver<R>(
        hosts: &mut crate::model::Hosts,
        registry: &mut AttachRegistry,
        f: impl FnOnce(&mut TmuxDriver, &mut DriverCtx) -> R,
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
        f(&mut TmuxDriver, &mut ctx)
    }

    fn jup_hosts() -> crate::model::Hosts {
        let mut hosts = crate::model::Hosts::default();
        hosts.insert(crate::model::Host::new(
            crate::transport::ssh("jup".into(), String::new(), "linux".into()),
            crate::mux::for_binary("tmux").unwrap(),
        ));
        hosts
    }

    fn sel(session: &str) -> Selection {
        Selection {
            host: "jup".into(),
            session: session.into(),
        }
    }

    /// Each session gets its own display client: showing a second session of the same
    /// host opens a connection for it and leaves the first one attached.
    #[tokio::test(flavor = "current_thread")]
    async fn tmux_driver_show_opens_one_connection_per_session() {
        let mut hosts = jup_hosts();
        let mut registry = AttachRegistry::new();
        registry.insert("jup/api", crate::display::attachment::fake_attachment(5));
        hosts
            .get_mut("jup")
            .unwrap()
            .display
            .set_shows("jup/api", "api");

        assert!(run_driver(&mut hosts, &mut registry, |d, ctx| d
            .show(&sel("build"), ctx)));
        let h = hosts.get("jup").unwrap();
        assert!(
            h.display.in_flight_contains("jup/build"),
            "the second session is attached off-loop under its own key"
        );
        assert_eq!(h.display.shows("jup/build"), Some("build"));
        assert_eq!(
            registry.get("jup/api").map(|att| att.id()),
            Some(5),
            "the first session keeps its client"
        );
    }

    /// A session whose client xmux holds is shown without attaching anything.
    #[tokio::test(flavor = "current_thread")]
    async fn tmux_driver_show_of_a_held_session_attaches_nothing() {
        let mut hosts = jup_hosts();
        let mut registry = AttachRegistry::new();
        registry.insert("jup/api", crate::display::attachment::fake_attachment(5));
        hosts
            .get_mut("jup")
            .unwrap()
            .display
            .set_shows("jup/api", "api");

        assert!(run_driver(&mut hosts, &mut registry, |d, ctx| d.show(&sel("api"), ctx)));
        assert!(hosts.get("jup").unwrap().display.in_flight_is_empty());
        assert_eq!(registry.get("jup/api").map(|att| att.id()), Some(5));
    }

    /// An inventory never opens a display, and it ends only the connection of a session
    /// it no longer lists.
    #[tokio::test(flavor = "current_thread")]
    async fn tmux_driver_sync_ends_only_unlisted_sessions() {
        let mut hosts = jup_hosts();
        let mut registry = AttachRegistry::new();
        for (key, id) in [("jup/api", 5), ("jup/gone", 6)] {
            registry.insert(key, crate::display::attachment::fake_attachment(id));
            hosts
                .get_mut("jup")
                .unwrap()
                .display
                .set_shows(key, crate::driver::session_of_key(key));
        }
        let sessions = vec![
            crate::driver::tests::sess("jup", "api"),
            crate::driver::tests::sess("jup", "build"),
        ];
        run_driver(&mut hosts, &mut registry, |d, ctx| {
            d.sync("jup", &sessions, ctx)
        });
        let h = hosts.get("jup").unwrap();
        assert!(
            registry.contains("jup/api"),
            "a listed session keeps its client"
        );
        assert!(!registry.contains("jup/gone"), "an unlisted one loses it");
        assert_eq!(h.display.shows("jup/gone"), None);
        assert!(
            h.display.in_flight_is_empty(),
            "inventory cannot open a display"
        );

        run_driver(&mut hosts, &mut registry, |d, ctx| d.sync("jup", &[], ctx));
        assert!(registry.is_empty(), "no sessions ⇒ no connection");
    }
}
