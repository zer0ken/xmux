//! xmux is a stateless cross-environment session switcher: one terminal that sees
//! and moves between every reachable tmux/psmux session — local and over ssh —
//! regardless of OS or mux kind.
//!
//! This is a binary-internal crate. The layers below `cli` are crate-internal;
//! `cli::run` is the sole public entry called by the binary shim in `main.rs`.

pub mod attach;
pub mod backend;
pub mod cli;
pub mod cockpit;
pub mod config;
pub mod control;
pub mod discovery;
pub mod display;
pub mod env;
pub mod host;
pub mod manage;
pub mod model;
pub mod mux;
pub mod prefs;
pub mod proxy;
pub mod session;
pub mod source;
pub mod state;
pub mod ui;
