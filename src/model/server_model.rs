//! How a mux server exposes its sessions: `Shared` (one aggregate server holds
//! every session — tmux; one PTY per HOST, moved between sessions with
//! `switch-client`) or `PerSession` (one server per session — psmux; one PTY per
//! SESSION). The supervisor reads THIS to shape the display key and the attach
//! fan-out, never the transport's `remote` flag.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerModel {
    Shared,
    PerSession,
}

impl ServerModel {
    /// The `AttachRegistry` key for `address` under this model. `Shared` ⇒ the host
    /// id (one PTY per host); `PerSession` ⇒ the full `source/session` address.
    pub fn display_key(self, host_id: &str, address: &str) -> String {
        match self {
            ServerModel::Shared => host_id.to_string(),
            ServerModel::PerSession => address.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_keys_by_host_id_not_session() {
        // Shared (tmux) keeps ONE PTY per host, so two sessions of one host share
        // the host-id key — matches cockpit.rs:247 (`s.remote => sel.source`).
        assert_eq!(ServerModel::Shared.display_key("jup", "jup/api"), "jup");
        assert_eq!(ServerModel::Shared.display_key("jup", "jup/db"), "jup");
    }

    #[test]
    fn per_session_keys_by_full_address() {
        // PerSession (psmux) keeps one PTY per session — matches cockpit.rs:249
        // (`_ => sel.address()`), the `source/session` address.
        assert_eq!(
            ServerModel::PerSession.display_key("local", "local/work"),
            "local/work"
        );
    }
}
