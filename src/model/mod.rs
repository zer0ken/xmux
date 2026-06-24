//! The host model: the machine boundary (`Transport`), the mux backend (`Mux`)
//! and its server model (`ServerModel`), and the plan/value types they exchange.
//! A `Host` (built in a later phase) is `Transport × Box<dyn Mux>`. The mux layer
//! is transport-blind: `Mux::switch_plan` returns intent; `Transport::lower_switch`
//! lowers it to a runnable command.

pub mod plan;
pub mod server_model;
pub mod transport;

pub use plan::{DeathSignal, DisplayTty, EventSource, SwitchPlan};
pub use server_model::ServerModel;
pub use transport::{LoweredSwitch, Transport};
