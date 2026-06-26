//! Runtime domain state: the single source of truth the new architecture's
//! components read from. This phase carries only the cockpit loop's
//! selection-domain fields; later phases fold in display, focus, inventory,
//! popup, and dirty tracking as the components that consume them land.
use crate::cockpit::Selection;
use std::time::Instant;

/// The cockpit's canonical runtime state.
#[derive(Default)]
pub struct State {
    /// What the tree cursor points at — the session/window to show.
    pub selection: Selection,
    /// The selection last actually attached/switched to (attach debounce latch).
    pub last_attached_sel: Selection,
    /// When set, a settled selection is attached once this instant passes.
    pub attach_deadline: Option<Instant>,
    /// The session address last persisted as the user's last-selected, so it is
    /// not rewritten on every window step within the same session.
    pub last_saved_session: String,
    // ponytail: display/focus/inventory/popup/dirty (spec §4) land in the phase
    // whose components consume them, not speculatively now.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_state_is_empty() {
        let s = State::default();
        assert!(s.selection.is_empty());
        assert!(s.last_attached_sel.is_empty());
        assert!(s.attach_deadline.is_none());
        assert_eq!(s.last_saved_session, "");
    }
}
