//! The cockpit's unified input vocabulary. Each input surface RESOLVES raw bytes
//! into a list of `Action`s and the cockpit run loop APPLIES them, so resolution
//! stays pure (side-effect free, unit-testable) and every command flows through one
//! apply site. `TermInput` (mux-focus keys) emits these; the tree-focus path joins
//! the same vocabulary.
use crate::model::{FocusTarget, Operation};
use ratatui::crossterm::event::KeyEvent;

#[derive(Debug, PartialEq)]
pub enum Action {
    /// Raw bytes to forward to the focused session's active pane.
    Forward(Vec<u8>),
    /// `prefix` then Left/Tab/Esc — move focus back to the tree. Carries any bytes
    /// that followed the switch command in the same read: focus has changed, so the
    /// caller must hand them to the tree, not the pane.
    FocusTree(Vec<u8>),
    /// Move focus to the mux pane (tree `Enter`, or `prefix` Right/Tab in tree focus).
    FocusMux,
    /// A tree key to hand to `Switcher::handle_key` (navigation / input row / kill).
    TreeKey(KeyEvent),
    /// `prefix` then `q` — quit the cockpit.
    Quit,
    /// `prefix ?` — toggle the keys help overlay. Focus stays on the mux pane.
    ShowHelp,
    /// `prefix h`/`l` or `prefix Ctrl+←/→` — adjust the tree width by this signed delta.
    Width(i32),
    /// `prefix t` — toggle auto-hide-tree mode.
    ToggleAutoHide,
}

impl Action {
    /// The domain command this action carries, if it is a semantic one. The
    /// byte-carrying variants (`Forward`, `FocusTree`'s replay bytes) and the pure
    /// render toggles (`ShowHelp`, `TreeKey`) are transport/render concerns with no
    /// domain meaning, so they project to `None`. `FocusTree(bytes)` resolves to
    /// `Focus(Tree)` — the bytes it also carries are replayed separately by the caller.
    pub fn as_operation(&self) -> Option<Operation> {
        match self {
            Action::Quit => Some(Operation::Quit),
            Action::Width(d) => Some(Operation::TreeWidth(*d)),
            Action::ToggleAutoHide => Some(Operation::ToggleAutoHide),
            Action::FocusMux => Some(Operation::Focus(FocusTarget::Terminal)),
            Action::FocusTree(_) => Some(Operation::Focus(FocusTarget::Tree)),
            Action::Forward(_) | Action::ShowHelp | Action::TreeKey(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{FocusTarget, Operation};
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn semantic_actions_project_to_operations() {
        assert_eq!(Action::Quit.as_operation(), Some(Operation::Quit));
        assert_eq!(
            Action::Width(-1).as_operation(),
            Some(Operation::TreeWidth(-1))
        );
        assert_eq!(
            Action::ToggleAutoHide.as_operation(),
            Some(Operation::ToggleAutoHide)
        );
        assert_eq!(
            Action::FocusMux.as_operation(),
            Some(Operation::Focus(FocusTarget::Terminal))
        );
        assert_eq!(
            Action::FocusTree(vec![]).as_operation(),
            Some(Operation::Focus(FocusTarget::Tree))
        );
    }
    #[test]
    fn byte_and_render_actions_have_no_operation() {
        assert_eq!(Action::Forward(vec![1, 2]).as_operation(), None);
        assert_eq!(
            Action::ShowHelp.as_operation(),
            None,
            "help is a render toggle, not a domain op"
        );
        assert_eq!(
            Action::TreeKey(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE)).as_operation(),
            None,
        );
    }
}
