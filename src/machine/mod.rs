//! The machine axis: how a mux argv reaches the server, SEPARATE from which mux
//! runs there (that is `Mux`). A `Transport` owns argv assembly and the ssh
//! wrapping only — it never decides a server model. Each machine family lives in
//! its own file behind the `Transport` trait — `Local` (`local.rs`), `Ssh`
//! (`ssh.rs`) — mirroring how each mux family lives behind `Mux`. Shared shell
//! vocabulary (`quote`/`remote_command`) is in `vocab.rs`, the peer of
//! `mux/vocab.rs`. A new family is a new file implementing `Transport` plus a
//! factory here; the trait and its callers name no concrete family.

pub mod local;
pub mod ssh;
pub mod vocab;

pub use local::Local;
pub use ssh::Ssh;

/// The machine boundary: turns a full mux argv (`argv[0]` = the mux binary) into a
/// runnable `(command, args)`, and wraps interactive/control/raw execution for the
/// machine it targets. Implementors are the machine families (`Local`, `Ssh`); no
/// caller branches on which one — it addresses a machine through this trait.
pub trait Transport: Send + Sync {
    /// `"local"` or the ssh alias — the stable host id and `Hosts` map key.
    fn host_id(&self) -> &str;

    /// True for a remote (ssh) machine. The ONLY query of transport kind — no `Mux`
    /// or supervisor code reads this to decide a server MODEL (that is `ServerModel`).
    fn is_remote(&self) -> bool {
        false
    }

    /// Turns a full mux argv (`argv[0]` = the mux binary) into the (command, args)
    /// to spawn.
    fn exec_argv(&self, tty: bool, mux_argv: &[String]) -> (String, Vec<String>);

    /// Lowers a mux attach argv into the interactive terminal-handover (cmd, args).
    /// This is the SOLE owner of the `exec`/window-fold/ssh-tty machinery.
    fn interactive_attach_argv(
        &self,
        mux_attach_argv: &[String],
        pre_select: Option<&[String]>,
    ) -> (String, Vec<String>);

    /// The argv for a `-CC` control-mode child given the mux's control argv.
    fn control_argv(&self, mux_control_argv: &[String]) -> Vec<String>;

    /// Joins a raw remote shell command behind the machine's execution wrapper.
    /// `None` when the machine issues no remote shell command (a local machine).
    fn raw_ssh_argv(&self, _remote_cmd: &str) -> Option<Vec<String>> {
        None
    }

    /// Clones into a fresh box — a spawned poll task needs an owned transport, and a
    /// trait object cannot derive `Clone`.
    fn clone_box(&self) -> Box<dyn Transport>;
}

impl Clone for Box<dyn Transport> {
    fn clone(&self) -> Self {
        self.clone_box()
    }
}

/// A boxed transport is itself a `Transport`, delegating to the inner value. This lets
/// a stored `Box<dyn Transport>` be passed where `&dyn Transport` is expected (via
/// `&boxed`) without an explicit reborrow at every call site.
impl Transport for Box<dyn Transport> {
    fn host_id(&self) -> &str {
        (**self).host_id()
    }
    fn is_remote(&self) -> bool {
        (**self).is_remote()
    }
    fn exec_argv(&self, tty: bool, mux_argv: &[String]) -> (String, Vec<String>) {
        (**self).exec_argv(tty, mux_argv)
    }
    fn interactive_attach_argv(
        &self,
        mux_attach_argv: &[String],
        pre_select: Option<&[String]>,
    ) -> (String, Vec<String>) {
        (**self).interactive_attach_argv(mux_attach_argv, pre_select)
    }
    fn control_argv(&self, mux_control_argv: &[String]) -> Vec<String> {
        (**self).control_argv(mux_control_argv)
    }
    fn raw_ssh_argv(&self, remote_cmd: &str) -> Option<Vec<String>> {
        (**self).raw_ssh_argv(remote_cmd)
    }
    fn clone_box(&self) -> Box<dyn Transport> {
        (**self).clone_box()
    }
}

