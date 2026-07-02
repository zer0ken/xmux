//! Per-host data types shared between the reader thread, writer thread, and app.

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::mux::{parse_panes, parse_sessions};
use crate::mux::{ControlProtocol, Line, Notif};
use crate::session::{Session, WindowPanes};

/// One host's session/window inventory, seeded from list-sessions/list-panes and
/// kept live by notifications. The app reads it to (re)build the tree. This is
/// a METADATA channel only — the per-session PTY attachments own the pixels.
pub struct HostInventory {
    pub sessions: Vec<Session>,
    pub panes: HashMap<String, Vec<WindowPanes>>,
}

impl HostInventory {
    pub fn new() -> Self {
        Self {
            sessions: Vec::new(),
            panes: HashMap::new(),
        }
    }
}

impl Default for HostInventory {
    fn default() -> Self {
        Self::new()
    }
}

/// A command for a host's writer thread. The writer builds the exact bytes.
pub enum HostCmd {
    /// A ready command line (newline-terminated).
    Send(String),
    Resize {
        cols: u16,
        rows: u16,
    },
    /// A command line whose `%begin` block carries a meaningful reply. The writer
    /// pushes `reply` onto the FIFO in lockstep with writing `line`, so the
    /// correlation cannot race the writer (pushing from the calling thread could).
    Query {
        line: String,
        reply: PendingReply,
    },
    Shutdown,
}

/// A parsed event the reader emits to the app's `select!` loop.
pub enum HostEvent {
    /// First list-sessions returned.
    Connected { host: String },
    /// A list-sessions / list-panes reply resolved — re-apply the inventory.
    Inventory { host: String },
    /// A `%`-notification reports the server's session/window STRUCTURE CHANGED
    /// (added, closed, renamed, or the set of sessions) — the app must REFETCH
    /// (re-run list-sessions + re-list panes), since the notification carries only an
    /// id, not the new structure. Resyncs the tree view + active-window markers (#5).
    Changed { host: String },
    /// `%session-window-changed $id @win`: a session's ACTIVE WINDOW switched (e.g.
    /// another client did prefix-n). Carries the notification's tmux SESSION id
    /// (`$id`) and WINDOW id (`@win`) so the app probes THAT SPECIFIC session's new
    /// active window and follows the tree selection to it (#2) — it must NOT guess the
    /// displayed session, which mismatches when a non-displayed session's window changes.
    ActiveWindowChanged {
        host: String,
        session_id: String,
        window_id: String,
    },
    /// An active-window probe resolved (`display-message -p
    /// '#{session_name}\t#{window_index}'`): the app moves the tree selection to
    /// window `window` of the RESOLVED `session` (a no-op unless the selection is on a
    /// window row of that session — see [`crate::ui::switcher::Switcher::select_window`]).
    Focus {
        host: String,
        session: String,
        window: i64,
    },
    /// `%exit` / EOF — reap.
    Exited {
        host: String,
        reason: Option<String>,
    },
    /// `%client-detached <client>` — some client of this host detached. The reader
    /// does not know which client is xmux's display attach (that tty lives on the
    /// supervisor's `Host.display_tty`), so it forwards the client tty; the supervisor
    /// reaps the display attach ONLY when `client` matches `Host.display_tty`.
    ClientDetached { host: String, client: String },
    /// A `list-clients` probe over the -CC control connection resolved: this host's
    /// display-client tty — the client the mux protocol identifies as xmux's display
    /// attach — or `None` if it has not registered yet. Captured OUT-OF-BAND over
    /// the control connection, not via an in-band attach-shell marker (a Windows
    /// ConPTY consumes the marker's OSC before the pump can read it). Recorded on
    /// `Host.display_tty` so a later `switch-client -c <tty>` targets xmux's own client.
    DisplayTty { host: String, tty: Option<String> },
    /// A detection probe resolved (`detect_and_correct`): the host's mux was
    /// (re)identified. `None` = still undetected / unreachable. Folded back via
    /// `apply_scan_result`; emitted by the fire-and-forget detection task.
    Scanned {
        source: String,
        detected: Option<Box<dyn crate::mux::Mux>>,
    },
    /// A POLL host re-enumerated its sessions. A poll host has no host-level control
    /// stream, so its [`HostManager`]-owned poll task emits this onto the same bus.
    /// `err` carries a transient enumeration failure (shown in the tree; attachments
    /// are kept — the keep-alive guarantee).
    Sessions {
        source: String,
        sessions: Vec<Session>,
        err: Option<String>,
    },
    /// A POLL host's per-session window/pane subtree resolved (keyed by the session's
    /// `source/name` address), emitted by the poll task after `Sessions`.
    Panes {
        address: String,
        panes: Vec<WindowPanes>,
    },
}

