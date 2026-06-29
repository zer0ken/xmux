//! Runtime domain state: the single source of truth the new architecture's
//! components read from. This phase carries the cockpit loop's selection and
//! display-truth fields; later phases fold in focus, inventory, popup, and dirty
//! tracking as the components that consume them land.
use crate::cockpit::Selection;
use std::time::Instant;

/// The cockpit's canonical runtime state.
#[derive(Default)]
pub struct State {
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
    // ponytail: focus/inventory/popup/dirty (spec §4) land in the phase whose
    // components consume them, not speculatively now.
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
    }
}
