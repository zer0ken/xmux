//! Per-host data types shared between the reader thread, writer thread, and cockpit.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::mux::{parse_panes, parse_sessions, SESSION_FORMAT};
use crate::proxy::control_proto::{
    classify, decode_output_into, pause_after_line, refresh_size_line, send_keys_line,
    strip_extended_prefix, Line, Notif,
};
use crate::proxy::screen::Grid;
use crate::session::{Session, WindowPanes};

/// One host's session/window inventory, seeded from list-sessions/list-windows
/// and kept live by notifications. The cockpit reads it to (re)build the tree.
pub struct HostInventory {
    pub sessions: Vec<Session>,
    pub panes: HashMap<String, Vec<WindowPanes>>,
    /// Name set by the last switch-client.
    pub attached_session: Option<String>,
    /// `"%N"` of the attached session.
    pub active_pane: Option<String>,
    /// Panes the server paused for flow control (`%pause`); cleared on `%continue`.
    /// The cockpit resumes each after the renderer has caught up.
    pub paused_panes: HashSet<String>,
}

impl HostInventory {
    pub fn new() -> Self {
        Self {
            sessions: Vec::new(),
            panes: HashMap::new(),
            attached_session: None,
            active_pane: None,
            paused_panes: HashSet::new(),
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
    SendKeys { pane: String, bytes: Vec<u8> },
    SwitchClient { target: String },
    Resize { cols: u16, rows: u16 },
    /// A command line whose `%begin` block carries a meaningful reply. The writer
    /// pushes `reply` onto the FIFO in lockstep with writing `line`, so the
    /// correlation cannot race the writer (pushing from the calling thread could).
    Query { line: String, reply: PendingReply },
    Shutdown,
}

/// A parsed event the reader emits to the cockpit's `select!` loop.
pub enum HostEvent {
    /// First list-sessions returned.
    Connected { host: String },
    /// Sessions/windows changed — rebuild tree.
    Inventory { host: String },
    /// `%output` fed the grid — redraw.
    Output { host: String },
    /// `%session-changed` confirmed.
    Attached { host: String, session: String },
    /// An active-pane/window probe resolved. `session` is the session the probe was
    /// validated against (carried so the cockpit need not re-read `attached_session`,
    /// which may have changed by the time it handles this event). `window` is that
    /// session's active window INDEX, `Some` only when the probe still matched, so
    /// the cockpit can sync the sidebar cursor after an external window change.
    Focus { host: String, session: String, window: Option<i64> },
    /// `%session-window-changed`: the attached session's active window changed
    /// (e.g. another client switched it). The cockpit probes the new active window.
    WindowChanged { host: String },
    /// `%exit` / EOF — reap.
    Exited { host: String, reason: Option<String> },
}

/// The reader's shared state the cockpit also reads.
pub struct ReaderState {
    pub grid: Arc<Mutex<Grid>>,
    pub inventory: Arc<Mutex<HostInventory>>,
    pub connecting: Arc<AtomicBool>,
}

/// The in-flight command correlation FIFO, shared with the writer.
pub type InFlight = Arc<Mutex<VecDeque<PendingReply>>>;

/// What a resolved `%begin…%end` block means to the reader.
pub enum PendingReply {
    ListSessions,
    ListPanes { address: String },
    /// An active-pane/window probe for `session`. `local` marks a per-session
    /// (psmux) connection, which never issues switch-client: there the reply may
    /// beat the `%session-changed`, so it is accepted while `attached_session` is
    /// unset. A host-level (tmux) probe (`local = false`) is accepted ONLY when
    /// `session` still matches the attached one, so a stale reply from a rapid
    /// re-switch cannot set the active pane for a session we already left.
    ActivePane { session: String, local: bool },
    /// `capture-pane -p -e` of the now-attached session: its body is the current
    /// screen, fed into the grid so a freshly-switched (static) session shows its
    /// content immediately instead of staying blank until it next changes.
    CaptureScreen,
    Ignore,
}

/// Runs the line state machine over `lines` (an `Iterator<Item=String>` of stdout
/// lines, already split on `\n`), driving `state`, `in_flight`, and emitting events
/// via `emit`. Returns when the iterator ends (child EOF). Pure over its inputs so
/// a test feeds canned bytes; the real reader wraps a `BufRead`.
pub fn run_reader<E: FnMut(HostEvent)>(
    host: &str,
    lines: impl Iterator<Item = String>,
    state: &ReaderState,
    in_flight: &InFlight,
    mut emit: E,
) {
    let mut decode_buf: Vec<u8> = Vec::with_capacity(4096);
    // num, kind, body — the open `%begin` block, if any.
    let mut block: Option<(u64, PendingReply, Vec<String>)> = None;
    for line in lines {
        // The entry DCS `\x1bP1000p` ([research §1]) introduces control mode. It may
        // arrive on its own line or glued to the first `%begin`. Strip it; a lone DCS
        // becomes empty (Body, ignored), a glued line classifies its remainder as
        // `%begin`. Correlation does NOT depend on it: blocks are matched by the
        // `%begin` flags bit (see below), not by a fragile startup-banner heuristic.
        let line = line.strip_prefix("\x1bP1000p").map(str::to_string).unwrap_or(line);
        // Inside a block, only the matching %end/%error closes it; everything
        // else is body (notifications never appear inside a block).
        if let Some((num, _, _)) = block.as_ref() {
            let num = *num;
            let close = matches!(classify(&line), Line::End { num: n } | Line::Error { num: n } if n == num);
            if close {
                let (_, kind, body) = block.take().unwrap();
                resolve_block(host, kind, &body, state, &mut emit);
            } else {
                // Re-borrow only to push; the `as_ref` borrow above has ended.
                block.as_mut().unwrap().2.push(line);
            }
            continue;
        }
        match classify(&line) {
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
            Line::Output { data, .. } => {
                decode_output_into(&mut decode_buf, data.as_bytes());
                feed_grid(state, &decode_buf);
                clear_connecting(state);
                emit(HostEvent::Output { host: host.to_string() });
            }
            Line::ExtendedOutput { rest, .. } => {
                let data = strip_extended_prefix(rest.as_bytes());
                decode_output_into(&mut decode_buf, data);
                feed_grid(state, &decode_buf);
                clear_connecting(state);
                emit(HostEvent::Output { host: host.to_string() });
            }
            Line::Notification(n) => dispatch_notif(host, n, state, &mut emit),
            // Stray frame/body outside a block.
            Line::End { .. } | Line::Error { .. } | Line::Body(_) => {}
        }
    }
    // Iterator ended = child stdout EOF.
    emit(HostEvent::Exited { host: host.to_string(), reason: None });
}

/// Resolves a closed `%begin…%end` block by applying its body to the inventory
/// and emitting the follow-up events.
fn resolve_block<E: FnMut(HostEvent)>(
    host: &str,
    kind: PendingReply,
    body: &[String],
    state: &ReaderState,
    emit: &mut E,
) {
    match kind {
        PendingReply::ListSessions => {
            let out = body.join("\n");
            let sessions = parse_sessions(host, &out);
            state.inventory.lock().unwrap().sessions = sessions;
            clear_connecting(state);
            emit(HostEvent::Connected { host: host.to_string() });
            emit(HostEvent::Inventory { host: host.to_string() });
        }
        PendingReply::ListPanes { address } => {
            let out = body.join("\n");
            let panes = parse_panes(&out);
            state.inventory.lock().unwrap().panes.insert(address, panes);
            emit(HostEvent::Inventory { host: host.to_string() });
        }
        PendingReply::ActivePane { session, local } => {
            // Body is a `display-message` line `PANE=%N WIN=<index>`. Record the
            // active pane and emit Focus carrying the active WINDOW index so the
            // cockpit can sync the sidebar cursor after an external window change.
            let pane = body.iter().find_map(|ln| {
                ln.split_whitespace().find_map(|f| f.strip_prefix("PANE=").map(str::to_string))
            });
            let window = body
                .iter()
                .find_map(|ln| ln.split_whitespace().find_map(|f| f.strip_prefix("WIN=")))
                .and_then(|w| w.parse::<i64>().ok());
            // Accept a HOST-level (tmux) probe only when `session` is still the
            // attached one (a stale reply from a rapid re-switch must not set the
            // pane). A per-session (psmux) probe never switch-clients, so its reply
            // can beat the `%session-changed` — accept it while unset too. A
            // rejected reply carries no window (it would sync the wrong session).
            let window = {
                let mut inv = state.inventory.lock().unwrap();
                let matched = inv.attached_session.as_deref() == Some(session.as_str())
                    || (local && inv.attached_session.is_none());
                if matched {
                    if let Some(p) = pane {
                        inv.active_pane = Some(p);
                    }
                    window
                } else {
                    None
                }
            };
            emit(HostEvent::Focus { host: host.to_string(), session: session.clone(), window });
        }
        PendingReply::CaptureScreen => {
            // Repaint the grid from the captured screen: reset the SGR state, clear
            // it, home the cursor, then feed the captured lines. The leading
            // `\x1b[m` matters because `\x1b[2J` paints cleared cells with the
            // currently-active background (background-colour erase); without the
            // reset, a non-default background left over from the previous screen
            // bleeds onto every cell the capture does not cover. control mode never
            // resends a static session's screen on switch-client, so this seeds it;
            // live %output takes over for subsequent changes.
            let mut bytes: Vec<u8> = b"\x1b[m\x1b[2J\x1b[3J\x1b[H".to_vec();
            for (i, line) in body.iter().enumerate() {
                if i > 0 {
                    bytes.extend_from_slice(b"\r\n");
                }
                bytes.extend_from_slice(line.as_bytes());
            }
            state.grid.lock().unwrap().feed(&bytes);
            emit(HostEvent::Output { host: host.to_string() });
        }
        PendingReply::Ignore => {}
    }
}

/// Applies one notification to the inventory and emits the matching event.
fn dispatch_notif<E: FnMut(HostEvent)>(
    host: &str,
    notif: Notif<'_>,
    state: &ReaderState,
    emit: &mut E,
) {
    match notif {
        Notif::SessionChanged { name, .. } => {
            state.inventory.lock().unwrap().attached_session = Some(name.to_string());
            emit(HostEvent::Attached { host: host.to_string(), session: name.to_string() });
        }
        Notif::SessionsChanged
        | Notif::WindowAdd { .. }
        | Notif::WindowClose { .. }
        | Notif::WindowRenamed { .. } => {
            // Cockpit re-issues list-sessions / list-windows on these.
            emit(HostEvent::Inventory { host: host.to_string() });
        }
        Notif::WindowPaneChanged { pane, .. } => {
            state.inventory.lock().unwrap().active_pane = Some(pane.to_string());
        }
        Notif::SessionWindowChanged { .. } => {
            // The attached session's active window changed (often another client
            // switched it). The cockpit probes the new active window and syncs the
            // sidebar cursor; the session list/pane structure is unchanged, so this
            // is NOT a tree rebuild.
            emit(HostEvent::WindowChanged { host: host.to_string() });
        }
        Notif::Exit { reason } => {
            emit(HostEvent::Exited {
                host: host.to_string(),
                reason: reason.map(str::to_string),
            });
        }
        Notif::ClientDetached => {
            emit(HostEvent::Exited { host: host.to_string(), reason: None });
        }
        Notif::Pause { pane } => {
            state.inventory.lock().unwrap().paused_panes.insert(pane.to_string());
        }
        Notif::Continue { pane } => {
            state.inventory.lock().unwrap().paused_panes.remove(pane);
        }
        Notif::LayoutChange { .. } | Notif::Other => {}
    }
}

/// Routes decoded `%output` bytes to the single repaint grid (v1: no per-pane
/// filtering — all output feeds the one grid).
fn feed_grid(state: &ReaderState, bytes: &[u8]) {
    state.grid.lock().unwrap().feed(bytes);
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
            HostCmd::SendKeys { pane, bytes } => {
                let line = send_keys_line(&pane, &bytes);
                // Empty burst yields no command line → push nothing (keeps lockstep).
                if !line.is_empty() {
                    in_flight.lock().unwrap().push_back(PendingReply::Ignore);
                    if w.write_all(line.as_bytes()).is_err() {
                        return;
                    }
                }
            }
            HostCmd::SwitchClient { target } => {
                // Two lines, two FIFO entries: the switch's own ack (Ignore), then
                // the active-pane probe. The probe targets the full `target` (so it
                // reports the SELECTED window's pane + index for a `session:window`
                // target), but the correlator validates against the SESSION NAME —
                // `%session-changed` reports the name, not `session:window`, so a
                // bare `target` would never match and the pane would stay unset.
                // Quote the target for the command lines (a name with spaces/quotes
                // would otherwise split into several control-mode args); the session
                // name for validation comes from the RAW target.
                let session = crate::mux::session_name(&target).to_string();
                let qt = crate::mux::quote_target(&target);
                in_flight.lock().unwrap().push_back(PendingReply::Ignore);
                if w.write_all(format!("switch-client -t {qt}\n").as_bytes()).is_err() {
                    return;
                }
                in_flight
                    .lock()
                    .unwrap()
                    .push_back(PendingReply::ActivePane { session, local: false });
                if w
                    .write_all(
                        format!("display-message -p -t {qt} 'PANE=#{{pane_id}} WIN=#{{window_index}}'\n")
                            .as_bytes(),
                    )
                    .is_err()
                {
                    return;
                }
            }
            HostCmd::Resize { cols, rows } => {
                in_flight.lock().unwrap().push_back(PendingReply::Ignore);
                if w.write_all(refresh_size_line(cols, rows).as_bytes()).is_err() {
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
/// OS threads. The cockpit holds the `cmd_tx` to drive it and reads `grid`/
/// `inventory`/`connecting` for the tree and the live screen.
pub struct HostClient {
    /// Stable host id (the source name), echoed back on every `HostEvent`.
    pub host: String,
    /// The repaint grid the reader feeds; the cockpit renders from it.
    pub grid: Arc<Mutex<Grid>>,
    /// Live session/window inventory, kept current by the reader.
    pub inventory: Arc<Mutex<HostInventory>>,
    /// True until any wire activity proves the channel is live.
    pub connecting: Arc<AtomicBool>,
    /// Current client size; updated by `resize`.
    pub size: (u16, u16),
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
    /// flow-control pause → list-sessions). `events` is the cockpit's loop sink.
    pub fn spawn(
        host: impl Into<String>,
        argv: &[String],
        cols: u16,
        rows: u16,
        events: tokio::sync::mpsc::UnboundedSender<HostEvent>,
        extra_env: &[(&str, &str)],
    ) -> anyhow::Result<HostClient> {
        anyhow::ensure!(!argv.is_empty(), "HostClient::spawn: argv must not be empty");
        let host = host.into();

        let mut cmd = Command::new(&argv[0]);
        cmd.args(&argv[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Strip EVERY mux session var (all `PSMUX*`, `TMUX`, `TMUX_PANE` — see
        // `source::is_mux_var`), not just `PSMUX_SESSION`: a per-session psmux
        // control child must not inherit stale psmux routing state (e.g. an
        // ambient `PSMUX_SESSION_NAME`) that could override its `-s <session>`
        // target and attach the wrong server.
        for (k, _) in std::env::vars() {
            if crate::source::is_mux_var(&k) {
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

        // Grid::new takes ROWS first.
        let grid = Arc::new(Mutex::new(Grid::new(rows, cols)));
        let inventory = Arc::new(Mutex::new(HostInventory::new()));
        let connecting = Arc::new(AtomicBool::new(true));
        let in_flight: InFlight = Arc::new(Mutex::new(VecDeque::new()));
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<HostCmd>();

        // Reader thread: stdout lines → state machine; events to the async loop via
        // the non-blocking, thread-safe UnboundedSender.
        let state = ReaderState {
            grid: Arc::clone(&grid),
            inventory: Arc::clone(&inventory),
            connecting: Arc::clone(&connecting),
        };
        let reader_host = host.clone();
        let reader_in_flight = Arc::clone(&in_flight);
        let reader_events = events.clone();
        let reader = std::thread::spawn(move || {
            let lines = BufReader::new(stdout)
                .lines()
                .map_while(Result::ok);
            run_reader(
                &reader_host,
                lines,
                &state,
                &reader_in_flight,
                |e| {
                    let _ = reader_events.send(e);
                },
            );
        });

        // Writer thread: owns the child stdin, drains the command channel.
        let writer_in_flight = Arc::clone(&in_flight);
        let writer = std::thread::spawn(move || {
            run_writer(cmd_rx, &mut stdin, &writer_in_flight);
        });

        // Connect sequence: refresh-client -C size, enable flow control, list.
        let _ = cmd_tx.send(HostCmd::Resize { cols, rows });
        let _ = cmd_tx.send(HostCmd::Send(pause_after_line(2)));
        let _ = cmd_tx.send(HostCmd::Query {
            // SESSION_FORMAT contains TABs; single-quote it so tmux's line parser
            // keeps it as one arg (an unquoted tab would split the format).
            line: format!("list-sessions -F '{SESSION_FORMAT}'\n"),
            reply: PendingReply::ListSessions,
        });

        Ok(HostClient {
            host,
            grid,
            inventory,
            connecting,
            size: (cols, rows),
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
            line: format!("list-sessions -F '{SESSION_FORMAT}'\n"),
            reply: PendingReply::ListSessions,
        });
    }

    /// Requests every pane across all windows of `session`, correlating the reply
    /// to `address` (the switcher's `source/name` session key) so the reader fills
    /// that session's window/pane subtree. Without this a session's children stay
    /// on the "loading…" placeholder forever — the control client never volunteers
    /// pane data, it must be asked.
    pub fn list_panes(&self, session: &str, address: String) {
        // Quote the target so a session name with spaces/quotes survives the
        // control-mode command parser (it splits on whitespace).
        let _ = self.cmd_tx.send(HostCmd::Query {
            line: format!(
                "list-panes -s -t {} -F '{}'\n",
                crate::mux::quote_target(session),
                crate::mux::PANE_FORMAT
            ),
            reply: PendingReply::ListPanes { address },
        });
    }

    /// Probe the active pane + window index of `target` (`display-message -p -t
    /// <target> 'PANE=%N WIN=<idx>'`). `target` may be a session (`api`) or a
    /// `session:window`; the reply's correlator validates against the SESSION NAME
    /// (the part before `:`), since `%session-changed` reports the name. `local =
    /// true` for a per-session psmux connection, which never issues switch-client —
    /// the connection IS its session — so its reply is accepted while
    /// `attached_session` is unset (otherwise `active_pane` stays `None` and input
    /// finds no pane). `local = false` for a host-level (tmux) probe, which stays
    /// strict so a stale reply from a rapid re-switch cannot set a left session's pane.
    pub fn probe_active_pane(&self, target: &str, local: bool) {
        let session = crate::mux::session_name(target).to_string();
        let _ = self.cmd_tx.send(HostCmd::Query {
            line: format!(
                "display-message -p -t {} 'PANE=#{{pane_id}} WIN=#{{window_index}}'\n",
                crate::mux::quote_target(target)
            ),
            reply: PendingReply::ActivePane { session, local },
        });
    }

    /// Make `target` (`session:window`) the active window of its session
    /// (`select-window -t <target>`). Used for a per-session psmux window-row
    /// selection so the connection streams that window's live `%output` and its
    /// active pane resolves — psmux has no control-mode switch-client to do it.
    pub fn select_window_on(&self, target: &str) {
        let _ = self.cmd_tx.send(HostCmd::Send(format!(
            "select-window -t {}\n",
            crate::mux::quote_target(target)
        )));
    }

    /// Switch this client to `session` (writer also probes the active pane).
    pub fn switch_client(&self, session: impl Into<String>) {
        let _ = self
            .cmd_tx
            .send(HostCmd::SwitchClient { target: session.into() });
    }

    /// Seed the grid with `target`'s current screen via `capture-pane -p -e` (its
    /// reply feeds the grid). Control mode only streams a pane on CHANGE, so a
    /// just-selected static session would otherwise stay blank (or show the
    /// previous session's stale content) until it next redraws; capturing paints
    /// it at once. `-e` keeps SGR colors; `-p` prints to the reply.
    pub fn capture_screen(&self, target: &str) {
        // Quote the target so a session/window name with spaces/quotes survives the
        // control-mode command parser.
        let _ = self.cmd_tx.send(HostCmd::Query {
            line: format!("capture-pane -p -e -t {}\n", crate::mux::quote_target(target)),
            reply: PendingReply::CaptureScreen,
        });
    }

    /// Resume a pane the server paused for flow control, once the renderer has
    /// caught up (queues `refresh-client -A %pane:continue`).
    pub fn resume_pane(&self, pane: &str) {
        let _ = self.cmd_tx.send(HostCmd::Send(
            crate::proxy::control_proto::continue_pane_line(pane),
        ));
    }

    /// Forward a raw input burst to `pane`.
    pub fn send_keys(&self, pane: impl Into<String>, bytes: Vec<u8>) {
        let _ = self
            .cmd_tx
            .send(HostCmd::SendKeys { pane: pane.into(), bytes });
    }

    /// Resize the grid (ROWS first) and tell the child its new client size.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.grid.lock().unwrap().resize(rows, cols);
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
    }
}

/// Owns one [`HostClient`] per host alias, spawning each lazily on first use and
/// reaping it on `%exit`/EOF. The bound is the host count: at most one control
/// child per host. Every client emits onto the one shared `events` sink the
/// cockpit's loop drains.
pub struct HostManager {
    clients: HashMap<String, HostClient>,
    events: tokio::sync::mpsc::UnboundedSender<HostEvent>,
}

impl HostManager {
    pub fn new(events: tokio::sync::mpsc::UnboundedSender<HostEvent>) -> Self {
        Self {
            clients: HashMap::new(),
            events,
        }
    }

    /// Ensures `host`'s client is connected, spawning it lazily from `src`. A
    /// no-op (`Ok(false)`) if already connected; otherwise spawns, inserts, and
    /// returns `Ok(true)`. `HostClient::spawn` queues the connect sequence
    /// (resize → pause-after → list-sessions) itself.
    pub fn ensure(
        &mut self,
        host: &str,
        src: &crate::source::Source,
        cols: u16,
        rows: u16,
    ) -> anyhow::Result<bool> {
        if self.clients.contains_key(host) {
            return Ok(false);
        }
        let client = HostClient::spawn(host, &src.control_argv(), cols, rows, self.events.clone(), &[])?;
        self.clients.insert(host.to_string(), client);
        Ok(true)
    }

    /// Ensures a per-SESSION local psmux client keyed by `key` (the session
    /// address), attaching `session` via `psmux -CC new-session -A -s <session>`
    /// so it routes to that session's own server (psmux is one-server-per-session,
    /// so a single host-level connection cannot reach the others). A no-op if `key`
    /// is already connected; returns `true` when a fresh client was spawned.
    pub fn ensure_session(
        &mut self,
        key: &str,
        src: &crate::source::Source,
        session: &str,
        cols: u16,
        rows: u16,
    ) -> anyhow::Result<bool> {
        if self.clients.contains_key(key) {
            return Ok(false);
        }
        let client = HostClient::spawn(
            key,
            &src.control_argv_session(session),
            cols,
            rows,
            self.events.clone(),
            &[],
        )?;
        self.clients.insert(key.to_string(), client);
        Ok(true)
    }

    pub fn get(&self, host: &str) -> Option<&HostClient> {
        self.clients.get(host)
    }

    pub fn get_mut(&mut self, host: &str) -> Option<&mut HostClient> {
        self.clients.get_mut(host)
    }

    /// `%exit`/EOF: drop the client (bounded teardown join). The cockpit keeps the
    /// last-known tree in its switcher state, so the inventory is not re-fetched here.
    pub fn reap(&mut self, host: &str) {
        if let Some(c) = self.clients.remove(host) {
            c.teardown();
        }
    }

    pub fn resize_all(&mut self, cols: u16, rows: u16) {
        for c in self.clients.values_mut() {
            c.resize(cols, rows);
        }
    }

    /// Drains and tears down every client (bounded join per client).
    pub fn teardown_all(self) {
        for (_, c) in self.clients {
            c.teardown();
        }
    }
}

#[cfg(test)]
impl HostManager {
    /// Inserts a real no-op control child keyed by `host`, proving the map insert
    /// without a live `-CC` server. `cmd.exe /c rem` spawns and exits immediately,
    /// so its stdout EOFs at once and `teardown`'s joins return. Shared by the
    /// `host` and `cockpit` test modules.
    pub(crate) fn insert_fake(&mut self, host: &str) {
        let argv: Vec<String> = ["cmd.exe", "/c", "rem"].iter().map(|s| s.to_string()).collect();
        let client = HostClient::spawn(host, &argv, 80, 24, self.events.clone(), &[]).expect("spawn");
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
        assert!(inv.attached_session.is_none());
        assert!(inv.active_pane.is_none());
    }

    #[test]
    fn host_event_carries_host() {
        let e = HostEvent::Attached { host: "jupiter06".into(), session: "api".into() };
        match e {
            HostEvent::Attached { host, session } => {
                assert_eq!(host, "jupiter06");
                assert_eq!(session, "api");
            }
            _ => panic!("variant"),
        }
    }

    /// Builds a `ReaderState` with a `cols`×`rows` grid (note `Grid::new` takes
    /// ROWS first), an empty inventory, and `connecting = true`.
    fn test_state(cols: u16, rows: u16) -> ReaderState {
        ReaderState {
            grid: Arc::new(Mutex::new(Grid::new(rows, cols))),
            inventory: Arc::new(Mutex::new(HostInventory::new())),
            connecting: Arc::new(AtomicBool::new(true)),
        }
    }

    #[test]
    fn reader_decodes_output_into_grid() {
        let state = test_state(80, 24);
        let in_flight: InFlight = Default::default();
        let mut events = Vec::new();
        let lines = vec!["%output %0 HELLO\\012WORLD".to_string()].into_iter();
        run_reader("jupiter06", lines, &state, &in_flight, |e| events.push(e));
        let g = state.grid.lock().unwrap();
        let mut buf = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 80, 24));
        g.render_into(&mut buf, ratatui::layout::Rect::new(0, 0, 80, 24));
        assert_eq!(buf[(0, 0)].symbol(), "H");
        assert!(events.iter().any(|e| matches!(e, HostEvent::Output { .. })));
    }

    #[test]
    fn reader_resolves_list_sessions_block_into_inventory() {
        let state = test_state(80, 24);
        let in_flight: InFlight = Default::default();
        in_flight.lock().unwrap().push_back(PendingReply::ListSessions);
        let mut events = Vec::new();
        let lines = vec![
            "%begin 1 5 1".to_string(),
            "2\t1\t1700000000\tapi".to_string(),
            "%end 1 5 1".to_string(),
        ]
        .into_iter();
        run_reader("jupiter06", lines, &state, &in_flight, |e| events.push(e));
        let inv = state.inventory.lock().unwrap();
        assert_eq!(inv.sessions.len(), 1);
        assert_eq!(inv.sessions[0].name, "api");
        assert_eq!(inv.sessions[0].source, "jupiter06");
        assert!(events.iter().any(|e| matches!(e, HostEvent::Connected { .. })));
        assert!(!state.connecting.load(std::sync::atomic::Ordering::Acquire));
    }

    #[test]
    fn reader_capture_screen_block_feeds_the_grid() {
        // A capture-pane reply must repaint the grid with the captured screen so a
        // freshly-switched static session is not blank.
        let state = test_state(80, 24);
        let in_flight: InFlight = Default::default();
        in_flight.lock().unwrap().push_back(PendingReply::CaptureScreen);
        let mut events = Vec::new();
        let lines = vec![
            "%begin 1 5 1".to_string(),
            "hello from the captured screen".to_string(),
            "%end 1 5 1".to_string(),
        ]
        .into_iter();
        run_reader("jupiter00", lines, &state, &in_flight, |e| events.push(e));
        let g = state.grid.lock().unwrap();
        let mut buf = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 80, 24));
        g.render_into(&mut buf, ratatui::layout::Rect::new(0, 0, 80, 24));
        let row0: String = (0..30).map(|x| buf[(x, 0)].symbol()).collect();
        assert!(row0.starts_with("hello from the captured screen"), "grid row0 = {row0:?}");
        assert!(events.iter().any(|e| matches!(e, HostEvent::Output { .. })));
    }

    #[test]
    fn reader_capture_screen_repaint_resets_stale_background() {
        // A prior screen can leave a non-default background active in the grid's
        // SGR state. `\x1b[2J` fills cleared cells with the CURRENTLY-ACTIVE
        // background (background-colour erase), so without an SGR reset first the
        // newly-captured screen inherits the stale background on every cell the
        // capture does not cover (issue #2: switching windows left the old
        // background uncleared).
        let state = test_state(80, 24);
        // The previous window left a red background active in the grid.
        state.grid.lock().unwrap().feed(b"\x1b[41m");
        let in_flight: InFlight = Default::default();
        in_flight.lock().unwrap().push_back(PendingReply::CaptureScreen);
        let lines = vec![
            "%begin 1 5 1".to_string(),
            "hi".to_string(),
            "%end 1 5 1".to_string(),
        ]
        .into_iter();
        run_reader("local/work", lines, &state, &in_flight, |_| {});
        let g = state.grid.lock().unwrap();
        let mut buf = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 80, 24));
        g.render_into(&mut buf, ratatui::layout::Rect::new(0, 0, 80, 24));
        // A cell the capture never covered (row 1) must be the default background,
        // not the stale red the clear would otherwise have painted.
        assert_eq!(
            buf[(0, 1)].bg,
            ratatui::style::Color::Reset,
            "cleared cell must use the default bg, not the stale red"
        );
    }

    #[test]
    fn reader_active_pane_resolves_when_attached_session_unset() {
        // A per-session local (psmux) connection never issues switch-client, so
        // `attached_session` can still be None when the active-pane probe resolves
        // (or the probe reply beats the `%session-changed`). The resolver must then
        // still record the pane — otherwise local terminal input finds no active
        // pane and is silently dropped (issue #6).
        let state = test_state(80, 24);
        let in_flight: InFlight = Default::default();
        in_flight
            .lock()
            .unwrap()
            .push_back(PendingReply::ActivePane { session: "work".into(), local: true });
        let lines = vec![
            "%begin 1 5 1".to_string(),
            "PANE=%7".to_string(),
            "%end 1 5 1".to_string(),
        ]
        .into_iter();
        run_reader("local/work", lines, &state, &in_flight, |_| {});
        assert_eq!(state.inventory.lock().unwrap().active_pane.as_deref(), Some("%7"));
    }

    #[test]
    fn reader_active_pane_ignores_stale_reply_for_a_left_session() {
        // Once attached to session B, a late probe reply for session A must NOT
        // clobber the active pane (the guard still protects a real switch).
        let state = test_state(80, 24);
        state.inventory.lock().unwrap().attached_session = Some("B".into());
        let in_flight: InFlight = Default::default();
        in_flight
            .lock()
            .unwrap()
            .push_back(PendingReply::ActivePane { session: "A".into(), local: false });
        let lines = vec![
            "%begin 1 5 1".to_string(),
            "PANE=%99".to_string(),
            "%end 1 5 1".to_string(),
        ]
        .into_iter();
        run_reader("h", lines, &state, &in_flight, |_| {});
        assert_eq!(state.inventory.lock().unwrap().active_pane, None, "stale reply ignored");
    }

    #[test]
    fn reader_active_pane_probe_parses_window_and_emits_focus() {
        // The focus probe returns `PANE=%N WIN=<index>`; the reader records the
        // pane and emits Focus carrying the active window index so the cockpit can
        // sync the sidebar cursor after an external window change (issue #5).
        let state = test_state(80, 24);
        state.inventory.lock().unwrap().attached_session = Some("api".into());
        let in_flight: InFlight = Default::default();
        in_flight
            .lock()
            .unwrap()
            .push_back(PendingReply::ActivePane { session: "api".into(), local: false });
        let mut events = Vec::new();
        let lines = vec![
            "%begin 1 5 1".to_string(),
            "PANE=%4 WIN=2".to_string(),
            "%end 1 5 1".to_string(),
        ]
        .into_iter();
        run_reader("jupiter06", lines, &state, &in_flight, |e| events.push(e));
        assert_eq!(state.inventory.lock().unwrap().active_pane.as_deref(), Some("%4"));
        // Focus carries the validated session so the cockpit need not re-read the
        // (possibly-changed) attached_session when it handles the event.
        assert!(events.iter().any(|e| matches!(
            e,
            HostEvent::Focus { host, session, window: Some(2) } if host == "jupiter06" && session == "api"
        )));
    }

    #[test]
    fn reader_host_level_probe_rejected_when_attached_session_unset() {
        // A HOST-level (tmux) active-pane probe (local=false) must NOT set the pane
        // while attached_session is unset: during a rapid re-switch a stale reply
        // could land before `%session-changed`. Only a per-session (local=true)
        // probe accepts the unset case.
        let state = test_state(80, 24);
        let in_flight: InFlight = Default::default();
        in_flight
            .lock()
            .unwrap()
            .push_back(PendingReply::ActivePane { session: "api".into(), local: false });
        let lines = vec![
            "%begin 1 5 1".to_string(),
            "PANE=%3 WIN=0".to_string(),
            "%end 1 5 1".to_string(),
        ]
        .into_iter();
        run_reader("jupiter06", lines, &state, &in_flight, |_| {});
        assert_eq!(
            state.inventory.lock().unwrap().active_pane,
            None,
            "host-level probe must be rejected while attached_session is unset"
        );
    }

    #[test]
    fn reader_focus_window_is_none_for_a_stale_session_reply() {
        // A probe reply for a session we have since left must not carry a window
        // index (it would sync the cursor to the wrong session's window).
        let state = test_state(80, 24);
        state.inventory.lock().unwrap().attached_session = Some("B".into());
        let in_flight: InFlight = Default::default();
        in_flight
            .lock()
            .unwrap()
            .push_back(PendingReply::ActivePane { session: "A".into(), local: false });
        let mut events = Vec::new();
        let lines = vec![
            "%begin 1 5 1".to_string(),
            "PANE=%9 WIN=7".to_string(),
            "%end 1 5 1".to_string(),
        ]
        .into_iter();
        run_reader("h", lines, &state, &in_flight, |e| events.push(e));
        assert!(events
            .iter()
            .any(|e| matches!(e, HostEvent::Focus { window: None, .. })));
    }

    #[test]
    fn reader_session_window_changed_emits_window_changed() {
        // An external client switching the attached session's active window makes
        // tmux emit %session-window-changed; the cockpit probes the new window.
        let state = test_state(80, 24);
        let in_flight: InFlight = Default::default();
        let mut events = Vec::new();
        run_reader(
            "jupiter06",
            vec!["%session-window-changed $0 @1".to_string()].into_iter(),
            &state,
            &in_flight,
            |e| events.push(e),
        );
        assert!(events
            .iter()
            .any(|e| matches!(e, HostEvent::WindowChanged { host } if host == "jupiter06")));
    }

    #[test]
    fn reader_session_changed_sets_attached_and_emits() {
        let state = test_state(80, 24);
        let in_flight: InFlight = Default::default();
        let mut events = Vec::new();
        run_reader(
            "jupiter06",
            vec!["%session-changed $1 api".to_string()].into_iter(),
            &state,
            &in_flight,
            |e| events.push(e),
        );
        assert_eq!(state.inventory.lock().unwrap().attached_session.as_deref(), Some("api"));
        assert!(events
            .iter()
            .any(|e| matches!(e, HostEvent::Attached { session, .. } if session == "api")));
    }

    #[test]
    fn reader_marks_and_clears_paused_pane() {
        let state = test_state(80, 24);
        let in_flight: InFlight = Default::default();
        run_reader("h", vec!["%pause %3".to_string()].into_iter(), &state, &in_flight, |_| {});
        assert!(state.inventory.lock().unwrap().paused_panes.contains("%3"));
        run_reader("h", vec!["%continue %3".to_string()].into_iter(), &state, &in_flight, |_| {});
        assert!(!state.inventory.lock().unwrap().paused_panes.contains("%3"));
    }

    #[test]
    fn resume_pane_queues_continue_line() {
        let (tx, rx) = std::sync::mpsc::channel::<HostCmd>();
        let in_flight: InFlight = Default::default();
        tx.send(HostCmd::Send(crate::proxy::control_proto::continue_pane_line("%3"))).unwrap();
        tx.send(HostCmd::Shutdown).unwrap();
        drop(tx);
        let mut out = Vec::new();
        run_writer(rx, &mut out, &in_flight);
        assert!(String::from_utf8(out).unwrap().contains("refresh-client -A %3:continue\n"));
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
            in_flight.lock().unwrap().push_back(PendingReply::ListSessions);
            let lines = vec![
                "\x1bP1000p".to_string(),
                "%begin 1 1 0".to_string(),
                "%end 1 1 0".to_string(),
                "%begin 1 2 1".to_string(),
                "2\t1\t1700000000\tapi".to_string(),
                "%end 1 2 1".to_string(),
            ]
            .into_iter();
            run_reader("jupiter06", lines, &state, &in_flight, |_| {});
            let inv = state.inventory.lock().unwrap();
            assert_eq!(inv.sessions.len(), 1, "flags=0 banner stole the ListSessions entry");
            assert_eq!(inv.sessions[0].name, "api");
        }
        // GLUED framing: the DCS prefixed onto the flags=0 banner `%begin` line.
        {
            let state = test_state(80, 24);
            let in_flight: InFlight = Default::default();
            in_flight.lock().unwrap().push_back(PendingReply::ListSessions);
            let lines = vec![
                "\x1bP1000p%begin 1 1 0".to_string(),
                "%end 1 1 0".to_string(),
                "%begin 1 2 1".to_string(),
                "2\t1\t1700000000\tapi".to_string(),
                "%end 1 2 1".to_string(),
            ]
            .into_iter();
            run_reader("jupiter06", lines, &state, &in_flight, |_| {});
            let inv = state.inventory.lock().unwrap();
            assert_eq!(inv.sessions.len(), 1, "glued flags=0 banner stole the ListSessions entry");
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
        in_flight.lock().unwrap().push_back(PendingReply::ListSessions);
        let lines = vec![
            "\x1bP1000p%begin 1 10 1".to_string(), // DCS glued to the first reply
            "2\t1\t1700000000\tapi".to_string(),
            "%end 1 10 1".to_string(),
            "%begin 1 11 0".to_string(), // spontaneous: must NOT consume a correlator
            "%end 1 11 0".to_string(),
        ]
        .into_iter();
        run_reader("jupiter00", lines, &state, &in_flight, |_| {});
        let inv = state.inventory.lock().unwrap();
        assert_eq!(inv.sessions.len(), 1, "list-sessions resolved against the flags=1 block");
        assert_eq!(inv.sessions[0].name, "api");
    }

    #[test]
    fn reader_spontaneous_block_does_not_steal_a_pending_reply() {
        // A flags=0 block arriving BEFORE our command reply (another client ran a
        // command first) must not consume our queued correlator.
        let state = test_state(80, 24);
        let in_flight: InFlight = Default::default();
        in_flight.lock().unwrap().push_back(PendingReply::ListSessions);
        let lines = vec![
            "%begin 1 5 0".to_string(), // spontaneous, flags=0
            "noise".to_string(),
            "%end 1 5 0".to_string(),
            "%begin 1 6 1".to_string(), // our list-sessions reply, flags=1
            "3\t1\t1700000000\twork".to_string(),
            "%end 1 6 1".to_string(),
        ]
        .into_iter();
        run_reader("h", lines, &state, &in_flight, |_| {});
        assert_eq!(state.inventory.lock().unwrap().sessions.len(), 1);
    }

    #[test]
    fn reader_exit_emits_exited() {
        let state = test_state(80, 24);
        let in_flight: InFlight = Default::default();
        let mut events = Vec::new();
        run_reader(
            "jupiter06",
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
    fn writer_serializes_commands_and_correlates() {
        let (tx, rx) = std::sync::mpsc::channel::<HostCmd>();
        let in_flight: InFlight = Default::default();
        tx.send(HostCmd::Send(pause_after_line(2))).unwrap();
        tx.send(HostCmd::SendKeys { pane: "%0".into(), bytes: vec![0x03] }).unwrap();
        tx.send(HostCmd::Resize { cols: 80, rows: 24 }).unwrap();
        tx.send(HostCmd::Shutdown).unwrap();
        drop(tx);
        let mut out: Vec<u8> = Vec::new();
        run_writer(rx, &mut out, &in_flight);
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("refresh-client -f pause-after=2\n"));
        assert!(s.contains("send-keys -t %0 -H 03\n"));
        assert!(s.contains("refresh-client -C 80x24\n"));
        // One Ignore per command line written: pause + send-keys + resize = 3.
        assert_eq!(in_flight.lock().unwrap().len(), 3);
    }

    #[test]
    fn writer_switch_client_pushes_correlation() {
        let (tx, rx) = std::sync::mpsc::channel::<HostCmd>();
        let in_flight: InFlight = Default::default();
        tx.send(HostCmd::SwitchClient { target: "api".into() }).unwrap();
        tx.send(HostCmd::Shutdown).unwrap();
        drop(tx);
        let mut out: Vec<u8> = Vec::new();
        run_writer(rx, &mut out, &in_flight);
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("switch-client -t api\n"));
        // PANE=/WIN= prefixes + message-arg form so the reader parses the active
        // pane AND the active window index (for the sidebar window sync).
        assert!(s.contains("display-message -p -t api 'PANE=#{pane_id} WIN=#{window_index}'\n"));
        // switch-client's ack pushes an Ignore ahead, so the ActivePane is not at
        // the front — it just must be present and lockstep-correct.
        assert!(in_flight
            .lock()
            .unwrap()
            .iter()
            .any(|r| matches!(r, PendingReply::ActivePane { .. })));
    }

    #[test]
    fn writer_switch_client_window_target_correlates_by_session_name() {
        // Selecting a WINDOW row switches to `session:window`; the probe targets
        // the full `session:window` (to report that window's pane + index) but the
        // correlator validates against the SESSION NAME (`%session-changed` reports
        // `api`, not `api:2`) — else the active pane stays unset and input to a
        // window-row selection is dropped.
        let (tx, rx) = std::sync::mpsc::channel::<HostCmd>();
        let in_flight: InFlight = Default::default();
        tx.send(HostCmd::SwitchClient { target: "api:2".into() }).unwrap();
        tx.send(HostCmd::Shutdown).unwrap();
        drop(tx);
        let mut out: Vec<u8> = Vec::new();
        run_writer(rx, &mut out, &in_flight);
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("switch-client -t api:2\n"));
        assert!(s.contains("display-message -p -t api:2 'PANE=#{pane_id} WIN=#{window_index}'\n"));
        assert!(
            in_flight
                .lock()
                .unwrap()
                .iter()
                .any(|r| matches!(r, PendingReply::ActivePane { session, .. } if session == "api")),
            "correlator validates against the session NAME, not the session:window target"
        );
    }

    #[test]
    fn writer_switch_client_quotes_target_with_spaces() {
        // A session/window name with a space must be quoted for the control-mode
        // parser (else it splits into several args), while the correlator still
        // validates against the raw session name.
        let (tx, rx) = std::sync::mpsc::channel::<HostCmd>();
        let in_flight: InFlight = Default::default();
        tx.send(HostCmd::SwitchClient { target: "my proj:2".into() }).unwrap();
        tx.send(HostCmd::Shutdown).unwrap();
        drop(tx);
        let mut out: Vec<u8> = Vec::new();
        run_writer(rx, &mut out, &in_flight);
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("switch-client -t 'my proj:2'\n"), "switch quoted: {s}");
        assert!(
            s.contains("display-message -p -t 'my proj:2' 'PANE=#{pane_id} WIN=#{window_index}'\n"),
            "probe quoted: {s}"
        );
        assert!(in_flight
            .lock()
            .unwrap()
            .iter()
            .any(|r| matches!(r, PendingReply::ActivePane { session, .. } if session == "my proj")));
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
            .push_back(PendingReply::ListPanes { address: "jupiter00/if".into() });
        let mut events = Vec::new();
        // PANE_FORMAT: window_index, window_active, pane_index, pane_active,
        // pane_current_command, window_name.
        let lines = vec![
            "%begin 1 5 1".to_string(),
            "0\t1\t0\t1\tbash\tmain".to_string(),
            "%end 1 5 1".to_string(),
        ]
        .into_iter();
        run_reader("jupiter00", lines, &state, &in_flight, |e| events.push(e));
        let inv = state.inventory.lock().unwrap();
        let panes = inv
            .panes
            .get("jupiter00/if")
            .expect("panes recorded under the session address");
        assert_eq!(panes.len(), 1, "one window parsed");
        assert!(events.iter().any(|e| matches!(e, HostEvent::Inventory { .. })));
    }

    #[test]
    fn writer_query_list_panes_correlates() {
        let (tx, rx) = std::sync::mpsc::channel::<HostCmd>();
        let in_flight: InFlight = Default::default();
        tx.send(HostCmd::Query {
            line: format!("list-panes -s -t if -F '{}'\n", crate::mux::PANE_FORMAT),
            reply: PendingReply::ListPanes { address: "jupiter00/if".into() },
        })
        .unwrap();
        tx.send(HostCmd::Shutdown).unwrap();
        drop(tx);
        let mut out: Vec<u8> = Vec::new();
        run_writer(rx, &mut out, &in_flight);
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("list-panes -s -t if -F"), "writes the list-panes command: {s}");
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
        let client = HostClient::spawn("local", &argv, 80, 24, tx, &[]).expect("spawn");
        // echo exits immediately, closing pipes → teardown's joins return promptly.
        client.teardown();
    }

    /// A constructible LOCAL `Source` whose `control_argv()` is valid (`cmd.exe`).
    /// `ensure` on an already-present host returns `Ok(false)` without spawning,
    /// so this argv is never actually executed in `manager_ensure_is_idempotent`.
    fn fake_source(host: &str) -> crate::source::Source {
        crate::source::Source {
            alias: host.into(),
            binary: "cmd.exe".into(),
            remote: false,
            control_path: String::new(),
            os: "windows".into(),
            socket: None,
            runner: None,
        }
    }

    #[test]
    fn manager_ensure_is_idempotent() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<HostEvent>();
        let mut mgr = HostManager::new(tx);
        mgr.insert_fake("jupiter06");
        assert!(mgr.get("jupiter06").is_some());
        // ensure on an already-connected host returns Ok(false) (no fresh connect).
        let src = fake_source("jupiter06");
        assert!(!mgr.ensure("jupiter06", &src, 80, 24).unwrap());
    }

    #[test]
    fn manager_reap_drops_client() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<HostEvent>();
        let mut mgr = HostManager::new(tx);
        mgr.insert_fake("jupiter06");
        mgr.reap("jupiter06");
        assert!(mgr.get("jupiter06").is_none(), "reaped client is dropped");
    }
}
