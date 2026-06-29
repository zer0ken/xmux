//! Runtime domain state: the single source of truth the new architecture's
//! components read from. Carries the cockpit loop's inventory, selection,
//! display-truth, and focus fields.
use crate::cockpit::Selection;
use crate::session::WindowPanes;
use crate::ui::tree::Group;
use std::collections::{HashMap, HashSet};
use std::time::Instant;

/// The cockpit's canonical runtime state.
#[derive(Default)]
pub struct State {
    /// Inventory — hosts → sessions → windows → panes (all reachable). The single
    /// source of truth every component reads, instead of reaching into the tree.
    // ponytail: flat fields, not an Inventory sub-struct — bundle them if a reader
    // ever needs the whole group at once.
    pub groups: Vec<Group>,
    pub panes: HashMap<String, Vec<WindowPanes>>,
    /// Sources whose `list-sessions` has not yet returned (host shows scanning…).
    pub scanning: HashSet<String>,
    /// Session addresses whose `list-panes` has resolved (success or failure).
    pub panes_loaded: HashSet<String>,
    /// Active fuzzy-filter text (drives the visible tree + the footer).
    pub filter: String,
    /// What the tree cursor points at — the session/window to show.
    pub selection: Selection,
    /// The address whose content is confirmed live in the on-screen terminal view —
    /// the single display truth. The grid is shown only when this matches the
    /// selection's session, so a stale attachment (mid-reattach) renders
    /// "(attaching…)" rather than the previous session: displayed == selected holds
    /// structurally. Set only at confirmation (a synchronous switch, or DisplayReady).
    pub displayed: Selection,
    /// When set, a settled selection is attached once this instant passes.
    pub attach_deadline: Option<Instant>,
    /// The session address last persisted as the user's last-selected, so it is
    /// not rewritten on every window step within the same session.
    pub last_saved_session: String,
    /// The cockpit's focus state machine — which pane keys go to and whether a
    /// modal is open. The single source of truth for focus.
    pub focus: crate::proxy::app::Focus,
}

impl State {
    /// Builds the inventory from a complete snapshot: every host is resolved
    /// (reachable or unreachable per its `err`) and every session's panes are
    /// considered known. Other state fields stay default.
    pub fn from_scan(scan: crate::ui::switcher::Scan) -> State {
        let panes_loaded = scan
            .groups
            .iter()
            .flat_map(|g| g.sessions.iter().map(|sess| sess.address()))
            .collect();
        State {
            groups: scan.groups,
            panes: scan.panes,
            panes_loaded,
            ..State::default()
        }
    }

    /// Seeds the inventory from the resolved source list alone — no probing — so
    /// the first frame paints host-skeleton rows, each in a scanning state. Other
    /// state fields stay default.
    pub fn from_sources(aliases: Vec<String>) -> State {
        let scanning = aliases.iter().cloned().collect();
        let groups = aliases
            .into_iter()
            .map(|source| Group {
                source,
                err: None,
                sessions: Vec::new(),
            })
            .collect();
        State {
            scanning,
            groups,
            ..State::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_state_is_empty() {
        let s = State::default();
        assert!(s.selection.is_empty());
        assert!(s.displayed.is_empty());
        assert!(s.attach_deadline.is_none());
        assert_eq!(s.last_saved_session, "");
        assert!(s.focus.is_tree_focused());
    }
}
