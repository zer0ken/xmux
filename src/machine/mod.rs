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

    /// True for a remote (ssh) machine. Used only to SHAPE ssh options — not to decide a
    /// server MODEL (that is `ServerModel`) nor the two capability predicates below.
    fn is_remote(&self) -> bool {
        false
    }

    /// True when a display attach on this machine runs THROUGH a host shell (so an attach
    /// can prepend a `tty >file` record snippet, and a `SwitchPlan::Shell` can run). A
    /// machine that spawns the mux binary directly is `false` (the default). NOT derived
    /// from `is_remote`: a local-but-shell family (WSL) sets this `true` while staying
    /// non-remote.
    fn runs_through_shell(&self) -> bool {
        false
    }

    /// True when THIS box's local mux registry (`~/.psmux`) is the authority for this
    /// host's sessions — enabling the registry-merge enumeration and the local
    /// `list-clients` tty probe. `false` (the default) for a machine whose sessions live
    /// on the far side. NOT derived from `is_remote`.
    fn local_registry_scope(&self) -> bool {
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
    fn runs_through_shell(&self) -> bool {
        (**self).runs_through_shell()
    }
    fn local_registry_scope(&self) -> bool {
        (**self).local_registry_scope()
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
/// picks a variant, and the `MachineKind` query methods ([`transport`](Self::transport),
/// [`local_socket`](Self::local_socket)) are the only code that matches on the kind. A new
/// family is a variant here plus one arm in each of those methods — no code OUTSIDE
/// `MachineKind` matches on the kind.
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
    /// A new family = a variant above + one arm here (and in the sibling `local_socket`);
    /// no code outside `MachineKind` matches on the kind.
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

    /// The local mux server socket (`-S`) this machine targets — `Some` only for a local
    /// machine on a non-default socket, `None` for a remote machine or the default socket.
    /// Like [`transport`](Self::transport), the match on the kind lives HERE on the type, so
    /// a new family is compiler-forced to state its socket in one place.
    pub fn local_socket(&self) -> Option<String> {
        match self {
            MachineKind::Local { socket } => socket.clone(),
            MachineKind::Ssh { .. } => None,
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

    #[test]
    fn local_socket_is_some_only_for_a_local_nondefault_socket() {
        assert_eq!(
            MachineKind::Local {
                socket: Some("/tmp/s".into())
            }
            .local_socket(),
            Some("/tmp/s".into())
        );
        assert_eq!(MachineKind::Local { socket: None }.local_socket(), None);
        assert_eq!(
            MachineKind::Ssh {
                alias: "prod".into(),
                control_path: String::new(),
                os: "linux".into(),
            }
            .local_socket(),
            None,
            "a remote machine has no local socket"
        );
    }

    #[test]
    fn capability_predicates_split_shell_from_registry_scope() {
        // The two capability predicates split the meanings `is_remote` conflated: local
        // psmux is the authority for THIS box's registry (registry scope) yet attaches
        // without a shell; ssh attaches THROUGH a shell yet has no local-registry
        // authority here. They are NOT derived from `is_remote`, so a future local-but-
        // shell family (WSL) can override a new combination.
        let local = local(None);
        assert!(local.local_registry_scope());
        assert!(!local.runs_through_shell());
        let ssh = ssh("prod".into(), String::new(), "linux".into());
        assert!(ssh.runs_through_shell());
        assert!(!ssh.local_registry_scope());
    }
}
