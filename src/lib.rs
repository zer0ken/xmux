//! xmux is a stateless cross-environment session switcher: one terminal that sees
//! and moves between every reachable tmux/psmux session — local and over ssh —
//! regardless of OS or mux kind.
//!
//! The library crate holds the layers (the Go `internal/` packages): the data
//! types, the mux argv/parse logic, the ssh/local source boundary, concurrent
//! discovery, lifecycle management, terminal handover, the control channel, and
//! the TUI. The binary crate (`main.rs`) wires them behind a CLI.

pub mod config;
pub mod mux;
pub mod session;
pub mod source;
