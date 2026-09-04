//! The shared per-host types: the inventory data plus the command/event/reply
//! types the reader thread, writer thread, and app exchange over their channels.

use std::collections::VecDeque;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use crate::session::Session;

/// One host's session inventory, seeded from list-sessions and
/// kept live by notifications. The app reads it to (re)build the tree. This is
/// a METADATA channel only - the per-session PTY attachments own the pixels.
pub struct HostInventory {
    pub sessions: Vec<Session>,
}

impl HostInventory {
    pub fn new() -> Self {
        Self {
            sessions: Vec::new(),
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
    /// them into `model::Host.inventory` (the single owner) - the reader keeps no
    /// shared inventory of its own.
    Connected {
        host: String,
        sessions: Vec<Session>,
    },
    /// A list-sessions reply resolved - carries the parsed sessions for the loop to
    /// fold into `model::Host.inventory` and re-apply to the tree.
    Inventory {
        host: String,
        sessions: Vec<Session>,
    },
    /// A `%`-notification reports the server's session/window STRUCTURE CHANGED
    /// (added, closed, renamed, or the set of sessions) - the app must REFETCH
    /// (re-run list-sessions), since the notification carries only an
    /// id, not the new structure. Resyncs the nav (#5).
    Changed { host: String },
    /// `%exit` / EOF - reap.
    Exited {
        host: String,
        reason: Option<String>,
    },
    /// `%client-detached <client>` - some client of this host detached. The reader
    /// does not know which client is xmux's display attach (that tty lives on the
    /// supervisor's `Host.display_tty`), so it forwards the client tty; the supervisor
    /// reaps the display attach ONLY when `client` matches `Host.display_tty`.
    ClientDetached { host: String, client: String },
    /// `%client-session-changed <client> $id <name>` - some client's attached session
    /// changed (another client, not this -CC metadata connection's own). The reader does
    /// not know which client is xmux's display attach (that tty lives on `Host.display_tty`),
    /// so it forwards the client tty + the new session name; the supervisor follows the nav
    /// selection ONLY when `client` matches `Host.display_tty` - i.e. xmux's OWN display PTY
    /// was moved to another session by the mux itself (e.g. the user's `prefix`+`s`).
    ClientSessionChanged {
        host: String,
        client: String,
        session: String,
    },
    /// A `list-clients` probe over the -CC control connection resolved: this host's
    /// display-client tty - the client the mux protocol identifies as xmux's display
    /// attach - or `None` if it has not registered yet. Captured OUT-OF-BAND over
    /// the control connection, not via an in-band attach-shell marker (a Windows
    /// ConPTY consumes the marker's OSC before the pump can read it). Recorded on
    /// `Host.display_tty` so a later `switch-client -c <tty>` targets xmux's own client.
    DisplayTty { host: String, tty: Option<String> },
    /// A machine's MUX DISCOVERY resolved: `muxes` is every mux xmux supports that
    /// answered on `machine`. Emitted once per machine by a fire-and-forget task, AFTER
    /// launch, so the app paints its configured sources immediately and the muxes nobody
    /// wrote down arrive as they are found. Carries the machine (not a source id): the
    /// answer is about the machine, and each mux beyond the one already served becomes a
    /// source of its own.
    MuxesFound { machine: String, muxes: Vec<String> },
    /// A ROSTER RE-RESOLUTION resolved: which machines the config and the roster
    /// providers name RIGHT NOW. Emitted by a fire-and-forget task a re-scan starts, so
    /// the subprocess each provider runs never blocks the loop. The loop reconciles it
    /// against the registries, because deciding what to add and what to tear down needs
    /// the host registry and the live connections.
    RosterResolved {
        roster: Box<crate::provision::env::Roster>,
    },
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
    /// are kept - the keep-alive guarantee).
    Sessions {
        source: String,
        sessions: Vec<Session>,
        err: Option<String>,
    },
    /// A MACHINE'S REACHABILITY probe resolved: `ssh <machine> true` (or an inline
    /// connect for a local/WSL machine). `err` is `None` when the machine connected,
    /// else ssh's own failure line - its auth-failure signature classifies the machine
    /// LOCKED and any other failure UNREACHABLE. Emitted once per machine, bounded, so
    /// only a connected machine goes on to mux discovery and a metadata channel; a
    /// locked or unreachable one classifies its cards without opening one. `rescan` is
    /// true when a re-scan raised the probe, so a connected machine re-enumerates its
    /// live channel instead of only ensuring it.
    MachineProbed {
        machine: String,
        err: Option<String>,
        rescan: bool,
    },
}
/// The reader's shared liveness flag the app also reads. The parsed inventory is no
/// longer held here - the reader carries sessions/panes on `HostEvent`s and the loop
/// folds them into `model::Host.inventory` (the single owner).
pub struct ReaderState {
    pub connecting: Arc<AtomicBool>,
}

/// The in-flight command correlation FIFO, shared with the writer.
pub type InFlight = Arc<Mutex<VecDeque<PendingReply>>>;

/// What a resolved `%begin…%end` block means to the reader.
pub enum PendingReply {
    ListSessions,
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
