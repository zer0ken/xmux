//! Thin per-source config/data for a mux server reachable from this machine (the
//! local mux, or a remote one over ssh): alias, mux binary, machine kind (socket /
//! ssh alias, control path, os), and an injectable runner. The off-loop `Ops`/CLI
//! paths assemble a value [`Host`](crate::model::Host) from this config (`host()`)
//! and drive its enumerate/manage/attach through the `Host`/`Mux`/`Transport` APIs;
//! the machine boundary itself - argv assembly and the ssh transport (connect-timeout,
//! injection-safe quoting) - lives entirely in `Transport`, built at the single
//! `MachineKind::transport` site. The mux-env rules live in `mux::vocab`.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::io::AsyncReadExt;

use crate::provision::config::Config;
use crate::session;
use crate::transport::CommandSpec;
use crate::transport::MachineKind;

/// The shell family each remote machine answered its probe with, keyed by machine and
/// shared by every [`Source`] one [`Env`](crate::provision::env::Env) holds. A value
/// host assembled off the event loop starts from the transport's default family, so
/// without this record a command composed there would assume POSIX on a machine already
/// known to answer with PowerShell.
///
/// `probe_locks` holds one lock per machine so concurrent first operations on the same
/// machine run its probe once.
#[derive(Clone, Default)]
pub(crate) struct RemoteShells {
    shells: Arc<
        std::sync::RwLock<std::collections::HashMap<String, crate::transport::vocab::RemoteShell>>,
    >,
    probe_locks:
        Arc<std::sync::Mutex<std::collections::HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
}

impl RemoteShells {
    pub(crate) fn get(&self, machine: &str) -> Option<crate::transport::vocab::RemoteShell> {
        self.shells
            .read()
            .expect("remote shell lock")
            .get(machine)
            .copied()
    }

    pub(crate) fn record(&self, machine: &str, shell: crate::transport::vocab::RemoteShell) {
        self.shells
            .write()
            .expect("remote shell lock")
            .insert(machine.to_string(), shell);
    }

    fn probe_lock(&self, machine: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.probe_locks
            .lock()
            .expect("remote shell probe lock")
            .entry(machine.to_string())
            .or_default()
            .clone()
    }
}

/// A failed command's outcome. Only a real non-zero exit carries stderr (and can
/// be classified benign); a missing binary or a connection failure surfaces as
/// [`RunError::Other`] (never benign).
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    /// A real process exit: carries stderr and the exit code. `126/127/255` are
    /// never a healthy-but-empty mux.
    #[error("{stderr}\ncommand exited with status {code}")]
    Exit { stderr: String, code: i32 },
    /// A spawn/transport failure (missing binary, connect failure) - never benign.
    #[error("{0}")]
    Other(String),
}

/// The text of a failure with the exit line [`RunError::Exit`] appends taken off its end,
/// leaving what the command itself wrote.
pub fn without_exit_line(text: &str) -> &str {
    let trimmed = text.trim_end();
    match trimmed.rsplit_once('\n') {
        Some((head, last)) if last.starts_with("command exited with status ") => head,
        None if trimmed.starts_with("command exited with status ") => "",
        _ => trimmed,
    }
}

/// Runs an external command and returns its stdout. A trait so the source layer
/// is testable without spawning processes.
#[async_trait]
pub trait Runner: Send + Sync {
    async fn run(&self, name: &str, args: &[String]) -> Result<Vec<u8>, RunError>;
    fn run_spec<'a>(
        &'a self,
        command: &'a CommandSpec,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<u8>, RunError>> + Send + 'a>>;
}

#[cfg(test)]
macro_rules! runner_spec_via_argv {
    () => {
        fn run_spec<'a>(
            &'a self,
            command: &'a $crate::transport::CommandSpec,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<Vec<u8>, $crate::model::source::RunError>>
                    + Send
                    + 'a,
            >,
        > {
            Box::pin(async move { self.run(command.program(), command.args()).await })
        }
    };
}
#[cfg(test)]
pub(crate) use runner_spec_via_argv;

