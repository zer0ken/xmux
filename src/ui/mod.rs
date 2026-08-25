//! The session switcher UI: the pure tree model (`tree`) and the interactive
//! ratatui application (`switcher`). The model layer is side-effect-free; the
//! rendering is layered on top separately.

pub mod chrome;
pub mod modal;
pub mod ops;
pub mod prefs;
pub(crate) mod palette;
pub mod run;
pub mod switcher;
pub mod tree;

pub use tree::{
    add_session, filter_groups, fuzzy_match, remove_session, rename_session, sort_by_recency, Group,
};

/// Braille frames of the one spinner xmux animates.
const SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// The spinner glyph for animation frame `frame`, which the chrome advances from
/// wall-clock so every marker turns at the same rate.
///
/// One helper for every in-flight marker in the UI: a card's unresolved level and the
/// hint bar's scan progress turn the SAME glyph on the SAME frame, so one glance reads
/// as one thing loading, not two unrelated animations.
pub(crate) fn spinner_glyph(frame: usize) -> char {
    SPINNER[frame % SPINNER.len()]
}
