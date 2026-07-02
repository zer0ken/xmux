//! Per-host metadata channels: the shared vocabulary plus the reader, writer,
//! client, poll, and manager concerns, each in its own submodule.

#[cfg(test)]
use crate::mux::ControlProtocol;

mod client;
mod inventory;
mod manager;
mod poll;
mod reader;
mod writer;

pub use client::HostClient;
pub use inventory::{HostCmd, HostEvent, HostInventory, InFlight, PendingReply, ReaderState};
pub use manager::HostManager;
pub use reader::run_reader;
pub use writer::run_writer;

/// The shared `'static` tmux control protocol, for tests that drive the reader/writer
/// or spawn a fake control child. Both the `host` and `app` test modules use it.
#[cfg(test)]
pub(crate) fn test_control_proto() -> &'static dyn ControlProtocol {
    crate::mux::for_binary("tmux")
        .control_protocol()
        .expect("tmux has a control protocol")
}
