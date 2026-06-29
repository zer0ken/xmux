//! Runtime domain state: the single source of truth the new architecture's
//! components read from. Carries the cockpit loop's inventory, selection,
//! display-truth, focus, and the open modal popup.
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
    /// The single open modal, if any (help / inline input / kill confirm / context
    /// menu). One Option — not four independent fields — so the modals' mutual
    /// exclusion is structural: opening one drops whatever was open, and two can
    /// never coexist. The switcher owns the modal behavior and the transient popup
    /// geometry (drag offset / drawn rect); this owns which modal is open + its content.
    pub(crate) popup: Option<crate::ui::switcher::Popup>,
}

impl State {
    /// True while a centered modal popup (help / inline input / kill confirm) is
    /// open. These three are draggable and drive [`ModalKind::Popup`]; the context
    /// menu is separate (pointer-anchored).
    ///
    /// [`ModalKind::Popup`]: crate::proxy::app::ModalKind::Popup
    pub fn is_modal_popup_open(&self) -> bool {
        use crate::ui::switcher::Popup;
        matches!(
            self.popup,
            Some(Popup::Help | Popup::Input(_) | Popup::Kill(_))
        )
    }

    /// True while an inline input (filter / rename / new) is open. The cockpit
    /// routes every key to the switcher then, with no focus-switch hijack.
    pub fn is_inputting(&self) -> bool {
        matches!(self.popup, Some(crate::ui::switcher::Popup::Input(_)))
    }

    /// True while the right-click context menu is open.
    pub fn menu_active(&self) -> bool {
        matches!(self.popup, Some(crate::ui::switcher::Popup::Menu(_)))
    }

    /// Which kind of modal is open — the focus machine derives its modal dimension
    /// from this each loop-top, so [`Focus`] can never mirror-and-desync from the
    /// open popup. A centered popup and the context menu are mutually exclusive.
    ///
    /// [`Focus`]: crate::proxy::app::Focus
    pub(crate) fn modal_kind(&self) -> Option<crate::proxy::app::ModalKind> {
        use crate::proxy::app::ModalKind;
        use crate::ui::switcher::Popup;
        match self.popup {
            Some(Popup::Help | Popup::Input(_) | Popup::Kill(_)) => Some(ModalKind::Popup),
            Some(Popup::Menu(_)) => Some(ModalKind::Menu),
            None => None,
        }
    }

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
        assert!(s.popup.is_none());
        assert!(!s.is_modal_popup_open());
        assert!(!s.is_inputting());
        assert!(!s.menu_active());
        assert!(s.modal_kind().is_none());
    }
}