/// The concrete, runnable shape of a display-client switch — what the driver hands to
/// `run_lowered`. Lives on the TRANSPORT side (it is the execution shape), not in the
/// mux's intent vocabulary. The mux never names these variants.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoweredSwitch {
    /// A local mux argv (`argv[0]` = binary) — run non-interactively.
    Local(Vec<String>),
    /// A full ssh argv carrying a guarded raw remote `switch-client` snippet, run via
    /// the same path `run_raw` uses.
    RawSsh(Vec<String>),
}

/// Which machine family a host reaches its mux over, carrying that family's own
/// construction data. The SINGLE representation of transport kind: config/`Hosts::build`
/// picks a variant, and [`MachineKind::transport`] is the one site that turns it into a
/// concrete [`Transport`]. A new family is a variant here plus one match arm — no other
/// `match`/`if` on kind exists.
#[derive(Clone, Debug)]
pub enum MachineKind {
    /// The local machine, optionally targeting a non-default mux socket (`-S`).
    Local { socket: Option<String> },
    /// A remote over ssh: the destination `alias`, its ControlMaster socket
    /// `control_path`, and the LOCAL platform `os` (gates ControlMaster).
    Ssh {
        alias: String,
        control_path: String,
        os: String,
    },
}

impl MachineKind {
    /// The one site that maps a machine kind to a concrete [`Transport`] (Decision A).
    /// A new family = a variant above + one arm here; no other `match`/`if` on kind.
    pub fn transport(self) -> Box<dyn Transport> {
        match self {
            MachineKind::Local { socket } => local(socket),
            MachineKind::Ssh {
                alias,
                control_path,
                os,
            } => ssh(alias, control_path, os),
        }
    }
}

/// A local machine transport targeting an optional non-default mux socket.
pub fn local(socket: Option<String>) -> Box<dyn Transport> {
    Box::new(Local { socket })
}

/// A remote (ssh) machine transport.
pub fn ssh(alias: String, control_path: String, os: String) -> Box<dyn Transport> {
    Box::new(Ssh {
        alias,
        control_path,
        os,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_factory_is_local_and_issues_no_raw_ssh() {
        let t = local(None);
        assert_eq!(t.host_id(), "local");
        assert!(!t.is_remote());
        assert!(
            t.raw_ssh_argv("anything").is_none(),
            "a local machine issues no remote shell command"
        );
    }

    #[test]
    fn ssh_factory_is_remote_with_alias_id() {
        let t = ssh("prod".into(), String::new(), "linux".into());
        assert_eq!(t.host_id(), "prod");
        assert!(t.is_remote());
    }

    #[test]
    fn boxed_transport_clones_via_clone_box() {
        let t = ssh("prod".into(), String::new(), "linux".into());
        let c = t.clone();
        assert_eq!(c.host_id(), "prod");
        assert!(c.is_remote());
    }

    #[test]
    fn machine_kind_selects_the_family_at_one_site() {
        // `MachineKind::transport` is the single site that maps a machine kind to a
        // concrete Transport (Decision A: a new family = a variant + one match arm).
        let local = MachineKind::Local {
            socket: Some("/tmp/s".into()),
        }
        .transport();
        assert_eq!(local.host_id(), "local");
        assert!(!local.is_remote());
        let (_n, args) = local.exec_argv(false, &["tmux".to_string(), "ls".to_string()]);
        assert!(
            args.windows(2)
                .any(|w| w == ["-S".to_string(), "/tmp/s".to_string()]),
            "the local socket threads into the transport as -S: {args:?}"
        );

        let ssh = MachineKind::Ssh {
            alias: "prod".into(),
            control_path: String::new(),
            os: "linux".into(),
        }
        .transport();
        assert_eq!(ssh.host_id(), "prod");
        assert!(ssh.is_remote());
    }
}
