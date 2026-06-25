//! Per-host data types shared between the reader thread, writer thread, and cockpit.

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::mux::{parse_panes, parse_sessions, SESSION_FORMAT};
use crate::proxy::control_proto::{classify, refresh_size_line, Line, Notif};
use crate::session::{Session, WindowPanes};

/// One host's session/window inventory, seeded from list-sessions/list-panes and
/// kept live by notifications. The cockpit reads it to (re)build the tree. This is
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
    /// A list-sessions / list-panes reply resolved — re-apply the inventory.
    Inventory { host: String },
    /// A `%`-notification reports the server's session/window STRUCTURE CHANGED
    /// (added, closed, renamed, or the set of sessions) — the cockpit must REFETCH
    /// (re-run list-sessions + re-list panes), since the notification carries only an
    /// id, not the new structure. Resyncs the sidebar tree + active-window markers (#5).
    Changed { host: String },
    /// `%session-window-changed`: a session's ACTIVE WINDOW switched. Like `Changed`
    /// the cockpit refetches the markers, but it additionally probes the displayed
    /// session's new active window so the sidebar cursor follows it (#2).
    WindowChanged { host: String },
    /// An active-window probe resolved (`display-message -p '#{window_index}'`): the
    /// cockpit moves the sidebar cursor to window `window` of `session` (a no-op
    /// unless the cursor is on a window row of that session — see
    /// [`crate::ui::switcher::Switcher::select_window`]).
    Focus { host: String, session: String, window: i64 },
    /// `%exit` / EOF — reap.
    Exited { host: String, reason: Option<String> },
    /// `%client-detached <client>` — some client of this host detached. The reader
    /// does not know which client is xmux's display attach (that tty lives on the
    /// supervisor's `Host.display_tty`), so it forwards the client tty; the supervisor
    /// reaps the display attach ONLY when `client` matches `Host.display_tty`.
    ClientDetached { host: String, client: String },
}

/// The reader's shared state the cockpit also reads.
pub struct ReaderState {
    pub inventory: Arc<Mutex<HostInventory>>,
    pub connecting: Arc<AtomicBool>,
}

/// The in-flight command correlation FIFO, shared with the writer.
pub type InFlight = Arc<Mutex<VecDeque<PendingReply>>>;

/// What a resolved `%begin…%end` block means to the reader.
pub enum PendingReply {
    ListSessions,
    ListPanes { address: String },
    /// An active-window probe for `session`: its block body is the active window
    /// index, resolved into a [`HostEvent::Focus`].
    ActiveWindow { session: String },
    Ignore,
}

/// The control-mode command line that prints `session`'s active window index
/// (`display-message -p -t <session> '#{window_index}'`). Pure so the wire format
/// — the escaped `#{…}` braces and the quoted target — is unit-tested.
pub fn active_window_query_line(session: &str) -> String {
    format!(
        "display-message -p -t {} '#{{window_index}}'\n",
        crate::mux::quote_target(session)
    )
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
        let line = line.strip_prefix("\x1bP1000p").map(str::to_string).unwrap_or(line);
        // Inside a block, only the matching %end/%error closes it; everything
        // else is body (notifications never appear inside a block).
        if let Some((num, _, _)) = block.as_ref() {
            let num = *num;
            let (close, is_err) = match classify(&line) {
                Line::End { num: n } if n == num => (true, false),
                Line::Error { num: n } if n == num => (true, true),
                _ => (false, false),
            };
            if close {
                let (_, kind, body) = block.take().unwrap();
                // Remember an error block's text ("no sessions" / "no server running"
                // / …) so a control client that dies before connecting carries it —
                // the cockpit then tells a reachable-but-empty mux from a dead host.
                if is_err {
                    let t = body.join(" ").trim().to_string();
                    if !t.is_empty() {
                        last_error = Some(t);
                    }
                }
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
            // %output is the per-pane PIXEL stream; the per-session PTY attachments
            // own pixels now, and the control client runs with `refresh-client -f
            // no-output`, so it should not arrive. If an older mux that lacks the
            // flag sends it anyway, discard it (just note the channel is live) — the
            // control client is metadata-only.
            Line::Output { .. } | Line::ExtendedOutput { .. } => clear_connecting(state),
            Line::Notification(n) => dispatch_notif(host, n, &last_error, &mut emit),
            // Stray frame/body outside a block.
            Line::End { .. } | Line::Error { .. } | Line::Body(_) => {}
        }
    }
    // Iterator ended = child stdout EOF.
    emit(HostEvent::Exited { host: host.to_string(), reason: last_error });
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
        PendingReply::ActiveWindow { session } => {
            // `display-message -p '#{window_index}'` prints a single line: the active
            // window index. Emit Focus so the cockpit follows the cursor (#2). A
            // missing/garbled body (no parseable index) yields no event.
            if let Some(window) = body.iter().find_map(|l| l.trim().parse::<i64>().ok()) {
                emit(HostEvent::Focus {
                    host: host.to_string(),
                    session,
                    window,
                });
            }
        }
        PendingReply::Ignore => {}
    }
}

