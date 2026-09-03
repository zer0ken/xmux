//! The transport axis: how a mux argv reaches the server, SEPARATE from which mux
//! runs there (that is `Mux`). A `Transport` owns argv assembly and the ssh
//! wrapping only — it never decides a server model. Each machine implementation lives in
//! its own file behind the `Transport` trait — `Local` (`local.rs`), `Ssh`
//! (`ssh.rs`), `Wsl` (`wsl.rs`) — mirroring how each mux implementation lives behind `Mux`. Shared shell
//! helpers (`quote`/`remote_command`) is in `vocab.rs`, the peer of
//! `mux/vocab.rs`. A new implementation is a new file implementing `Transport` plus a
//! factory here; the trait and its callers name no concrete implementation.

pub mod local;
pub mod ssh;
pub mod vocab;
pub mod wsl;

pub use local::Local;
pub use ssh::Ssh;
pub use wsl::Wsl;

/// The machine boundary: turns a full mux argv (`argv[0]` = the mux binary) into a
/// runnable `(command, args)`, and wraps interactive/control/raw execution for the
/// machine it targets. Implementors are the machine implementations (`Local`, `Ssh`, `Wsl`); no
/// caller branches on which one — it addresses a machine through this trait.
pub trait Transport: Send + Sync {
    /// `"local"`, the ssh alias, or `wsl.<distro>` — the stable host id and `Hosts` map
    /// key.
    fn host_id(&self) -> &str;

    /// True for a remote (ssh) machine. Used only to SHAPE ssh options — not to decide a
    /// server MODEL (that is `ServerModel`) nor the two capability predicates below.
    fn is_remote(&self) -> bool {
        false
    }

    /// True when a display attach on this machine runs THROUGH a host shell (so an attach
    /// can prepend a `tty >file` record snippet, and a `SwitchPlan::Shell` can run). A
    /// machine that spawns the mux binary directly is `false` (the default). NOT derived
    /// from `is_remote`: a local-but-shell implementation (WSL) sets this `true` while staying
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

    /// True when the machine's `-CC` control child must run on a pty the spawner
    /// allocates for it. The remote path already forces one (`ssh -tt`) and WSL
    /// wraps its child in `script`; a machine that spawns the mux binary directly
    /// on a Unix box (local tmux) has no such flag and the `-CC` client dies on
    /// pipe stdio, so the spawner must give it a pty itself.
    fn control_needs_pty(&self) -> bool {
        false
    }

    /// Joins a raw remote shell command behind the machine's execution wrapper.
    /// `None` when the machine issues no remote shell command (a local machine).
    fn raw_shell_argv(&self, _remote_cmd: &str) -> Option<Vec<String>> {
        None
    }

    /// The argv that establishes a password-authenticated ControlMaster for this
    /// machine (the unlock), or `None` when the machine has no reusable master (a
    /// local/WSL machine with no password, or Windows ssh without ControlMaster).
    fn unlock_argv(&self, _user: &str) -> Option<Vec<String>> {
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
    fn control_needs_pty(&self) -> bool {
        (**self).control_needs_pty()
    }
    fn raw_shell_argv(&self, remote_cmd: &str) -> Option<Vec<String>> {
        (**self).raw_shell_argv(remote_cmd)
    }
    fn unlock_argv(&self, user: &str) -> Option<Vec<String>> {
        (**self).unlock_argv(user)
    }
    fn clone_box(&self) -> Box<dyn Transport> {
        (**self).clone_box()
    }
}

/// The concrete, runnable shape of a display-client switch — what the driver hands to
/// `run_lowered`. Lives on the TRANSPORT side (it is the execution shape), not in the
/// mux's intent set. The mux never names these variants.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoweredSwitch {
    /// A local mux argv (`argv[0]` = binary) — run non-interactively.
    Local(Vec<String>),
    /// A full ssh argv carrying a guarded raw remote `switch-client` snippet, run via
    /// the same path `run_raw` uses.
    RawSsh(Vec<String>),
}

