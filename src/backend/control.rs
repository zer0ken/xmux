//! The control-mode (`-CC`) protocol surface a backend exposes to the host reader.
//!
//! A mux with a host-level control stream (tmux) hides ALL its `-CC` wire details
//! behind this trait: line framing/classification, the notification→event policy
//! table, and the control-mode command-line builders. `host.rs` drives the reader
//! state machine + FIFO correlation but names no tmux protocol specifics directly —
//! it reaches them only through `Backend::control_protocol`.

use crate::host::HostEvent;

pub use crate::backend::tmux::control_proto::{Line, Notif};

/// The tmux-flavored control-mode protocol. Stateless: every method is a pure
/// function of its arguments, so the implementor is a unit struct shared `'static`.
pub trait ControlProtocol: Send + Sync {
    /// Classifies one control-mode stdout line (trailing `\n` already stripped) by
    /// frame shape. The caller's IDLE/IN-BLOCK state machine decides what to do with
    /// each shape; this only determines the shape.
    fn classify<'a>(&self, line: &'a str) -> Line<'a>;

    /// Maps one notification to the cockpit event it triggers, or `None` for an inert
    /// notification (the metadata client holds no per-session display state). This is
    /// the protocol's notification→event POLICY. `host` is echoed into the event;
    /// `last_error` is the last `%error` block's text, folded into a reasonless `%exit`.
    fn notif_event(
        &self,
        host: &str,
        notif: Notif<'_>,
        last_error: &Option<String>,
    ) -> Option<HostEvent>;

    /// The plain (`Send`, no meaningful reply) command lines of the connect preamble,
    /// in order. The client size is set separately via `size_line` (a `Resize` cmd) and
    /// `list-sessions` separately via `list_sessions_line` (a correlated `Query`).
    fn connect_lines(&self) -> Vec<String>;

    /// `list-sessions -F <fmt>` — the correlated query whose block resolves the inventory.
    fn list_sessions_line(&self) -> String;

    /// `list-panes -s -t <session> -F <fmt>` — the correlated query for `session`'s subtree.
    fn list_panes_line(&self, session: &str) -> String;

    /// `display-message -p -t <session> '#{window_index}'` — prints `session`'s active
    /// window index; the reply resolves to a `HostEvent::Focus`.
    fn active_window_line(&self, session: &str) -> String;

    /// `select-window -t <target>` — makes `target` (`session:window`) the active window.
    fn select_window_line(&self, target: &str) -> String;

    /// `switch-client -c <display_tty> -t <session>` — moves the named display client.
    fn switch_client_line(&self, display_tty: &str, session: &str) -> String;

    /// `refresh-client -C <cols>x<rows>` — the client-size formatter.
    fn size_line(&self, cols: u16, rows: u16) -> String;
}
