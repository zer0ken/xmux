//! The ssh machine transport: wraps a mux argv in an ssh connection with the
//! right tty/batch/ControlMaster options and a quiet login shell. Untrusted argv
//! elements are per-arg quoted via [`super::vocab::remote_command`].

use super::vocab::{remote_command, RemoteShell, MARKED_SHELL_PROBE, SHELL_PROBE};
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
    pub credentials: crate::transport::auth::Credentials,
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
    /// alias beginning with `-` is the destination, never an option. A held credential
    /// forces askpass with one password attempt; without one, BatchMode keeps every
    /// command non-interactive, including a tty attach. ControlMaster is multiplexed
    /// only on a non-windows local side with a control path.
    fn ssh_opts(
        &self,
        tty: bool,
        purpose: SshPurpose,
    ) -> (
        Vec<String>,
        Option<crate::transport::auth::AskpassAccess>,
        Option<String>,
    ) {
        let mut a: Vec<String> = Vec::new();
        let requested_access = match purpose {
            SshPurpose::Login => self.credentials.pending_access(&self.alias),
            SshPurpose::Normal => self.credentials.access(&self.alias),
            SshPurpose::CliAttach => None,
        };
        let force = self.credentials.force_askpass_supported();
        let access = requested_access.filter(|_| force || (cfg!(unix) && !tty));
        let unavailable = if access.is_none() && purpose != SshPurpose::CliAttach {
            self.credentials
                .unavailable_reason(&self.alias, purpose == SshPurpose::Login)
                .or_else(|| {
                    (!force && tty && self.credentials.contains(&self.alias)).then(|| {
                        "this OpenSSH version cannot take a password from xmux; update OpenSSH or register a key from a terminal".to_string()
                    })
                })
        } else {
            None
        };
        if tty {
            a.push("-t".into());
        }
        if access.is_none() && purpose != SshPurpose::CliAttach {
            a.push("-o".into());
            a.push("BatchMode=yes".into());
        } else {
            if let Some(access) = &access {
                a.push("-o".into());
                a.push("NumberOfPasswordPrompts=1".into());
                if access.key_opens_no_session() {
                    a.extend(PASSWORD_ONLY.iter().map(|opt| opt.to_string()));
                } else {
                    a.push("-o".into());
                    a.push(KEY_FIRST.into());
                }
            }
        }
        a.push("-o".into());
        a.push(format!("ConnectTimeout={CONNECT_TIMEOUT}"));
        let strict = self
            .credentials
            .profile(&self.alias)
            .and_then(|profile| profile.strict_host_key_checking);
        if purpose == SshPurpose::Login && strict.as_deref() == Some("ask") {
            a.push("-o".into());
            a.push("StrictHostKeyChecking=accept-new".into());
        }
        if self.multiplexes() {
            a.push("-o".into());
            a.push("ControlMaster=auto".into());
            a.push("-o".into());
            a.push(format!("ControlPath={}", self.control_path));
            a.push("-o".into());
            a.push("ControlPersist=60s".into());
        }
        let login = access
            .as_ref()
            .map(crate::transport::auth::AskpassAccess::login)
            .unwrap_or(&self.login);
        for opt in login.options() {
            a.push("-o".into());
            a.push(opt);
        }
        a.push("--".into());
        a.push(self.alias.clone());
        (a, access, unavailable)
    }

    fn command(
        &self,
        args: Vec<String>,
        access: Option<crate::transport::auth::AskpassAccess>,
        tty: bool,
        unavailable: Option<String>,
    ) -> crate::transport::CommandSpec {
        let generation = self.credentials.generation(&self.alias);
        let retry_args = access
            .as_ref()
            .filter(|access| !access.key_opens_no_session())
            .and_then(|_| password_only(&args));
        let command = crate::transport::CommandSpec::new("ssh", args)
            .with_credential_generation(generation)
            .with_auth_unavailable(unavailable);
        match access {
            Some(access) => {
                let old_unix = cfg!(unix) && !self.credentials.force_askpass_supported();
                let with_auth = |command: crate::transport::CommandSpec| {
                    let command = command.with_auth(access.clone(), old_unix && !tty);
                    if !tty {
                        command.detach_tty()
                    } else {
                        command
                    }
                };
                let command = with_auth(command);
                match retry_args {
                    Some(retry_args) => command.with_password_only_retry(with_auth(
                        crate::transport::CommandSpec::new("ssh", retry_args)
                            .with_credential_generation(generation),
                    )),
                    None => command,
                }
            }
            None => command,
        }
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

    fn set_credentials(&mut self, credentials: crate::transport::auth::Credentials) {
        self.credentials = credentials;
    }

    fn has_credential(&self) -> bool {
        self.credentials.contains(&self.alias)
    }

    fn credential_generation(&self) -> u64 {
        self.credentials.generation(&self.alias)
    }

    fn probe_diagnostic(&self, diagnostic: String) -> String {
        let strict = self
            .credentials
            .profile(&self.alias)
            .and_then(|profile| profile.strict_host_key_checking);
        if matches!(strict.as_deref(), Some("true" | "yes"))
            && crate::transport::diagnostic::host_key_unknown(&diagnostic)
        {
            format!(
                "the host key must be added first\nrun: ssh -o BatchMode=no -o StrictHostKeyChecking=ask -- {}",
                shell_quote(&self.alias)
            )
        } else {
            diagnostic
        }
    }

    fn exec_argv(&self, tty: bool, mux_argv: &[String]) -> crate::transport::CommandSpec {
        let (mut args, access, unavailable) = self.ssh_opts(tty, SshPurpose::Normal);
        args.push(self.login_shell_command(&remote_command(mux_argv)));
        self.command(args, access, tty, unavailable)
    }

    /// A REMOTE interactive attach requests a pty and runs `exec <attach>`: the `exec`
    /// replaces the ssh login shell so the connection closes cleanly on detach. Its
    /// authentication remains forced askpass or BatchMode, never a terminal prompt.
    ///
    /// `exec` is POSIX shell syntax, so a remote outside that family gets the attach
    /// alone. What it costs there is one shell process living beside the attach for the
    /// length of the session; what prepending it would cost is the attach never running.
    fn interactive_attach_argv(&self, mux_attach_argv: &[String]) -> crate::transport::CommandSpec {
        let attach = remote_command(mux_attach_argv);
        let remote_cmd = if self.shell.runs_posix_snippets() {
            format!("exec {attach}")
        } else {
            attach
        };
        let (mut args, access, unavailable) = self.ssh_opts(true, SshPurpose::Normal);
        args.push(self.login_shell_command(&remote_cmd));
        self.command(args, access, true, unavailable)
    }

    fn cli_attach_argv(&self, mux_attach_argv: &[String]) -> crate::transport::CommandSpec {
        let attach = remote_command(mux_attach_argv);
        let remote_cmd = if self.shell.runs_posix_snippets() {
            format!("exec {attach}")
        } else {
            attach
        };
        let (mut args, access, unavailable) = self.ssh_opts(true, SshPurpose::CliAttach);
        args.push(self.login_shell_command(&remote_cmd));
        self.command(args, access, true, unavailable)
    }

    /// The remote forces a pty with `-tt` because a pipe-only ssh dies before emitting
    /// control-mode output. Authentication follows the same held-credential rule as
    /// every other ssh command.
    fn control_argv(&self, mux_control_argv: &[String]) -> crate::transport::CommandSpec {
        let mut args = vec!["-tt".to_string()];
        let (opts, access, unavailable) = self.ssh_opts(false, SshPurpose::Normal);
        args.extend(opts);
        args.push(self.login_shell_command(&remote_command(mux_control_argv)));
        self.command(args, access, false, unavailable)
    }

    /// Joins a raw remote shell command behind the ssh options. A POSIX command uses the
    /// same quiet login shell as mux argv, except for the shell-family probe that decides
    /// whether POSIX syntax is valid. The caller must `quote` any untrusted value inside
    /// `remote_cmd` (see [`super::vocab::quote`]).
    fn raw_shell_argv(&self, remote_cmd: &str) -> Option<crate::transport::CommandSpec> {
        let (mut args, access, unavailable) = self.ssh_opts(false, SshPurpose::Normal);
        // The probe must reach the account's default shell directly: it is how xmux
        // learns whether POSIX syntax, including this login wrapper, is valid there.
        let command = if matches!(remote_cmd, SHELL_PROBE | MARKED_SHELL_PROBE) {
            remote_cmd.to_string()
        } else {
            self.login_shell_command(remote_cmd)
        };
        args.push(command);
        Some(self.command(args, access, false, unavailable))
    }

    fn login_argv(&self, remote_cmd: &str) -> Option<crate::transport::CommandSpec> {
        let (mut args, access, unavailable) = self.ssh_opts(false, SshPurpose::Login);
        let strict_yes = self
            .credentials
            .profile(&self.alias)
            .and_then(|profile| profile.strict_host_key_checking)
            .is_some_and(|value| matches!(value.as_str(), "true" | "yes"));
        let help = strict_yes.then(|| {
            // The suggested command runs in the user's terminal, where ssh must be free to
            // show the fingerprint and ask: batch mode and xmux's prompt limits would stop it.
            let mut check = Vec::with_capacity(args.len());
            let mut options = args.iter();
            while let Some(arg) = options.next() {
                if arg == "-o" {
                    if let Some(value) = options.next() {
                        let interactive_only = [
                            "BatchMode=",
                            "NumberOfPasswordPrompts=",
                            "PreferredAuthentications=",
                        ]
                        .iter()
                        .any(|key| value.starts_with(key));
                        if !interactive_only {
                            check.push(arg.clone());
                            check.push(value.clone());
                        }
                    }
                } else {
                    check.push(arg.clone());
                }
            }
            let destination = check
                .iter()
                .position(|arg| arg == "--")
                .unwrap_or(check.len());
            check.splice(
                destination..destination,
                [
                    "-o".to_string(),
                    "BatchMode=no".to_string(),
                    "-o".to_string(),
                    "StrictHostKeyChecking=ask".to_string(),
                ],
            );
            format!(
                "ssh {}",
                check
                    .iter()
                    .map(|arg| shell_quote(arg))
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        });
        args.push(remote_cmd.to_string());
        let command = self.command(args, access, false, unavailable);
        Some(match help {
            Some(help) => command.with_host_key_command(help),
            None => command,
        })
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

/// A held password still lets ssh try a key first, so a host that takes a key needs no
/// password prompt.
const KEY_FIRST: &str = "PreferredAuthentications=publickey,password,keyboard-interactive";

/// The options of a command that authenticates with the held password alone, for a host
/// that accepts a key and then closes the connection before a session opens.
const PASSWORD_ONLY: [&str; 4] = [
    "-o",
    "PubkeyAuthentication=no",
    "-o",
    "PreferredAuthentications=password,keyboard-interactive",
];

/// `args` with the key-first option replaced by [`PASSWORD_ONLY`], or `None` when the
/// options before the destination do not try a key first.
fn password_only(args: &[String]) -> Option<Vec<String>> {
    let destination = args.iter().position(|arg| arg == "--")?;
    let at = args[..destination]
        .windows(2)
        .position(|pair| pair[0] == "-o" && pair[1] == KEY_FIRST)?;
    let mut retry = args[..at].to_vec();
    retry.extend(PASSWORD_ONLY.iter().map(|opt| opt.to_string()));
    retry.extend_from_slice(&args[at + 2..]);
    Some(retry)
}

fn shell_quote(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/' | ':' | '='))
    {
        return value.to_string();
    }
    #[cfg(windows)]
    {
        format!("'{}'", value.replace('\'', "''"))
    }
    #[cfg(not(windows))]
    {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SshPurpose {
    Normal,
    Login,
    CliAttach,
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
            credentials: crate::transport::auth::Credentials::default(),
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
        let a = ssh("prod", "linux", "/tmp/cm.sock")
            .ssh_opts(false, SshPurpose::Normal)
            .0;
        let joined = a.join(" ");
        assert!(joined.contains("BatchMode=yes"), "{a:?}");
        assert!(joined.contains("ConnectTimeout=5"), "{a:?}");
        assert!(joined.contains("ControlMaster=auto"), "{a:?}");
        assert_eq!(a[a.len() - 2], "--");
        assert_eq!(a[a.len() - 1], "prod");
    }

    #[test]
    fn ssh_opts_interactive_without_password_is_non_interactive() {
        let a = ssh("prod", "linux", "")
            .ssh_opts(true, SshPurpose::Normal)
            .0;
        let joined = a.join(" ");
        assert!(joined.contains("-t"), "{a:?}");
        assert!(joined.contains("BatchMode=yes"), "{a:?}");
    }

    #[test]
    fn ssh_opts_windows_omits_control_master() {
        let a = ssh("prod", "windows", "/tmp/cm.sock")
            .ssh_opts(false, SshPurpose::Normal)
            .0;
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
            t.interactive_attach_argv(&attach).last().unwrap(),
            "sh -lc '{ exec tmux attach -t api\n} 1>&3 2>&4' 3>&1 4>&2 1>/dev/null 2>/dev/null",
            "a POSIX remote keeps the exec"
        );

        t.set_remote_shell(RemoteShell::Other);
        assert_eq!(
            t.interactive_attach_argv(&attach).last().unwrap(),
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
        let before = t.exec_argv(false, &argv(&["tmux", "ls"])).join(" ");
        assert!(!before.contains("User="), "{before}");

        t.set_login(Login {
            address: Some("100.87.27.26".into()),
            port: Some(2222),
            user: Some("hrlee".into()),
        });
        let after = t.exec_argv(false, &argv(&["tmux", "ls"])).join(" ");
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
        let a =
            ssh("prod", "linux", "").exec_argv(false, &argv(&["tmux", "kill-session", "-t", "x"]));
        assert_eq!(a.program(), "ssh");
        assert_eq!(
            a.last().unwrap(),
            "sh -lc '{ tmux kill-session -t x\n} 1>&3 2>&4' 3>&1 4>&2 1>/dev/null 2>/dev/null"
        );
    }

    #[test]
    fn login_shell_wrapper_preserves_quoted_mux_arguments() {
        let a = ssh("prod", "linux", "").exec_argv(
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
    fn ssh_opts_carry_the_login_overrides_and_keep_the_alias() {
        // The overrides ride as `-o` keywords, so the destination stays the alias and the
        // machine's own ssh-config stanza still supplies whatever they do not name.
        let mut t = ssh("prod", "linux", "/tmp/cm.sock");
        t.login = Login {
            address: Some("100.88.0.0".into()),
            port: Some(2222),
            user: Some("alice".into()),
        };
        let joined = t.exec_argv(false, &["true".to_string()]).join(" ");
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
            .join(" ");
        for k in ["HostName=", "Port=", "User="] {
            assert!(!joined.contains(k), "{k} must not appear: {joined}");
        }
    }

    #[tokio::test]
    async fn a_held_password_uses_forced_askpass_without_putting_the_secret_in_argv_or_env() {
        let root = std::env::temp_dir().join(format!(
            "xmux-ssh-auth-{}-{}",
            std::process::id(),
            crate::transport::auth::request_test_token()
        ));
        let credentials = crate::transport::auth::Credentials::new(root.clone());
        let pending = credentials
            .begin(
                "prod",
                Login {
                    address: Some("127.0.0.1".into()),
                    port: Some(2222),
                    user: Some("dev".into()),
                },
                "never-in-command".into(),
            )
            .unwrap()
            .unwrap();
        assert!(pending.promote());
        let mut transport = ssh("prod", "windows", "");
        transport.set_credentials(credentials.clone());

        let command = transport.exec_argv(false, &argv(&["tmux", "ls"]));
        let joined = command.argv().join(" ");
        assert!(!joined.contains("BatchMode=yes"), "{joined}");
        assert!(joined.contains("NumberOfPasswordPrompts=1"), "{joined}");
        assert!(
            !joined.contains("StrictHostKeyChecking=accept-new"),
            "{joined}"
        );
        assert!(!joined.contains("never-in-command"), "{joined}");
        assert!(command
            .env()
            .iter()
            .any(|(key, value)| { key == "SSH_ASKPASS_REQUIRE" && value == "force" }));
        assert!(command
            .env()
            .iter()
            .all(|(_, value)| !value.contains("never-in-command")));
        let endpoint = command
            .env()
            .iter()
            .find(|(key, _)| key == "XMUX_ASKPASS_ENDPOINT")
            .map(|(_, value)| std::path::PathBuf::from(value))
            .unwrap();
        let token = command
            .env()
            .iter()
            .find(|(key, _)| key == "XMUX_ASKPASS_TOKEN")
            .map(|(_, value)| value.as_str())
            .unwrap();
        let supplied = crate::transport::auth::request_password(
            &endpoint,
            token,
            "dev@127.0.0.1's password: ",
        )
        .await
        .expect("broker reply");
        assert_eq!(supplied.as_deref(), Some("never-in-command"));
        assert!(!command
            .forget_refused_password(1, "dev@127.0.0.1: Permission denied (publickey,password)."));
        assert!(credentials.contains("prod"));
        assert!(!command.forget_refused_password(
            255,
            "tmux: error connecting to /tmp/tmux-1000/default (Permission denied)"
        ));
        assert!(credentials.contains("prod"));
        assert!(command.forget_refused_password(
            255,
            "dev@127.0.0.1: Permission denied (publickey,password)."
        ));
        assert!(
            !credentials.contains("prod"),
            "a refusal after askpass supplied the password forgets it"
        );
        drop(transport);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn accept_new_is_only_on_the_submitted_login() {
        let root = std::env::temp_dir().join(format!(
            "xmux-ssh-login-policy-{}-{}",
            std::process::id(),
            crate::transport::auth::request_test_token()
        ));
        let credentials = crate::transport::auth::Credentials::new(root.clone());
        credentials.set_profiles(std::collections::HashMap::from([(
            "prod".into(),
            crate::transport::auth::SshProfile {
                strict_host_key_checking: Some("ask".into()),
                ..Default::default()
            },
        )]));
        credentials
            .begin("prod", Login::default(), "secret".into())
            .unwrap();
        let mut transport = ssh("prod", "windows", "");
        transport.set_credentials(credentials.clone());
        let login = transport.login_argv("true").unwrap().join(" ");
        assert!(login.contains("StrictHostKeyChecking=accept-new"));
        assert!(!transport
            .exec_argv(false, &argv(&["true"]))
            .join(" ")
            .contains("accept-new"));
        credentials.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn an_explicit_strict_host_key_policy_is_never_weakened() {
        let root = std::env::temp_dir().join(format!(
            "xmux-ssh-strict-policy-{}-{}",
            std::process::id(),
            crate::transport::auth::request_test_token()
        ));
        let credentials = crate::transport::auth::Credentials::new(root.clone());
        credentials.set_profiles(std::collections::HashMap::from([(
            "prod".into(),
            crate::transport::auth::SshProfile {
                strict_host_key_checking: Some("true".into()),
                ..Default::default()
            },
        )]));
        credentials
            .begin("prod", Login::default(), "secret".into())
            .unwrap();
        let mut transport = ssh("prod", "windows", "");
        transport.set_credentials(credentials.clone());
        let login = transport.login_argv("true").unwrap();
        assert!(!login.join(" ").contains("accept-new"));
        assert!(login
            .host_key_command()
            .is_some_and(|command| command.contains("StrictHostKeyChecking=ask")
                && !command.contains("NumberOfPasswordPrompts")));
        credentials.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn the_suggested_host_key_command_can_ask_without_a_password() {
        let root = std::env::temp_dir().join(format!(
            "xmux-ssh-strict-key-only-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let credentials = crate::transport::auth::Credentials::new(root.clone());
        credentials.set_profiles(std::collections::HashMap::from([(
            "prod".into(),
            crate::transport::auth::SshProfile {
                strict_host_key_checking: Some("true".into()),
                ..Default::default()
            },
        )]));
        let mut transport = ssh("prod", "windows", "");
        transport.set_credentials(credentials.clone());
        let login = transport.login_argv("true").unwrap();
        assert!(login.join(" ").contains("BatchMode=yes"));
        let help = login.host_key_command().expect("strict policy help");
        assert!(help.contains("StrictHostKeyChecking=ask"), "{help}");
        assert!(!help.contains("BatchMode=yes"), "{help}");
        assert!(help.contains("BatchMode=no"), "{help}");
        credentials.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn missing_effective_host_key_policy_does_not_add_accept_new() {
        let root = std::env::temp_dir().join(format!(
            "xmux-ssh-missing-policy-{}-{}",
            std::process::id(),
            crate::transport::auth::request_test_token()
        ));
        let credentials = crate::transport::auth::Credentials::new(root.clone());
        credentials
            .begin("prod", Login::default(), "secret".into())
            .unwrap();
        let mut transport = ssh("prod", "windows", "");
        transport.set_credentials(credentials.clone());
        let login = transport.login_argv("true").unwrap();
        assert!(!login.join(" ").contains("accept-new"));
        credentials.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn strict_unknown_host_key_is_unreachable_with_a_fingerprint_command() {
        let transport = ssh("prod", "windows", "");
        transport
            .credentials
            .set_profiles(std::collections::HashMap::from([(
                "prod".into(),
                crate::transport::auth::SshProfile {
                    strict_host_key_checking: Some("true".into()),
                    ..Default::default()
                },
            )]));
        let reason = transport.probe_diagnostic("Host key verification failed.".into());
        assert_eq!(
            crate::model::FailureKind::from_error(&reason),
            crate::model::FailureKind::Unreachable
        );
        assert!(reason.contains("ssh -o BatchMode=no -o StrictHostKeyChecking=ask -- prod"));
    }

    #[test]
    fn fingerprint_command_quotes_shell_metacharacters() {
        assert_eq!(shell_quote("prod;echo exposed"), "'prod;echo exposed'");
        assert_eq!(shell_quote("plain-host"), "plain-host");

        let alias = "prod;echo exposed";
        let transport = ssh(alias, "windows", "");
        transport
            .credentials
            .set_profiles(std::collections::HashMap::from([(
                alias.into(),
                crate::transport::auth::SshProfile {
                    strict_host_key_checking: Some("yes".into()),
                    ..Default::default()
                },
            )]));
        assert!(transport
            .login_argv("true")
            .unwrap()
            .host_key_command()
            .is_some_and(|command| command.ends_with("-- 'prod;echo exposed'")));
    }

    #[test]
    fn a_machine_without_a_password_stays_non_interactive() {
        let command = ssh("prod", "windows", "").exec_argv(false, &argv(&["tmux", "ls"]));
        assert!(command.argv().join(" ").contains("BatchMode=yes"));
        assert!(command.env().is_empty());
    }

    #[test]
    fn interactive_attach_remote_execs_over_ssh_tty() {
        let a = ssh("prod", "linux", "")
            .interactive_attach_argv(&argv(&["tmux", "attach", "-t", "api"]));
        assert_eq!(a.program(), "ssh");
        assert!(a.iter().any(|s| s == "-t"), "{a:?}");
        assert!(a.join(" ").contains("BatchMode=yes"), "{a:?}");
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
        let marked = ssh("prod", "linux", "")
            .raw_shell_argv(super::super::vocab::MARKED_SHELL_PROBE)
            .unwrap();
        assert_eq!(
            marked.last().unwrap(),
            super::super::vocab::MARKED_SHELL_PROBE
        );
    }

    #[tokio::test]
    async fn a_key_that_opens_no_session_is_retried_once_with_the_password_alone() {
        let root = std::env::temp_dir().join(format!(
            "xmux-ssh-key-session-{}-{}",
            std::process::id(),
            crate::transport::auth::request_test_token()
        ));
        let credentials = crate::transport::auth::Credentials::new(root.clone());
        let pending = credentials
            .begin("prod", Login::default(), "secret".into())
            .unwrap()
            .unwrap();
        assert!(pending.promote());
        let mut transport = ssh("prod", "linux", "/tmp/cm.sock");
        transport.set_credentials(credentials.clone());
        let dropped = "Connection reset by 127.0.0.1 port 22";

        let command = transport.exec_argv(false, &argv(&["tmux", "ls"]));
        assert!(command.argv().contains(&KEY_FIRST.to_string()));
        assert!(command.password_only_retry(1, dropped).is_none());
        assert!(command
            .password_only_retry(255, "dev@prod: Permission denied (publickey,password).")
            .is_none());
        let retry = command
            .password_only_retry(255, dropped)
            .expect("a drop before the password prompt is retried");
        let joined = retry.argv().join(" ");
        assert!(joined.contains("PubkeyAuthentication=no"), "{joined}");
        assert!(
            joined.contains("PreferredAuthentications=password,keyboard-interactive"),
            "{joined}"
        );
        assert!(!joined.contains("publickey"), "{joined}");
        assert_eq!(
            retry.argv().last(),
            command.argv().last(),
            "the retry runs the same remote command"
        );
        assert!(retry.has_credential());
        assert!(
            retry.password_only_retry(255, dropped).is_none(),
            "the retry runs once"
        );

        // Every command composed later skips the key from the start.
        let later = transport.exec_argv(false, &argv(&["tmux", "ls"]));
        let joined = later.argv().join(" ");
        assert!(joined.contains("PubkeyAuthentication=no"), "{joined}");
        assert!(later.password_only_retry(255, dropped).is_none());
        drop(transport);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_drop_after_the_password_was_handed_over_is_not_retried() {
        let root = std::env::temp_dir().join(format!(
            "xmux-ssh-key-session-supplied-{}-{}",
            std::process::id(),
            crate::transport::auth::request_test_token()
        ));
        let credentials = crate::transport::auth::Credentials::new(root.clone());
        let pending = credentials
            .begin(
                "prod",
                Login {
                    address: Some("127.0.0.1".into()),
                    port: None,
                    user: Some("dev".into()),
                },
                "secret".into(),
            )
            .unwrap()
            .unwrap();
        assert!(pending.promote());
        let mut transport = ssh("prod", "windows", "");
        transport.set_credentials(credentials.clone());
        let command = transport.exec_argv(false, &argv(&["tmux", "ls"]));
        let env = |name: &str| {
            command
                .env()
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
                .unwrap()
        };
        let supplied = crate::transport::auth::request_password(
            std::path::Path::new(&env("XMUX_ASKPASS_ENDPOINT")),
            &env("XMUX_ASKPASS_TOKEN"),
            "dev@127.0.0.1's password: ",
        )
        .await
        .expect("broker reply");
        assert_eq!(supplied.as_deref(), Some("secret"));
        assert!(command
            .password_only_retry(255, "Connection closed by 127.0.0.1 port 22")
            .is_none());
        drop(transport);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_command_without_a_password_has_no_retry() {
        let command = ssh("prod", "linux", "/tmp/cm.sock").exec_argv(false, &argv(&["tmux", "ls"]));
        assert!(command
            .password_only_retry(255, "Connection reset by 127.0.0.1 port 22")
            .is_none());
    }
}