/// Maps one notification to the cockpit event it triggers (the metadata client
/// holds no per-session display state, so notifications emit events, not mutate it).
fn dispatch_notif<E: FnMut(HostEvent)>(
    host: &str,
    notif: Notif<'_>,
    last_error: &Option<String>,
    emit: &mut E,
) {
    match notif {
        Notif::SessionsChanged
        | Notif::WindowAdd { .. }
        | Notif::WindowClose { .. }
        | Notif::WindowRenamed { .. } => {
            // The server's session/window STRUCTURE changed; the cockpit refetches
            // (list-sessions + re-list every session's panes), so the sidebar's
            // session list AND per-session active-window markers resync (#5). The
            // notification carries only an id, so a blanket refetch is simplest.
            emit(HostEvent::Changed { host: host.to_string() });
        }
        Notif::SessionWindowChanged { .. } => {
            // A session's ACTIVE WINDOW switched (e.g. another client did prefix-n).
            // WindowChanged refetches the markers like Changed AND has the cockpit
            // probe the displayed session's new active window so the sidebar cursor
            // follows it (#2). The notification carries only ids ($session @window),
            // so the cursor target is resolved by the cockpit's probe, not here.
            emit(HostEvent::WindowChanged { host: host.to_string() });
        }
        // `%session-changed` (the metadata client's own auto-attached session) and
        // `%window-pane-changed` (a pane became active) do not affect the sidebar
        // tree — the per-session PTY attachments own the live pane — so they are inert.
        Notif::SessionChanged { .. } | Notif::WindowPaneChanged { .. } => {}
        Notif::Exit { reason } => {
            // `%exit` may carry its own reason; otherwise fall back to the last error
            // block ("no sessions" / "no server running") so an empty mux is not
            // mistaken for a dead host.
            emit(HostEvent::Exited {
                host: host.to_string(),
                reason: reason.map(str::to_string).or_else(|| last_error.clone()),
            });
        }
        Notif::ClientDetached { client } => emit(HostEvent::ClientDetached {
            host: host.to_string(),
            client: client.to_string(),
        }),
        // %pause/%continue are output flow-control; with `no-output` set there is no
        // output to pause, so they are inert for this metadata-only client.
        Notif::Pause { .. } | Notif::Continue { .. } => {}
        Notif::LayoutChange { .. } | Notif::Other => {}
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
/// OS threads. The cockpit holds the `cmd_tx` to drive it and reads `inventory`/
/// `connecting` for the sidebar tree. This is a METADATA / change-event /
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

        // Connect sequence: size the client, then SUPPRESS %output — this control
        // connection is a metadata / change-event / `select-window` channel ONLY;
        // the per-session PTY attaches own the pixels, so streaming pane output here
        // is pure waste (and risks flooding the single-threaded loop). `no-output`
        // keeps notifications (%window-*/%session-*) flowing but stops %output. An
        // older mux that lacks the flag just %errors it (correlated as Ignore) —
        // harmless. Then list sessions.
        let _ = cmd_tx.send(HostCmd::Resize { cols, rows });
        let _ = cmd_tx.send(HostCmd::Send("refresh-client -f no-output\n".to_string()));
        let _ = cmd_tx.send(HostCmd::Query {
            // SESSION_FORMAT contains TABs; single-quote it so tmux's line parser
            // keeps it as one arg (an unquoted tab would split the format).
            line: format!("list-sessions -F '{SESSION_FORMAT}'\n"),
            reply: PendingReply::ListSessions,
        });

        Ok(HostClient {
            host,
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

    /// Probes `session`'s active window index over this control client. The reply
    /// resolves to a [`HostEvent::Focus`] so the cockpit follows the sidebar cursor
    /// to the new active window after an external `%session-window-changed` (#2).
    pub fn probe_active_window(&self, session: &str) {
        let _ = self.cmd_tx.send(HostCmd::Query {
            line: active_window_query_line(session),
            reply: PendingReply::ActiveWindow {
                session: session.to_string(),
            },
        });
    }

    /// Make `target` (`session:window`) the active window of its session
    /// (`select-window -t <target>`) over this control client. Used to
    /// programmatically switch a window for a window-row selection: the real
    /// attached PTY client follows because the session's active window changes
    /// server-side (#4).
    pub fn select_window_on(&self, target: &str) {
        let _ = self.cmd_tx.send(HostCmd::Send(format!(
            "select-window -t {}\n",
            crate::mux::quote_target(target)
        )));
    }

    /// Tell the child its new client size (the metadata client's size; the PTY
    /// attachments are sized independently by the cockpit).
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

    /// Ensures `host`'s control client is connected, spawning it lazily from `src`.
    /// A no-op (`Ok(false)`) if already connected; otherwise spawns, inserts, and
    /// returns `Ok(true)`. `HostClient::spawn` queues the connect sequence
    /// (resize → no-output → list-sessions) itself.
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

    pub fn get(&self, host: &str) -> Option<&HostClient> {
        self.clients.get(host)
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
        let client =
            HostClient::spawn(host, &argv, 80, 24, self.events.clone(), &[]).expect("spawn");
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
        let e = HostEvent::Changed { host: "jupiter06".into() };
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
            remote: true,
            control_path: String::new(),
            os: std::env::consts::OS.into(),
            socket: None,
            runner: None,
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<HostEvent>();
        let mut mgr = HostManager::new(tx);
        mgr.ensure("jupiter06", &src, 80, 24).expect("spawn control client");
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut connected = false;
        while !connected && Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_secs(20), rx.recv()).await {
                Ok(Some(HostEvent::Connected { .. })) => connected = true,
                Ok(Some(_)) => continue,
                _ => break,
            }
        }
        assert!(connected, "control client must connect to jupiter06 + resolve list-sessions");
        let sessions = mgr.get("jupiter06").unwrap().inventory.lock().unwrap().sessions.clone();
        eprintln!("jupiter06 sessions: {:?}", sessions.iter().map(|s| &s.name).collect::<Vec<_>>());
        assert!(!sessions.is_empty(), "jupiter06 inventory must list its real sessions");
        mgr.teardown_all();
    }

    #[test]
    fn reader_structure_notifications_emit_changed() {
        // A `%`-notification that the server's session/window STRUCTURE changed
        // (added, closed, renamed, or the set of sessions) must emit Changed: it
        // carries only an id, so the cockpit refetches (re-list-sessions +
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
            run_reader("jupiter06", vec![line.to_string()].into_iter(), &state, &in_flight, |e| events.push(e));
            assert!(
                events.iter().any(|e| matches!(e, HostEvent::Changed { host } if host == "jupiter06")),
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
        // these the sidebar misses real-time window add/delete there. They must emit
        // Changed exactly like their linked counterparts so the cockpit refetches.
        for line in [
            "%unlinked-window-add @4",
            "%unlinked-window-close @4",
            "%unlinked-window-renamed @4 logs",
        ] {
            let state = test_state(80, 24);
            let in_flight: InFlight = Default::default();
            let mut events = Vec::new();
            run_reader("jupiter06", vec![line.to_string()].into_iter(), &state, &in_flight, |e| events.push(e));
            assert!(
                events.iter().any(|e| matches!(e, HostEvent::Changed { host } if host == "jupiter06")),
                "{line:?} must emit Changed"
            );
        }
    }

    #[test]
    fn session_window_changed_emits_window_changed_not_changed() {
        // A session's ACTIVE WINDOW switched (`%session-window-changed $id @win`):
        // emit the dedicated WindowChanged so the cockpit not only refetches the
        // markers but also probes the displayed session's new active window and
        // follows the sidebar cursor to it (#2). It must NOT collapse to a blanket
        // Changed (which only refetches and would leave the cursor behind).
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
        assert!(
            events.iter().any(|e| matches!(e, HostEvent::WindowChanged { host } if host == "jupiter06")),
            "%session-window-changed must emit WindowChanged"
        );
        assert!(
            !events.iter().any(|e| matches!(e, HostEvent::Changed { .. })),
            "%session-window-changed must not collapse to a blanket Changed"
        );
    }

    #[test]
    fn reader_resolves_active_window_block_into_focus() {
        // The active-window probe (`display-message -p '#{window_index}'`) returns a
        // single line: the index. Resolving its block emits Focus carrying the
        // session (from the correlator) + the parsed window index, so the cockpit
        // moves the sidebar cursor to that window row.
        let state = test_state(80, 24);
        let in_flight: InFlight = Default::default();
        in_flight
            .lock()
            .unwrap()
            .push_back(PendingReply::ActiveWindow { session: "api".into() });
        let mut events = Vec::new();
        let lines = vec![
            "%begin 1 5 1".to_string(),
            "2".to_string(),
            "%end 1 5 1".to_string(),
        ]
        .into_iter();
        run_reader("jupiter06", lines, &state, &in_flight, |e| events.push(e));
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
        // The probe targets the session and prints its active window index. The
        // format braces are escaped (so `#{window_index}` reaches tmux literally)
        // and a session name with spaces is quoted for the control-mode parser.
        assert_eq!(
            active_window_query_line("api"),
            "display-message -p -t api '#{window_index}'\n"
        );
        assert_eq!(
            active_window_query_line("my proj"),
            "display-message -p -t 'my proj' '#{window_index}'\n"
        );
    }

    #[test]
    fn reader_session_changed_and_pane_changed_are_inert() {
        // `%session-changed` (the metadata client's own attach) and
        // `%window-pane-changed` do not affect the sidebar tree, so they must NOT
        // trigger a Changed refetch. (run_reader always emits a trailing Exited on
        // EOF, so assert specifically that no Changed was emitted.)
        for line in ["%session-changed $1 api", "%window-pane-changed @1 %2"] {
            let state = test_state(80, 24);
            let in_flight: InFlight = Default::default();
            let mut events = Vec::new();
            run_reader("jupiter06", vec![line.to_string()].into_iter(), &state, &in_flight, |e| events.push(e));
            assert!(
                !events.iter().any(|e| matches!(e, HostEvent::Changed { .. })),
                "{line:?} must not trigger a refetch"
            );
        }
    }

    #[test]
    fn client_detached_emits_host_scoped_event_with_client() {
        let mut events = Vec::new();
        dispatch_notif(
            "jupiter06",
            Notif::ClientDetached { client: "/dev/pts/3" },
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
    fn reader_no_sessions_error_makes_exit_carry_the_reason() {
        // An empty / no-server mux: `tmux -CC attach` emits a "no sessions" %error
        // block then a bare %exit. The reader must fold the error body into the Exited
        // reason so the cockpit can tell "reachable but empty" from "dead host".
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
        run_reader("jupiter06", lines, &state, &in_flight, |e| events.push(e));
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
        tx.send(HostCmd::Send("refresh-client -f no-output\n".to_string())).unwrap();
        tx.send(HostCmd::Resize { cols: 80, rows: 24 }).unwrap();
        tx.send(HostCmd::Shutdown).unwrap();
        drop(tx);
        let mut out: Vec<u8> = Vec::new();
        run_writer(rx, &mut out, &in_flight);
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