/// Which machine kind a host reaches its mux over, carrying that kind's own
/// construction data. The SINGLE representation of transport kind: config/`Hosts::build`
/// picks a variant, and the `MachineKind` query methods ([`transport`](Self::transport),
/// [`local_socket`](Self::local_socket)) are the only code that matches on the kind. A new
/// kind is a variant here plus one arm in each of those methods — no code OUTSIDE
/// `MachineKind` matches on the kind.
#[derive(Clone, Debug)]
pub enum MachineKind {
    /// The local machine, optionally targeting a non-default mux socket (`-S`). `id` is
    /// the source id it answers as (empty ⇒ the bare `local`).
    Local {
        #[allow(missing_docs)]
        id: String,
        socket: Option<String>,
    },
    /// A remote over ssh: the source `id` it answers as (empty ⇒ `alias`), the
    /// destination `alias`, its ControlMaster socket `control_path`, and the LOCAL
    /// platform `os` (gates ControlMaster).
    Ssh {
        id: String,
        alias: String,
        control_path: String,
        os: String,
    },
    /// A WSL distribution on this machine: the source `id` it answers as (empty means the
    /// bare machine name `wsl.<distro>`) and the `distro` name `wsl.exe -d` takes.
    Wsl { id: String, distro: String },
}

/// The [`MachineKind`] for `machine`, answering as the source `id`.
///
/// The SINGLE place a machine's construction data is assembled, so a source added LATER
/// (an async mux discovery result) reaches its machine exactly as one built at launch
/// does, and the `Host` the loop drives and the `Source` the off-loop ops use cannot
/// disagree about how to get there. The ControlMaster socket is per MACHINE, not per
/// source: several muxes on one machine share the one multiplexed connection.
pub fn kind_for(
    machine: &str,
    id: String,
    os: &str,
    xmux_dir: &std::path::Path,
    local_socket: Option<String>,
) -> MachineKind {
    if machine == crate::session::LOCAL_SOURCE {
        MachineKind::Local {
            id,
            socket: local_socket,
        }
    } else if let Some(distro) = crate::session::wsl_distro_of(machine) {
        // The kind is read back OUT of the machine name, so a source added later (an
        // async mux-discovery answer carries a bare machine name and nothing else) reaches
        // its distribution the same way one built at launch does.
        MachineKind::Wsl {
            id,
            distro: distro.to_string(),
        }
    } else {
        MachineKind::Ssh {
            id,
            alias: machine.to_string(),
            control_path: xmux_dir
                .join(format!("cm-{machine}.sock"))
                .to_string_lossy()
                .into_owned(),
            os: os.to_string(),
        }
    }
}

impl MachineKind {
    /// The one site that maps a machine kind to a concrete [`Transport`] (Decision A).
    /// A new kind = a variant above + one arm here (and in the sibling `local_socket`);
    /// no code outside `MachineKind` matches on the kind.
    /// How this machine is ADDRESSED, in words, with the wait that bounds reaching it.
    ///
    /// Shown, never parsed: the unreachable screen states it, so a host that failed says
    /// what it was asked over. It lives here because this is where a machine implementation's
    /// construction data already lives - the alternative is a caller that matches on the
    /// implementation to describe it, and every such caller drifts from `transport`.
    pub fn addressed_as(&self) -> String {
        match self {
            MachineKind::Local { .. } => "this box, no connection to make".to_string(),
            MachineKind::Ssh { alias, .. } => {
                format!("ssh to {alias}, given {}s to connect", ssh::CONNECT_TIMEOUT)
            }
            MachineKind::Wsl { distro, .. } => format!("wsl distribution {distro}"),
        }
    }

    /// The socket / multiplexed-connection path this machine addresses its mux through,
    /// or empty when it addresses one without a path.
    pub fn socket_path(&self) -> String {
        match self {
            MachineKind::Local { socket, .. } => socket.clone().unwrap_or_default(),
            MachineKind::Ssh { control_path, .. } => control_path.clone(),
            MachineKind::Wsl { .. } => String::new(),
        }
    }

    pub fn transport(self) -> Box<dyn Transport> {
        match self {
            MachineKind::Local { id, socket } if id.is_empty() => local(socket),
            MachineKind::Local { id, socket } => local_as(id, socket),
            MachineKind::Ssh {
                id,
                alias,
                control_path,
                os,
            } if id.is_empty() || id == alias => ssh(alias, control_path, os),
            MachineKind::Ssh {
                id,
                alias,
                control_path,
                os,
            } => ssh_as(id, alias, control_path, os),
            MachineKind::Wsl { id, distro } if id.is_empty() => wsl(distro),
            MachineKind::Wsl { id, distro } => wsl_as(id, distro),
        }
    }

