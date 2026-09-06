//! The ssh machine transport: wraps a mux argv in an ssh connection with the
//! right tty/batch/ControlMaster options. Untrusted argv elements are per-arg
//! quoted via [`super::vocab::remote_command`].

use super::vocab::remote_command;
use super::Transport;

/// Bounds the ssh TCP connect; the per-host scan timeout must exceed it so a
/// slow-but-alive remote is not cancelled mid-connect. Also the number the machine
/// describes itself with, so what is SHOWN and what is passed to ssh are one value.
pub(crate) const CONNECT_TIMEOUT: &str = "5";

/// A remote over ssh. `control_path` is the ControlMaster socket (empty ⇒ no
/// multiplex, e.g. a Windows local side); `os` is the LOCAL platform (gates
/// ControlMaster). `alias` is the ssh DESTINATION, and `id` is the SOURCE id this
/// transport answers as - the two differ when a machine serves several muxes, since
/// each mux is its own source reached at the same destination.
#[derive(Clone, Debug)]
pub struct Ssh {
    pub id: String,
    pub alias: String,
    pub control_path: String,
    pub os: String,
    /// The connection values the user supplied for this machine, empty until they do.
    pub login: Login,
}

/// The three connection values ssh never asks for and must know before it dials: where
/// to go, on which port, and as whom. A machine that does not answer with the values ssh
/// resolves on its own is reached with these instead.
///
/// Each is applied as an `-o` OVERRIDE, never by replacing the destination. The machine
/// keeps its alias, so its `~/.ssh/config` stanza still supplies everything the override
/// does not name, and a command-line override outranks the file for what it does name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Login {
    pub address: Option<String>,
    pub port: Option<u16>,
    pub user: Option<String>,
}

impl Login {
    /// The `-o` pairs this login names, in ssh's own keyword spelling. Empty when the
    /// user supplied nothing, which is the state of every machine that just works.
    pub(crate) fn options(&self) -> Vec<String> {
        let mut a = Vec::new();
        if let Some(address) = &self.address {
            a.push(format!("HostName={address}"));
        }
        if let Some(port) = self.port {
            a.push(format!("Port={port}"));
        }
        if let Some(user) = &self.user {
            a.push(format!("User={user}"));
        }
        a
    }

    /// True when the user supplied nothing, so the machine is reached exactly as ssh
    /// would reach it unaided.
    pub fn is_empty(&self) -> bool {
        self.address.is_none() && self.port.is_none() && self.user.is_none()
    }
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
        for opt in self.login.options() {
            a.push("-o".into());
            a.push(opt);
        }
        a.push("--".into());
        a.push(self.alias.clone());
        a
    }
}

