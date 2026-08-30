//! The app's raw-byte INPUT set. Each input surface RESOLVES raw bytes
//! into a list of these `Action`s, so resolution stays pure (side-effect free,
//! unit-testable). The semantic ones project to the DOMAIN [`crate::model::Action`]
//! via [`Action::as_action`], which `State::apply` folds in; the byte-carrying and
//! render-only variants are dispatched directly. `TermInput` (terminal-view focus keys) emits
//! these; the nav-focus path joins the same set.
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
    /// `prefix` then Left/Tab — move focus back to the nav. Carries any bytes
    /// that followed the switch command in the same read: focus has changed, so the
    /// caller must hand them to the nav, not the pane.
    FocusNav(Vec<u8>),
    /// Move focus to the terminal view (nav `Enter`, or `prefix` Right/Tab in nav focus).
    FocusTerminal,
    /// A nav key to hand to `Switcher::handle_key` (navigation / input row).
    NavKey(KeyEvent),
    /// `prefix` then `q` — quit the app.
    Quit,
    /// `prefix ?` — toggle the keys help modal. Focus stays on the terminal view.
    ShowHelp,
    /// `prefix h`/`l` or `prefix Ctrl+←/→` — adjust the nav WIDTH by this signed delta
    /// (the horizontal axis; applied only in the Side layout).
    Width(i32),
    /// `prefix Ctrl+↑/↓` — adjust the nav HEIGHT by this signed delta (the vertical axis;
    /// applied only in the portrait Top layout). +1 grows (taller), -1 shrinks.
    Height(i32),
    /// `prefix t` — toggle auto-hide-nav mode.
    ToggleAutoHide,
}

impl Action {
    /// The DOMAIN action this input action carries, if it is a semantic one. The
    /// byte-carrying variants (`Forward`, `FocusNav`'s replay bytes) and the pure
    /// render toggles (`ShowHelp`, `NavKey`) are transport/render concerns with no
    /// domain meaning, so they project to `None`. `FocusNav(bytes)` resolves to
    /// `Focus(Nav)` — the bytes it also carries are replayed separately by the caller.
    pub fn as_action(&self) -> Option<crate::model::Action> {
        use crate::model::Action as DomainAction;
        match self {
            Action::Quit => Some(DomainAction::Quit),
            Action::Width(d) => Some(DomainAction::NavWidth(*d)),
            Action::ToggleAutoHide => Some(DomainAction::ToggleAutoHide),
            Action::FocusTerminal => Some(DomainAction::Focus(FocusTarget::Terminal)),
            Action::FocusNav(_) => Some(DomainAction::Focus(FocusTarget::Nav)),
            // Height resize is key-driven only (no ctl verb yet); it is applied directly on
            // the nav-input path, not through a domain action.
            Action::Height(_) | Action::Forward(_) | Action::ShowHelp | Action::NavKey(_) => None,
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
            Some(DomainAction::NavWidth(-1))
        );
        assert_eq!(
            Action::ToggleAutoHide.as_action(),
            Some(DomainAction::ToggleAutoHide)
        );
        assert_eq!(
            Action::FocusTerminal.as_action(),
            Some(DomainAction::Focus(FocusTarget::Terminal))
        );
        assert_eq!(
            Action::FocusNav(vec![]).as_action(),
            Some(DomainAction::Focus(FocusTarget::Nav))
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
            Action::NavKey(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE)).as_action(),
            None,
        );
    }
}