    /// The local mux server socket (`-S`) this machine targets — `Some` only for a local
    /// machine on a non-default socket, `None` for any other kind or the default socket.
    /// Like [`transport`](Self::transport), the match on the kind lives HERE on the type, so
    /// a new implementation is compiler-forced to state its socket in one place.
    pub fn local_socket(&self) -> Option<String> {
        match self {
            MachineKind::Local { socket, .. } => socket.clone(),
            // A WSL distribution is a machine of its own: the `$TMUX` socket this machine is
            // running inside is a Windows-side path that names nothing in the distro.
            MachineKind::Ssh { .. } | MachineKind::Wsl { .. } => None,
        }
    }
}

/// A local machine transport targeting an optional non-default mux socket, answering
/// as the bare `local` source - this machine serving one mux.
pub fn local(socket: Option<String>) -> Box<dyn Transport> {
    Box::new(Local {
        socket,
        ..Local::default()
    })
}

/// A local machine transport answering as the source `id`. Used when this machine serves
/// SEVERAL muxes and each needs its own key.
pub fn local_as(id: String, socket: Option<String>) -> Box<dyn Transport> {
    Box::new(Local { id, socket })
}

/// A remote (ssh) machine transport answering as the source `alias` - that machine
/// serving one mux.
pub fn ssh(alias: String, control_path: String, os: String) -> Box<dyn Transport> {
    Box::new(Ssh {
        id: alias.clone(),
        alias,
        control_path,
        os,
    })
}

/// A remote (ssh) machine transport answering as the source `id` while still reaching
/// the machine at `alias`. Used when a machine serves SEVERAL muxes.
pub fn ssh_as(id: String, alias: String, control_path: String, os: String) -> Box<dyn Transport> {
    Box::new(Ssh {
        id,
        alias,
        control_path,
        os,
    })
}

/// A WSL machine transport for `distro`, answering as the bare machine name
/// `wsl.<distro>` — that distribution serving one mux.
pub fn wsl(distro: String) -> Box<dyn Transport> {
    Box::new(Wsl {
        id: crate::session::WSL_PREFIX.to_string() + &distro,
        distro,
    })
}