/// The reader's shared state the app also reads.
pub struct ReaderState {
    pub inventory: Arc<Mutex<HostInventory>>,
    pub connecting: Arc<AtomicBool>,
}

/// The in-flight command correlation FIFO, shared with the writer.
pub type InFlight = Arc<Mutex<VecDeque<PendingReply>>>;

/// What a resolved `%begin…%end` block means to the reader.
pub enum PendingReply {
    ListSessions,
    ListPanes {
        address: String,
    },
    /// An active-window probe: its block body is `<session_name>\t<window_index>`
    /// (the probe targeted a session id, so the name is resolved by the reply, not the
    /// correlator), resolved into a [`HostEvent::Focus`].
    ActiveWindow,
    /// A `list-clients` probe: the mux protocol parses the block body for xmux's own
    /// display-client tty (`ControlProtocol::parse_display_client_tty`), resolved into a
    /// [`HostEvent::DisplayTty`]. The reader names no wire format.
    DisplayClientTty,
    Ignore,
}

/// Runs the line state machine over `lines` (an `Iterator<Item=String>` of stdout
/// lines, already split on `\n`), driving `state`, `in_flight`, and emitting events
/// via `emit`. Returns when the iterator ends (child EOF). Pure over its inputs so
/// a test feeds canned bytes; the real reader wraps a `BufRead`.
pub fn run_reader<E: FnMut(HostEvent)>(
    host: &str,
    proto: &dyn ControlProtocol,
    lines: impl Iterator<Item = String>,
    state: &ReaderState,
    in_flight: &InFlight,
    mut emit: E,
) {
    // num, kind, body — the open `%begin` block, if any.
    let mut block: Option<(u64, PendingReply, Vec<String>)> = None;
    // The last %error block's text, so a never-connected exit carries a meaningful
    // reason (notably "no sessions" / "no server running" → reachable-but-empty).
    let mut last_error: Option<String> = None;
    for line in lines {
        // The entry DCS `\x1bP1000p` ([research §1]) introduces control mode. It may
        // arrive on its own line or glued to the first `%begin`. Strip it; a lone DCS
        // becomes empty (Body, ignored), a glued line classifies its remainder as
        // `%begin`. Correlation does NOT depend on it: blocks are matched by the
        // `%begin` flags bit (see below), not by a fragile startup-banner heuristic.
        let line = line
            .strip_prefix("\x1bP1000p")
            .map(str::to_string)
            .unwrap_or(line);
        // Inside a block, only the matching %end/%error closes it; everything
        // else is body (notifications never appear inside a block).
        if let Some((num, _, _)) = block.as_ref() {
            let num = *num;
            let (close, is_err) = match proto.classify(&line) {
                Line::End { num: n } if n == num => (true, false),
                Line::Error { num: n } if n == num => (true, true),
                _ => (false, false),
            };
            if close {
                let (_, kind, body) = block.take().unwrap();
                // Remember an error block's text ("no sessions" / "no server running"
                // / …) so a control client that dies before connecting carries it —
                // the app then tells a reachable-but-empty mux from a dead host.
                if is_err {
                    let t = body.join(" ").trim().to_string();
                    if !t.is_empty() {
                        last_error = Some(t);
                    }
                }
                resolve_block(host, kind, &body, state, proto, &mut emit);
            } else {
                // Re-borrow only to push; the `as_ref` borrow above has ended.
                block.as_mut().unwrap().2.push(line);
            }
            continue;
        }
        match proto.classify(&line) {
            Line::Begin { num, control } => {
                // A block replying to a command WE sent (flags bit 0 set) pops the
                // correlation FIFO; a spontaneous block (startup banner, another
                // client's command, a hook — flags bit 0 clear) consumes ZERO FIFO
                // entries, so it can never shift our replies. This is robust across
                // tmux versions (3.4 emits a separate flags=0 banner; 3.5a glues the
                // DCS to the first flags=1 reply and trails a flags=0 block).
                let kind = if control {
                    in_flight
                        .lock()
                        .unwrap()
                        .pop_front()
                        .unwrap_or(PendingReply::Ignore)
                } else {
                    PendingReply::Ignore
                };
                block = Some((num, kind, Vec::new()));
            }
            // %output is the per-pane PIXEL stream; the per-session PTY attachments
            // own pixels now, and the control client runs with `refresh-client -f
            // no-output`, so it should not arrive. If an older mux that lacks the
            // flag sends it anyway, discard it (just note the channel is live) — the
            // control client is metadata-only.
            Line::Output { .. } | Line::ExtendedOutput { .. } => clear_connecting(state),
            Line::Notification(n) => dispatch_notif(host, proto, n, &last_error, &mut emit),
            // Stray frame/body outside a block.
            Line::End { .. } | Line::Error { .. } | Line::Body(_) => {}
        }
    }
    // Iterator ended = child stdout EOF.
    emit(HostEvent::Exited {
        host: host.to_string(),
        reason: last_error,
    });
}

