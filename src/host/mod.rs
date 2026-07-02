//! Per-host metadata channels: the shared vocabulary plus the reader, writer,
//! client, poll, and manager concerns, each in its own submodule.

use std::collections::HashMap;

#[cfg(test)]
use crate::mux::ControlProtocol;

mod client;
mod inventory;
mod reader;
mod writer;

pub use client::HostClient;
pub use inventory::{HostCmd, HostEvent, HostInventory, InFlight, PendingReply, ReaderState};
pub use reader::run_reader;
pub use writer::run_writer;

/// A POLL host's self-looping enumeration task. A poll host has no host-level control
/// stream, so the [`HostManager`] owns this task to re-enumerate sessions + panes on
/// the mux's cadence and emit them as [`HostEvent`]s onto the same bus the control
/// clients use. Runs until aborted (reap / teardown) or the event receiver is dropped
/// (app exit). Mirrors a control client's connect-then-stream role for poll muxes.
async fn run_poll(
    source: String,
    transport: Box<dyn crate::machine::Transport>,
    mux: Box<dyn crate::mux::Mux>,
    interval_ms: u64,
    events: tokio::sync::mpsc::UnboundedSender<HostEvent>,
) {
    // Fixed-cadence ticker: the first tick is immediate (enumerate on spawn), then a
    // sweep every `interval_ms` of wall-clock. Skip ticks missed while one enumeration
    // ran long, so a slow probe paces the loop instead of piling up overlapping sweeps.
    let mut ticker = tokio::time::interval(std::time::Duration::from_millis(interval_ms));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Per-source last-known name set: suppress INFO when the enumeration is identical to
    // the previous sweep (reduces log noise for idle polls while keeping change visibility).
    let mut last_names: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut first_poll = true;
    loop {
        ticker.tick().await;
        // `poll_once` (the mux-blind sweep) hands each event back here. The app's
        // receiver dropping (its exit) is the loop's other stop condition besides abort,
        // so a failed send latches `gone` and the loop returns after this sweep.
        let mut gone = false;
        mux.poll_once(&source, &transport, &crate::source::ExecRunner, &mut |ev| {
            // Log enumeration at the producer (where `err` is in hand): on success emit
            // INFO when the set changed, TRACE when unchanged; on error emit WARN. This
            // keeps the log quiet for idle polls while making changes and failures visible.
            if let HostEvent::Sessions {
                source: ref host,
                ref sessions,
                ref err,
            } = ev
            {
                let n = sessions.len();
                if let Some(error) = err {
                    tracing::warn!(host, error, "enumeration_failed");
                } else {
                    let names: std::collections::BTreeSet<String> =
                        sessions.iter().map(|s| s.name.clone()).collect();
                    if first_poll || names != last_names {
                        let names_list: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
                        tracing::info!(host, n, names = ?names_list, "sessions_enumerated");
                        last_names = names;
                        first_poll = false;
                    } else {
                        tracing::trace!(host, n, "sessions_enumerated_unchanged");
                    }
                }
            }
            if events.send(ev).is_err() {
                gone = true;
            }
        })
        .await;
        if gone {
            return;
        }
    }
}

/// The `-CC` control child's argv for `host`, composed across the two orthogonal axes:
/// the MUX supplies the control payload via `Mux::control_argv` (never a hardcoded
/// `-CC attach` literal), and the MACHINE wraps it via `Transport::control_argv` (local
/// `-S` splice, or `ssh -tt … <payload>`). `None` for a mux with no host-level control
/// stream (it is polled), so a Poll host produces no argv.
fn control_argv(host: &crate::model::Host) -> Option<Vec<String>> {
    let mux_control = host.mux.control_argv()?;
    Some(host.transport.control_argv(&mux_control))
}

/// Owns each host's metadata channel, spawned lazily on first use and reaped on
/// `%exit`/EOF (control) or abort (poll). A CONTROL host gets one `-CC` [`HostClient`];
/// a POLL host gets one [`run_poll`] task. The bound is the host count: at most one of
/// either per host. Both emit onto the one shared `events` sink the app's loop drains.
pub struct HostManager {
    clients: HashMap<String, HostClient>,
    polls: HashMap<String, tokio::task::JoinHandle<()>>,
    events: tokio::sync::mpsc::UnboundedSender<HostEvent>,
}

