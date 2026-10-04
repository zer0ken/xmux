//! The session switcher UI: the pure tree model (`tree`) and the interactive
//! ratatui application (`switcher`). The model layer is side-effect-free; the
//! rendering is layered on top separately.

pub mod chrome;
pub mod modal;
pub mod ops;
pub(crate) mod palette;
pub mod run;
pub mod switcher;
pub(crate) mod toast;
pub mod tree;

pub use tree::{
    add_session, filter_groups, fuzzy_match, remove_session, rename_session, sort_by_name, Group,
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

#[cfg(test)]
mod tests {
    use unicode_width::UnicodeWidthChar;

    #[test]
    fn persistent_ui_glyphs_stay_on_the_safe_one_cell_allow_list() {
        const SAFE: &[char] = &['❯', '✓', '✗', '⠋', '╭', '▲', '?', '…', '·'];
        let persistent = [
            crate::ui::switcher::SELECTED_MARK.chars().next().unwrap(),
            crate::ui::chrome::BLOCK_MARK.chars().next().unwrap(),
            crate::ui::chrome::UNREACHABLE_MARK.chars().next().unwrap(),
            crate::ui::chrome::LIST_FAILED_MARK.chars().next().unwrap(),
            crate::ui::switcher::MIDDLE_ELLIPSIS,
        ]
        .into_iter()
        .chain(
            [
                crate::state::notify::Level::Success,
                crate::state::notify::Level::Info,
                crate::state::notify::Level::Warning,
                crate::state::notify::Level::Error,
            ]
            .map(|level| level.glyph().chars().next().unwrap()),
        );
        for glyph in persistent.chain(std::iter::once(super::SPINNER[0])) {
            assert!(SAFE.contains(&glyph), "unsafe UI glyph {glyph:?}");
            assert_eq!(
                UnicodeWidthChar::width(glyph),
                Some(1),
                "wide UI glyph {glyph:?}"
            );
        }
    }
}
