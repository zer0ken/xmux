//! The shared per-host vocabulary: the inventory data plus the command/event/reply
//! types the reader thread, writer thread, and app exchange over their channels.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

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
    /// First list-sessions returned. Carries the parsed sessions so the loop folds
    /// them into `model::Host.inventory` (the single owner) — the reader keeps no
    /// shared inventory of its own.
    Connected {
        host: String,
        sessions: Vec<Session>,
    },
    /// A list-sessions reply resolved — carries the parsed sessions for the loop to
    /// fold into `model::Host.inventory` and re-apply to the tree. (Pane subtrees
    /// arrive separately as [`HostEvent::Panes`], the same carrier the poll path uses.)
    Inventory {
        host: String,
        sessions: Vec<Session>,
    },
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
    /// stream, so its [`HostManager`](super::HostManager)-owned poll task emits this onto the
    /// same bus. `err` carries a transient enumeration failure (shown in the tree; attachments
    /// are kept — the keep-alive guarantee).
    Sessions {
        source: String,
        sessions: Vec<Session>,
        err: Option<String>,
    },
    /// A per-session window/pane subtree resolved (keyed by the session's
    /// `source/name` address). Emitted by the poll task after `Sessions`, and by the
    /// control reader when a `list-panes` block resolves — both paths carry pane data
    /// the same way, applied purely by `apply_event`.
    Panes {
        address: String,
        panes: Vec<WindowPanes>,
    },
}

/// The reader's shared liveness flag the app also reads. The parsed inventory is no
/// longer held here — the reader carries sessions/panes on `HostEvent`s and the loop
/// folds them into `model::Host.inventory` (the single owner).
pub struct ReaderState {
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
}
