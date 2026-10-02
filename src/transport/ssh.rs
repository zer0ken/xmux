//! The ssh machine transport: wraps a mux argv in an ssh connection with the
//! right tty/batch/ControlMaster options and a quiet login shell. Untrusted argv
//! elements are per-arg quoted via [`super::vocab::remote_command`].

use super::vocab::{remote_command, RemoteShell, SHELL_PROBE};
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
    /// Which shell family the far side answers with. `Posix` until the reachability
    /// probe says otherwise, so a machine that has not been asked yet is addressed the
    /// way every POSIX remote is.
    pub shell: RemoteShell,
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
    /// Whether this side can share ONE authenticated connection across several ssh runs.
    /// Windows ssh has no connection multiplexing, and a machine addressed without a
    /// control path has nowhere to put the socket. Asked in one place, because a channel
    /// that opens a master and one that reuses it must never disagree about whether there
    /// is one.
    fn multiplexes(&self) -> bool {
        self.os != "windows" && !self.control_path.is_empty()
    }

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
        if self.multiplexes() {
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

    /// Runs a command through the remote's login PATH without letting shell startup
    /// output enter the command's stdout or stderr.
    ///
    /// The outer shell saves ssh's streams on file descriptors 3 and 4, then starts the
    /// login shell with its ordinary streams pointed at `/dev/null`. The command group restores
    /// both streams only after login startup has completed. This keeps parsed mux output
    /// clean, including the control path where ssh allocates a pty and combines streams.
    fn login_shell_command(&self, command: &str) -> String {
        if !self.shell.runs_posix_snippets() {
            return command.to_string();
        }
        // The group carries the restoring redirection for every command in a multi-command
        // snippet, not only the last one; the newline lets a snippet end in `;` or `&`.
        let command = format!("{{ {command}\n}} 1>&3 2>&4");
        let shell = remote_command(&["sh".into(), "-lc".into(), command]);
        format!("{shell} 3>&1 4>&2 1>/dev/null 2>/dev/null")
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

    /// Only where this side multiplexes: every run then rides the one master the first
    /// run authenticated. Without multiplexing every run is a login of its own.
    fn reuses_connection(&self) -> bool {
        self.multiplexes()
    }

    fn remote_shell(&self) -> RemoteShell {
        self.shell
    }

    fn set_remote_shell(&mut self, shell: RemoteShell) {
        self.shell = shell;
    }

    fn set_login(&mut self, login: Login) {
        self.login = login;
    }

    fn exec_argv(&self, tty: bool, mux_argv: &[String]) -> (String, Vec<String>) {
        let mut args = self.ssh_opts(tty);
        args.push(self.login_shell_command(&remote_command(mux_argv)));
        ("ssh".into(), args)
    }

    /// A REMOTE interactive attach requests a pty (`-t`, no BatchMode) and runs
    /// `exec <attach>`: the `exec` replaces the ssh login shell so the connection
    /// closes cleanly on detach.
    ///
    /// `exec` is POSIX shell syntax, so a remote outside that family gets the attach
    /// alone. What it costs there is one shell process living beside the attach for the
    /// length of the session; what prepending it would cost is the attach never running.
    fn interactive_attach_argv(&self, mux_attach_argv: &[String]) -> (String, Vec<String>) {
        let attach = remote_command(mux_attach_argv);
        let remote_cmd = if self.shell.runs_posix_snippets() {
            format!("exec {attach}")
        } else {
            attach
        };
        let mut args = self.ssh_opts(true);
        args.push(self.login_shell_command(&remote_cmd));
        ("ssh".into(), args)
    }

    /// The remote forces a pty with `-tt` (a pipe-only ssh dies before emitting
    /// control-mode output) and runs over `BatchMode=yes`.
    fn control_argv(&self, mux_control_argv: &[String]) -> Vec<String> {
        let mut args = vec!["-tt".to_string()];
        args.extend(self.ssh_opts(false));
        args.push(self.login_shell_command(&remote_command(mux_control_argv)));
        let mut v = vec!["ssh".to_string()];
        v.extend(args);
        v
    }

    /// Joins a raw remote shell command behind the ssh options. A POSIX command uses the
    /// same quiet login shell as mux argv, except for the shell-family probe that decides
    /// whether POSIX syntax is valid. The caller must `quote` any untrusted value inside
    /// `remote_cmd` (see [`super::vocab::quote`]).
    fn raw_shell_argv(&self, remote_cmd: &str) -> Option<Vec<String>> {
        let mut v = vec!["ssh".to_string()];
        v.extend(self.ssh_opts(false));
        // The probe must reach the account's default shell directly: it is how xmux
        // learns whether POSIX syntax, including this login wrapper, is valid there.
        let command = if remote_cmd == SHELL_PROBE {
            remote_cmd.to_string()
        } else {
            self.login_shell_command(remote_cmd)
        };
        v.push(command);
        Some(v)
    }

    /// The login: a real ssh with no BatchMode, so every question it has reaches the
    /// person watching it. The remote command is the caller's to append.
    ///
    /// Where this side multiplexes, it forces a NEW master over the SAME control socket
    /// every other ssh shares and runs `true`, so what it leaves behind is an
    /// authenticated connection the later `BatchMode` channels reuse. Where it does not -
    /// Windows, whose ssh has no connection multiplexing - the same login runs without
    /// those options and leaves nothing behind, which costs the reuse and NOTHING else:
    /// ssh asks about the host key before it authenticates, and the answer is written to
    /// `known_hosts`, so accepting a key is a login that outlasts any connection. A host
    /// that then needs a password is asked for one again on the next probe, which is the
    /// truth about that machine on this platform rather than a reason to refuse the login.
    fn login_argv(&self, login: &Login) -> Option<Vec<String>> {
        let mut v = vec![
            "ssh".to_string(),
            "-o".into(),
            format!("ConnectTimeout={CONNECT_TIMEOUT}"),
        ];
        if self.multiplexes() {
            v.push("-o".into());
            v.push("ControlMaster=yes".into());
            v.push("-o".into());
            v.push(format!("ControlPath={}", self.control_path));
            v.push("-o".into());
            v.push("ControlPersist=60s".into());
        }
        // The values the user is submitting, not the ones this transport was built with:
        // the whole point of the run is to try something that has not worked yet.
        for opt in login.options() {
            v.push("-o".into());
            v.push(opt);
        }
        v.push("--".into());
        v.push(self.alias.clone());
        // No remote command: the caller appends the one this login is FOR. That command
        // runs inside the session the user just authenticated, which is the only session
        // some platforms will ever have.
        Some(v)
    }

    fn clone_box(&self) -> Box<dyn Transport> {
        Box::new(self.clone())
    }

    fn clone_as(&self, id: &str) -> Box<dyn Transport> {
        Box::new(Self {
            id: id.to_string(),
            ..self.clone()
        })
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
            shell: RemoteShell::default(),
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
    fn a_posix_remote_execs_the_attach_and_a_non_posix_one_does_not() {
        // `exec` replaces the login shell so the connection closes cleanly on detach,
        // and it is POSIX syntax: a remote outside that family would fail the whole
        // attach on the prefix alone, so it gets the attach by itself.
        let attach = argv(&["tmux", "attach", "-t", "api"]);

        let mut t = ssh("prod", "linux", "");
        assert_eq!(
            t.interactive_attach_argv(&attach).1.last().unwrap(),
            "sh -lc '{ exec tmux attach -t api\n} 1>&3 2>&4' 3>&1 4>&2 1>/dev/null 2>/dev/null",
            "a POSIX remote keeps the exec"
        );

        t.set_remote_shell(RemoteShell::Other);
        assert_eq!(
            t.interactive_attach_argv(&attach).1.last().unwrap(),
            "tmux attach -t api",
            "a non-POSIX remote gets the attach with no exec"
        );
    }

    #[test]
    fn a_recorded_login_rides_every_later_command() {
        // A login authenticates once and its connection then ends. Nothing but this
        // recording survives it, so without it the next command out would name no
        // account and ssh would fall back to whoever runs xmux - a different user on the
        // remote, and a refusal that reads as the login not having worked.
        let mut t = ssh("prod", "linux", "");
        let before = t.exec_argv(false, &argv(&["tmux", "ls"])).1.join(" ");
        assert!(!before.contains("User="), "{before}");

        t.set_login(Login {
            address: Some("100.87.27.26".into()),
            port: Some(2222),
            user: Some("hrlee".into()),
        });
        let after = t.exec_argv(false, &argv(&["tmux", "ls"])).1.join(" ");
        for expected in ["HostName=100.87.27.26", "Port=2222", "User=hrlee"] {
            assert!(after.contains(expected), "{expected} missing from {after}");
        }
    }

    #[test]
    fn a_remote_is_posix_until_the_probe_says_otherwise() {
        let mut t = ssh("prod", "linux", "");
        assert_eq!(t.remote_shell(), RemoteShell::Posix);
        t.set_remote_shell(RemoteShell::Other);
        assert_eq!(t.remote_shell(), RemoteShell::Other);
    }

    #[test]
    fn exec_argv_remote_wraps_in_ssh() {
        let (n, a) =
            ssh("prod", "linux", "").exec_argv(false, &argv(&["tmux", "kill-session", "-t", "x"]));
        assert_eq!(n, "ssh");
        assert_eq!(
            a.last().unwrap(),
            "sh -lc '{ tmux kill-session -t x\n} 1>&3 2>&4' 3>&1 4>&2 1>/dev/null 2>/dev/null"
        );
    }

    #[test]
    fn login_shell_wrapper_preserves_quoted_mux_arguments() {
        let (_n, a) = ssh("prod", "linux", "").exec_argv(
            false,
            &argv(&[
                "tmux",
                "rename-session",
                "-t",
                "old",
                "evil'; touch /tmp/pwned; echo '",
            ]),
        );
        assert_eq!(
            a.last().unwrap(),
            "sh -lc '{ tmux rename-session -t old '\\''evil'\\''\\'\\'''\\''; touch /tmp/pwned; echo '\\''\\'\\'''\\'''\\''\n} 1>&3 2>&4' 3>&1 4>&2 1>/dev/null 2>/dev/null"
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
        assert_eq!(
            got.last().unwrap(),
            "sh -lc '{ tmux -CC attach\n} 1>&3 2>&4' 3>&1 4>&2 1>/dev/null 2>/dev/null"
        );
    }

    #[test]
    fn raw_shell_argv_some_for_ssh() {
        let got = ssh("prod", "linux", "")
            .raw_shell_argv("c=$(tty); echo $c")
            .unwrap();
        assert_eq!(got[0], "ssh");
        assert_eq!(
            got.last().unwrap(),
            "sh -lc '{ c=$(tty); echo $c\n} 1>&3 2>&4' 3>&1 4>&2 1>/dev/null 2>/dev/null"
        );
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
        assert!(
            joined.ends_with("-- prod"),
            "the destination ends it; what the login is FOR is appended by the caller: {joined}"
        );
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

    /// Windows ssh cannot share one authenticated connection, so the login leaves nothing
    /// behind there. It still RUNS: ssh asks about the host key before it authenticates
    /// and writes the answer to `known_hosts`, so accepting a key is a login whose whole
    /// result outlives the connection. Refusing to run it would cost that for a reason
    /// that only touches the reuse.
    #[test]
    fn a_login_runs_on_windows_without_the_options_windows_has_no_use_for() {
        let argv = ssh("prod", "windows", "")
            .login_argv(&Login::default())
            .expect("a remote host has a login to run on any platform");
        let joined = argv.join(" ");
        assert!(
            !joined.contains("ControlMaster") && !joined.contains("ControlPath"),
            "nothing is asked of an ssh that cannot multiplex: {joined}"
        );
        assert!(
            !joined.contains("BatchMode"),
            "the login must be able to ask its questions: {joined}"
        );
        assert_eq!(
            argv.last().unwrap(),
            "prod",
            "the destination ends it; the remote command is the caller's to append"
        );
    }

    /// Where this side multiplexes, the login opens the master every later channel rides.
    #[test]
    fn a_login_opens_the_master_where_one_can_be_left() {
        let joined = ssh("prod", "linux", "/tmp/cm.sock")
            .login_argv(&Login::default())
            .expect("a remote host has a login")
            .join(" ");
        assert!(joined.contains("ControlMaster=yes"), "{joined}");
        assert!(joined.contains("ControlPath=/tmp/cm.sock"), "{joined}");
        assert!(joined.contains("ControlPersist=60s"), "{joined}");
    }

    /// A machine with nowhere to put the socket is in the same position as Windows: the
    /// login runs, and leaves no connection behind.
    #[test]
    fn a_login_without_a_control_path_leaves_nothing_behind() {
        let joined = ssh("prod", "linux", "")
            .login_argv(&Login::default())
            .expect("a remote host has a login")
            .join(" ");
        assert!(!joined.contains("ControlMaster"), "{joined}");
    }

    #[test]
    fn interactive_attach_remote_execs_over_ssh_tty() {
        let (n, a) = ssh("prod", "linux", "")
            .interactive_attach_argv(&argv(&["tmux", "attach", "-t", "api"]));
        assert_eq!(n, "ssh");
        assert!(a.iter().any(|s| s == "-t"), "{a:?}");
        assert!(!a.join(" ").contains("BatchMode"), "{a:?}");
        assert_eq!(
            a.last().unwrap(),
            "sh -lc '{ exec tmux attach -t api\n} 1>&3 2>&4' 3>&1 4>&2 1>/dev/null 2>/dev/null"
        );
    }

    #[test]
    fn shell_probe_stays_direct_until_the_remote_shell_is_known() {
        let got = ssh("prod", "linux", "")
            .raw_shell_argv(super::super::vocab::SHELL_PROBE)
            .unwrap();
        assert_eq!(got.last().unwrap(), "echo $0");
    }
}
