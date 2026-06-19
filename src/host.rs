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
    ActivePane { session: String },
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
    // A `-CC` client emits an unsolicited startup `%begin…%end` banner introduced
    // by the entry DCS `\x1bP1000p` ([research §1]), BEFORE the first command reply.
    // That banner must consume ZERO FIFO entries, else every later pop shifts by one
    // and command replies resolve against the wrong correlator. This flag, armed by
    // the DCS, marks the very next `%begin` as the banner (opened with `Ignore`, no
    // FIFO pop) in BOTH framings: a lone DCS line, and the DCS glued to `%begin`.
    let mut expect_startup_block = false;
    for line in lines {
        // The entry DCS may arrive alone or glued to the first `%begin`. Strip it
        // and arm the flag; a lone DCS becomes empty (falls through as Body), a
        // glued line continues to classify its stripped remainder as `%begin`.
        let line = match line.strip_prefix("\x1bP1000p") {
            Some(rest) => {
                expect_startup_block = true;
                rest.to_string()
            }
            None => line,
        };
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
            Line::Begin { num } => {
                let kind = if expect_startup_block {
                    // The startup banner: ignore its body and do NOT pop the FIFO,
                    // so the real command replies stay in lockstep.
                    expect_startup_block = false;
                    PendingReply::Ignore
                } else {
                    in_flight
                        .lock()
                        .unwrap()
                        .pop_front()
                        .unwrap_or(PendingReply::Ignore)
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
        PendingReply::ActivePane { session } => {
            // Body is a `display-message` line `PANE=%N …`. Record the active pane
            // only when it belongs to the session that is currently attached, so a
            // late reply for a session we have since left does not clobber state.
            if let Some(pane) = body
                .iter()
                .find_map(|ln| ln.split_whitespace().find_map(|f| f.strip_prefix("PANE=")))
            {
                let mut inv = state.inventory.lock().unwrap();
                if inv.attached_session.as_deref() == Some(session.as_str()) {
                    inv.active_pane = Some(pane.to_string());
                }
            }
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
            emit(HostEvent::Inventory { host: host.to_string() });
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
/// correlation entry per command LINE written, so the FIFO stays in lockstep with
/// the `%begin` blocks the reader pops. Flushes after each write so a real child
/// sees commands promptly. Returns on `Shutdown` (or channel close).
pub fn run_writer<W: Write>(
    rx: std::sync::mpsc::Receiver<HostCmd>,
    w: &mut W,
    in_flight: &InFlight,
) {
    while let Ok(cmd) = rx.recv() {
        match cmd {
            HostCmd::Send(line) => {
                let _ = w.write_all(line.as_bytes());
                in_flight.lock().unwrap().push_back(PendingReply::Ignore);
            }
            HostCmd::SendKeys { pane, bytes } => {
                let line = send_keys_line(&pane, &bytes);
                // Empty burst yields no command line → push nothing (keeps lockstep).
                if !line.is_empty() {
                    let _ = w.write_all(line.as_bytes());
                    in_flight.lock().unwrap().push_back(PendingReply::Ignore);
                }
            }
            HostCmd::SwitchClient { target } => {
                // Two lines, two FIFO entries: the switch's own ack (Ignore), then
                // the active-pane probe. The probe MUST emit a `PANE=#{pane_id}`
                // body so the reader's `strip_prefix("PANE=")` resolver parses it.
                let _ = w.write_all(format!("switch-client -t {target}\n").as_bytes());
                in_flight.lock().unwrap().push_back(PendingReply::Ignore);
                let _ = w.write_all(
                    format!("display-message -p -t {target} 'PANE=#{{pane_id}}'\n").as_bytes(),
                );
                in_flight
                    .lock()
                    .unwrap()
                    .push_back(PendingReply::ActivePane { session: target });
            }
            HostCmd::Resize { cols, rows } => {
                let _ = w.write_all(refresh_size_line(cols, rows).as_bytes());
                in_flight.lock().unwrap().push_back(PendingReply::Ignore);
            }
            HostCmd::Query { line, reply } => {
                let _ = w.write_all(line.as_bytes());
                in_flight.lock().unwrap().push_back(reply);
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
    ) -> anyhow::Result<HostClient> {
        anyhow::ensure!(!argv.is_empty(), "HostClient::spawn: argv must not be empty");
        let host = host.into();

        let mut child = Command::new(&argv[0])
            .args(&argv[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env_remove("PSMUX_SESSION")
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .spawn()?;

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

    /// Switch this client to `session` (writer also probes the active pane).
    pub fn switch_client(&self, session: impl Into<String>) {
        let _ = self
            .cmd_tx
            .send(HostCmd::SwitchClient { target: session.into() });
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
        let client = HostClient::spawn(host, &src.control_argv(), cols, rows, self.events.clone())?;
        self.clients.insert(host.to_string(), client);
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
        let client = HostClient::spawn(host, &argv, 80, 24, self.events.clone()).expect("spawn");
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
            "%begin 1 5 0".to_string(),
            "2\t1\t1700000000\tapi".to_string(),
            "%end 1 5 0".to_string(),
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

    /// The `-CC` entry DCS `\x1bP1000p` introduces an unsolicited startup banner
    /// `%begin…%end` block BEFORE the first command reply. That banner must consume
    /// zero FIFO entries; otherwise the queued `ListSessions` resolves against the
    /// empty banner block and the real list-sessions reply is misattributed. Proven
    /// in BOTH framings: the DCS on its own line, and the DCS glued to `%begin`.
    #[test]
    fn reader_startup_banner_keeps_fifo_lockstep() {
        // SEPARATE-line framing: a lone DCS line, then the empty banner block, then
        // the real list-sessions reply.
        {
            let state = test_state(80, 24);
            let in_flight: InFlight = Default::default();
            in_flight.lock().unwrap().push_back(PendingReply::ListSessions);
            let lines = vec![
                "\x1bP1000p".to_string(),
                "%begin 1 1 0".to_string(),
                "%end 1 1 0".to_string(),
                "%begin 1 2 0".to_string(),
                "2\t1\t1700000000\tapi".to_string(),
                "%end 1 2 0".to_string(),
            ]
            .into_iter();
            run_reader("jupiter06", lines, &state, &in_flight, |_| {});
            let inv = state.inventory.lock().unwrap();
            assert_eq!(inv.sessions.len(), 1, "startup banner stole the ListSessions entry");
            assert_eq!(inv.sessions[0].name, "api");
        }
        // GLUED framing: the DCS prefixed onto the first `%begin` line.
        {
            let state = test_state(80, 24);
            let in_flight: InFlight = Default::default();
            in_flight.lock().unwrap().push_back(PendingReply::ListSessions);
            let lines = vec![
                "\x1bP1000p%begin 1 1 0".to_string(),
                "%end 1 1 0".to_string(),
                "%begin 1 2 0".to_string(),
                "2\t1\t1700000000\tapi".to_string(),
                "%end 1 2 0".to_string(),
            ]
            .into_iter();
            run_reader("jupiter06", lines, &state, &in_flight, |_| {});
            let inv = state.inventory.lock().unwrap();
            assert_eq!(inv.sessions.len(), 1, "glued startup banner stole the ListSessions entry");
            assert_eq!(inv.sessions[0].name, "api");
        }
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
        // PANE= prefix + message-arg form so the Task-8 reader's `strip_prefix("PANE=")`
        // resolver parses the active pane (a bare `-F '#{pane_id}'` would not match).
        assert!(s.contains("display-message -p -t api 'PANE=#{pane_id}'\n"));
        // switch-client's ack pushes an Ignore ahead, so the ActivePane is not at
        // the front — it just must be present and lockstep-correct.
        assert!(in_flight
            .lock()
            .unwrap()
            .iter()
            .any(|r| matches!(r, PendingReply::ActivePane { .. })));
    }

    #[test]
    #[ignore = "real -CC is the live gate; this just proves a piped child spawns + tears down"]
    fn host_client_spawns_piped_child() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<HostEvent>();
        let argv: Vec<String> = ["cmd.exe", "/c", "echo", "hi"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let client = HostClient::spawn("local", &argv, 80, 24, tx).expect("spawn");
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
