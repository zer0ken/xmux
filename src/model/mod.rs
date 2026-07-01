//! The host model: the machine boundary (`Transport`), the mux backend (`Mux`)
//! and its server model (`ServerModel`), and the plan/value types they exchange.
//! A `Host` (built in a later phase) is `Transport × Box<dyn Mux>`. The mux layer
//! is transport-blind: it supplies mux argv and the `Transport` decides how to run it.

pub mod action;
pub mod death;
pub mod host;
pub mod hosts;
pub mod plan;
pub mod server_model;
pub mod transport;

pub use action::{Action, Command, EventEffect, FocusTarget, MuxOp};
pub use death::{
    display_tty_marker_prefix, matches_display_tty, parse_display_tty_marker, psmux_port_path,
    psmux_session_is_live,
};
pub use host::{Host, HostDisplay, Liveness};
pub use hosts::Hosts;
pub use plan::{DeathSignal, DisplayTty, EventSource};
pub use server_model::ServerModel;
pub use transport::{LoweredSwitch, Transport};
