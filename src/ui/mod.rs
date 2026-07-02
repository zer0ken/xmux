//! The session switcher UI: the pure tree model (`tree`) and the interactive
//! ratatui application (`switcher`). The model layer is side-effect-free; the
//! rendering is layered on top separately.

pub mod chrome;
pub mod modal;
pub mod ops;
pub mod run;
pub mod switcher;
pub mod tree;

pub use tree::{
    add_session, filter_groups, fuzzy_match, remove_session, rename_session, sort_by_recency, Group,
};