/// Resolves a closed `%begin…%end` block by applying its body to the inventory
/// and emitting the follow-up events. `proto` supplies the mux-specific parse of a
/// `list-clients` body (the display-client tty), so the reader names no tmux wire detail.
fn resolve_block<E: FnMut(HostEvent)>(
    host: &str,
    kind: PendingReply,
    body: &[String],
    state: &ReaderState,
    proto: &dyn ControlProtocol,
    emit: &mut E,
) {
    match kind {
        PendingReply::ListSessions => {
            let out = body.join("\n");
            let sessions = parse_sessions(host, &out);
            state.inventory.lock().unwrap().sessions = sessions;
            clear_connecting(state);
            emit(HostEvent::Connected {
                host: host.to_string(),
            });
            emit(HostEvent::Inventory {
                host: host.to_string(),
            });
        }
        PendingReply::ListPanes { address } => {
            let out = body.join("\n");
            let panes = parse_panes(&out);
            state.inventory.lock().unwrap().panes.insert(address, panes);
            emit(HostEvent::Inventory {
                host: host.to_string(),
            });
        }
        PendingReply::ActiveWindow => {
            // `display-message -p '#{session_name}\t#{window_index}'` prints one line:
            // `<name>\t<index>`. The probe targeted a session id, so the RESOLVED name
            // comes back in the reply. Emit Focus for that session so the app follows
            // the selection (#2). A missing/garbled body (no `name\tindex`) yields no event.
            if let Some((session, window)) = body.iter().find_map(|l| {
                let (name, idx) = l.split_once('\t')?;
                let idx = idx.trim().parse::<i64>().ok()?;
                Some((name.to_string(), idx))
            }) {
                emit(HostEvent::Focus {
                    host: host.to_string(),
                    session,
                    window,
                });
            }
        }
        PendingReply::DisplayClientTty => {
            // A `list-clients` body: the mux protocol parses out the display attach's tty
            // — the reader names no wire detail.
            emit(HostEvent::DisplayTty {
                host: host.to_string(),
                tty: proto.parse_display_client_tty(body),
            });
        }
        PendingReply::Ignore => {}
    }
}

/// Maps one notification to the app event it triggers and emits it. The policy
/// table lives behind the mux's [`ControlProtocol::notif_event`] (a tmux protocol
/// detail); this thin wrapper just forwards the event when there is one.
fn dispatch_notif<E: FnMut(HostEvent)>(
    host: &str,
    proto: &dyn ControlProtocol,
    notif: Notif<'_>,
    last_error: &Option<String>,
    emit: &mut E,
) {
    if let Some(event) = proto.notif_event(host, notif, last_error) {
        emit(event);
    }
}

/// Marks the host as connected once any wire activity proves the channel is live.
fn clear_connecting(state: &ReaderState) {
    state.connecting.store(false, Ordering::Release);
}

/// Drains the command channel, writing exact command bytes to `w` and pushing ONE
/// correlation entry per command LINE, so the FIFO stays in lockstep with the
/// `%begin` blocks the reader pops. The correlator is pushed BEFORE the bytes
/// reach the child, so the reader can never observe the reply's `%begin` before
/// its FIFO entry exists (the writer thread and the reader thread race otherwise).
/// A write error means the pipe is broken (the child died) — return so no further
/// stale entries are queued; the reader hits EOF and the client is reaped. Flushes
/// after each command so a real child sees it promptly. Returns on `Shutdown`.
pub fn run_writer<W: Write>(
    rx: std::sync::mpsc::Receiver<HostCmd>,
    proto: &dyn ControlProtocol,
    w: &mut W,
    in_flight: &InFlight,
) {
    while let Ok(cmd) = rx.recv() {
        match cmd {
            HostCmd::Send(line) => {
                in_flight.lock().unwrap().push_back(PendingReply::Ignore);
                if w.write_all(line.as_bytes()).is_err() {
                    return;
                }
            }
            HostCmd::Resize { cols, rows } => {
                in_flight.lock().unwrap().push_back(PendingReply::Ignore);
                if w.write_all(proto.size_line(cols, rows).as_bytes()).is_err() {
                    return;
                }
            }
            HostCmd::Query { line, reply } => {
                in_flight.lock().unwrap().push_back(reply);
                if w.write_all(line.as_bytes()).is_err() {
                    return;
                }
            }
            HostCmd::Shutdown => return,
        }
        let _ = w.flush();
    }
}