/// A WSL machine transport answering as the source `id` while still reaching the same
/// `distro`. Used when a distribution serves SEVERAL muxes.
pub fn wsl_as(id: String, distro: String) -> Box<dyn Transport> {
    Box::new(Wsl { id, distro })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_qualified_transport_keeps_reaching_the_same_machine() {
        // Two muxes on one machine are two SOURCES at one DESTINATION: the id
        // distinguishes them, the ssh argv must not change.
        let one = ssh("prod".into(), String::new(), "linux".into());
        let two = ssh_as(
            "prod:zellij".into(),
            "prod".into(),
            String::new(),
            "linux".into(),
        );
        assert_eq!(one.host_id(), "prod");
        assert_eq!(two.host_id(), "prod:zellij");
        let (n1, a1) = one.exec_argv(false, &["tmux".to_string(), "ls".to_string()]);
        let (n2, a2) = two.exec_argv(false, &["tmux".to_string(), "ls".to_string()]);
        assert_eq!((n1, a1), (n2, a2), "same destination, same argv");
    }

    #[test]
    fn a_qualified_local_transport_is_still_this_box() {
        let l = local_as("local:zellij".into(), None);
        assert_eq!(l.host_id(), "local:zellij");
        assert!(crate::session::is_local_source(l.host_id()));
        assert!(l.local_registry_scope(), "still this box's registry scope");
    }

    #[test]
    fn local_factory_is_local_and_issues_no_raw_ssh() {
        let t = local(None);
        assert_eq!(t.host_id(), "local");
        assert!(!t.is_remote());
        assert!(
            t.raw_shell_argv("anything").is_none(),
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
    fn machine_kind_selects_the_implementation_at_one_site() {
        // `MachineKind::transport` is the single site that maps a machine kind to a
        // concrete Transport (Decision A: a new kind = a variant + one match arm).
        let local = MachineKind::Local {
            id: String::new(),
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
            id: String::new(),
            alias: "prod".into(),
            control_path: String::new(),
            os: "linux".into(),
        }
        .transport();
        assert_eq!(ssh.host_id(), "prod");
        assert!(ssh.is_remote());
    }

    #[test]
    fn a_wsl_machine_name_selects_the_wsl_kind() {
        // `kind_for` is the single assembly site, and the WSL kind is chosen by the
        // machine NAME — nothing else is threaded in to say which kind this is.
        let kind = kind_for(
            "wsl.Ubuntu-24.04",
            String::new(),
            "windows",
            std::path::Path::new("/x"),
            Some("/tmp/tmux-1000/work".into()),
        );
        assert!(matches!(kind, MachineKind::Wsl { .. }));
        assert_eq!(
            kind.local_socket(),
            None,
            "this box's $TMUX socket names nothing inside the distribution"
        );
        let t = kind.transport();
        assert_eq!(t.host_id(), "wsl.Ubuntu-24.04");
        assert!(!t.is_remote());
        let (name, args) = t.exec_argv(false, &["tmux".to_string(), "ls".to_string()]);
        assert_eq!(name, "wsl.exe");
        assert!(
            args.windows(2)
                .any(|w| w == ["-d".to_string(), "Ubuntu-24.04".to_string()]),
            "the distribution threads into the transport as -d: {args:?}"
        );
    }

    #[test]
    fn a_qualified_wsl_transport_keeps_reaching_the_same_distribution() {
        // Two muxes in one distribution are two SOURCES at one destination, exactly as
        // for ssh: the id tells them apart and the wsl.exe argv must not change.
        let one = kind_for(
            "wsl.Ubuntu",
            String::new(),
            "windows",
            std::path::Path::new("/x"),
            None,
        )
        .transport();
        let two = kind_for(
            "wsl.Ubuntu",
            "wsl.Ubuntu:zellij".to_string(),
            "windows",
            std::path::Path::new("/x"),
            None,
        )
        .transport();
        assert_eq!(one.host_id(), "wsl.Ubuntu");
        assert_eq!(two.host_id(), "wsl.Ubuntu:zellij");
        assert_eq!(
            one.exec_argv(false, &["tmux".to_string(), "ls".to_string()]),
            two.exec_argv(false, &["tmux".to_string(), "ls".to_string()]),
            "same destination, same argv"
        );
    }

    #[test]
    fn local_socket_is_some_only_for_a_local_nondefault_socket() {
        assert_eq!(
            MachineKind::Local {
                id: String::new(),
                socket: Some("/tmp/s".into())
            }
            .local_socket(),
            Some("/tmp/s".into())
        );
        assert_eq!(
            MachineKind::Local {
                id: String::new(),
                socket: None
            }
            .local_socket(),
            None
        );
        assert_eq!(
            MachineKind::Ssh {
                id: String::new(),
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
        // authority here. Neither is derived from `is_remote`, which is what lets WSL take
        // the third combination: local, so no ssh option is shaped, yet shell-based, and
        // holding a registry of its own inside the distribution.
        let local = local(None);
        assert!(local.local_registry_scope());
        assert!(!local.runs_through_shell());
        let ssh = ssh("prod".into(), String::new(), "linux".into());
        assert!(ssh.runs_through_shell());
        assert!(!ssh.local_registry_scope());
        let wsl = wsl("Ubuntu".into());
        assert!(!wsl.is_remote());
        assert!(wsl.runs_through_shell());
        assert!(!wsl.local_registry_scope());
    }
}

#[cfg(test)]
mod describe_tests {
    use super::*;

    #[test]
    fn each_kind_says_how_it_is_addressed_and_over_what_path() {
        // Shown on the unreachable screen: what a failed host was asked over. The ssh
        // wait is the SAME constant the option carries, so the words and the command
        // cannot disagree.
        let ssh = MachineKind::Ssh {
            id: String::new(),
            alias: "prod".into(),
            control_path: "/tmp/cm.sock".into(),
            os: "linux".into(),
        };
        assert_eq!(
            ssh.addressed_as(),
            format!("ssh to prod, given {}s to connect", ssh::CONNECT_TIMEOUT)
        );
        assert_eq!(ssh.socket_path(), "/tmp/cm.sock");

        let local = MachineKind::Local {
            id: String::new(),
            socket: Some("/tmp/psmux.sock".into()),
        };
        assert!(local.addressed_as().contains("this box"));
        assert_eq!(local.socket_path(), "/tmp/psmux.sock");

        // A machine addressed without a path states none, and the screen then carries no
        // such row rather than an empty one.
        let wsl = MachineKind::Wsl {
            id: String::new(),
            distro: "Ubuntu".into(),
        };
        assert!(wsl.addressed_as().contains("Ubuntu"));
        assert_eq!(wsl.socket_path(), "");
    }
}
