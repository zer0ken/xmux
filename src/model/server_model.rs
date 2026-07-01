//! How a mux server exposes its sessions: `Shared` (one aggregate server holds
//! every session — tmux; one PTY per HOST, moved between sessions with
//! `switch-client`) or `PerSession` (one server per session — psmux; one PTY per
//! SESSION). The supervisor reads THIS to shape the attach fan-out, never the
//! transport's `remote` flag.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerModel {
    Shared,
    PerSession,
}
