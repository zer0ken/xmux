//! The cockpit's raw-byte INPUT vocabulary. Each input surface RESOLVES raw bytes
//! into a list of these `Action`s, so resolution stays pure (side-effect free,
//! unit-testable). The semantic ones project to the DOMAIN [`crate::model::Action`]
//! via [`Action::as_action`], which `State::apply` folds in; the byte-carrying and
//! render-only variants are dispatched directly. `TermInput` (mux-focus keys) emits
//! these; the tree-focus path joins the same vocabulary.
//!
//! This `Action` is distinct from the domain `crate::model::Action`: this is the
//! input layer (raw bytes → intent-or-transport), that is the domain layer (the
//! single thing `State::apply` accepts). They live in separate modules so the two
//! never get conflated.
use crate::model::FocusTarget;
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
    /// The DOMAIN action this input action carries, if it is a semantic one. The
    /// byte-carrying variants (`Forward`, `FocusTree`'s replay bytes) and the pure
    /// render toggles (`ShowHelp`, `TreeKey`) are transport/render concerns with no
    /// domain meaning, so they project to `None`. `FocusTree(bytes)` resolves to
    /// `Focus(Tree)` — the bytes it also carries are replayed separately by the caller.
    pub fn as_action(&self) -> Option<crate::model::Action> {
        use crate::model::Action as DomainAction;
        match self {
            Action::Quit => Some(DomainAction::Quit),
            Action::Width(d) => Some(DomainAction::TreeWidth(*d)),
            Action::ToggleAutoHide => Some(DomainAction::ToggleAutoHide),
            Action::FocusMux => Some(DomainAction::Focus(FocusTarget::Terminal)),
            Action::FocusTree(_) => Some(DomainAction::Focus(FocusTarget::Tree)),
            Action::Forward(_) | Action::ShowHelp | Action::TreeKey(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Action as DomainAction, FocusTarget};
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn semantic_actions_project_to_domain_actions() {
        assert_eq!(Action::Quit.as_action(), Some(DomainAction::Quit));
        assert_eq!(
            Action::Width(-1).as_action(),
            Some(DomainAction::TreeWidth(-1))
        );
        assert_eq!(
            Action::ToggleAutoHide.as_action(),
            Some(DomainAction::ToggleAutoHide)
        );
        assert_eq!(
            Action::FocusMux.as_action(),
            Some(DomainAction::Focus(FocusTarget::Terminal))
        );
        assert_eq!(
            Action::FocusTree(vec![]).as_action(),
            Some(DomainAction::Focus(FocusTarget::Tree))
        );
    }
    #[test]
    fn byte_and_render_actions_have_no_domain_action() {
        assert_eq!(Action::Forward(vec![1, 2]).as_action(), None);
        assert_eq!(
            Action::ShowHelp.as_action(),
            None,
            "help is a render toggle, not a domain action"
        );
        assert_eq!(
            Action::TreeKey(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE)).as_action(),
            None,
        );
    }
}
