//! Per-host data types shared between the reader thread, writer thread, and cockpit.

use std::collections::HashMap;

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
}