/// One control-mode (`-CC`) host process: a piped child plus its reader and writer
/// OS threads. The app holds the `cmd_tx` to drive it and reads `inventory`/
/// `connecting` for the tree view. This is a METADATA / change-event /
/// `select-window` channel only — the per-session PTY attachments own the pixels.
pub struct HostClient {
    /// Stable host id (the source name), echoed back on every `HostEvent`.
    pub host: String,
    /// Live session/window inventory, kept current by the reader.
    pub inventory: Arc<Mutex<HostInventory>>,
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

        let inventory = Arc::new(Mutex::new(HostInventory::new()));
        let connecting = Arc::new(AtomicBool::new(true));
        let in_flight: InFlight = Arc::new(Mutex::new(VecDeque::new()));
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<HostCmd>();

        // Reader thread: stdout lines → state machine; events to the async loop via
        // the non-blocking, thread-safe UnboundedSender.
        let state = ReaderState {
            inventory: Arc::clone(&inventory),
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
            inventory,
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
        // The control argv is now composed from `host` (transport × mux); the source is
        // no longer read here. It stays in the signature because `rescan` forwards it and the
        // callers hand it in; a later stage retires the source-threading entirely.
        _src: &crate::source::Source,
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
    pub fn rescan(
        &mut self,
        id: &str,
        host: &crate::model::Host,
        src: &crate::source::Source,
        cols: u16,
        rows: u16,
    ) {
        if let Some(c) = self.clients.get(id) {
            c.list_sessions();
            return;
        }
        if let Some(h) = self.polls.remove(id) {
            h.abort();
            let _ = self.ensure(id, host, src, cols, rows);
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

    #[test]
    fn inventory_starts_empty() {
        let inv = HostInventory::new();
        assert!(inv.sessions.is_empty());
        assert!(inv.panes.is_empty());
    }

    #[test]
    fn host_event_carries_host() {
        let e = HostEvent::Changed {
            host: "jupiter06".into(),
        };
        match e {
            HostEvent::Changed { host } => assert_eq!(host, "jupiter06"),
            _ => panic!("variant"),
        }
    }

    /// Builds a `ReaderState` with an empty inventory and `connecting = true`. The
    /// control client is metadata-only now (no grid).
    fn test_state(_cols: u16, _rows: u16) -> ReaderState {
        ReaderState {
            inventory: Arc::new(Mutex::new(HostInventory::new())),
            connecting: Arc::new(AtomicBool::new(true)),
        }
    }

    #[test]
    fn reader_resolves_list_sessions_block_into_inventory() {
        let state = test_state(80, 24);
        let in_flight: InFlight = Default::default();
        in_flight
            .lock()
            .unwrap()
            .push_back(PendingReply::ListSessions);
        let mut events = Vec::new();
        let lines = vec![
            "%begin 1 5 1".to_string(),
            "2\t1\t1700000000\tapi".to_string(),
            "%end 1 5 1".to_string(),
        ]
        .into_iter();
        run_reader(
            "jupiter06",
            test_control_proto(),
            lines,
            &state,
            &in_flight,
            |e| events.push(e),
        );
        let inv = state.inventory.lock().unwrap();
        assert_eq!(inv.sessions.len(), 1);
        assert_eq!(inv.sessions[0].name, "api");
        assert_eq!(inv.sessions[0].source, "jupiter06");
        assert!(events
            .iter()
            .any(|e| matches!(e, HostEvent::Connected { .. })));
        assert!(!state.connecting.load(std::sync::atomic::Ordering::Acquire));
    }

    // LIVE: connects to the real `jupiter06` over ssh and verifies the control-mode
    // METADATA path end-to-end — connect → list-sessions resolves → inventory has the
    // host's real sessions. Uses PIPES (not a ConPTY), so it works headlessly even
    // inside a mux. `#[ignore]` because it needs network + the host reachable:
    //   cargo test -p xmux host::tests::live_jupiter06 -- --ignored --nocapture
    #[ignore = "live: ssh to jupiter06; run on demand"]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn live_jupiter06_control_lists_sessions() {
        use std::time::{Duration, Instant};
        let src = crate::source::Source {
            alias: "jupiter06".into(),
            binary: "tmux".into(),
            kind: crate::machine::MachineKind::Ssh {
                alias: "jupiter06".into(),
                control_path: String::new(),
                os: std::env::consts::OS.into(),
            },
            runner: None,
        };
        let host = crate::model::Host::new(
            crate::machine::ssh("jupiter06".into(), String::new(), "linux".into()),
            crate::mux::for_binary("tmux"),
        );
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<HostEvent>();
        let mut mgr = HostManager::new(tx);
        mgr.ensure("jupiter06", &host, &src, 80, 24)
            .expect("spawn control client");
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut connected = false;
        while !connected && Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_secs(20), rx.recv()).await {
                Ok(Some(HostEvent::Connected { .. })) => connected = true,
                Ok(Some(_)) => continue,
                _ => break,
            }
        }
        assert!(
            connected,
            "control client must connect to jupiter06 + resolve list-sessions"
        );
        let sessions = mgr
            .get("jupiter06")
            .unwrap()
            .inventory
            .lock()
            .unwrap()
            .sessions
            .clone();
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
    fn reader_structure_notifications_emit_changed() {
        // A `%`-notification that the server's session/window STRUCTURE changed
        // (added, closed, renamed, or the set of sessions) must emit Changed: it
        // carries only an id, so the app refetches (re-list-sessions +
        // re-list-panes) to resync the tree + active-window markers (#5).
        for line in [
            "%window-add @4",
            "%window-close @4",
            "%window-renamed @4 logs",
            "%sessions-changed",
        ] {
            let state = test_state(80, 24);
            let in_flight: InFlight = Default::default();
            let mut events = Vec::new();
            run_reader(
                "jupiter06",
                test_control_proto(),
                vec![line.to_string()].into_iter(),
                &state,
                &in_flight,
                |e| events.push(e),
            );
            assert!(
                events
                    .iter()
                    .any(|e| matches!(e, HostEvent::Changed { host } if host == "jupiter06")),
                "{line:?} must emit Changed"
            );
        }
    }

    #[test]
    fn reader_unlinked_window_notifications_emit_changed() {
        // A window added/closed/renamed in a session OTHER than the control client's
        // OWN attached session arrives as `%unlinked-window-*` (tmux sends the plain
        // `%window-*` form only for the client's current session). The displayed
        // session is usually NOT the control client's session, so without handling
        // these the tree view misses real-time window add/delete there. They must emit
        // Changed exactly like their linked counterparts so the app refetches.
        for line in [
            "%unlinked-window-add @4",
            "%unlinked-window-close @4",
            "%unlinked-window-renamed @4 logs",
        ] {
            let state = test_state(80, 24);
            let in_flight: InFlight = Default::default();
            let mut events = Vec::new();
            run_reader(
                "jupiter06",
                test_control_proto(),
                vec![line.to_string()].into_iter(),
                &state,
                &in_flight,
                |e| events.push(e),
            );
            assert!(
                events
                    .iter()
                    .any(|e| matches!(e, HostEvent::Changed { host } if host == "jupiter06")),
                "{line:?} must emit Changed"
            );
        }
    }

    #[test]
    fn session_window_changed_emits_active_window_changed_with_payload() {
        // A session's ACTIVE WINDOW switched (`%session-window-changed $id @win`):
        // emit ActiveWindowChanged CARRYING the notification's session id + window id,
        // so the app probes THAT SPECIFIC session (not a guessed displayed one)
        // and follows the tree selection to it (#2). It must NOT collapse to a blanket
        // Changed (which only refetches and would leave the selection behind), and it must
        // NOT drop the payload to a host-only event (which forces the guess).
        let state = test_state(80, 24);
        let in_flight: InFlight = Default::default();
        let mut events = Vec::new();
        run_reader(
            "jupiter06",
            test_control_proto(),
            vec!["%session-window-changed $0 @1".to_string()].into_iter(),
            &state,
            &in_flight,
            |e| events.push(e),
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                HostEvent::ActiveWindowChanged { host, session_id, window_id }
                    if host == "jupiter06" && session_id == "$0" && window_id == "@1"
            )),
            "%session-window-changed must emit ActiveWindowChanged with the $id/@win payload"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, HostEvent::Changed { .. })),
            "%session-window-changed must not collapse to a blanket Changed"
        );
    }

    #[test]
    fn reader_resolves_active_window_block_into_focus() {
        // The active-window probe (`display-message -p '#{session_name}\t#{window_index}'`)
        // returns a single line: `<name>\t<index>`. Resolving its block emits Focus
        // carrying the RESOLVED session name + parsed window index (the probe targeted a
        // session id, so the name comes back in the reply — not the correlator), so the
        // app moves the tree selection to that window row of the correct session.
        let state = test_state(80, 24);
        let in_flight: InFlight = Default::default();
        in_flight
            .lock()
            .unwrap()
            .push_back(PendingReply::ActiveWindow);
        let mut events = Vec::new();
        let lines = vec![
            "%begin 1 5 1".to_string(),
            "api\t2".to_string(),
            "%end 1 5 1".to_string(),
        ]
        .into_iter();
        run_reader(
            "jupiter06",
            test_control_proto(),
            lines,
            &state,
            &in_flight,
            |e| events.push(e),
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                HostEvent::Focus { host, session, window }
                    if host == "jupiter06" && session == "api" && *window == 2
            )),
            "an active-window block resolves to Focus"
        );
    }

    #[test]
    fn active_window_query_line_quotes_and_escapes() {
        // The probe targets a session (by name or `$id`) and prints its active window's
        // session name + index so the reply resolves BOTH. The format braces are escaped
        // (so `#{session_name}`/`#{window_index}` reach tmux literally) and a target with
        // spaces is quoted for the control-mode parser.
        let proto = test_control_proto();
        // A session id target (`$0`) is quoted by the control-mode target quoter (the `$`
        // is outside the bare-safe set); tmux strips the single-quotes and resolves `$0`
        // as the session id.
        assert_eq!(
            proto.active_window_line("$0"),
            "display-message -p -t '$0' '#{session_name}\t#{window_index}'\n"
        );
        assert_eq!(
            proto.active_window_line("my proj"),
            "display-message -p -t 'my proj' '#{session_name}\t#{window_index}'\n"
        );
    }

    #[test]
    fn reader_session_changed_and_pane_changed_are_inert() {
        // `%session-changed` (the metadata client's own attach) and
        // `%window-pane-changed` do not affect the tree view, so they must NOT
        // trigger a Changed refetch. (run_reader always emits a trailing Exited on
        // EOF, so assert specifically that no Changed was emitted.)
        for line in ["%session-changed $1 api", "%window-pane-changed @1 %2"] {
            let state = test_state(80, 24);
            let in_flight: InFlight = Default::default();
            let mut events = Vec::new();
            run_reader(
                "jupiter06",
                test_control_proto(),
                vec![line.to_string()].into_iter(),
                &state,
                &in_flight,
                |e| events.push(e),
            );
            assert!(
                !events
                    .iter()
                    .any(|e| matches!(e, HostEvent::Changed { .. })),
                "{line:?} must not trigger a refetch"
            );
        }
    }

    #[test]
    fn client_detached_emits_host_scoped_event_with_client() {
        let mut events = Vec::new();
        dispatch_notif(
            "jupiter06",
            test_control_proto(),
            Notif::ClientDetached {
                client: "/dev/pts/3",
            },
            &Some("ignored".into()),
            &mut |e| events.push(e),
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                HostEvent::ClientDetached { host, client }
                    if host == "jupiter06" && client == "/dev/pts/3"
            )),
            "%client-detached emits a host-scoped ClientDetached carrying the client tty"
        );
        assert!(
            !events.iter().any(|e| matches!(e, HostEvent::Exited { .. })),
            "it must NOT reap the host (no Exited) — that is the supervisor's tty-matched job"
        );
    }

    /// The `-CC` entry DCS `\x1bP1000p` introduces control mode, and tmux 3.3.6/3.4
    /// emit a flags=0 startup banner block BEFORE the first command reply. The
    /// banner (flags=0) must consume zero FIFO entries; the real list-sessions reply
    /// (flags=1) pops `ListSessions`. Proven in BOTH framings: the DCS on its own
    /// line, and the DCS glued to the banner `%begin`.
    #[test]
    fn reader_startup_banner_keeps_fifo_lockstep() {
        // SEPARATE-line framing: a lone DCS line, the flags=0 banner, then the
        // flags=1 list-sessions reply.
        {
            let state = test_state(80, 24);
            let in_flight: InFlight = Default::default();
            in_flight
                .lock()
                .unwrap()
                .push_back(PendingReply::ListSessions);
            let lines = vec![
                "\x1bP1000p".to_string(),
                "%begin 1 1 0".to_string(),
                "%end 1 1 0".to_string(),
                "%begin 1 2 1".to_string(),
                "2\t1\t1700000000\tapi".to_string(),
                "%end 1 2 1".to_string(),
            ]
            .into_iter();
            run_reader(
                "jupiter06",
                test_control_proto(),
                lines,
                &state,
                &in_flight,
                |_| {},
            );
            let inv = state.inventory.lock().unwrap();
            assert_eq!(
                inv.sessions.len(),
                1,
                "flags=0 banner stole the ListSessions entry"
            );
            assert_eq!(inv.sessions[0].name, "api");
        }
        // GLUED framing: the DCS prefixed onto the flags=0 banner `%begin` line.
        {
            let state = test_state(80, 24);
            let in_flight: InFlight = Default::default();
            in_flight
                .lock()
                .unwrap()
                .push_back(PendingReply::ListSessions);
            let lines = vec![
                "\x1bP1000p%begin 1 1 0".to_string(),
                "%end 1 1 0".to_string(),
                "%begin 1 2 1".to_string(),
                "2\t1\t1700000000\tapi".to_string(),
                "%end 1 2 1".to_string(),
            ]
            .into_iter();
            run_reader(
                "jupiter06",
                test_control_proto(),
                lines,
                &state,
                &in_flight,
                |_| {},
            );
            let inv = state.inventory.lock().unwrap();
            assert_eq!(
                inv.sessions.len(),
                1,
                "glued flags=0 banner stole the ListSessions entry"
            );
            assert_eq!(inv.sessions[0].name, "api");
        }
    }

    #[test]
    fn reader_uses_begin_flags_to_correlate_not_a_banner_heuristic() {
        // A %begin block replying to a command WE sent carries flags=1; a
        // spontaneous block (startup banner, another client's command, a hook) is
        // flags=0. The reader pops the correlation FIFO only for flags=1, so a
        // spontaneous block never shifts the replies. tmux 3.5a glues the entry DCS
        // to the FIRST real reply (flags=1) and emits a trailing spontaneous block
        // (flags=0); the old single-banner-skip heuristic mis-skipped the real
        // reply, desynced the FIFO, and resolved list-sessions empty.
        let state = test_state(80, 24);
        let in_flight: InFlight = Default::default();
        in_flight
            .lock()
            .unwrap()
            .push_back(PendingReply::ListSessions);
        let lines = vec![
            "\x1bP1000p%begin 1 10 1".to_string(), // DCS glued to the first reply
            "2\t1\t1700000000\tapi".to_string(),
            "%end 1 10 1".to_string(),
            "%begin 1 11 0".to_string(), // spontaneous: must NOT consume a correlator
            "%end 1 11 0".to_string(),
        ]
        .into_iter();
        run_reader(
            "jupiter00",
            test_control_proto(),
            lines,
            &state,
            &in_flight,
            |_| {},
        );
        let inv = state.inventory.lock().unwrap();
        assert_eq!(
            inv.sessions.len(),
            1,
            "list-sessions resolved against the flags=1 block"
        );
        assert_eq!(inv.sessions[0].name, "api");
    }

    #[test]
    fn reader_spontaneous_block_does_not_steal_a_pending_reply() {
        // A flags=0 block arriving BEFORE our command reply (another client ran a
        // command first) must not consume our queued correlator.
        let state = test_state(80, 24);
        let in_flight: InFlight = Default::default();
        in_flight
            .lock()
            .unwrap()
            .push_back(PendingReply::ListSessions);
        let lines = vec![
            "%begin 1 5 0".to_string(), // spontaneous, flags=0
            "noise".to_string(),
            "%end 1 5 0".to_string(),
            "%begin 1 6 1".to_string(), // our list-sessions reply, flags=1
            "3\t1\t1700000000\twork".to_string(),
            "%end 1 6 1".to_string(),
        ]
        .into_iter();
        run_reader("h", test_control_proto(), lines, &state, &in_flight, |_| {});
        assert_eq!(state.inventory.lock().unwrap().sessions.len(), 1);
    }

    #[test]
    fn reader_exit_emits_exited() {
        let state = test_state(80, 24);
        let in_flight: InFlight = Default::default();
        let mut events = Vec::new();
        run_reader(
            "jupiter06",
            test_control_proto(),
            vec!["%exit too far behind".to_string()].into_iter(),
            &state,
            &in_flight,
            |e| events.push(e),
        );
        assert!(events.iter().any(|e| matches!(
            e,
            HostEvent::Exited { reason: Some(r), .. } if r == "too far behind"
        )));
    }

    #[test]
    fn reader_no_sessions_error_makes_exit_carry_the_reason() {
        // An empty / no-server mux: `tmux -CC attach` emits a "no sessions" %error
        // block then a bare %exit. The reader must fold the error body into the Exited
        // reason so the app can tell "reachable but empty" from "dead host".
        let state = test_state(80, 24);
        let in_flight: InFlight = Default::default();
        let mut events = Vec::new();
        let lines = vec![
            "%begin 1 1 0".to_string(),
            "no sessions".to_string(),
            "%error 1 1 0".to_string(),
            "%exit".to_string(),
        ]
        .into_iter();
        run_reader(
            "jupiter06",
            test_control_proto(),
            lines,
            &state,
            &in_flight,
            |e| events.push(e),
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                HostEvent::Exited { reason: Some(r), .. } if r.contains("no sessions")
            )),
            "the exit reason carries the no-sessions error"
        );
    }

    #[test]
    fn writer_serializes_commands_and_correlates() {
        // The writer writes each command's exact bytes and pushes ONE Ignore
        // correlator per command line, keeping the FIFO in lockstep with the
        // `%begin` blocks the reader pops.
        let (tx, rx) = std::sync::mpsc::channel::<HostCmd>();
        let in_flight: InFlight = Default::default();
        tx.send(HostCmd::Send("refresh-client -f no-output\n".to_string()))
            .unwrap();
        tx.send(HostCmd::Resize { cols: 80, rows: 24 }).unwrap();
        tx.send(HostCmd::Shutdown).unwrap();
        drop(tx);
        let mut out: Vec<u8> = Vec::new();
        run_writer(rx, test_control_proto(), &mut out, &in_flight);
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("refresh-client -f no-output\n"));
        assert!(s.contains("refresh-client -C 80x24\n"));
        // One Ignore per command line written: send + resize = 2.
        assert_eq!(in_flight.lock().unwrap().len(), 2);
    }

    #[test]
    fn reader_resolves_list_panes_block_into_inventory() {
        // A session's window/pane subtree must arrive via an explicit list-panes
        // query (correlated to the session's `source/name` address); otherwise the
        // session stays on "loading…" forever.
        let state = test_state(80, 24);
        let in_flight: InFlight = Default::default();
        in_flight
            .lock()
            .unwrap()
            .push_back(PendingReply::ListPanes {
                address: "jupiter00/if".into(),
            });
        let mut events = Vec::new();
        // PANE_FORMAT: window_index, window_active, pane_index, pane_active,
        // pane_current_command, window_name.
        let lines = vec![
            "%begin 1 5 1".to_string(),
            "0\t1\t0\t1\tbash\tmain".to_string(),
            "%end 1 5 1".to_string(),
        ]
        .into_iter();
        run_reader(
            "jupiter00",
            test_control_proto(),
            lines,
            &state,
            &in_flight,
            |e| events.push(e),
        );
        let inv = state.inventory.lock().unwrap();
        let panes = inv
            .panes
            .get("jupiter00/if")
            .expect("panes recorded under the session address");
        assert_eq!(panes.len(), 1, "one window parsed");
        assert!(events
            .iter()
            .any(|e| matches!(e, HostEvent::Inventory { .. })));
    }

    #[test]
    fn reader_resolves_display_tty_block_into_event() {
        // A list-clients block resolves to the NON-control client's tty (xmux's display
        // attach), ignoring the -CC metadata client regardless of line order.
        let state = test_state(80, 24);
        let in_flight: InFlight = Default::default();
        in_flight
            .lock()
            .unwrap()
            .push_back(PendingReply::DisplayClientTty);
        let mut events = Vec::new();
        let lines = vec![
            "%begin 1 5 1".to_string(),
            "/dev/pts/7 control-mode".to_string(),
            "/dev/pts/3 active-pane".to_string(),
            "%end 1 5 1".to_string(),
        ]
        .into_iter();
        run_reader(
            "jupiter00",
            test_control_proto(),
            lines,
            &state,
            &in_flight,
            |e| events.push(e),
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                HostEvent::DisplayTty { host, tty: Some(t) } if host == "jupiter00" && t == "/dev/pts/3"
            )),
            "a list-clients block resolves to the non-control client's tty"
        );
    }

    #[test]
    fn writer_query_list_panes_correlates() {
        let (tx, rx) = std::sync::mpsc::channel::<HostCmd>();
        let in_flight: InFlight = Default::default();
        tx.send(HostCmd::Query {
            line: format!("list-panes -s -t if -F '{}'\n", crate::mux::PANE_FORMAT),
            reply: PendingReply::ListPanes {
                address: "jupiter00/if".into(),
            },
        })
        .unwrap();
        tx.send(HostCmd::Shutdown).unwrap();
        drop(tx);
        let mut out: Vec<u8> = Vec::new();
        run_writer(rx, test_control_proto(), &mut out, &in_flight);
        let s = String::from_utf8(out).unwrap();
        assert!(
            s.contains("list-panes -s -t if -F"),
            "writes the list-panes command: {s}"
        );
        assert!(
            matches!(in_flight.lock().unwrap().front(), Some(PendingReply::ListPanes { address }) if address == "jupiter00/if"),
            "pushes the ListPanes correlator keyed by the session address"
        );
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
    fn fake_source(host: &str) -> crate::source::Source {
        crate::source::Source {
            alias: host.into(),
            binary: "cmd.exe".into(),
            kind: crate::machine::MachineKind::Local { socket: None },
            runner: None,
        }
    }

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
        let src = fake_source("jupiter06");
        let host =
            crate::model::Host::new(crate::machine::local(None), crate::mux::for_binary("psmux"));
        assert!(!mgr.ensure("jupiter06", &host, &src, 80, 24).unwrap());
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
        let src = fake_source("local");
        assert!(
            mgr.ensure("local", &host, &src, 80, 24).unwrap(),
            "first ensure spawns the poll task"
        );
        assert!(
            mgr.get("local").is_none(),
            "a poll host has no control client"
        );
        assert!(
            !mgr.ensure("local", &host, &src, 80, 24).unwrap(),
            "ensure is idempotent while the poll task lives"
        );
        mgr.reap("local");
        assert!(
            mgr.ensure("local", &host, &src, 80, 24).unwrap(),
            "reap aborted the task so ensure re-spawns it"
        );
        mgr.teardown_all();
    }
}
