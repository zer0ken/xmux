//! A cross-machine, cross-mux session switcher.
//!
//! One terminal that sees and moves between every reachable supported mux session
//! across local machines, WSL, and SSH, regardless of OS or mux kind.
//!
//! This is a binary-internal crate. The layers below `cli` are crate-internal;
//! `cli::run` is the sole public entry called by the binary shim in `main.rs`.

pub mod app;
pub mod cli;
pub mod display;
pub mod driver;
pub mod link;
pub mod logging;
pub mod model;
pub mod mux;
pub mod provision;
pub mod session;
pub mod state;
pub mod transport;
pub mod ui;
