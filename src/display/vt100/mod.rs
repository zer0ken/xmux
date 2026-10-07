//! The terminal parser the grid runs a session's output through: the `vt100` crate
//! (0.16.2, MIT, by Jesse Luehrs; its license is beside this file), carried in the crate
//! so the grid can keep what the published crate does not model. See this directory's
//! Working Notes for what differs from the published crate.

// The crate's public surface is kept whole; the grid uses part of it.
#![allow(dead_code)]
#![allow(clippy::all)]

mod attrs;
mod callbacks;
mod cell;
mod grid;
mod parser;
mod perform;
mod row;
mod screen;
mod term;

pub use attrs::Color;
pub use callbacks::Callbacks;
pub use cell::Cell;
pub use parser::Parser;
pub use screen::{MouseProtocolEncoding, MouseProtocolMode, Screen};
