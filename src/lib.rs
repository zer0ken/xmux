//! xmux is a stateless cross-environment session switcher: one terminal that sees
//! and moves between every reachable tmux/psmux session — local and over ssh —
//! regardless of OS or mux kind.
//!
//! This is a binary-internal crate. The layers below `cli` are crate-internal;
//! `cli::run` is the sole public entry called by the binary shim in `main.rs`.

pub mod app;
pub mod cli;
pub mod config;
pub mod discovery;
pub mod display;
pub mod driver;
pub mod env;
pub mod link;
pub mod logging;
pub mod model;
pub mod mux;
pub mod roster;
pub mod session;
pub mod state;
pub mod transport;
pub mod ui;
pub mod update;