impl HostManager {
    pub fn new(events: tokio::sync::mpsc::UnboundedSender<HostEvent>) -> Self {
        Self {
            clients: HashMap::new(),
            polls: HashMap::new(),
            events,
        }
    }

    /// A clone of the shared event-bus sender, for the fire-and-forget detection task
    /// (`spawn_host_detection`) that emits `HostEvent::Scanned` onto the same bus.
    pub fn events(&self) -> tokio::sync::mpsc::UnboundedSender<HostEvent> {
        self.events.clone()
    }

    /// Ensures `id`'s metadata channel is live, picking the channel from the host's
    /// `event_source()` — the ONE place that reads it. CONTROL → spawn a `-CC` client
    /// (connect sequence queued by `HostClient::spawn`); POLL → spawn a self-looping
    /// poll task at the mux's interval. A no-op (`Ok(false)`) if already live.
    pub fn ensure(
        &mut self,
        id: &str,
        host: &crate::model::Host,
        cols: u16,
        rows: u16,
    ) -> anyhow::Result<bool> {
        // A finished poll task leaves a dead JoinHandle in the map (the loop is otherwise
        // infinite, so this only happens if its body panicked). Drop it so this re-ensure
        // (startup, selection move, or the reconnect sweep) respawns it instead of treating
        // the corpse as live — this is what makes the reconnect sweep a real liveness check.
        if self.polls.get(id).is_some_and(|h| h.is_finished()) {
            self.polls.remove(id);
        }
        if self.clients.contains_key(id) || self.polls.contains_key(id) {
            return Ok(false);
        }
        match host.mux.event_source() {
            crate::model::EventSource::Control => {
                // A Control event source guarantees a control protocol (both come from
                // the same mux): tmux is the only mux that reports either.
                let proto = host.mux.control_protocol().ok_or_else(|| {
                    anyhow::anyhow!("mux has a control event source but no control protocol")
                })?;
                // The control argv composes the two orthogonal axes: the mux payload from
                // Mux::control_argv wrapped by Transport::control_argv (no hardcoded verb,
                // no hand-rolled ssh/-S here). A Control event source guarantees a payload.
                let argv = control_argv(host).ok_or_else(|| {
                    anyhow::anyhow!("mux has a control event source but no control argv")
                })?;
                let client =
                    HostClient::spawn(id, proto, &argv, cols, rows, self.events.clone(), &[])?;
                self.clients.insert(id.to_string(), client);
            }
            crate::model::EventSource::Poll { interval_ms } => {
                let handle = tokio::spawn(run_poll(
                    id.to_string(),
                    host.transport.clone(),
                    host.mux.clone_box(),
                    interval_ms,
                    self.events.clone(),
                ));
                self.polls.insert(id.to_string(), handle);
            }
        }
        Ok(true)
    }

    pub fn get(&self, host: &str) -> Option<&HostClient> {
        self.clients.get(host)
    }

    /// Immediate re-enumeration on demand (`r` / menu reconnect). A CONTROL host
    /// re-issues list-sessions; a POLL host's task is aborted and respawned so the next
    /// enumeration fires NOW instead of at the next interval. Branches on which channel
    /// the manager holds — it does NOT read the mux's event source.
    pub fn rescan(&mut self, id: &str, host: &crate::model::Host, cols: u16, rows: u16) {
        if let Some(c) = self.clients.get(id) {
            c.list_sessions();
            return;
        }
        if let Some(h) = self.polls.remove(id) {
            h.abort();
            let _ = self.ensure(id, host, cols, rows);
        }
    }

    /// `%exit`/EOF (control) or explicit drop (poll): tear down the channel. The app
    /// keeps the last-known tree in its switcher state, so the inventory is not refetched.
    pub fn reap(&mut self, host: &str) {
        if let Some(c) = self.clients.remove(host) {
            c.teardown();
        }
        if let Some(h) = self.polls.remove(host) {
            h.abort();
        }
    }

    pub fn resize_all(&mut self, cols: u16, rows: u16) {
        for c in self.clients.values_mut() {
            c.resize(cols, rows);
        }
    }

    /// Drains and tears down every channel (bounded join per control client; abort per poll task).
    pub fn teardown_all(self) {
        for (_, c) in self.clients {
            c.teardown();
        }
        for (_, h) in self.polls {
            h.abort();
        }
    }
}

