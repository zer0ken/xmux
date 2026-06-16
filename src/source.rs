//! Abstracts a mux server reachable from this machine: the local mux, or a
//! remote one over ssh. It owns the machine boundary — argv assembly, ssh
//! transport with connect-timeout and injection-safe quoting, and the
//! reachable-but-empty vs unreachable distinction — so the layers above speak in
//! sessions, not transports.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;

use crate::config::Config;
use crate::mux;
use crate::session::{self, Session};

/// Bounds the ssh TCP connect; the per-source scan timeout must exceed it so a
/// slow-but-alive remote is not cancelled mid-connect.
const CONNECT_TIMEOUT: &str = "5";

/// A failed command's outcome. Only a real non-zero exit carries stderr (and can
/// be classified benign); a missing binary or a connection failure surfaces as
/// [`RunError::Other`] (never benign).
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    /// A real process exit: carries stderr and the exit code. `126/127/255` are
    /// never a healthy-but-empty mux.
    #[error("command failed (exit {code}): {stderr}")]
    Exit { stderr: String, code: i32 },
    /// A spawn/transport failure (missing binary, connect failure) — never benign.
    #[error("{0}")]
    Other(String),
}

/// Runs an external command and returns its stdout. A trait so the source layer
/// is testable without spawning processes.
#[async_trait]
pub trait Runner: Send + Sync {
    async fn run(&self, name: &str, args: &[String]) -> Result<Vec<u8>, RunError>;
}

/// The real runner: spawns the command via tokio, stripping mux env so a local
/// command run from inside a mux (e.g. the popup) is not refused as nesting.
pub struct ExecRunner;

#[async_trait]
impl Runner for ExecRunner {
    async fn run(&self, name: &str, args: &[String]) -> Result<Vec<u8>, RunError> {
        let mut cmd = tokio::process::Command::new(name);
        cmd.args(args);
        cmd.kill_on_drop(true); // a cancelled (timed-out) scan kills the child
        cmd.env_clear();
        for (k, v) in std::env::vars() {
            if !is_mux_var(&k) {
                cmd.env(k, v);
            }
        }
        let output = cmd
            .output()
            .await
            .map_err(|e| RunError::Other(e.to_string()))?;
        if output.status.success() {
            Ok(output.stdout)
        } else {
            Err(RunError::Exit {
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                code: output.status.code().unwrap_or(-1),
            })
        }
    }
}

fn is_mux_var(key: &str) -> bool {
    key.starts_with("TMUX") || key.starts_with("PSMUX")
}

/// Returns env entries (`K=V`) with every `TMUX*`/`PSMUX*` variable removed.
pub fn mux_clean_env(env: &[String]) -> Vec<String> {
    env.iter()
        .filter(|e| !(e.starts_with("TMUX") || e.starts_with("PSMUX")))
        .cloned()
        .collect()
}

/// One mux server. Remote sources run their mux over ssh.
#[derive(Clone)]
pub struct Source {
    /// `"local"` or an ssh-config alias.
    pub alias: String,
    /// mux binary name on that machine.
    pub binary: String,
    pub remote: bool,
    /// ssh ControlMaster socket (non-windows remotes).
    pub control_path: String,
    /// platform of the machine running xmux (gates ControlMaster).
    pub os: String,
    /// injectable; `None` ⇒ the real exec runner.
    pub runner: Option<Arc<dyn Runner>>,
}

