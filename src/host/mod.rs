//! Per-host metadata channels: the shared vocabulary plus the reader, writer,
//! client, poll, and manager concerns, each in its own submodule.

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::mux::ControlProtocol;

mod inventory;
mod reader;
mod writer;

pub use inventory::{HostCmd, HostEvent, HostInventory, InFlight, PendingReply, ReaderState};
pub use reader::run_reader;
pub use writer::run_writer;

/// One control-mode (`-CC`) host process: a piped child plus its reader and writer
/// OS threads. The app holds the `cmd_tx` to drive it and reads `connecting` for the
/// spinner; the session/window inventory is carried on `HostEvent`s and owned by
/// `model::Host.inventory`. This is a METADATA / change-event / `select-window`
/// channel only — the per-session PTY attachments own the pixels.
pub struct HostClient {
    /// Stable host id (the source name), echoed back on every `HostEvent`.
    pub host: String,
    /// True until any wire activity proves the channel is live.
    pub connecting: Arc<AtomicBool>,
    /// Current client size; updated by `resize`.
    pub size: (u16, u16),
    /// The mux's control-mode protocol — builds every command line this client
    /// sends. Shared `'static` (the impl is stateless), so the reader/writer threads
    /// borrow it without owning a clone.
    proto: &'static dyn ControlProtocol,
    /// Queue commands to the writer thread.
    cmd_tx: std::sync::mpsc::Sender<HostCmd>,
    child: Child,
    reader: Option<JoinHandle<()>>,
    writer: Option<JoinHandle<()>>,
    /// Drains the child's stderr to EOF so a child that writes more than the pipe
    /// buffer (ssh banners/warnings) cannot block and wedge the connection.
    stderr_drain: Option<JoinHandle<()>>,
}

impl HostClient {
    /// Spawns `argv` as a piped control-mode child at `cols×rows`, starts the
    /// reader + writer OS threads, and queues the connect sequence (resize →
    /// flow-control pause → list-sessions). `events` is the app's loop sink.
    pub fn spawn(
        host: impl Into<String>,
        proto: &'static dyn ControlProtocol,
        argv: &[String],
        cols: u16,
        rows: u16,
        events: tokio::sync::mpsc::UnboundedSender<HostEvent>,
        extra_env: &[(&str, &str)],
    ) -> anyhow::Result<HostClient> {
        anyhow::ensure!(
            !argv.is_empty(),
            "HostClient::spawn: argv must not be empty"
        );
        let host = host.into();

        let mut cmd = Command::new(&argv[0]);
        cmd.args(&argv[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Strip EVERY mux session var (all `PSMUX*`, `TMUX`, `TMUX_PANE` — see
        // `mux::vocab::is_mux_var`), not just `PSMUX_SESSION`: a per-session psmux
        // control child must not inherit stale psmux routing state (e.g. an
        // ambient `PSMUX_SESSION_NAME`) that could override its `-s <session>`
        // target and attach the wrong server.
        for (k, _) in std::env::vars() {
            if crate::mux::vocab::is_mux_var(&k) {
                cmd.env_remove(&k);
            }
        }
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn()?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("child stdout missing"))?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("child stdin missing"))?;
        let mut stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("child stderr missing"))?;

        // Drain stderr to EOF and discard it. Without this, a child that writes more
        // than the pipe buffer to stderr (ssh banners/warnings) blocks and wedges the
        // connection. EOF arrives when the child dies (like stdout), so the join is bounded.
        let stderr_drain = std::thread::spawn(move || {
            let _ = std::io::copy(&mut stderr, &mut std::io::sink());
        });

        let connecting = Arc::new(AtomicBool::new(true));
        let in_flight: InFlight = Arc::new(Mutex::new(VecDeque::new()));
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<HostCmd>();

        // Reader thread: stdout lines → state machine; events to the async loop via
        // the non-blocking, thread-safe UnboundedSender.
        let state = ReaderState {
            connecting: Arc::clone(&connecting),
        };
        let reader_host = host.clone();
        let reader_in_flight = Arc::clone(&in_flight);
        let reader_events = events.clone();
        let reader = std::thread::spawn(move || {
            let lines = BufReader::new(stdout).lines().map_while(Result::ok);
            run_reader(&reader_host, proto, lines, &state, &reader_in_flight, |e| {
                let _ = reader_events.send(e);
            });
        });

        // Writer thread: owns the child stdin, drains the command channel.
        let writer_in_flight = Arc::clone(&in_flight);
        let writer = std::thread::spawn(move || {
            run_writer(cmd_rx, proto, &mut stdin, &writer_in_flight);
        });

        // Connect sequence: size the client, then run the mux's connect preamble
        // (it SUPPRESSES %output — this control connection is a metadata / change-event /
        // `select-window` channel ONLY; the per-session PTY attaches own the pixels), then
        // list sessions (the correlated query whose block resolves the inventory).
        let _ = cmd_tx.send(HostCmd::Resize { cols, rows });
        for line in proto.connect_lines() {
            let _ = cmd_tx.send(HostCmd::Send(line));
        }
        let _ = cmd_tx.send(HostCmd::Query {
            line: proto.list_sessions_line(),
            reply: PendingReply::ListSessions,
        });

        Ok(HostClient {
            host,
            connecting,
            size: (cols, rows),
            proto,
            cmd_tx,
            child,
            reader: Some(reader),
            writer: Some(writer),
            stderr_drain: Some(stderr_drain),
        })
    }