impl Transport for Ssh {
    fn host_id(&self) -> &str {
        // The SOURCE id, not the destination: several muxes on one machine are several
        // sources reached at the same `alias`.
        &self.id
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
    /// `exec <attach>`: the `exec` replaces the ssh login shell so the connection
    /// closes cleanly on detach.
    fn interactive_attach_argv(&self, mux_attach_argv: &[String]) -> (String, Vec<String>) {
        let attach = remote_command(mux_attach_argv);
        let remote_cmd = format!("exec {attach}");
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
    fn raw_shell_argv(&self, remote_cmd: &str) -> Option<Vec<String>> {
        let mut v = vec!["ssh".to_string()];
        v.extend(self.ssh_opts(false));
        v.push(remote_cmd.to_string());
        Some(v)
    }

    /// The login: force a NEW master over the SAME control socket every other ssh
    /// shares, with no BatchMode so it can prompt, and run `true` so the master
    /// lingers via `ControlPersist` after auth. `None` on Windows, where ssh has no
    /// ControlMaster socket to leave authenticated.
    fn login_argv(&self, login: &Login) -> Option<Vec<String>> {
        if self.os == "windows" {
            return None; // no ControlMaster socket to leave authenticated
        }
        let mut v = vec![
            "ssh".to_string(),
            "-o".into(),
            "ControlMaster=yes".into(),
            "-o".into(),
            format!("ControlPath={}", self.control_path),
            "-o".into(),
            "ControlPersist=60s".into(),
            "-o".into(),
            format!("ConnectTimeout={CONNECT_TIMEOUT}"),
        ];
        // The values the user is submitting, not the ones this transport was built with:
        // the whole point of the run is to try something that has not worked yet.
        for opt in login.options() {
            v.push("-o".into());
            v.push(opt);
        }
        v.push("--".into());
        v.push(self.alias.clone());
        // The connection itself IS the work: it leaves the authenticated master behind,
        // and the remote command only has to exit.
        v.push("true".into());
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
            id: alias.to_string(),
            alias: alias.into(),
            control_path: cp.into(),
            os: os.into(),
            login: Login::default(),
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
    fn raw_shell_argv_some_for_ssh() {
        let got = ssh("prod", "linux", "")
            .raw_shell_argv("c=$(tty); echo $c")
            .unwrap();
        assert_eq!(got[0], "ssh");
        assert_eq!(got.last().unwrap(), "c=$(tty); echo $c");
        assert!(
            got.iter().any(|s: &String| s.contains("BatchMode=yes")),
            "{got:?}"
        );
    }

    #[test]
    fn ssh_login_argv_forces_a_master_with_the_same_control_path() {
        let login = Login {
            user: Some("alice".into()),
            ..Default::default()
        };
        let got = ssh("prod", "linux", "/tmp/cm.sock")
            .login_argv(&login)
            .unwrap();
        assert_eq!(got[0], "ssh");
        let joined = got.join(" ");
        assert!(joined.contains("ControlMaster=yes"), "{joined}");
        assert!(joined.contains("ControlPath=/tmp/cm.sock"), "{joined}");
        assert!(joined.contains("User=alice"), "{joined}");
        assert!(
            !joined.contains("BatchMode"),
            "the login must be able to prompt: {joined}"
        );
        assert_eq!(got.last().unwrap(), "true");
        assert!(joined.ends_with("-- prod true"), "{joined}");
    }

    #[test]
    fn ssh_opts_carry_the_login_overrides_and_keep_the_alias() {
        // The overrides ride as `-o` keywords, so the destination stays the alias and the
        // machine's own ssh-config stanza still supplies whatever they do not name.
        let mut t = ssh("prod", "linux", "/tmp/cm.sock");
        t.login = Login {
            address: Some("100.88.0.0".into()),
            port: Some(2222),
            user: Some("alice".into()),
        };
        let joined = t.exec_argv(false, &["true".to_string()]).1.join(" ");
        assert!(joined.contains("HostName=100.88.0.0"), "{joined}");
        assert!(joined.contains("Port=2222"), "{joined}");
        assert!(joined.contains("User=alice"), "{joined}");
        assert!(
            joined.contains("-- prod"),
            "the alias is the destination: {joined}"
        );
    }

    #[test]
    fn ssh_opts_carry_nothing_when_no_login_was_supplied() {
        let joined = ssh("prod", "linux", "/tmp/cm.sock")
            .exec_argv(false, &["true".to_string()])
            .1
            .join(" ");
        for k in ["HostName=", "Port=", "User="] {
            assert!(!joined.contains(k), "{k} must not appear: {joined}");
        }
    }

    #[test]
    fn ssh_login_argv_is_none_on_windows() {
        assert_eq!(
            ssh("prod", "windows", "").login_argv(&Login::default()),
            None,
            "no ControlMaster on Windows to reuse"
        );
    }

    #[test]
    fn interactive_attach_remote_execs_over_ssh_tty() {
        let (n, a) = ssh("prod", "linux", "")
            .interactive_attach_argv(&argv(&["tmux", "attach", "-t", "api"]));
        assert_eq!(n, "ssh");
        assert!(a.iter().any(|s| s == "-t"), "{a:?}");
        assert!(!a.join(" ").contains("BatchMode"), "{a:?}");
        assert_eq!(a.last().unwrap(), "exec tmux attach -t api");
    }
}
