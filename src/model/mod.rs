//! The host model: `Host` (its `transport` is a `machine::Transport`, its `mux` a
//! `Box<dyn Mux>`), the mux's server model (`ServerModel`), and the plan/value types
//! they exchange. The mux layer is transport-blind: it supplies mux argv and the
//! `machine::Transport` decides how to run it. The two axes themselves live in
//! `crate::machine` (MACHINE) and `crate::mux` (MUX).

pub mod action;
pub mod death;
pub mod host;
pub mod hosts;
pub mod plan;
pub mod selection;
pub mod server_model;

pub use action::{Action, Command, EventEffect, FocusTarget, MuxOp};
pub use death::{
    display_tty_marker_prefix, matches_display_tty, parse_display_tty_marker, psmux_port_path,
    psmux_session_is_live,
};
pub use host::{Host, HostDisplay, Liveness};
pub use hosts::Hosts;
pub use plan::{DeathSignal, DisplayTty, EventSource};
pub use selection::Selection;
pub use server_model::ServerModel;
