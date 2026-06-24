//! The machine boundary: how a mux argv reaches the server. SEPARATE from which
//! mux runs there (that is `Mux`). `Local` injects `-S <socket>`; `Ssh` wraps the
//! argv in an ssh connection with the right tty/batch options. This owns argv
//! assembly only — it never decides a server model.

use crate::session::LOCAL_SOURCE;

/// Bounds the ssh TCP connect; the per-host scan timeout must exceed it so a
/// slow-but-alive remote is not cancelled mid-connect.
#[allow(dead_code)]
const CONNECT_TIMEOUT: &str = "5";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Transport {
    /// The local machine. `socket` targets a non-default mux server (`-S <socket>`,
    /// parsed from `$TMUX`); `None` ⇒ the default socket.
    Local { socket: Option<String> },
    /// A remote over ssh. `control_path` is the ControlMaster socket (empty ⇒ no
    /// multiplex, e.g. a Windows local side); `os` is the LOCAL platform (gates
    /// ControlMaster). `alias` is the ssh destination.
    Ssh { alias: String, control_path: String, os: String },
}

impl Transport {
    /// `"local"` or the ssh alias — the stable host id and `Hosts` map key.
    pub fn host_id(&self) -> &str {
        match self {
            Transport::Local { .. } => LOCAL_SOURCE,
            Transport::Ssh { alias, .. } => alias,
        }
    }

    /// True for `Ssh`. The ONLY query of transport kind — no `Mux` or supervisor
    /// code reads this to decide a server MODEL (that is `ServerModel`).
    pub fn is_remote(&self) -> bool {
        matches!(self, Transport::Ssh { .. })
    }

    /// The ssh options preceding the remote command, ending with `-- <alias>` so an
    /// alias beginning with `-` is the destination, never an option. `tty` requests
    /// a pty and omits BatchMode so auth can prompt; else `BatchMode=yes` so a
    /// listing never hangs. ControlMaster is multiplexed only on a non-windows local
    /// side with a control path.
    #[allow(dead_code)]
    fn ssh_opts(&self, tty: bool) -> Vec<String> {
        let Transport::Ssh { alias, control_path, os } = self else {
            return Vec::new();
        };
        let mut a: Vec<String> = Vec::new();
        if tty {
            a.push("-t".into());
        } else {
            a.push("-o".into());
            a.push("BatchMode=yes".into());
        }
        a.push("-o".into());
        a.push(format!("ConnectTimeout={CONNECT_TIMEOUT}"));
        if os != "windows" && !control_path.is_empty() {
            a.push("-o".into());
            a.push("ControlMaster=auto".into());
            a.push("-o".into());
            a.push(format!("ControlPath={control_path}"));
            a.push("-o".into());
            a.push("ControlPersist=60s".into());
        }
        a.push("--".into());
        a.push(alias.clone());
        a
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ssh(alias: &str, os: &str, cp: &str) -> Transport {
        Transport::Ssh { alias: alias.into(), control_path: cp.into(), os: os.into() }
    }

    #[test]
    fn host_id_is_local_for_local_and_alias_for_ssh() {
        assert_eq!(Transport::Local { socket: None }.host_id(), "local");
        assert_eq!(ssh("prod", "linux", "").host_id(), "prod");
    }

    #[test]
    fn is_remote_only_for_ssh() {
        assert!(!Transport::Local { socket: Some("/x".into()) }.is_remote());
        assert!(ssh("prod", "linux", "").is_remote());
    }

    #[test]
    fn ssh_opts_non_interactive_batches_and_multiplexes() {
        // Pinned against source.rs:117-143 (ssh_args(false)).
        let a = ssh("prod", "linux", "/tmp/cm.sock").ssh_opts(false);
        let joined = a.join(" ");
        assert!(joined.contains("BatchMode=yes"), "{a:?}");
        assert!(joined.contains("ConnectTimeout=5"), "{a:?}");
        assert!(joined.contains("ControlMaster=auto"), "{a:?}");
        assert_eq!(a[a.len() - 2], "--");
        assert_eq!(a[a.len() - 1], "prod");
    }

    #[test]
    fn ssh_opts_interactive_requests_tty_no_batch() {
        let a = ssh("prod", "linux", "").ssh_opts(true);
        let joined = a.join(" ");
        assert!(joined.contains("-t"), "{a:?}");
        assert!(!joined.contains("BatchMode"), "{a:?}");
    }

    #[test]
    fn ssh_opts_windows_omits_control_master() {
        let a = ssh("prod", "windows", "/tmp/cm.sock").ssh_opts(false);
        assert!(!a.join(" ").contains("ControlMaster"), "{a:?}");
    }
}
