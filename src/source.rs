//! Abstracts a mux server reachable from this machine: the local mux, or a
//! remote one over ssh. It owns the machine boundary — argv assembly, ssh
//! transport with connect-timeout and injection-safe quoting, and the
//! reachable-but-empty vs unreachable distinction — so the layers above speak in
//! sessions, not transports.

use std::path::{Path, PathBuf};
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
/// command run from inside a mux is not refused as nesting.
pub struct ExecRunner;

#[async_trait]
impl Runner for ExecRunner {
    async fn run(&self, name: &str, args: &[String]) -> Result<Vec<u8>, RunError> {
        let mut cmd = tokio::process::Command::new(name);
        cmd.args(args);
        // Isolate stdin: these are non-interactive mux/ssh commands (list-sessions,
        // switch-client, …) that read no input. Without this, ssh inherits the parent
        // console tty and resets its mode (raw → canonical) for its own escape handling,
        // wrecking the cockpit's raw mode until ssh exits — the terminal then echoes keys
        // and only flushes input on Enter.
        cmd.stdin(std::process::Stdio::null());
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

pub fn is_mux_var(key: &str) -> bool {
    // Exactly tmux's session markers and any psmux var; NOT a blanket `TMUX`
    // prefix, which would also drop unrelated vars like `TMUX_TMPDIR` (selects
    // the socket dir) or `TMUXP_*` (the separate tmuxp tool).
    matches!(key, "TMUX" | "TMUX_PANE") || key.starts_with("PSMUX")
}

/// Returns env entries (`K=V`) with every mux session variable removed.
pub fn mux_clean_env(env: &[String]) -> Vec<String> {
    env.iter()
        .filter(|e| !is_mux_var(e.split('=').next().unwrap_or(e)))
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
    /// The local mux server socket to target (`-S`), parsed from `$TMUX` when
    /// xmux runs inside a mux (e.g. `tmux -L work`). `None` ⇒ the default
    /// socket. Only meaningful for the local source.
    pub socket: Option<String>,
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
            // Windows OpenSSH lacks ControlMaster, so remote probes can't reuse one
            // ssh connection there; only multiplex elsewhere. On Windows the
            // cockpit's scan cache (stale-while-revalidate) softens the cost by
            // showing last-known sessions at once instead of blocking on a fresh
            // handshake every picker open.
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
            let mut args: Vec<String> = Vec::new();
            if let Some(sock) = self.socket.as_deref().filter(|s| !s.is_empty()) {
                // Target the exact mux server the user is on (e.g. `tmux -L work`),
                // not just the default socket — so listing/select agree with the
                // teleport's switch-client (which inherits the same `$TMUX`).
                args.push("-S".into());
                args.push(sock.to_string());
            }
            args.extend_from_slice(&mux_argv[1..]);
            return (mux_argv[0].clone(), args);
        }
        let mut args = self.ssh_args(tty);
        args.push(remote_command(mux_argv));
        ("ssh".into(), args)
    }

    /// The argv that hands the terminal over to attach this source's named
    /// session (over `ssh -t` for a remote).
    ///
    /// `window` is the window index to land on. For a LOCAL source the caller
    /// pre-selects it with a separate, instant local command, so it is ignored
    /// here. For a REMOTE source the selection is folded into the SAME `ssh -t`
    /// connection (`select-window ; exec attach`) so there is no second
    /// connection that could hang on a stalled remote or be silently skipped by
    /// interactive auth, and the chosen window is preserved.
    pub fn attach_command(&self, name: &str, window: Option<i64>) -> Vec<String> {
        if !self.remote {
            let (n, a) = self.exec_argv(true, &mux::attach(&self.binary, name));
            let mut v = vec![n];
            v.extend(a);
            return v;
        }
        // Record THIS attach's own client tty to a per-host file before exec'ing the
        // attach, so a later switch-client (see `switch_client_remote_cmd`) targets this
        // exact display client — never the user's own attached client or the -CC client.
        let tty_cap = format!("tty >{} 2>/dev/null; ", quote(&self.client_tty_path()));
        let remote_cmd = match window {
            Some(w) => format!(
                "{tty_cap}{} ; exec {}",
                remote_command(&mux::select_window(
                    &self.binary,
                    &mux::window_target(name, w),
                )),
                remote_command(&mux::attach(&self.binary, name)),
            ),
            None => format!("{tty_cap}exec {}", remote_command(&mux::attach(&self.binary, name))),
        };
        let mut args = self.ssh_args(true);
        args.push(remote_cmd);
        let mut v = vec!["ssh".to_string()];
        v.extend(args);
        v
    }

