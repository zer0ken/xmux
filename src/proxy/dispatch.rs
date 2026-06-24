//! The cockpit's unified input vocabulary. Each input surface RESOLVES raw bytes
//! into a list of `Action`s and the cockpit run loop APPLIES them, so resolution
//! stays pure (side-effect free, unit-testable) and every command flows through one
//! apply site. `TermInput` (mux-focus keys) emits these; the tree-focus path joins
//! the same vocabulary.
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
