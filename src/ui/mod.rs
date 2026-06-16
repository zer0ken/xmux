//! The session switcher UI: the pure tree model (`tree`), the pane ANSI →
//! styled-text translation (`ansi`), and the interactive ratatui application
//! (`switcher`). The model layer is side-effect-free; the rendering is layered
//! on top separately.

pub mod ansi;
pub mod tree;

pub use tree::{
    add_session, filter_groups, fuzzy_match, remove_session, rename_session, sort_by_recency, Group,
};