    /// The argv that spawns this source's control-mode (`-CC`) child. Control mode
    /// is a text protocol over pipes, so the remote form uses no `ssh -t`
    /// (`ssh_args(false)` keeps `BatchMode=yes` and `ConnectTimeout`). The local
    /// form injects `-S <socket>` exactly as [`Source::exec_argv`] does when xmux
    /// targets a non-default mux server.
    pub fn control_argv(&self) -> Vec<String> {
        if !self.remote {
            let mut v = vec![self.binary.clone()];
            if let Some(sock) = self.socket.as_deref().filter(|s| !s.is_empty()) {
                v.push("-S".into());
                v.push(sock.to_string());
            }
            v.push("-CC".into());
            v.push("attach".into());
            return v;
        }
        // Force a remote pty (`-tt`): `tmux -CC attach` does a `tcgetattr` on its
        // stdin and exits immediately when stdin is not a tty, so a pipe-only ssh
        // (no `-t`) dies before emitting any control-mode output. xmux drives ssh's
        // stdin/stdout as pipes, so a plain `-t` would not allocate a pty either —
        // `-tt` forces one. BatchMode/ConnectTimeout still come from ssh_args(false).
        let mut args = vec!["-tt".to_string()];
        args.extend(self.ssh_args(false));
        args.push(remote_command(&[
            self.binary.clone(),
            "-CC".into(),
            "attach".into(),
        ]));
        let mut v = vec!["ssh".to_string()];
        v.extend(args);
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

    /// Runs a RAW remote shell command over ssh (the string is passed verbatim to
    /// the remote login shell, NOT quoted per-arg like [`Source::run`]) — for a
    /// command that itself needs shell features (`$(...)`, pipes). Remote-only; any
    /// untrusted value inside `remote_cmd` must already be [`quote`]d by the caller.
    pub async fn run_raw(&self, remote_cmd: &str) -> Result<Vec<u8>, RunError> {
        if !self.remote {
            return Ok(Vec::new());
        }
        let mut args = self.ssh_args(false);
        args.push(remote_cmd.to_string());
        self.run_with().run("ssh", &args).await
    }

    /// The remote shell command that switches THIS host's per-host PTY client to
    /// `session`. The per-host model keeps one `tmux attach` client per host; to move
    /// it between sessions we `switch-client -c <that client> -t <session>`. The
    /// client is identified as the one whose `client_flags` lack `control-mode` (the
    /// `-CC` metadata connection has it); a plain `tmux switch-client` would instead
    /// move the control client. Returns the snippet for [`Source::run_raw`].
    pub fn switch_client_remote_cmd(&self, session: &str) -> String {
        let b = &self.binary;
        let s = quote(session);
        let path = quote(&self.client_tty_path());
        // Read the tty xmux's display attach recorded for THIS host and switch only that
        // client. Guard on a non-empty value so a missing file never runs
        // `switch-client -c ""` (which would move the calling client instead).
        format!("c=$(cat {path} 2>/dev/null); [ -n \"$c\" ] && {b} switch-client -c \"$c\" -t {s}")
    }

    /// Per-host path on the REMOTE where xmux's display attach records its own client
    /// tty (`tty`), so `switch_client_remote_cmd` can target THAT client and no other.
    /// ponytail: one path per host alias — two xmux instances against the same remote
    /// account would share it; suffix with a per-instance id if that ever collides.
    fn client_tty_path(&self) -> String {
        format!("/tmp/.xmux-cli-{}", self.alias)
    }

    /// True for a LOCAL psmux source. psmux is one-server-per-session (no aggregate
    /// server): a bare `list-sessions` returns at most the one server it routed to,
    /// so its sessions must be enumerated from psmux's filesystem registry instead.
    pub fn is_local_psmux(&self) -> bool {
        !self.remote && self.binary == "psmux"
    }

    /// Enumerates the local psmux default-socket sessions. psmux is one-server-per-
    /// session over localhost TCP with no aggregate server: a bare `list-sessions`
    /// aggregates every live session WHEN its default route reaches a live server,
    /// but that route can land on a dead/stale server and return nothing even though
    /// sessions exist. So the filesystem registry (`~/.psmux/<name>.port`, the
    /// substrate psmux itself discovers with) is the authoritative EXISTENCE set, and
    /// a single `list-sessions` supplies the display detail; the two are merged so
    /// every live session surfaces with full detail when available.
    async fn list_sessions_psmux(&self) -> Result<Vec<Session>, RunError> {
        let names = read_psmux_registry_dir(&psmux_registry_dir());
        let detail = match self.run(&mux::list_sessions(&self.binary)).await {
            Ok(out) => mux::parse_sessions(&self.alias, &String::from_utf8_lossy(&out)),
            // The default route could not answer; the registry names still surface.
            Err(_) => Vec::new(),
        };
        Ok(merge_psmux_sessions(&self.alias, names, detail))
    }

    /// Returns the source's sessions. A reachable mux with no sessions returns an
    /// empty vec; an unreachable source returns an error.
    pub async fn list_sessions(&self) -> Result<Vec<Session>, RunError> {
        if self.is_local_psmux() {
            return self.list_sessions_psmux().await;
        }
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
    reason_is_no_sessions(stderr)
}

/// True when `text` (a mux error / exit reason) means "reachable but no server /
/// no sessions" rather than a real transport failure. The control-mode path gets a
/// plain string (the `%exit` / `%error` reason), not a [`RunError`], so it calls
/// this directly. Matches the marker as a line PREFIX so a login banner / MOTD line
/// like "you have no sessions pending" cannot masquerade as the idle mux.
pub fn reason_is_no_sessions(text: &str) -> bool {
    text.to_lowercase().split('\n').any(|line| {
        let line = line.trim();
        line.starts_with("no server running") || line.starts_with("no sessions")
    })
}

/// psmux's per-machine session registry directory (`~/.psmux`). Each live session
/// has a `<name>.port` file there (psmux is one-server-per-session over localhost
/// TCP, with this directory as its discovery substrate — there is no aggregate
/// server to list).
fn psmux_registry_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".psmux")
}