/// The shared `'static` tmux control protocol, for tests that drive the reader/writer
/// or spawn a fake control child. Both the `host` and `app` test modules use it.
#[cfg(test)]
pub(crate) fn test_control_proto() -> &'static dyn ControlProtocol {
    crate::mux::for_binary("tmux")
        .control_protocol()
        .expect("tmux has a control protocol")
}

#[cfg(test)]
impl HostManager {
    /// Inserts a real no-op control child keyed by `host`, proving the map insert
    /// without a live `-CC` server. `cmd.exe /c rem` spawns and exits immediately,
    /// so its stdout EOFs at once and `teardown`'s joins return. Shared by the
    /// `host` and `app` test modules.
    pub(crate) fn insert_fake(&mut self, host: &str) {
        let argv: Vec<String> = ["cmd.exe", "/c", "rem"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let client = HostClient::spawn(
            host,
            test_control_proto(),
            &argv,
            80,
            24,
            self.events.clone(),
            &[],
        )
        .expect("spawn");
        self.clients.insert(host.to_string(), client);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // LIVE: connects to the real `jupiter06` over ssh and verifies the control-mode
    // METADATA path end-to-end — connect → list-sessions resolves → inventory has the
    // host's real sessions. Uses PIPES (not a ConPTY), so it works headlessly even
    // inside a mux. `#[ignore]` because it needs network + the host reachable:
    //   cargo test -p xmux host::tests::live_jupiter06 -- --ignored --nocapture
    #[ignore = "live: ssh to jupiter06; run on demand"]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn live_jupiter06_control_lists_sessions() {
        use std::time::{Duration, Instant};
        let host = crate::model::Host::new(
            crate::machine::ssh("jupiter06".into(), String::new(), "linux".into()),
            crate::mux::for_binary("tmux"),
        );
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<HostEvent>();
        let mut mgr = HostManager::new(tx);
        mgr.ensure("jupiter06", &host, 80, 24)
            .expect("spawn control client");
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut sessions = Vec::new();
        let mut connected = false;
        while !connected && Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_secs(20), rx.recv()).await {
                Ok(Some(HostEvent::Connected { sessions: s, .. })) => {
                    // The event carries the parsed inventory (no shared lock to read).
                    sessions = s;
                    connected = true;
                }
                Ok(Some(_)) => continue,
                _ => break,
            }
        }
        assert!(
            connected,
            "control client must connect to jupiter06 + resolve list-sessions"
        );
        eprintln!(
            "jupiter06 sessions: {:?}",
            sessions.iter().map(|s| &s.name).collect::<Vec<_>>()
        );
        assert!(
            !sessions.is_empty(),
            "jupiter06 inventory must list its real sessions"
        );
        mgr.teardown_all();
    }

    /// A constructible LOCAL `Source` for the manager tests: its runner defaults to the
    /// real exec runner and its `cmd.exe` binary is a real local program, so if `ensure`
    /// ever did spawn it the process would exist rather than fail to launch. In these
    /// tests it stays dormant — `ensure` on an already-present host returns `Ok(false)`
    /// and a poll host's task is aborted before its runner is exercised.
    fn ssh_host(alias: &str, bin: &str, os: &str, control_path: &str) -> crate::model::Host {
        crate::model::Host::new(
            crate::machine::ssh(alias.into(), control_path.into(), os.into()),
            crate::mux::for_binary(bin),
        )
    }

    fn local_host(bin: &str, socket: Option<&str>) -> crate::model::Host {
        crate::model::Host::new(
            crate::machine::local(socket.map(str::to_string)),
            crate::mux::for_binary(bin),
        )
    }

    #[test]
    fn control_argv_local_default_socket_is_bare_cc_attach() {
        // A local Control host (tmux, default socket) spawns `[bin, -CC, attach]`.
        let host = local_host("tmux", None);
        assert_eq!(
            control_argv(&host),
            Some(vec!["tmux".to_string(), "-CC".into(), "attach".into()])
        );
    }

    #[test]
    fn control_argv_local_non_default_socket_injects_dash_s() {
        // A local Control host on a non-default socket splices `-S <sock>` after the binary.
        let host = local_host("tmux", Some("/tmp/tmux-1000/work"));
        assert_eq!(
            control_argv(&host),
            Some(vec![
                "tmux".to_string(),
                "-S".into(),
                "/tmp/tmux-1000/work".into(),
                "-CC".into(),
                "attach".into()
            ])
        );
    }

    #[test]
    fn control_argv_remote_forces_pty_over_batch_ssh() {
        // A remote Control host forces a pty (`-tt`) and runs `<bin> -CC attach` over
        // a BatchMode ssh connection.
        let host = ssh_host("prod", "tmux", "linux", "");
        let got = control_argv(&host).expect("a Control host has a control argv");
        assert_eq!(got[0], "ssh");
        assert!(got.iter().any(|s| s == "-tt"), "{got:?}");
        assert!(
            got.iter().any(|s: &String| s.contains("BatchMode=yes")),
            "{got:?}"
        );
        assert_eq!(got.last().unwrap(), "tmux -CC attach");
    }

    #[test]
    fn control_argv_is_the_transport_over_backend_composition() {
        // The mux payload comes from Mux::control_argv (NOT a hardcoded literal),
        // and the machine wrapping comes from Transport::control_argv — the two compose.
        for host in [
            local_host("tmux", None),
            local_host("tmux", Some("/tmp/tmux-1000/work")),
            ssh_host("prod", "tmux", "linux", ""),
        ] {
            let mux_payload = host
                .mux
                .control_argv()
                .expect("tmux supplies a control argv");
            assert_eq!(
                control_argv(&host),
                Some(host.transport.control_argv(&mux_payload)),
                "control argv must equal transport.control_argv(&mux.control_argv())"
            );
        }
    }

    #[test]
    fn control_argv_is_none_for_a_poll_backend() {
        // A psmux (Poll) host has no host-level control stream, so no control argv.
        let host = local_host("psmux", None);
        assert_eq!(control_argv(&host), None);
    }

    #[test]
    fn manager_ensure_is_idempotent() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<HostEvent>();
        let mut mgr = HostManager::new(tx);
        mgr.insert_fake("jupiter06");
        assert!(mgr.get("jupiter06").is_some());
        // ensure on an already-connected host returns Ok(false) (no fresh connect).
        let host =
            crate::model::Host::new(crate::machine::local(None), crate::mux::for_binary("psmux"));
        assert!(!mgr.ensure("jupiter06", &host, 80, 24).unwrap());
    }

    #[test]
    fn manager_reap_drops_client() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<HostEvent>();
        let mut mgr = HostManager::new(tx);
        mgr.insert_fake("jupiter06");
        mgr.reap("jupiter06");
        assert!(mgr.get("jupiter06").is_none(), "reaped client is dropped");
    }

    #[tokio::test]
    async fn manager_ensure_poll_host_owns_poll_task_lifecycle() {
        // A poll host (psmux, EventSource::Poll) gets a self-looping poll TASK owned by
        // the manager — not a control client. ensure is idempotent while the task lives;
        // reap aborts it so a later ensure re-spawns it. get() returns None throughout
        // (a poll host has no `-CC` control client, only the poll task).
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<HostEvent>();
        let mut mgr = HostManager::new(tx);
        let host = crate::model::Host::new(
            crate::machine::local(None),
            crate::mux::for_kind("psmux", "psmux-no-such-binary"),
        );
        assert!(
            mgr.ensure("local", &host, 80, 24).unwrap(),
            "first ensure spawns the poll task"
        );
        assert!(
            mgr.get("local").is_none(),
            "a poll host has no control client"
        );
        assert!(
            !mgr.ensure("local", &host, 80, 24).unwrap(),
            "ensure is idempotent while the poll task lives"
        );
        mgr.reap("local");
        assert!(
            mgr.ensure("local", &host, 80, 24).unwrap(),
            "reap aborted the task so ensure re-spawns it"
        );
        mgr.teardown_all();
    }

    #[tokio::test]
    async fn ensure_needs_no_source_arg() {
        // ensure composes the control/poll channel from the host alone (transport × mux);
        // it takes no Source. A poll host (psmux) is idempotent while its task lives.
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<HostEvent>();
        let mut mgr = HostManager::new(tx);
        let host = crate::model::Host::new(
            crate::machine::local(None),
            crate::mux::for_kind("psmux", "psmux-no-such-binary"),
        );
        assert!(
            mgr.ensure("local", &host, 80, 24).unwrap(),
            "first ensure spawns the poll task without a source"
        );
        assert!(
            !mgr.ensure("local", &host, 80, 24).unwrap(),
            "ensure is idempotent while the poll task lives"
        );
        mgr.teardown_all();
    }
}
