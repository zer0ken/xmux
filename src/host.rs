//! Per-host data types shared between the reader thread, writer thread, and cockpit.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::mux::{parse_panes, parse_sessions};
use crate::proxy::control_proto::{
    classify, decode_output_into, strip_extended_prefix, Line, Notif,
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
}

impl HostInventory {
    pub fn new() -> Self {
        Self {
            sessions: Vec::new(),
            panes: HashMap::new(),
            attached_session: None,
            active_pane: None,
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
    for line in lines {
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
                let kind = in_flight
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or(PendingReply::Ignore);
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
        Notif::Pause { .. } | Notif::Continue { .. } | Notif::LayoutChange { .. } | Notif::Other => {}
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
}