/// The real runner: spawns the command via tokio, stripping mux env so a local
/// command run from inside a mux is not refused as nesting.
pub struct ExecRunner;

#[async_trait]
impl Runner for ExecRunner {
    async fn run(&self, name: &str, args: &[String]) -> Result<Vec<u8>, RunError> {
        self.run_spec(&CommandSpec::new(name, args.to_vec())).await
    }

    fn run_spec<'a>(
        &'a self,
        command: &'a CommandSpec,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<u8>, RunError>> + Send + 'a>>
    {
        Box::pin(async move { self.run_spec_output(command).await.map(|(out, _)| out) })
    }
}

impl ExecRunner {
    /// Runs a probe with both streams available, so SSH's authentication report can be
    /// read without putting diagnostic text into the remote command's stdout.
    pub async fn run_spec_output(
        &self,
        command: &CommandSpec,
    ) -> Result<(Vec<u8>, String), RunError> {
        let name = command.program();
        let args = command.args();
        let mut cmd = tokio::process::Command::new(name);
        let auth_log = command
            .observe_auth()
            .then(crate::transport::auth_log::AuthLog::new);
        if let Some(log) = &auth_log {
            cmd.args(log.args());
        }
        cmd.args(args);
        // Isolate stdin: these are non-interactive mux/ssh commands (list-sessions,
        // switch-client, …) that read no input. Without this, ssh inherits the parent
        // console tty and resets its mode (raw → canonical) for its own escape handling,
        // wrecking the app's raw mode until ssh exits - the terminal then echoes keys
        // and only flushes input on Enter.
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        cmd.kill_on_drop(true); // a cancelled (timed-out) scan kills the child
        cmd.env_clear();
        for (k, v) in std::env::vars() {
            if !crate::mux::vocab::is_mux_var(&k) {
                cmd.env(k, v);
            }
        }
        cmd.envs(command.env().iter().cloned());
        #[cfg(unix)]
        if command.should_detach_tty() {
            use std::os::unix::process::CommandExt as _;
            unsafe {
                cmd.as_std_mut().pre_exec(|| {
                    if libc::setsid() == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        let mut child = cmd.spawn().map_err(|e| RunError::Other(e.to_string()))?;
        let mut stdout = child.stdout.take().expect("spawn with piped stdout");
        let mut stderr = child.stderr.take().expect("spawn with piped stderr");

        // Both pipes are drained WHILE the child runs, not after its exit: a command
        // whose output exceeds the OS pipe capacity (65,536 bytes on Linux) blocks on
        // its next write and never exits, so a wait-then-read order would hold every
        // such command until the budget kills it and lose its output. join! polls both
        // drains and the exit wait together, so completion does not depend on the
        // output size.
        //
        // The command applies its OWN budget here so a timeout can tear the child down
        // cleanly: kill, reap, then drain both pipes to EOF. Draining to EOF means no
        // read is pending when the handles drop; on Windows an in-flight read at
        // handle-close crashes as "IO is still pending on closed socket" (0xC0000005,
        // the enumeration_failed in issue #116). The sweep-level budget
        // (within_poll_budget) is one second longer so this teardown always wins.
        let mut out = Vec::new();
        let mut err = Vec::new();
        let outcome = tokio::time::timeout(crate::mux::POLL_CMD_TIMEOUT, async {
            let (_, _, status) = tokio::join!(
                stdout.read_to_end(&mut out),
                stderr.read_to_end(&mut err),
                child.wait(),
            );
            status
        })
        .await;
        let status = match outcome {
            Err(_) => {
                let _ = child.start_kill();
                let _ = child.wait().await;
                // Drain to EOF so the pipes close with no pending read.
                let _ = stdout.read_to_end(&mut out).await;
                let _ = stderr.read_to_end(&mut err).await;
                return Err(RunError::Other(format!(
                    "timed out\n{name} did not answer within {}s",
                    crate::mux::POLL_CMD_TIMEOUT.as_secs()
                )));
            }
            Ok(status) => status.map_err(|e| RunError::Other(e.to_string()))?,
        };
        if let Some(log) = auth_log {
            let diagnostics = tokio::task::spawn_blocking(move || log.read())
                .await
                .unwrap_or_default();
            err.extend_from_slice(diagnostics.as_bytes());
        }
        if status.success() {
            Ok((out, String::from_utf8_lossy(&err).into_owned()))
        } else {
            let code = status.code().unwrap_or(-1);
            let raw = String::from_utf8_lossy(&err).into_owned();
            let raw = match command.auth_unavailable() {
                Some(reason) => {
                    format!("xmux credential broker unavailable: {reason}\n{raw}")
                }
                None => raw,
            };
            let stderr =
                crate::transport::diagnostic::explain(&raw, command.password_was_supplied());
            command.forget_refused_password(code, &stderr);
            Err(RunError::Exit {
                // Trim the trailing newline the command's stderr carries, so the
                // error reads as one line wherever it is rendered.
                stderr,
                code,
            })
        }
    }
}

/// One mux server. Remote sources run their mux over ssh.
#[derive(Clone)]
pub struct Source {
    /// `"local"` or an ssh-config alias.
    pub alias: String,
    /// mux binary name on that machine.
    pub binary: String,
    /// Which machine kind (and its construction data - socket / ssh alias, control
    /// path, os) this source reaches its mux over. The single representation of transport
    /// kind; `transport()` maps it to a concrete `Transport` at one site.
    pub kind: MachineKind,
    /// injectable; `None` ⇒ the real exec runner.
    pub runner: Option<Arc<dyn Runner>>,
    pub(crate) remote_shells: RemoteShells,
    pub(crate) credentials: crate::transport::auth::Credentials,
}

impl Source {
    pub(crate) fn run_with(&self) -> &dyn Runner {
        match &self.runner {
            Some(r) => r.as_ref(),
            None => &ExecRunner,
        }
    }

    /// Assembles a value [`Host`](crate::model::Host) from this source's config -
    /// transport from [`kind`](Self::kind) at the single `MachineKind::transport` site,
    /// mux from [`binary`](Self::binary) - for the off-loop `Ops`/CLI paths that cannot
    /// borrow the event loop's live `&mut Host`. The runner stays with the source
    /// (`run_with`), injected into the host's enumerate/manage/attach calls.
    pub(crate) fn host(&self) -> crate::model::Host {
        let mut transport = self.kind.clone().transport();
        if let Some(shell) = self
            .remote_shells
            .get(crate::session::machine_of(&self.alias))
        {
            transport.set_remote_shell(shell);
        }
        transport.set_credentials(self.credentials.clone());
        crate::model::Host::new(
            transport,
            crate::mux::for_binary(&self.binary).expect("a source's binary is a registry name"),
        )
    }

    /// [`host`](Self::host) for an operation about to run a command: a remote machine
    /// with no recorded shell family is probed first, so the command is composed for
    /// the shell that will read it. Called only from an operation something asked for,
    /// so the probe adds one round trip to that request and never runs on its own.
    pub(crate) async fn host_for_op(&self) -> Result<crate::model::Host, RunError> {
        let machine = crate::session::machine_of(&self.alias);
        let mut host = self.host();
        if host.transport.is_remote() && self.remote_shells.get(machine).is_none() {
            let probe_lock = self.remote_shells.probe_lock(machine);
            let _probe = probe_lock.lock().await;
            match self.remote_shells.get(machine) {
                Some(shell) => host.transport.set_remote_shell(shell),
                None => {
                    if let Some(argv) = host
                        .transport
                        .raw_shell_argv(crate::transport::vocab::SHELL_PROBE)
                    {
                        let out = self.run_with().run_spec(&argv).await?;
                        let shell = crate::transport::vocab::RemoteShell::from_probe(&out);
                        self.remote_shells.record(machine, shell);
                        host.transport.set_remote_shell(shell);
                    }
                }
            }
        }
        Ok(host)
    }
}

// The reachable-but-empty classification lives in `mux/`. The app reaches its
// `%exit`/`%error`-reason check through `crate::model::source::reason_is_no_sessions`, so the
// name is re-exported here to keep that path resolving.
pub(crate) use crate::mux::reason_is_no_sessions;

/// Assembles the source list for a config: local first, then each ssh host
/// (ssh-config aliases merged with config overrides) in order, then each WSL
/// distribution. WSL comes last so adding the implementation leaves every id an existing
/// install already had in the position it had.
///
/// `local_muxes` is the RESOLVED local mux list (`Env` resolves it once, discovering
/// what this machine has when the config says `auto`), passed in rather than re-derived so
/// the source ids here and the host ids in `Hosts::build` cannot disagree.
pub fn build(
    cfg: &Config,
    ssh_aliases: &[String],
    wsl_distros: &[String],
    os: &str,
    local_muxes: &[String],
    xmux_dir: &Path,
    local_socket: Option<String>,
) -> Vec<Source> {
    // One source per (machine, mux): this machine contributes one for each mux it serves.
    let qualified = local_muxes.len() > 1;
    let mut srcs: Vec<Source> = local_muxes
        .iter()
        .map(|bin| {
            let id = session::source_id(session::LOCAL_SOURCE, bin, qualified);
            for_machine_mux(
                session::LOCAL_SOURCE,
                bin,
                id,
                os,
                xmux_dir,
                local_socket.clone(),
            )
        })
        .collect();
    for spec in cfg
        .host_specs(ssh_aliases)
        .into_iter()
        .chain(cfg.wsl_specs(wsl_distros))
    {
        srcs.push(for_machine_mux(
            &spec.alias,
            &spec.bin,
            spec.id,
            os,
            xmux_dir,
            None,
        ));
    }
    srcs
}

/// One [`Source`] for the mux binary `bin` on `machine`, answering as the source `id`.
/// The machine half comes from [`crate::transport::kind_for`], so this source and the
/// `Host` the loop drives for the same pair reach the machine the same way. A source
/// DISCOVERED after launch is built here too, which is what makes it as operable as a
/// configured one (create / panes / border styles all resolve through the source list).
///
/// The socket is filtered by what `bin` accepts before the machine is given it: this is
/// one of the two sites where the mux is known alongside the machine, and the machine
/// axis names no mux, so the choice can only be made here. Both sites filter the same
/// raw value the same way, which is what keeps a source and its `Host` on one server.
pub fn for_machine_mux(
    machine: &str,
    bin: &str,
    id: String,
    os: &str,
    xmux_dir: &Path,
    local_socket: Option<String>,
) -> Source {
    let local_socket = crate::mux::server_socket_for(bin, local_socket);
    Source {
        alias: id.clone(),
        binary: bin.to_string(),
        kind: crate::transport::kind_for(machine, id, os, xmux_dir, local_socket),
        runner: None,
        remote_shells: RemoteShells::default(),
        credentials: crate::transport::auth::Credentials::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_puts_local_first() {
        // `db` writes no mux, so it has no source until it answers which it serves.
        let cfg = Config {
            hosts: vec![crate::provision::config::HostConfig {
                ssh: "prod".into(),
                mux: "tmux".into(),
            }],
            ..Config::default()
        };
        let aliases: Vec<String> = ["prod", "db"].iter().map(|s| s.to_string()).collect();
        let srcs = build(
            &cfg,
            &aliases,
            &[],
            "linux",
            &["tmux".to_string()],
            Path::new("/home/u/.xmux"),
            None,
        );
        assert_eq!(srcs.len(), 2);
        assert_eq!(srcs[0].alias, "local");
        assert!(matches!(srcs[0].kind, MachineKind::Local { .. }));
        assert_eq!(srcs[1].alias, "prod");
        assert!(matches!(srcs[1].kind, MachineKind::Ssh { .. }));
        assert_eq!(srcs[1].binary, "tmux");
    }
    #[test]
    fn a_local_zellij_source_is_built_without_the_tmux_socket() {
        // The socket comes from `$TMUX`, which is set whenever xmux runs inside a mux -
        // the normal case. Handing it to zellij made every local zellij source fail its
        // listing on argument parsing, so it never reaches the machine at all.
        let dir = Path::new("/tmp/xmux");
        let sock = Some("/tmp/psmux-1/default".to_string());
        let z = for_machine_mux(
            "local",
            "zellij",
            "local:zellij".into(),
            "linux",
            dir,
            sock.clone(),
        );
        assert_eq!(z.kind.local_socket(), None, "no socket reaches zellij");

        // The tmux implementation still targets the server it was told to.
        let p = for_machine_mux(
            "local",
            "psmux",
            "local:psmux".into(),
            "linux",
            dir,
            sock.clone(),
        );
        assert_eq!(p.kind.local_socket(), sock);
    }

    /// The echo command for the host platform (test-only): `cmd /C echo` on Windows,
    /// `sh -c` elsewhere - keeps the runner test portable.
    fn echo_cmd(text: &str) -> (String, Vec<String>) {
        #[cfg(windows)]
        {
            ("cmd".into(), vec!["/C".into(), "echo".into(), text.into()])
        }
        #[cfg(not(windows))]
        {
            ("sh".into(), vec!["-c".into(), format!("echo {text}")])
        }
    }

    #[tokio::test]
    async fn exec_runner_captures_stdout() {
        let (name, args) = echo_cmd("hello-xmux");
        let out = ExecRunner
            .run(&name, &args)
            .await
            .unwrap_or_else(|e| panic!("echo failed: {e:?}"));
        assert!(
            String::from_utf8_lossy(&out).contains("hello-xmux"),
            "stdout captured, got {:?}",
            String::from_utf8_lossy(&out)
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn ssh_auth_diagnostics_do_not_hold_probe_or_login_pipes_open() {
        use std::os::unix::fs::PermissionsExt;
        let script = crate::transport::auth_log::AuthLog::new();
        std::fs::write(
            script.path(),
            r#"#!/bin/sh
if [ "$1" = '-v' ] && [ "$2" = '-E' ]; then
  printf 'debug1: Authenticated to box using "publickey".\n' > "$3"
  sleep 3 >/dev/null 2>&1 &
else
  printf 'debug1: Authenticated to box using "publickey".\n' >&2
  sleep 3 >/dev/null &
fi
echo probe-ok
"#,
        )
        .unwrap();
        std::fs::set_permissions(script.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let command = CommandSpec::new(script.path().to_string_lossy().into_owned(), Vec::new())
            .with_auth_observation();
        let (out, diagnostics) = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            ExecRunner.run_spec_output(&command),
        )
        .await
        .expect("probe pipes must close while the shared master remains alive")
        .unwrap();
        assert!(String::from_utf8_lossy(&out).contains("probe-ok"));
        assert_eq!(
            crate::model::AuthMethod::from_ssh_stderr(&diagnostics),
            Some(crate::model::AuthMethod::PublicKey)
        );
        let (_, done) = crate::link::unlock::start_login(
            "box".into(),
            command,
            std::time::Duration::from_secs(2),
        );
        let login = tokio::time::timeout(std::time::Duration::from_secs(2), done)
            .await
            .expect("login pipes must close while the shared master remains alive")
            .unwrap();
        assert!(login.outcome.is_ok(), "{:?}", login.outcome);
        assert_eq!(login.auth_method, Some(crate::model::AuthMethod::PublicKey));
    }

    #[tokio::test]
    async fn exec_runner_trims_the_trailing_newline_from_stderr() {
        // A failing command's captured stderr ends in a newline ("... not found\n");
        // the error must not carry it, or every render of the message wraps the tail
        // onto its own line (`xmux ls` showed the closing paren alone on a line).
        #[cfg(windows)]
        let (name, args) = (
            "cmd",
            vec!["/C".to_string(), "echo boom 1>&2 & exit 1".to_string()],
        );
        #[cfg(not(windows))]
        let (name, args) = (
            "sh",
            vec!["-c".to_string(), "echo boom >&2; exit 1".to_string()],
        );
        let err = ExecRunner.run(name, &args).await.expect_err("must fail");
        let RunError::Exit { stderr, .. } = &err else {
            panic!("expected an exit error, got {err:?}");
        };
        assert_eq!(stderr, "boom", "trailing newline trimmed, got {stderr:?}");
    }

    #[tokio::test]
    async fn exec_runner_reports_when_the_password_broker_is_unavailable() {
        #[cfg(windows)]
        let command = CommandSpec::new(
            "cmd",
            vec![
                "/C".into(),
                "echo dev@host: Permission denied (publickey,password). 1>&2 & exit 255".into(),
            ],
        );
        #[cfg(not(windows))]
        let command = CommandSpec::new(
            "sh",
            vec![
                "-c".into(),
                "echo 'dev@host: Permission denied (publickey,password).' >&2; exit 255".into(),
            ],
        );
        let command = command.with_auth_unavailable(Some("broken pipe".into()));
        let err = ExecRunner
            .run_spec(&command)
            .await
            .expect_err("must fail without the broker");
        assert!(
            err.to_string()
                .starts_with("xmux could not provide the held password"),
            "{err}"
        );
    }

    /// A command whose stdout exceeds the OS pipe capacity (65,536 bytes on
    /// Linux). The child blocks on its next write once the pipe fills and never
    /// exits, so a runner that waits for the exit before reading holds the call
    /// until the 6s budget kills it and returns a timeout error instead of the
    /// output.
    #[tokio::test]
    async fn exec_runner_returns_stdout_larger_than_the_pipe_capacity() {
        let big = BigOutput::new("stdout");
        let (name, args) = big.stdout_command();
        let out = ExecRunner
            .run(&name, &args)
            .await
            .unwrap_or_else(|e| panic!("large stdout must succeed: {e:?}"));
        assert_eq!(out.len(), BigOutput::BYTES);
    }

    /// The same overflow on the stderr pipe: a command whose stderr exceeds the
    /// pipe capacity must still surface as a real exit error carrying the whole
    /// stderr, not as a budget timeout. Diagnostics are bounded before display.
    #[tokio::test]
    async fn exec_runner_returns_stderr_larger_than_the_pipe_capacity() {
        let big = BigOutput::new("stderr");
        let (name, args) = big.stderr_command();
        let err = ExecRunner.run(&name, &args).await.expect_err("must fail");
        let RunError::Exit { stderr, code } = &err else {
            panic!("expected an exit error, got {err:?}");
        };
        assert_eq!(*code, 1);
        assert_eq!(stderr.len(), crate::transport::diagnostic::MAX_DIAGNOSTIC);
    }

    /// A file of known size, and the command that copies it to one of the two pipes.
    ///
    /// The bytes come from a FILE rather than from a program that generates them,
    /// because these two tests race the runner's own six-second budget. The
    /// interpreter that generated them takes most of a second to start on an idle
    /// machine and, on a loaded CI runner, longer than the budget allows, which
    /// failed the test for the one reason it is not about. Copying a file needs only
    /// the shell each platform already has.
    struct BigOutput {
        path: std::path::PathBuf,
    }

    impl BigOutput {
        const BYTES: usize = 200_000;

        fn new(tag: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("xmux-pipe-{tag}-{}.bin", std::process::id()));
            std::fs::write(&path, vec![b'a'; Self::BYTES]).expect("write the payload");
            Self { path }
        }

        fn display(&self) -> String {
            self.path.display().to_string()
        }

        #[cfg(windows)]
        fn stdout_command(&self) -> (String, Vec<String>) {
            (
                "cmd".into(),
                vec!["/c".into(), "type".into(), self.display()],
            )
        }

        #[cfg(not(windows))]
        fn stdout_command(&self) -> (String, Vec<String>) {
            ("cat".into(), vec![self.display()])
        }

        #[cfg(windows)]
        fn stderr_command(&self) -> (String, Vec<String>) {
            // `type` writes to stdout, so the redirection and the exit code have to
            // live somewhere. They go in a SCRIPT rather than in the command line:
            // Rust escapes a quote inside an argument as `\"`, which cmd does not
            // read as a quote, so a command line carrying a quoted path arrives
            // mangled and `type` reports a file it cannot find. Handing cmd a script
            // path is one plain argument, and the quoting inside the script is this
            // test's own. `%~dp0` names the script's directory, so the payload is
            // found whatever the temp directory is called.
            let script = self.path.with_extension("cmd");
            std::fs::write(
                &script,
                format!(
                    "@echo off\r\ntype \"%~dp0{}\" 1>&2\r\nexit /b 1\r\n",
                    self.path
                        .file_name()
                        .expect("the payload has a file name")
                        .to_string_lossy()
                ),
            )
            .expect("write the emitter");
            (
                "cmd".into(),
                vec!["/c".into(), script.display().to_string()],
            )
        }

        #[cfg(not(windows))]
        fn stderr_command(&self) -> (String, Vec<String>) {
            // The path arrives as a positional argument rather than inside the
            // snippet, so the shell never parses it.
            (
                "sh".into(),
                vec![
                    "-c".into(),
                    "cat \"$1\" >&2; exit 1".into(),
                    "sh".into(),
                    self.display(),
                ],
            )
        }
    }

    impl Drop for BigOutput {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
            // The Windows emitter sits beside the payload; removing it on every
            // platform costs one failed call and keeps this to one rule.
            let _ = std::fs::remove_file(self.path.with_extension("cmd"));
        }
    }

    // LIVE: the timeout path runs a real hung command for the full POLL_CMD_TIMEOUT
    // (6s), so it is ignored and run on demand:
    //   cargo test --lib model::source::tests::exec_runner_times_out_and_kills -- --ignored
    // It asserts the command's own budget returns a timeout error AND that the child is
    // reaped (the process is gone) rather than left behind - the teardown that on
    // Windows avoids the "IO is still pending on closed socket" crash (#116).
    #[ignore = "live: sleeps for the full 6s command budget"]
    #[tokio::test]
    async fn exec_runner_times_out_and_kills() {
        // A command that outlives the budget and runs as a SINGLE process: a shell
        // wrapper (`sh -c sleep 30`) forks a grandchild that inherits the pipe
        // write ends, so the post-kill drain would wait out the grandchild instead
        // of returning at EOF. `sleep` execs directly on Unix; PowerShell's
        // Start-Sleep is an in-process cmdlet on Windows.
        #[cfg(windows)]
        let (name, args) = (
            "powershell",
            vec![
                "-NoProfile".to_string(),
                "-Command".to_string(),
                "Start-Sleep -Seconds 30".to_string(),
            ],
        );
        #[cfg(not(windows))]
        let (name, args) = ("sleep", vec!["30".to_string()]);
        let t0 = std::time::Instant::now();
        let err = ExecRunner
            .run(name, &args)
            .await
            .expect_err("must time out");
        assert!(
            err.to_string().contains("did not answer"),
            "timeout names the hung command, got {err:?}"
        );
        // The budget is the 6s command budget (plus scheduling slack), NOT the full 30s
        // hang - proof the child was killed and not left running.
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(20),
            "child was killed and reaped, took {:?}",
            t0.elapsed()
        );
    }
}