/// The live DEFAULT-SOCKET session names among psmux registry filenames. A live
/// session is a `<base>.port` file; sibling `.key`/`.sid`/bookkeeping files are
/// ignored. A base containing `__` is excluded: it is either the warm-pool standby
/// (`__warm__`) or a `-L`-namespaced session (`<ns>__<name>`), neither of which is
/// a default-socket session. Sorted + deduped for a stable order.
fn psmux_session_names(filenames: &[String]) -> Vec<String> {
    let mut names: Vec<String> = filenames
        .iter()
        .filter_map(|f| f.strip_suffix(".port"))
        .filter(|base| !base.contains("__"))
        .map(str::to_string)
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Reads psmux's registry `dir` and returns its live default-socket session names.
/// A missing/unreadable directory yields an empty list (no local sessions).
fn read_psmux_registry_dir(dir: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let files: Vec<String> = rd
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    psmux_session_names(&files)
}

/// Merges psmux's registry session NAMES (authoritative existence) with the
/// `list-sessions` DETAIL rows. A `list-sessions` row wins for a session it covers
/// (full windows/attached/recency); a registry name it omits is still surfaced with
/// a minimal placeholder, so a failed/partial `list-sessions` never blanks the
/// sidebar. Deduped on name (a session in both sources appears once).
fn merge_psmux_sessions(source: &str, names: Vec<String>, detail: Vec<Session>) -> Vec<Session> {
    let covered: std::collections::HashSet<String> =
        detail.iter().map(|s| s.name.clone()).collect();
    let mut out = detail;
    for name in names {
        if !covered.contains(&name) {
            out.push(Session {
                source: source.to_string(),
                name,
                windows: 1,
                attached: false,
                last_attached: 0,
            });
        }
    }
    out
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
///
/// Assumes the remote login shell is POSIX (`sh`/`bash`/`zsh`), which is the
/// supported remote target: a remote source runs `tmux`, and tmux remotes are
/// POSIX. [`quote`]'s single-quote escaping is correct and injection-safe there.
/// A Windows remote whose ssh default shell is `cmd.exe` is NOT a supported
/// remote (the local side may be Windows/psmux, but remotes are POSIX); cmd.exe
/// does not treat single quotes as quoting, so addressing it correctly would need
/// an explicit per-host shell — a separate feature, intentionally not assumed here.
pub fn remote_command(argv: &[String]) -> String {
    argv.iter().map(|a| quote(a)).collect::<Vec<_>>().join(" ")
}

/// Assembles the source list for a config: local first, then each ssh host
/// (ssh-config aliases merged with config overrides) in order.
pub fn build(
    cfg: &Config,
    ssh_aliases: &[String],
    os: &str,
    xmux_dir: &Path,
    local_socket: Option<String>,
) -> Vec<Source> {
    let mut srcs = vec![Source {
        alias: session::LOCAL_SOURCE.to_string(),
        binary: cfg.local_bin(os),
        remote: false,
        control_path: String::new(),
        os: os.to_string(),
        socket: local_socket,
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
            socket: None,
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
            socket: None,
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
    fn exec_argv_local_with_socket_injects_dash_s() {
        // A local source on a non-default socket targets it explicitly via -S so
        // listing/select hit the same server the user's client (switch-client) is on.
        let mut s = src("local", "tmux", false, "linux", "");
        s.socket = Some("/tmp/tmux-1000/work".into());
        let argv: Vec<String> = ["tmux", "list-sessions", "-F", "x"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (name, args) = s.exec_argv(false, &argv);
        assert_eq!(name, "tmux");
        assert_eq!(
            args,
            vec!["-S", "/tmp/tmux-1000/work", "list-sessions", "-F", "x"]
        );
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
    fn attach_command_local_ignores_window() {
        // Local pre-selects the window with a separate (instant) local command;
        // attach_command itself just attaches, with or without a window.
        let loc = src("local", "psmux", false, "", "");
        assert_eq!(
            loc.attach_command("dev", None),
            vec!["psmux", "attach", "-t", "dev"]
        );
        assert_eq!(
            loc.attach_command("dev", Some(3)),
            vec!["psmux", "attach", "-t", "dev"]
        );
    }

    #[test]
    fn attach_command_remote_without_window() {
        let rem = src("prod", "tmux", true, "linux", "");
        let got = rem.attach_command("api", None);
        assert_eq!(got[0], "ssh");
        assert!(got.iter().any(|s| s == "-t"), "{got:?}");
        // Records this client's own tty to a per-host file (so switch-client can later
        // target THIS client and no other), then attaches.
        assert_eq!(
            got.last().unwrap(),
            "tty >/tmp/.xmux-cli-prod 2>/dev/null; exec tmux attach -t api"
        );
    }

    #[test]
    fn attach_command_remote_folds_window_into_one_connection() {
        // The window pre-selection and the attach run over a SINGLE `ssh -t`, so
        // there is no second connection to hang on a stalled remote or to lose the
        // selection to interactive auth.
        let rem = src("prod", "tmux", true, "linux", "");
        let got = rem.attach_command("api", Some(2));
        assert_eq!(got[0], "ssh");
        assert!(got.iter().any(|s| s == "-t"), "{got:?}");
        assert_eq!(
            got.last().unwrap(),
            "tty >/tmp/.xmux-cli-prod 2>/dev/null; tmux select-window -t 'api:2' ; exec tmux attach -t api"
        );
    }

    #[tokio::test]
    async fn list_sessions_parses_output() {
        // The generic (aggregate-server) path: a single list-sessions returns every
        // session. Local tmux / any remote mux uses it; local psmux does NOT (it is
        // one-server-per-session — see the psmux registry tests below).
        let mut s = src("local", "tmux", false, "", "");
        s.runner = Some(FakeRunner::ok("3\t1\t1781246739\teditor\n1\t0\t\tbuild\n"));
        let got = s.list_sessions().await.unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].name, "editor");
        assert_eq!(got[0].windows, 3);
        assert!(got[0].attached);
        assert_eq!(got[0].source, "local");
        assert_eq!(got[1].last_attached, 0);
    }

    #[test]
    fn is_local_psmux_only_for_local_psmux() {
        // psmux is one-server-per-session; the registry enumeration path is taken
        // only for a LOCAL psmux source, never local tmux or any remote.
        assert!(src("local", "psmux", false, "", "").is_local_psmux());
        assert!(!src("local", "tmux", false, "", "").is_local_psmux());
        assert!(!src("prod", "psmux", true, "", "").is_local_psmux());
    }

    #[test]
    fn psmux_session_names_excludes_warm_namespaced_and_non_port() {
        // psmux writes `<base>.port` per live session in its registry dir. A base
        // containing `__` is the warm-pool standby (`__warm__`) or a `-L`-namespaced
        // session (`<ns>__<name>`); neither is a default-socket session. Sibling
        // `.key`/`.sid`/bookkeeping files are not `.port` and are ignored.
        let files: Vec<String> = [
            "xmux.port",
            "build.port",
            "__warm__.port",
            "ns__sess.port",
            "xmux.key",
            "xmux.sid",
            "last_session",
            "next_session_id",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            psmux_session_names(&files),
            vec!["build".to_string(), "xmux".to_string()]
        );
    }

    #[test]
    fn psmux_session_names_empty() {
        assert!(psmux_session_names(&[]).is_empty());
    }

    #[test]
    fn read_psmux_registry_dir_scans_port_files() {
        let dir = std::env::temp_dir().join(format!("xmux-psmux-reg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for f in ["alpha.port", "beta.port", "__warm__.port", "x__y.port", "alpha.key"] {
            std::fs::write(dir.join(f), b"1234").unwrap();
        }
        let got = read_psmux_registry_dir(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(got, vec!["alpha".to_string(), "beta".to_string()]);
    }

    #[test]
    fn read_psmux_registry_dir_missing_is_empty() {
        let dir = std::env::temp_dir().join("xmux-psmux-absent-zzzz-does-not-exist");
        assert!(read_psmux_registry_dir(&dir).is_empty());
    }

    #[test]
    fn merge_psmux_sessions_prefers_detail_and_keeps_registry_only() {
        // psmux's `list-sessions` aggregates every live default-socket session in one
        // call, so its rows carry the real detail (windows/attached/recency). The
        // registry (`*.port`) is the authoritative EXISTENCE set: a name present in
        // the registry but missing from the (possibly failed/partial) list-sessions
        // output is still surfaced, with minimal placeholder detail.
        let detail = vec![
            Session { source: "local".into(), name: "editor".into(), windows: 3, attached: true, last_attached: 200 },
        ];
        let names = vec!["editor".to_string(), "build".to_string()];
        let got = merge_psmux_sessions("local", names, detail);
        assert_eq!(got.len(), 2, "no duplicate for the session in both sources");
        let editor = got.iter().find(|s| s.name == "editor").unwrap();
        assert_eq!(editor.windows, 3, "detail row wins (full info)");
        assert!(editor.attached);
        let build = got.iter().find(|s| s.name == "build").unwrap();
        assert_eq!(build.source, "local");
        assert_eq!(build.windows, 1, "registry-only session gets minimal placeholder detail");
    }

    #[test]
    fn merge_psmux_sessions_empty_registry_falls_back_to_detail() {
        // If the registry read yields nothing (e.g. unreadable), the list-sessions
        // detail still stands on its own.
        let detail = vec![
            Session { source: "local".into(), name: "only".into(), windows: 1, attached: false, last_attached: 5 },
        ];
        let got = merge_psmux_sessions("local", Vec::new(), detail);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "only");
    }

    #[test]
    fn merge_psmux_sessions_no_detail_surfaces_registry_names() {
        // The reported failure: list-sessions returns nothing even though sessions
        // exist. The registry names must still surface so the sidebar is not blank.
        let got = merge_psmux_sessions("local", vec!["a".into(), "b".into()], Vec::new());
        let names: Vec<&str> = got.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b"]);
        assert!(got.iter().all(|s| s.source == "local"));
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

    #[test]
    fn reason_is_no_sessions_matches_line_prefix_markers() {
        assert!(reason_is_no_sessions("no sessions"));
        assert!(reason_is_no_sessions("no server running on /tmp/tmux-1000/default"));
        assert!(!reason_is_no_sessions("connection timed out"));
        // Not a line prefix → not the idle mux (a MOTD must not masquerade).
        assert!(!reason_is_no_sessions("you have no sessions pending"));
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
    fn is_mux_var_is_precise() {
        // Strips exactly tmux's session markers and psmux vars.
        assert!(is_mux_var("TMUX"));
        assert!(is_mux_var("TMUX_PANE"));
        assert!(is_mux_var("PSMUX_SESSION"));
        // Keeps unrelated vars that merely share the TMUX prefix.
        assert!(!is_mux_var("TMUXP_LAYOUT")); // tmuxp, a different tool
        assert!(!is_mux_var("TMUX_TMPDIR")); // selects the socket dir — must survive
        assert!(!is_mux_var("PATH"));
    }

    #[test]
    fn mux_clean_env_keeps_lookalike_vars() {
        let input: Vec<String> = ["TMUX=/x,1,0", "TMUXP_LAYOUT=tiled", "TMUX_TMPDIR=/tmp/t"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let out = mux_clean_env(&input);
        assert!(!out.iter().any(|e| e.starts_with("TMUX=")), "{out:?}");
        assert!(out.iter().any(|e| e == "TMUXP_LAYOUT=tiled"), "{out:?}");
        assert!(out.iter().any(|e| e == "TMUX_TMPDIR=/tmp/t"), "{out:?}");
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
    fn control_argv_local_plain_and_socket() {
        let loc = src("local", "psmux", false, "", "");
        assert_eq!(loc.control_argv(), vec!["psmux", "-CC", "attach"]);
        // A local source on a non-default socket injects -S before -CC.
        let mut s = src("local", "tmux", false, "linux", "");
        s.socket = Some("/tmp/tmux-1000/work".into());
        assert_eq!(
            s.control_argv(),
            vec!["tmux", "-S", "/tmp/tmux-1000/work", "-CC", "attach"]
        );
    }

    #[test]
    fn control_argv_remote_forces_pty() {
        let rem = src("prod", "tmux", true, "linux", "");
        let got = rem.control_argv();
        assert_eq!(got[0], "ssh");
        // `tmux -CC attach` does a tcgetattr and exits without a tty, and xmux
        // drives ssh over pipes, so the control connection forces a remote pty.
        assert!(
            got.iter().any(|s| s == "-tt"),
            "control-mode ssh forces a pty with -tt: {got:?}"
        );
        assert!(
            got.iter().any(|s: &String| s.contains("BatchMode=yes")),
            "the control connection must never hang on a prompt: {got:?}"
        );
        assert_eq!(got.last().unwrap(), "tmux -CC attach");
    }

    #[test]
    fn switch_client_remote_cmd_targets_xmux_own_captured_client() {
        let rem = src("prod", "tmux", true, "linux", "");
        let cmd = rem.switch_client_remote_cmd("api");
        // Switches ONLY xmux's own display client — identified by the tty it recorded to
        // its per-host file at attach — so the user's own attached client (or the -CC
        // metadata client) is never moved. Guarded on a non-empty tty.
        assert!(cmd.contains("cat /tmp/.xmux-cli-prod"), "{cmd}");
        assert!(cmd.contains("[ -n \"$c\" ]"), "{cmd}");
        assert!(cmd.contains("switch-client -c \"$c\""), "{cmd}");
        assert!(cmd.trim_end().ends_with("-t api"), "{cmd}");
        // A session name with shell metacharacters is quoted (injection-safe).
        let evil = rem.switch_client_remote_cmd("a;rm -rf /");
        assert!(evil.contains("-t 'a;rm -rf /'"), "{evil}");
    }

    #[tokio::test]
    async fn run_raw_is_noop_for_local() {
        // run_raw is remote-only (a raw remote shell command); local returns empty.
        let loc = src("local", "psmux", false, "windows", "");
        assert!(loc.run_raw("anything").await.unwrap().is_empty());
    }

    #[test]
    fn build_puts_local_first() {
        let cfg = Config::default();
        let aliases: Vec<String> = ["prod", "db"].iter().map(|s| s.to_string()).collect();
        let srcs = build(&cfg, &aliases, "linux", Path::new("/home/u/.xmux"), None);
        assert_eq!(srcs.len(), 3);
        assert_eq!(srcs[0].alias, "local");
        assert!(!srcs[0].remote);
        assert_eq!(srcs[1].alias, "prod");
        assert!(srcs[1].remote);
        assert_eq!(srcs[1].binary, "tmux");
    }
}