impl Source {
    /// Builds the ssh options preceding the remote command, ending with
    /// `-- <alias>` so an alias beginning with `-` is treated as the destination,
    /// never an option.
    pub fn ssh_args(&self, tty: bool) -> Vec<String> {
        let mut a: Vec<String> = Vec::new();
        if tty {
            a.push("-t".into()); // request a pty; omit BatchMode so auth can prompt
        } else {
            a.push("-o".into());
            a.push("BatchMode=yes".into()); // listing must never hang on a prompt
        }
        a.push("-o".into());
        a.push(format!("ConnectTimeout={CONNECT_TIMEOUT}"));
        if self.os != "windows" && !self.control_path.is_empty() {
            // Windows OpenSSH lacks ControlMaster; only multiplex elsewhere.
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

    /// Turns a full mux argv (`argv[0]` = the mux binary) into the executable
    /// name and args to run: local runs the mux directly; remote wraps it in ssh.
    pub fn exec_argv(&self, tty: bool, mux_argv: &[String]) -> (String, Vec<String>) {
        if !self.remote {
            return (mux_argv[0].clone(), mux_argv[1..].to_vec());
        }
        let mut args = self.ssh_args(tty);
        args.push(remote_command(mux_argv));
        ("ssh".into(), args)
    }

    /// The argv that hands the terminal over to attach this source's named
    /// session (over `ssh -t` for a remote).
    pub fn attach_command(&self, name: &str) -> Vec<String> {
        let (n, a) = self.exec_argv(true, &mux::attach(&self.binary, name));
        let mut v = vec![n];
        v.extend(a);
        v
    }

    fn run_with(&self) -> &dyn Runner {
        match &self.runner {
            Some(r) => r.as_ref(),
            None => &ExecRunner,
        }
    }

    /// Executes a non-interactive mux command and returns its stdout.
    pub async fn run(&self, mux_argv: &[String]) -> Result<Vec<u8>, RunError> {
        let (name, args) = self.exec_argv(false, mux_argv);
        self.run_with().run(&name, &args).await
    }

    /// Returns the source's sessions. A reachable mux with no sessions returns an
    /// empty vec; an unreachable source returns an error.
    pub async fn list_sessions(&self) -> Result<Vec<Session>, RunError> {
        match self.run(&mux::list_sessions(&self.binary)).await {
            Ok(out) => Ok(mux::parse_sessions(
                &self.alias,
                &String::from_utf8_lossy(&out),
            )),
            Err(e) => {
                if is_no_sessions(&e) {
                    Ok(Vec::new())
                } else {
                    Err(e)
                }
            }
        }
    }
}

/// Reports whether `err` means "the mux is reachable but has no sessions" rather
/// than "the host is unreachable". tmux exits non-zero with a "no server
/// running" message when idle, so this distinguishes an empty-but-alive mux from
/// a dead one. Only a real command exit (carrying stderr) can be benign; a
/// missing binary or a connect failure is always unreachable.
pub fn is_no_sessions(err: &RunError) -> bool {
    let RunError::Exit { stderr, code } = err else {
        return false;
    };
    // command-not-found (127), not-executable (126), and ssh failure (255) are
    // never a healthy-but-empty mux — a broken host must not be hidden as empty.
    if matches!(code, 126 | 127 | 255) {
        return false;
    }
    // Match the marker as a line PREFIX, not anywhere — so a login banner or MOTD
    // line like "you have no sessions pending" cannot masquerade as the idle mux.
    let lower = stderr.to_lowercase();
    for line in lower.split('\n') {
        let line = line.trim();
        if line.starts_with("no server running") || line.starts_with("no sessions") {
            return true;
        }
    }
    false
}

/// Renders one argument safe for a POSIX shell. A string of only safe characters
/// passes through; anything else is single-quoted with embedded single-quotes
/// escaped as `'\''`. This is the SOLE point an untrusted value (a session name
/// from a remote list-sessions) enters a remote shell command.
pub fn quote(s: &str) -> String {
    if s.is_empty() {
        return "''".into();
    }
    if is_shell_safe(s) {
        return s.into();
    }
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn is_shell_safe(s: &str) -> bool {
    s.chars()
        .all(|r| r.is_ascii_alphanumeric() || matches!(r, '-' | '_' | '.' | '/'))
}

/// Joins a mux argv into a single shell command line, quoting each element, for
/// execution by the remote shell ssh hands it to.
pub fn remote_command(argv: &[String]) -> String {
    argv.iter().map(|a| quote(a)).collect::<Vec<_>>().join(" ")
}

/// Assembles the source list for a config: local first, then each ssh host
/// (ssh-config aliases merged with config overrides) in order.
pub fn build(cfg: &Config, ssh_aliases: &[String], os: &str, xmux_dir: &Path) -> Vec<Source> {
    let mut srcs = vec![Source {
        alias: session::LOCAL_SOURCE.to_string(),
        binary: cfg.local_bin(os),
        remote: false,
        control_path: String::new(),
        os: os.to_string(),
        runner: None,
    }];
    for spec in cfg.host_specs(ssh_aliases) {
        let control_path = xmux_dir
            .join(format!("cm-{}.sock", spec.alias))
            .to_string_lossy()
            .into_owned();
        srcs.push(Source {
            alias: spec.alias,
            binary: spec.bin,
            remote: true,
            control_path,
            os: os.to_string(),
            runner: None,
        });
    }
    srcs
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records the last command and returns a canned result.
    struct FakeRunner {
        result: std::sync::Mutex<Option<Result<Vec<u8>, RunError>>>,
    }

    impl FakeRunner {
        fn ok(out: &str) -> Arc<dyn Runner> {
            Arc::new(FakeRunner {
                result: std::sync::Mutex::new(Some(Ok(out.as_bytes().to_vec()))),
            })
        }
        fn err(e: RunError) -> Arc<dyn Runner> {
            Arc::new(FakeRunner {
                result: std::sync::Mutex::new(Some(Err(e))),
            })
        }
    }

    #[async_trait]
    impl Runner for FakeRunner {
        async fn run(&self, _name: &str, _args: &[String]) -> Result<Vec<u8>, RunError> {
            self.result
                .lock()
                .unwrap()
                .take()
                .unwrap_or_else(|| Ok(Vec::new()))
        }
    }

    fn src(alias: &str, binary: &str, remote: bool, os: &str, control_path: &str) -> Source {
        Source {
            alias: alias.into(),
            binary: binary.into(),
            remote,
            control_path: control_path.into(),
            os: os.into(),
            runner: None,
        }
    }

    #[test]
    fn quote_neutralizes_shell_metachars() {
        let cases: &[(&str, &str)] = &[
            ("plain", "plain"),
            ("with space", "'with space'"),
            ("", "''"),
            ("a/b-c_d.e", "a/b-c_d.e"),
            ("$(rm -rf /)", "'$(rm -rf /)'"),
            ("a';rm -rf /;'b", r"'a'\'';rm -rf /;'\''b'"),
            ("`whoami`", "'`whoami`'"),
        ];
        for &(input, want) in cases {
            assert_eq!(quote(input), want, "quote({input:?})");
        }
    }

    #[test]
    fn remote_command_joins_quoted() {
        let argv: Vec<String> = ["tmux", "rename-session", "-t", "old", "evil; rm -rf /"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            remote_command(&argv),
            "tmux rename-session -t old 'evil; rm -rf /'"
        );
    }

    #[test]
    fn ssh_args_non_interactive() {
        let s = src("prod", "tmux", true, "linux", "/tmp/cm.sock");
        let a = s.ssh_args(false);
        let joined = a.join(" ");
        assert!(joined.contains("BatchMode=yes"), "{a:?}");
        assert!(joined.contains("ConnectTimeout=5"), "{a:?}");
        assert!(joined.contains("ControlMaster=auto"), "{a:?}");
        assert_eq!(a[a.len() - 2], "--");
        assert_eq!(a[a.len() - 1], "prod");
    }

    #[test]
    fn ssh_args_interactive_requests_tty() {
        let s = src("prod", "tmux", true, "linux", "");
        let a = s.ssh_args(true);
        let joined = a.join(" ");
        assert!(joined.contains("-t"), "{a:?}");
        assert!(!joined.contains("BatchMode"), "{a:?}");
    }

    #[test]
    fn ssh_args_windows_omits_control_master() {
        let s = src("prod", "tmux", true, "windows", "/tmp/cm.sock");
        let a = s.ssh_args(false);
        assert!(!a.join(" ").contains("ControlMaster"), "{a:?}");
    }

    #[test]
    fn exec_argv_local() {
        let s = src("local", "psmux", false, "", "");
        let argv: Vec<String> = ["psmux", "list-sessions", "-F", "x"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (name, args) = s.exec_argv(false, &argv);
        assert_eq!(name, "psmux");
        assert_eq!(args, vec!["list-sessions", "-F", "x"]);
    }

    #[test]
    fn exec_argv_remote() {
        let s = src("prod", "tmux", true, "linux", "");
        let argv: Vec<String> = ["tmux", "kill-session", "-t", "x"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (name, args) = s.exec_argv(false, &argv);
        assert_eq!(name, "ssh");
        assert_eq!(args.last().unwrap(), "tmux kill-session -t x");
    }

    #[test]
    fn attach_command_local_and_remote() {
        let loc = src("local", "psmux", false, "", "");
        assert_eq!(
            loc.attach_command("dev"),
            vec!["psmux", "attach", "-t", "dev"]
        );
        let rem = src("prod", "tmux", true, "linux", "");
        let got = rem.attach_command("api");
        assert_eq!(got[0], "ssh");
        assert_eq!(got.last().unwrap(), "tmux attach -t api");
        assert!(got.iter().any(|s| s == "-t"), "{got:?}");
    }

    #[tokio::test]
    async fn list_sessions_parses_output() {
        let mut s = src("local", "psmux", false, "", "");
        s.runner = Some(FakeRunner::ok("3\t1\t1781246739\teditor\n1\t0\t\tbuild\n"));
        let got = s.list_sessions().await.unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].name, "editor");
        assert_eq!(got[0].windows, 3);
        assert!(got[0].attached);
        assert_eq!(got[0].source, "local");
        assert_eq!(got[1].last_attached, 0);
    }

    #[tokio::test]
    async fn list_sessions_benign_no_server_is_empty_not_error() {
        let mut s = src("prod", "tmux", true, "linux", "");
        s.runner = Some(FakeRunner::err(RunError::Exit {
            stderr: "no server running on /tmp/tmux-1000/default".into(),
            code: 1,
        }));
        let got = s.list_sessions().await.unwrap();
        assert!(got.is_empty());
    }

    #[tokio::test]
    async fn list_sessions_unreachable_is_error() {
        let mut s = src("prod", "tmux", true, "linux", "");
        s.runner = Some(FakeRunner::err(RunError::Other(
            "ssh: connect to host prod port 22: Connection timed out".into(),
        )));
        assert!(s.list_sessions().await.is_err());
    }

    #[test]
    fn is_no_sessions_classification() {
        assert!(is_no_sessions(&RunError::Exit {
            code: 1,
            stderr: "no server running on /tmp/tmux-1000/default".into(),
        }));
        assert!(is_no_sessions(&RunError::Exit {
            code: 1,
            stderr: "no sessions".into(),
        }));
        assert!(!is_no_sessions(&RunError::Exit {
            code: 1,
            stderr: "permission denied".into(),
        }));
        // A banner line merely CONTAINING the phrase must not misclassify.
        assert!(!is_no_sessions(&RunError::Exit {
            code: 1,
            stderr: "Last login...\nYou have no sessions pending.\n".into(),
        }));
        // command-not-found / ssh failure are never benign.
        assert!(!is_no_sessions(&RunError::Exit {
            code: 127,
            stderr: "tmux: command not found\nno sessions\n".into(),
        }));
        assert!(!is_no_sessions(&RunError::Exit {
            code: 255,
            stderr: "ssh: connect failed\n".into(),
        }));
        // A non-exit error (missing binary / connect failure) is NOT benign.
        assert!(!is_no_sessions(&RunError::Other(
            "exec: \"tmux\": executable file not found".into()
        )));
    }

    #[test]
    fn mux_clean_env_strips_mux_vars() {
        let input: Vec<String> = [
            "PATH=/bin",
            "TMUX=/x,1,0",
            "TMUX_PANE=%1",
            "PSMUX_SESSION=dev",
            "HOME=/h",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let out = mux_clean_env(&input);
        for e in &out {
            assert!(
                !e.starts_with("TMUX") && !e.starts_with("PSMUX"),
                "leaked {e:?}"
            );
        }
        assert!(out.iter().any(|e| e == "PATH=/bin"));
        assert!(out.iter().any(|e| e == "HOME=/h"));
    }

    #[test]
    fn build_puts_local_first() {
        let cfg = Config::default();
        let aliases: Vec<String> = ["prod", "db"].iter().map(|s| s.to_string()).collect();
        let srcs = build(&cfg, &aliases, "linux", Path::new("/home/u/.xmux"));
        assert_eq!(srcs.len(), 3);
        assert_eq!(srcs[0].alias, "local");
        assert!(!srcs[0].remote);
        assert_eq!(srcs[1].alias, "prod");
        assert!(srcs[1].remote);
        assert_eq!(srcs[1].binary, "tmux");
    }
}
