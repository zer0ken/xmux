//! The host model: the machine boundary (`Transport`), the mux backend (`Mux`)
//! and its server model (`ServerModel`), and the plan/value types they exchange.
//! A `Host` (built in a later phase) is `Transport × Box<dyn Mux>`. The mux layer
//! is transport-blind: `Mux::switch_plan` returns intent; `Transport::lower_switch`
//! lowers it to a runnable command.

pub mod death;
pub mod host;
pub mod hosts;
pub mod mux;
pub mod operation;
pub mod plan;
pub mod server_model;
pub mod transport;

pub use death::{
    display_tty_marker_prefix, matches_display_tty, parse_display_tty_marker, psmux_port_path,
    psmux_session_is_live,
};
pub use host::{Host, HostDisplay, Liveness, SyncAction};
pub use hosts::Hosts;
pub use mux::{for_binary, Mux, Psmux, Tmux};
pub use operation::{FocusTarget, Operation};
pub use plan::{DeathSignal, DisplayTty, EventSource, SwitchPlan};
pub use server_model::ServerModel;
pub use transport::{LoweredSwitch, Transport};