    /// Re-issues list-sessions on demand (control-mode lines carry no binary
    /// prefix — we are already inside the tmux command interpreter).
    pub fn list_sessions(&self) {
        let _ = self.cmd_tx.send(HostCmd::Query {
            line: self.proto.list_sessions_line(),
            reply: PendingReply::ListSessions,
        });
    }

    /// Requests every pane across all windows of `session`, correlating the reply
    /// to `address` (the switcher's `source/name` session key) so the reader fills
    /// that session's window/pane subtree. Without this a session's children stay
    /// on the "loading…" placeholder forever — the control client never volunteers
    /// pane data, it must be asked.
    pub fn list_panes(&self, session: &str, address: String) {
        let _ = self.cmd_tx.send(HostCmd::Query {
            line: self.proto.list_panes_line(session),
            reply: PendingReply::ListPanes { address },
        });
    }

    /// Probes `target`'s active window over this control client. `target` is the tmux
    /// SESSION id (`$id`) from the `%session-window-changed` payload — probing that
    /// SPECIFIC session, never a guessed displayed one. The reply carries the resolved
    /// session name + window index and resolves to a [`HostEvent::Focus`] so the app
    /// follows the tree selection to the new active window (#2).
    pub fn probe_active_window(&self, target: &str) {
        let _ = self.cmd_tx.send(HostCmd::Query {
            line: self.proto.active_window_line(target),
            reply: PendingReply::ActiveWindow,
        });
    }

    /// Probes this host's display-client tty over the -CC control connection
    /// (`list-clients`). The reply resolves to a [`HostEvent::DisplayTty`] the
    /// supervisor records on `Host.display_tty`. Captured over the control connection,
    /// NOT via an in-band attach-shell marker — a Windows ConPTY consumes the marker's
    /// OSC before the display pump can read it, so the marker never lands for a remote
    /// host. With the tty known, a session switch is an in-place `switch-client -c <tty>`.
    pub fn capture_display_tty(&self) {
        let _ = self.cmd_tx.send(HostCmd::Query {
            line: self.proto.display_clients_line(),
            reply: PendingReply::DisplayClientTty,
        });
    }

    /// Make `target` (`session:window`) the active window of its session
    /// (`select-window -t <target>`) over this control client. Used to
    /// programmatically switch a window for a window-row selection: the real
    /// attached PTY client follows because the session's active window changes
    /// server-side (#4).
    pub fn select_window_on(&self, target: &str) {
        let _ = self
            .cmd_tx
            .send(HostCmd::Send(self.proto.select_window_line(target)));
    }

    /// Move xmux's display client (`display_tty`) to `session` over THIS control
    /// connection (`switch-client -c <tty> -t <session>`). The shared (tmux) session
    /// switch: routing it over the already-open `-CC` connection avoids spawning a
    /// fresh `ssh` per switch — on Windows ssh has no ControlMaster, so each fresh
    /// exec pays a full connect+auth handshake (~0.5s), which is the switch lag (#2).
    /// The server moves the named client regardless of which client issues the command.
    pub fn switch_client_on(&self, display_tty: &str, session: &str) {
        let _ = self.cmd_tx.send(HostCmd::Send(
            self.proto.switch_client_line(display_tty, session),
        ));
    }

    /// Force a full redraw of xmux's display client (`refresh-client -t <tty>`) over THIS
    /// control connection, issued right after a `switch-client`. A switch moves the client
    /// but does not always repaint a locally-cleared grid; a fresh attach repaints fully,
    /// and this gives the in-place switch the same full repaint so the new session shows.
    pub fn refresh_client_on(&self, display_tty: &str) {
        let _ = self
            .cmd_tx
            .send(HostCmd::Send(self.proto.refresh_client_line(display_tty)));
    }

    /// Tell the child its new client size (the metadata client's size; the PTY
    /// attachments are sized independently by the app).
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.size = (cols, rows);
        let _ = self.cmd_tx.send(HostCmd::Resize { cols, rows });
    }

    /// Stop the host: the writer returns on `Shutdown`, `child.kill()` closes the
    /// child's stdout/stderr so the reader's `lines()` and the stderr drain both
    /// hit EOF, then all threads join.
    ///
    /// The join is bounded in practice: we use PIPES (not ConPTY), so killing the
    /// child closes stdout/stderr immediately and the reader + stderr drain reach
    /// EOF — no `ClosePseudoConsole` stall is possible here (that risk is PTY-only).
    pub fn teardown(mut self) {
        let _ = self.cmd_tx.send(HostCmd::Shutdown);
        let _ = self.child.kill();
        if let Some(h) = self.writer.take() {
            let _ = h.join();
        }
        if let Some(h) = self.reader.take() {
            let _ = h.join();
        }
        if let Some(h) = self.stderr_drain.take() {
            let _ = h.join();
        }
        // Reap the killed child so it is not left a zombie (Unix) / leaked handle.
        // It was just killed and uses PIPES (not ConPTY), so this returns at once.
        let _ = self.child.wait();
    }
}

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

    #[test]
    #[ignore = "real -CC is the live gate; this just proves a piped child spawns + tears down"]
    fn host_client_spawns_piped_child() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<HostEvent>();
        let argv: Vec<String> = ["cmd.exe", "/c", "echo", "hi"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let client = HostClient::spawn("local", test_control_proto(), &argv, 80, 24, tx, &[])
            .expect("spawn");
        // echo exits immediately, closing pipes → teardown's joins return promptly.
        client.teardown();
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
