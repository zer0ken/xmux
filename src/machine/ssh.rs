//! The ssh machine transport: wraps a mux argv in an ssh connection with the
//! right tty/batch/ControlMaster options. Untrusted argv elements are per-arg
//! quoted via [`super::vocab::remote_command`].

use super::vocab::remote_command;
use super::Transport;

/// Bounds the ssh TCP connect; the per-host scan timeout must exceed it so a
/// slow-but-alive remote is not cancelled mid-connect.
const CONNECT_TIMEOUT: &str = "5";

/// A remote over ssh. `control_path` is the ControlMaster socket (empty ⇒ no
/// multiplex, e.g. a Windows local side); `os` is the LOCAL platform (gates
/// ControlMaster). `alias` is the ssh destination.
#[derive(Clone, Debug)]
pub struct Ssh {
    pub alias: String,
    pub control_path: String,
    pub os: String,
}

impl Ssh {
    /// The ssh options preceding the remote command, ending with `-- <alias>` so an
    /// alias beginning with `-` is the destination, never an option. `tty` requests
    /// a pty and omits BatchMode so auth can prompt; else `BatchMode=yes` so a
    /// listing never hangs. ControlMaster is multiplexed only on a non-windows local
    /// side with a control path.
    fn ssh_opts(&self, tty: bool) -> Vec<String> {
        let mut a: Vec<String> = Vec::new();
        if tty {
            a.push("-t".into());
        } else {
            a.push("-o".into());
            a.push("BatchMode=yes".into());
        }
        a.push("-o".into());
        a.push(format!("ConnectTimeout={CONNECT_TIMEOUT}"));
        if self.os != "windows" && !self.control_path.is_empty() {
            a.push("-o".into());
            a.push("ControlMaster=auto".into());
            a.push("-o".into());
            a.push(format!("ControlPath={}", self.control_path));
            a.push("-o".into());
            a.push("ControlPersist=60s".into());
        }
        a.push("--".into());
        a.push(self.alias.clone());
        a
    }
}

impl Transport for Ssh {
    fn host_id(&self) -> &str {
        &self.alias
    }

    fn is_remote(&self) -> bool {
        true
    }

    /// A remote attach runs through the ssh login shell, so an attach can record its tty
    /// and a `SwitchPlan::Shell` can execute. Its sessions live on the far side, so
    /// `local_registry_scope` stays the default `false`.
    fn runs_through_shell(&self) -> bool {
        true
    }

    fn exec_argv(&self, tty: bool, mux_argv: &[String]) -> (String, Vec<String>) {
        let mut args = self.ssh_opts(tty);
        args.push(remote_command(mux_argv));
        ("ssh".into(), args)
    }

    /// A REMOTE interactive attach requests a pty (`-t`, no BatchMode) and runs
    /// `[<pre_select> ; ] exec <attach>`: the `exec` replaces the ssh login shell so
    /// the connection closes cleanly on detach, and folding `pre_select` into the SAME
    /// connection means no second connection to hang on a stalled remote or lose the
    /// selection to interactive auth.
    fn interactive_attach_argv(
        &self,
        mux_attach_argv: &[String],
        pre_select: Option<&[String]>,
    ) -> (String, Vec<String>) {
        let attach = remote_command(mux_attach_argv);
        let remote_cmd = match pre_select {
            Some(sel) => format!("{} ; exec {}", remote_command(sel), attach),
            None => format!("exec {attach}"),
        };
        let mut args = self.ssh_opts(true);
        args.push(remote_cmd);
        ("ssh".into(), args)
    }

    /// The remote forces a pty with `-tt` (a pipe-only ssh dies before emitting
    /// control-mode output) and runs over `BatchMode=yes`.
    fn control_argv(&self, mux_control_argv: &[String]) -> Vec<String> {
        let mut args = vec!["-tt".to_string()];
        args.extend(self.ssh_opts(false));
        args.push(remote_command(mux_control_argv));
        let mut v = vec!["ssh".to_string()];
        v.extend(args);
        v
    }

    /// Joins a raw remote shell command behind the ssh options. The caller must
    /// `quote` any untrusted value inside `remote_cmd` (see [`super::vocab::quote`]).
    fn raw_ssh_argv(&self, remote_cmd: &str) -> Option<Vec<String>> {
        let mut v = vec!["ssh".to_string()];
        v.extend(self.ssh_opts(false));
        v.push(remote_cmd.to_string());
        Some(v)
    }

    fn clone_box(&self) -> Box<dyn Transport> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ssh(alias: &str, os: &str, cp: &str) -> Ssh {
        Ssh {
            alias: alias.into(),
            control_path: cp.into(),
            os: os.into(),
        }
    }
    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn host_id_is_alias_and_is_remote() {
        assert_eq!(ssh("prod", "linux", "").host_id(), "prod");
        assert!(ssh("prod", "linux", "").is_remote());
    }

    #[test]
    fn ssh_opts_non_interactive_batches_and_multiplexes() {
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
    fn exec_argv_remote_wraps_in_ssh() {
        let (n, a) =
            ssh("prod", "linux", "").exec_argv(false, &argv(&["tmux", "kill-session", "-t", "x"]));
        assert_eq!(n, "ssh");
        assert_eq!(a.last().unwrap(), "tmux kill-session -t x");
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
    fn raw_ssh_argv_some_for_ssh() {
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

    #[test]
    fn interactive_attach_remote_without_pre_select_execs_over_ssh_tty() {
        let (n, a) = ssh("prod", "linux", "")
            .interactive_attach_argv(&argv(&["tmux", "attach", "-t", "api"]), None);
        assert_eq!(n, "ssh");
        assert!(a.iter().any(|s| s == "-t"), "{a:?}");
        assert!(!a.join(" ").contains("BatchMode"), "{a:?}");
        assert_eq!(a.last().unwrap(), "exec tmux attach -t api");
    }

    #[test]
    fn interactive_attach_remote_folds_pre_select_into_one_connection() {
        // The window pre-selection and the attach run over a SINGLE `ssh -t`, so there is
        // no second connection to hang on a stalled remote or to lose the selection to
        // interactive auth. The remote command is `<select-window> ; exec <attach>`.
        let (n, a) = ssh("prod", "linux", "").interactive_attach_argv(
            &argv(&["tmux", "attach", "-t", "api"]),
            Some(&argv(&["tmux", "select-window", "-t", "api:2"])),
        );
        assert_eq!(n, "ssh");
        assert!(a.iter().any(|s| s == "-t"), "{a:?}");
        assert_eq!(
            a.last().unwrap(),
            "tmux select-window -t 'api:2' ; exec tmux attach -t api"
        );
    }
}
