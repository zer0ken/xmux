//! One control-mode (`-CC`) host process: the piped child plus its reader,
//! writer, and stderr-drain threads, and the command API the app drives it with.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::mux::ControlProtocol;

use super::{run_reader, run_writer, HostCmd, HostEvent, InFlight, PendingReply, ReaderState};

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::test_control_proto;

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
}
