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
    /// The mux's control-mode protocol — builds every command line this client
    /// sends. Shared `'static` (the impl is stateless), so the reader/writer threads
    /// borrow it without owning a clone.
    proto: &'static dyn ControlProtocol,
    /// Queue commands to the writer thread.
    cmd_tx: std::sync::mpsc::Sender<HostCmd>,
    /// The control child, boxed so a piped child and a PTY child share one field.
    child: Box<dyn portable_pty::Child + Send + Sync>,
    /// Keeps the askpass token valid until this control child is reaped.
    _auth: Option<Box<crate::transport::auth::CommandAuth>>,
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
    /// the connect sequence (mux preamble, then list-sessions).
    /// `events` is the app's loop sink.
    #[allow(clippy::too_many_arguments)] // one cohesive spawn API; callers pass all eight
    pub fn spawn(
        host: impl Into<String>,
        proto: &'static dyn ControlProtocol,
        command: &crate::transport::CommandSpec,
        cols: u16,
        rows: u16,
        events: tokio::sync::mpsc::UnboundedSender<HostEvent>,
        pty: bool,
    ) -> anyhow::Result<HostClient> {
        anyhow::ensure!(
            !command.is_empty(),
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
                spawn_pty_child(command, cols, rows)?
            }
            #[cfg(not(unix))]
            {
                let _ = (cols, rows);
                unreachable!("a pty control spawn is Unix-only; no native local -CC on Windows")
            }
        } else {
            spawn_piped_child(command)?
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
        let writer_host = host.clone();
        let writer = std::thread::spawn(move || {
            run_writer(&writer_host, cmd_rx, &mut stdin, &writer_in_flight);
        });

        // Connect sequence: run the mux's metadata-client preamble, then list sessions
        // (the correlated query whose block resolves the inventory).
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
            proto,
            cmd_tx,
            child,
            _auth: command.auth_guard().map(Box::new),
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

    /// Probes this host's display-client tty over the -CC control connection. The reply
    /// resolves to a [`HostEvent::DisplayTty`] the supervisor records on
    /// `Host.display_tty`. Carried over the control connection, NOT via an in-band
    /// attach-shell marker — a Windows ConPTY consumes the marker's OSC before the
    /// display pump can read it, so the marker never lands for a remote host. With the
    /// tty known, a session switch is an in-place `switch-client -c <tty>`.
    /// Probes this host's display-client tty for `host_key`, the key the display attach
    /// recorded itself under. Returns whether the probe reached the writer thread. A
    /// writer that has returned on a broken pipe has dropped the receiver, so the send
    /// reports the refusal instead of leaving a command that never went out looking
    /// delivered.
    pub fn capture_display_tty(&self, host_key: &str) -> bool {
        let lines = self.proto.display_tty_lines(host_key);
        self.cmd_tx.send(HostCmd::Send(lines.stage)).is_ok()
            && self
                .cmd_tx
                .send(HostCmd::Query {
                    line: lines.read,
                    reply: PendingReply::DisplayClientTty,
                })
                .is_ok()
            && self.cmd_tx.send(HostCmd::Send(lines.clear)).is_ok()
    }

    /// Move xmux's display client (`display_tty`) to `session` over THIS control
    /// connection (`switch-client -c <tty> -t <session>`). The shared (tmux) session
    /// switch: routing it over the already-open `-CC` connection avoids spawning a
    /// fresh `ssh` per switch — on Windows ssh has no ControlMaster, so each fresh
    /// exec pays a full connect+auth handshake (~0.5s), which is the switch lag (#2).
    /// The server moves the named client regardless of which client issues the command.
    /// Returns whether the command reached the writer thread, which is as far as this
    /// side can observe: the writer owns the child's stdin, and it drops the receiver
    /// when a write breaks. A refused send is a switch that provably never went out,
    /// so the caller must not record the client as moved. A send that is accepted is
    /// not yet a switch that landed — only the mux's own session-changed notice says
    /// that.
    pub fn switch_client_on(&self, display_tty: &str, session: &str) -> bool {
        self.cmd_tx
            .send(HostCmd::Send(
                self.proto.switch_client_line(display_tty, session),
            ))
            .is_ok()
    }

    /// Force a full redraw of xmux's display client (`refresh-client -t <tty>`) over THIS
    /// control connection, issued right after a `switch-client`. A switch moves the client
    /// but does not always repaint a locally-cleared grid; a fresh attach repaints fully,
    /// and this gives the in-place switch the same full repaint so the new session shows.
    /// Returns whether the command reached the writer thread, on the same terms as
    /// [`HostClient::switch_client_on`].
    pub fn refresh_client_on(&self, display_tty: &str) -> bool {
        self.cmd_tx
            .send(HostCmd::Send(self.proto.refresh_client_line(display_tty)))
            .is_ok()
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
fn spawn_piped_child(command: &crate::transport::CommandSpec) -> anyhow::Result<Spawned> {
    let mut cmd = Command::new(command.program());
    cmd.args(command.args())
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
    for (k, v) in command.env() {
        cmd.env(k, v);
    }
    #[cfg(unix)]
    if command.should_detach_tty() {
        use std::os::unix::process::CommandExt as _;
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
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
    command: &crate::transport::CommandSpec,
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
    let mut cmd = CommandBuilder::new(command.program());
    cmd.args(command.args());
    // The same mux-session-var strip and `extra_env` the piped spawn applies, so
    // the two spawn shapes give the child the same environment.
    for (k, _) in std::env::vars() {
        if crate::mux::vocab::is_mux_var(&k) {
            cmd.env_remove(&k);
        }
    }
    for (k, v) in command.env() {
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
        let command = crate::transport::CommandSpec::from_argv(argv);
        let client = HostClient::spawn("local", test_control_proto(), &command, 80, 24, tx, false)
            .expect("spawn");
        // echo exits immediately, closing pipes → teardown's joins return promptly.
        client.teardown();
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn control_client_keeps_askpass_live_but_cannot_outlive_credential_removal() {
        async fn run(remove_before_prompt: bool) -> String {
            let root = std::env::temp_dir().join(format!(
                "xmux-control-auth-{}-{}",
                std::process::id(),
                crate::transport::auth::request_test_token()
            ));
            std::fs::create_dir_all(&root).unwrap();
            let output = root.join("answer.txt");
            let credentials = crate::transport::auth::Credentials::new(root.clone());
            let access = credentials
                .begin(
                    "pwbox",
                    crate::transport::Login {
                        user: Some("dev".into()),
                        ..Default::default()
                    },
                    "secret".into(),
                )
                .unwrap()
                .unwrap();
            assert!(access.promote());
            let seed =
                crate::transport::CommandSpec::new("stub", Vec::new()).with_auth(access, false);
            let endpoint = seed
                .env()
                .iter()
                .find(|(key, _)| key == "XMUX_ASKPASS_ENDPOINT")
                .map(|(_, value)| std::path::Path::new(value))
                .unwrap();
            let pipe = format!(
                "xmux-{}",
                endpoint.file_stem().and_then(|stem| stem.to_str()).unwrap()
            );
            let output_literal = output.to_string_lossy().replace('\'', "''");
            let script = format!(
                "$ErrorActionPreference='Stop'; Start-Sleep -Milliseconds 200; \
                 $p=[IO.Pipes.NamedPipeClientStream]::new('.', '{pipe}', [IO.Pipes.PipeDirection]::InOut); \
                 $p.Connect(2000); \
                 $w=[IO.StreamWriter]::new($p, [Text.UTF8Encoding]::new($false), 1024, $true); $w.AutoFlush=$true; \
                 $q=@{{token=$env:XMUX_ASKPASS_TOKEN;prompt=\"dev@pwbox's password: \";secret_prompt=$true}} | ConvertTo-Json -Compress; $w.WriteLine($q); \
                 $r=[IO.StreamReader]::new($p, [Text.UTF8Encoding]::new($false), $false, 1024, $true); \
                 $n=[int]$r.ReadLine(); $answer=''; if($n -gt 0){{$buf=New-Object char[] $n; [void]$r.ReadBlock($buf,0,$n); $answer=-join $buf}}; \
                 [IO.File]::WriteAllText('{output_literal}', $answer)"
            );
            let command = crate::transport::CommandSpec::new(
                "powershell.exe",
                vec![
                    "-NoProfile".into(),
                    "-NonInteractive".into(),
                    "-Command".into(),
                    script,
                ],
            )
            .with_auth(
                credentials.access("pwbox").expect("active credential"),
                false,
            );
            let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<HostEvent>();
            let client =
                HostClient::spawn("pwbox", test_control_proto(), &command, 80, 24, tx, false)
                    .expect("spawn delayed control stub");
            drop(command);
            drop(seed);
            if remove_before_prompt {
                credentials.remove("pwbox");
            }
            // The deadline only catches a hang: under a parallel test load on a CI
            // runner, powershell.exe alone can take several seconds to start.
            tokio::time::timeout(std::time::Duration::from_secs(30), async {
                while !output.exists() {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
            })
            .await
            .expect("stub wrote its broker answer");
            client.teardown();
            let answer = std::fs::read_to_string(&output).unwrap();
            credentials.shutdown();
            let _ = std::fs::remove_dir_all(root);
            answer
        }

        assert_eq!(run(false).await, "secret");
        assert_eq!(run(true).await, "");
    }
}
