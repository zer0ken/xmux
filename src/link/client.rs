//! One control-mode (`-CC`) host process: the piped child plus its reader,
//! writer, and stderr-drain threads, and the command API the app drives it with.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::mux::ControlProtocol;

use super::{run_reader, run_writer, HostCmd, HostEvent, InFlight, PendingReply, ReaderState};

/// One control-mode (`-CC`) host process: a piped child plus its reader and writer
/// OS threads. The app holds the `cmd_tx` to drive it and reads `connecting` for the
/// spinner; the session/window inventory is carried on `HostEvent`s and owned by
/// `model::Host.inventory`. This is a METADATA / change-event / `switch-client`
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
    /// The control child, boxed so a piped child and a PTY child share one field.
    child: Box<dyn portable_pty::Child + Send + Sync>,
    reader: Option<JoinHandle<()>>,
    writer: Option<JoinHandle<()>>,
    /// Drains the child's stderr to EOF so a child that writes more than the pipe
    /// buffer (ssh banners/warnings) cannot block and wedge the connection.
    stderr_drain: Option<JoinHandle<()>>,
}

/// The spawned control child and its stdio handles, boxed so the piped and PTY
/// spawn shapes share one type. `stderr_drain` is the piped spawn's stderr-drain
/// thread handle; a PTY child has no separate stderr (it shares the one master
/// stream), so it carries `None`.
pub(super) struct Spawned {
    pub(super) child: Box<dyn portable_pty::Child + Send + Sync>,
    pub(super) stdout: Box<dyn std::io::Read + Send>,
    pub(super) stdin: Box<dyn std::io::Write + Send>,
    pub(super) stderr_drain: Option<JoinHandle<()>>,
}

impl HostClient {
    /// Spawns `argv` as a control-mode child at `cols×rows` - through a pty when
    /// `pty` (a transport says its `-CC` client needs a terminal on its stdin),
    /// else as a piped child - starts the reader + writer OS threads, and queues
    /// the connect sequence (resize → flow-control pause → list-sessions).
    /// `events` is the app's loop sink.
    #[allow(clippy::too_many_arguments)] // one cohesive spawn API; callers pass all eight
    pub fn spawn(
        host: impl Into<String>,
        proto: &'static dyn ControlProtocol,
        argv: &[String],
        cols: u16,
        rows: u16,
        events: tokio::sync::mpsc::UnboundedSender<HostEvent>,
        extra_env: &[(&str, &str)],
        pty: bool,
    ) -> anyhow::Result<HostClient> {
        anyhow::ensure!(
            !argv.is_empty(),
            "HostClient::spawn: argv must not be empty"
        );
        let host = host.into();

        // The child spawn shape: a PTY when the transport requires one (a local
        // `-CC` mux client on Unix dies on pipe stdio - `tcgetattr failed`), else
        // the piped spawn with a stderr drain.
        let Spawned {
            child,
            stdout,
            mut stdin,
            stderr_drain,
        } = if pty {
            #[cfg(unix)]
            {
                spawn_pty_child(argv, extra_env, cols, rows)?
            }
            #[cfg(not(unix))]
            {
                unreachable!("a pty control spawn is Unix-only; no native local -CC on Windows")
            }
        } else {
            spawn_piped_child(argv, extra_env)?
        };

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
        // `switch-client` channel ONLY; the per-session PTY attaches own the pixels), then
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
            stderr_drain,
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
        // It was just killed, so this returns at once.
        let _ = self.child.wait();
    }
}

/// Spawns the control child as a piped process (stdin/stdout/stderr all pipes)
/// with the mux session vars stripped and `extra_env` applied, plus a stderr drain
/// thread so a child that writes more than the pipe buffer to stderr (ssh
/// banners/warnings) cannot block and wedge the connection. EOF arrives when the
/// child dies, so the drain's join is bounded.
fn spawn_piped_child(argv: &[String], extra_env: &[(&str, &str)]) -> anyhow::Result<Spawned> {
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
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("child stdin missing"))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("child stderr missing"))?;
    let stderr_drain = std::thread::spawn(move || {
        let _ = std::io::copy(&mut stderr, &mut std::io::sink());
    });
    Ok(Spawned {
        child: Box::new(child),
        stdout: Box::new(stdout),
        stdin: Box::new(stdin),
        stderr_drain: Some(stderr_drain),
    })
}

/// Spawns the control child on a pty this process allocates (Unix). A `-CC` mux
/// client reads its own stdin's terminal attributes and dies when stdin is not a
/// terminal (`tcgetattr failed: Inappropriate ioctl for device`), and the control
/// child's stdio would otherwise be pipes - so a local tmux control stream must get
/// a pty the way the remote's `ssh -tt` and WSL's `script` wrapper force one. A
/// pty child has no separate stderr (it shares the one master stream), so there is
/// no drain handle to return.
#[cfg(unix)]
pub(super) fn spawn_pty_child(
    argv: &[String],
    extra_env: &[(&str, &str)],
    cols: u16,
    rows: u16,
) -> anyhow::Result<Spawned> {
    use portable_pty::{native_pty_system, CommandBuilder, PtySize};

    let pair = native_pty_system().openpty(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    let mut cmd = CommandBuilder::new(&argv[0]);
    cmd.args(&argv[1..]);
    // The same mux-session-var strip and `extra_env` the piped spawn applies, so
    // the two spawn shapes give the child the same environment.
    for (k, _) in std::env::vars() {
        if crate::mux::vocab::is_mux_var(&k) {
            cmd.env_remove(&k);
        }
    }
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);
    let reader = pair.master.try_clone_reader()?;
    let writer = pair.master.take_writer()?;
    Ok(Spawned {
        child,
        stdout: reader,
        stdin: writer,
        stderr_drain: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link::test_control_proto;

    #[test]
    #[ignore = "real -CC is the live gate; this just proves a piped child spawns + tears down"]
    fn host_client_spawns_piped_child() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<HostEvent>();
        let argv: Vec<String> = ["cmd.exe", "/c", "echo", "hi"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let client =
            HostClient::spawn("local", test_control_proto(), &argv, 80, 24, tx, &[], false)
                .expect("spawn");
        // echo exits immediately, closing pipes → teardown's joins return promptly.
        client.teardown();
    }
}
