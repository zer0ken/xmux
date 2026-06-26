//! The machine boundary: how a mux argv reaches the server. SEPARATE from which
//! mux runs there (that is `Mux`). `Local` injects `-S <socket>`; `Ssh` wraps the
//! argv in an ssh connection with the right tty/batch options. This owns argv
//! assembly only — it never decides a server model.

use crate::model::plan::SwitchPlan;
use crate::session::LOCAL_SOURCE;

/// Bounds the ssh TCP connect; the per-host scan timeout must exceed it so a
/// slow-but-alive remote is not cancelled mid-connect.
const CONNECT_TIMEOUT: &str = "5";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Transport {
    /// The local machine. `socket` targets a non-default mux server (`-S <socket>`,
    /// parsed from `$TMUX`); `None` ⇒ the default socket.
    Local { socket: Option<String> },
    /// A remote over ssh. `control_path` is the ControlMaster socket (empty ⇒ no
    /// multiplex, e.g. a Windows local side); `os` is the LOCAL platform (gates
    /// ControlMaster). `alias` is the ssh destination.
    Ssh {
        alias: String,
        control_path: String,
        os: String,
    },
}

/// The concrete, runnable result of lowering a `SwitchPlan` — what the supervisor
/// hands to the runner. Lives on the TRANSPORT side (it is the execution shape),
/// not in the mux's intent vocabulary. The mux never names these variants.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoweredSwitch {
    /// A local mux argv (`argv[0]` = binary) — run non-interactively.
    Local(Vec<String>),
    /// A full ssh argv carrying a guarded raw remote `switch-client` snippet, run via
    /// the same path `run_raw` uses.
    RawSsh(Vec<String>),
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
    fn ssh_opts(&self, tty: bool) -> Vec<String> {
        let Transport::Ssh {
            alias,
            control_path,
            os,
        } = self
        else {
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

    /// Turns a full mux argv (`argv[0]` = the mux binary) into the (command, args)
    /// to spawn. Local injects `-S <socket>` for a non-default server; ssh wraps the
    /// per-arg-quoted command behind the ssh options.
    pub fn exec_argv(&self, tty: bool, mux_argv: &[String]) -> (String, Vec<String>) {
        match self {
            Transport::Local { socket } => {
                let mut args: Vec<String> = Vec::new();
                if let Some(sock) = socket.as_deref().filter(|s| !s.is_empty()) {
                    args.push("-S".into());
                    args.push(sock.to_string());
                }
                args.extend_from_slice(&mux_argv[1..]);
                (mux_argv[0].clone(), args)
            }
            Transport::Ssh { .. } => {
                let mut args = self.ssh_opts(tty);
                args.push(crate::source::remote_command(mux_argv));
                ("ssh".into(), args)
            }
        }
    }

    /// The argv for a `-CC` control-mode child given the mux's control argv. Local
    /// splices `-S <socket>` after the binary; remote forces a pty with `-tt` (a
    /// pipe-only ssh dies before emitting control-mode output) and runs over
    /// `BatchMode=yes`.
    pub fn control_argv(&self, mux_control_argv: &[String]) -> Vec<String> {
        match self {
            Transport::Local { socket } => {
                let mut v = vec![mux_control_argv[0].clone()];
                if let Some(sock) = socket.as_deref().filter(|s| !s.is_empty()) {
                    v.push("-S".into());
                    v.push(sock.to_string());
                }
                v.extend_from_slice(&mux_control_argv[1..]);
                v
            }
            Transport::Ssh { .. } => {
                let mut args = vec!["-tt".to_string()];
                args.extend(self.ssh_opts(false));
                args.push(crate::source::remote_command(mux_control_argv));
                let mut v = vec!["ssh".to_string()];
                v.extend(args);
                v
            }
        }
    }

    /// Joins a raw remote shell command behind the ssh options. `None` for `Local`
    /// (a local raw shell command is never issued). The caller must `quote` any
    /// untrusted value inside `remote_cmd` (see `source::quote`).
    pub fn raw_ssh_argv(&self, remote_cmd: &str) -> Option<Vec<String>> {
        match self {
            Transport::Local { .. } => None,
            Transport::Ssh { .. } => {
                let mut v = vec!["ssh".to_string()];
                v.extend(self.ssh_opts(false));
                v.push(remote_cmd.to_string());
                Some(v)
            }
        }
    }

    /// Lowers a transport-blind `SwitchPlan` (a mux intent) into a runnable command.
    /// The single boundary where "switch the shared client to this session" becomes
    /// either a LOCAL `switch-client` argv or a REMOTE guarded raw command. The mux
    /// supplies `mux_switch_argv` (closed over the captured display tty); the transport
    /// decides only HOW to run it. `PerSessionNoOp` ⇒ `None` (nothing to switch).
    /// Resolves the mux/transport boundary: neither variant of `LoweredSwitch`
    /// leaks into the mux layer.
    pub fn lower_switch(
        &self,
        plan: &SwitchPlan,
        mux_switch_argv: &dyn Fn(&str) -> Vec<String>,
    ) -> Option<LoweredSwitch> {
        let SwitchPlan::Switch { session } = plan else {
            return None;
        };
        let argv = mux_switch_argv(session);
        match self {
            Transport::Local { .. } => Some(LoweredSwitch::Local(argv)),
            Transport::Ssh { .. } => {
                let raw = crate::source::remote_command(&argv);
                self.raw_ssh_argv(&raw).map(LoweredSwitch::RawSsh)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::plan::SwitchPlan;

    fn ssh(alias: &str, os: &str, cp: &str) -> Transport {
        Transport::Ssh {
            alias: alias.into(),
            control_path: cp.into(),
            os: os.into(),
        }
    }

    fn local(socket: Option<&str>) -> Transport {
        Transport::Local {
            socket: socket.map(str::to_string),
        }
    }
    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn host_id_is_local_for_local_and_alias_for_ssh() {
        assert_eq!(Transport::Local { socket: None }.host_id(), "local");
        assert_eq!(ssh("prod", "linux", "").host_id(), "prod");
    }

    #[test]
    fn is_remote_only_for_ssh() {
        assert!(!Transport::Local {
            socket: Some("/x".into())
        }
        .is_remote());
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

    #[test]
    fn exec_argv_local_plain_and_socket() {
        // Pinned against source.rs:147.
        let (n, a) = local(None).exec_argv(false, &argv(&["psmux", "list-sessions", "-F", "x"]));
        assert_eq!(n, "psmux");
        assert_eq!(a, argv(&["list-sessions", "-F", "x"]));

        let (n, a) = local(Some("/tmp/tmux-1000/work"))
            .exec_argv(false, &argv(&["tmux", "list-sessions", "-F", "x"]));
        assert_eq!(n, "tmux");
        assert_eq!(
            a,
            argv(&["-S", "/tmp/tmux-1000/work", "list-sessions", "-F", "x"])
        );
    }

    #[test]
    fn exec_argv_remote_wraps_in_ssh() {
        let (n, a) =
            ssh("prod", "linux", "").exec_argv(false, &argv(&["tmux", "kill-session", "-t", "x"]));
        assert_eq!(n, "ssh");
        assert_eq!(a.last().unwrap(), "tmux kill-session -t x");
    }

    #[test]
    fn control_argv_local_injects_socket_before_cc() {
        // The mux control argv is `[bin, -CC, attach]`; local splices -S after the binary.
        assert_eq!(
            local(None).control_argv(&argv(&["psmux", "-CC", "attach"])),
            argv(&["psmux", "-CC", "attach"])
        );
        assert_eq!(
            local(Some("/tmp/tmux-1000/work")).control_argv(&argv(&["tmux", "-CC", "attach"])),
            argv(&["tmux", "-S", "/tmp/tmux-1000/work", "-CC", "attach"])
        );
    }

    #[test]
    fn control_argv_remote_forces_pty() {
        let got = ssh("prod", "linux", "").control_argv(&argv(&["tmux", "-CC", "attach"]));
        assert_eq!(got[0], "ssh");
        assert!(got.iter().any(|s| s == "-tt"), "{got:?}");
        assert!(
            got.iter().any(|s: &String| s.contains("BatchMode=yes")),
            "{got:?}"
        );
        assert_eq!(got.last().unwrap(), "tmux -CC attach");
    }

    #[test]
    fn raw_ssh_argv_none_for_local_some_for_ssh() {
        // Pinned against source.rs:253 (run_raw is remote-only).
        assert!(local(None).raw_ssh_argv("anything").is_none());
        let got = ssh("prod", "linux", "")
            .raw_ssh_argv("c=$(tty); echo $c")
            .unwrap();
        assert_eq!(got[0], "ssh");
        assert_eq!(got.last().unwrap(), "c=$(tty); echo $c");
        assert!(
            got.iter().any(|s: &String| s.contains("BatchMode=yes")),
            "{got:?}"
        );
    }

    // The closure the supervisor supplies, closed over the captured display tty.
    // Here a fixed tty stands in. Matches the shape Mux::switch_client_argv builds.
    fn switch_argv_for(tty: &str) -> impl Fn(&str) -> Vec<String> + '_ {
        move |session: &str| argv(&["tmux", "switch-client", "-c", tty, "-t", session])
    }

    #[test]
    fn lower_switch_no_op_for_per_session() {
        // PerSession has nothing to switch — lowering yields None on any transport.
        assert!(local(None)
            .lower_switch(&SwitchPlan::PerSessionNoOp, &switch_argv_for("/dev/pts/3"))
            .is_none());
        assert!(ssh("prod", "linux", "")
            .lower_switch(&SwitchPlan::PerSessionNoOp, &switch_argv_for("/dev/pts/3"))
            .is_none());
    }

    #[test]
    fn lower_switch_local_is_a_direct_mux_argv() {
        // A Shared LOCAL host (tmux) switches its one shared client with a direct
        // local switch-client argv — NOT an ssh raw command (the M2 correctness hole).
        let got = local(None)
            .lower_switch(
                &SwitchPlan::Switch {
                    session: "api".into(),
                },
                &switch_argv_for("/dev/pts/3"),
            )
            .expect("Shared local lowers to a switch");
        assert_eq!(
            got,
            LoweredSwitch::Local(argv(&[
                "tmux",
                "switch-client",
                "-c",
                "/dev/pts/3",
                "-t",
                "api"
            ]))
        );
    }

    #[test]
    fn lower_switch_remote_is_a_guarded_ssh_raw_command() {
        // A Shared REMOTE host (tmux over ssh) switches via run_raw: the argv is
        // joined per-arg-quoted and wrapped behind the ssh options.
        let got = ssh("prod", "linux", "")
            .lower_switch(
                &SwitchPlan::Switch {
                    session: "api".into(),
                },
                &switch_argv_for("/dev/pts/3"),
            )
            .expect("Shared remote lowers to a switch");
        let LoweredSwitch::RawSsh(v) = got else {
            panic!("remote lowers to RawSsh: {got:?}")
        };
        assert_eq!(v[0], "ssh");
        assert_eq!(v.last().unwrap(), "tmux switch-client -c /dev/pts/3 -t api");
        assert!(
            v.iter().any(|s: &String| s.contains("BatchMode=yes")),
            "{v:?}"
        );
    }
}
