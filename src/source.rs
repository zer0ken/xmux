//! Abstracts a mux server reachable from this machine: the local mux, or a
//! remote one over ssh. It carries the per-source config/data (alias, mux binary,
//! socket, control path, os) plus the mux-env quoting vocab (`is_mux_var` /
//! `mux_clean_env`) and the reachable-but-empty vs unreachable distinction. The
//! machine boundary itself — argv assembly and the ssh transport (connect-timeout,
//! injection-safe quoting) — lives in `Transport`; this source delegates to it
//! (`transport()`), so the layers above speak in sessions, not transports.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;

use crate::config::Config;
use crate::mux;
use crate::session::{self, Session};

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
    /// The argv that hands the terminal over to attach this source's named
    /// session (over `ssh -t` for a remote).
    ///
    /// Composes the two axes: the MUX supplies the attach argv via
    /// `Backend::attach_plan` (so local psmux uses `new-session -A -s <name>`, routing to
    /// the session's OWN server, not a warm clone from a bare `attach -t`), and the
    /// MACHINE wraps it via `Transport::interactive_attach_argv` (local `-S` injection, or
    /// `ssh -t` with `[<select-window> ;] exec <attach>`). `window` is the window index to
    /// land on: for a REMOTE source the selection is folded into the SAME `ssh -t`
    /// connection so there is no second connection that could hang or be lost to
    /// interactive auth; a LOCAL source pre-selects it with a separate instant command, so
    /// the transport ignores it here.
    pub fn interactive_attach_command(&self, name: &str, window: Option<i64>) -> Vec<String> {
        let mux = crate::mux::for_binary(&self.binary);
        let attach = mux.attach_plan(name, window);
        let pre_select = window.map(|w| mux.select_window_plan(&mux::window_target(name, w)));
        let (n, a) = self
            .transport()
            .interactive_attach_argv(&attach, pre_select.as_deref());
        let mut v = vec![n];
        v.extend(a);
        v
    }

    pub(crate) fn run_with(&self) -> &dyn Runner {
        match &self.runner {
            Some(r) => r.as_ref(),
            None => &ExecRunner,
        }
    }

    /// The machine transport this source reaches its mux over. A remote source is an
    /// ssh transport carrying the same alias/control-path/os; a local source is a
    /// `Local` transport carrying the same `-S` socket. `Transport` is the sole owner
    /// of argv/ssh wrapping (`Transport::exec_argv`), so callers lower this source's
    /// commands through the transport rather than the source itself.
    pub(crate) fn transport(&self) -> crate::model::Transport {
        if self.remote {
            crate::model::Transport::Ssh {
                alias: self.alias.clone(),
                control_path: self.control_path.clone(),
                os: self.os.clone(),
            }
        } else {
            crate::model::Transport::Local {
                socket: self.socket.clone(),
            }
        }
    }

    /// Returns the source's sessions. A reachable mux with no sessions returns an
    /// empty vec; an unreachable source returns an error.
    ///
    /// Enumeration (which mux, its registry-merge vs aggregate-list behaviour, and the
    /// reachable-but-empty classification) lives entirely in `backend`: `for_binary`
    /// selects the mux from the binary name and `Backend::enumerate` runs the probe
    /// over this source's [`Source::transport`]. This is a thin shim — the source layer
    /// no longer branches on the mux kind.
    pub async fn list_sessions(&self) -> Result<Vec<Session>, RunError> {
        crate::mux::for_binary(&self.binary)
            .enumerate(&self.transport(), self.run_with())
            .await
    }
}

// The reachable-but-empty classification lives in `mux/`. The cockpit reaches its
// `%exit`/`%error`-reason check through `crate::source::reason_is_no_sessions`, so the
// name is re-exported here to keep that path resolving.
pub(crate) use crate::mux::reason_is_no_sessions;

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
    fn interactive_attach_local_psmux_routes_to_the_per_session_server() {
        // The bug FIX: local psmux must attach via `new-session -A -s <name>` (routing to
        // that session's OWN server), NOT a bare `attach -t <name>` (which lands on a warm
        // clone / the wrong session). The mux axis (Backend::attach_plan) supplies this;
        // the local pre-select is a separate command, so the window is ignored here.
        let loc = src("local", "psmux", false, "", "");
        assert_eq!(
            loc.interactive_attach_command("dev", None),
            vec!["psmux", "new-session", "-A", "-s", "dev"]
        );
        assert_eq!(
            loc.interactive_attach_command("dev", Some(3)),
            vec!["psmux", "new-session", "-A", "-s", "dev"]
        );
    }

    #[test]
    fn interactive_attach_local_tmux_is_a_plain_attach() {
        // A LOCAL tmux (Shared) attach stays `attach -t <name>` (unchanged).
        let loc = src("local", "tmux", false, "", "");
        assert_eq!(
            loc.interactive_attach_command("dev", None),
            vec!["tmux", "attach", "-t", "dev"]
        );
    }

    #[test]
    fn interactive_attach_remote_tmux_without_window() {
        let rem = src("prod", "tmux", true, "linux", "");
        let got = rem.interactive_attach_command("api", None);
        assert_eq!(got[0], "ssh");
        assert!(got.iter().any(|s| s == "-t"), "{got:?}");
        assert_eq!(got.last().unwrap(), "exec tmux attach -t api");
    }

    #[test]
    fn interactive_attach_remote_tmux_folds_window_into_one_connection() {
        // The window pre-selection and the attach run over a SINGLE `ssh -t`, so
        // there is no second connection to hang on a stalled remote or to lose the
        // selection to interactive auth.
        let rem = src("prod", "tmux", true, "linux", "");
        let got = rem.interactive_attach_command("api", Some(2));
        assert_eq!(got[0], "ssh");
        assert!(got.iter().any(|s| s == "-t"), "{got:?}");
        assert_eq!(
            got.last().unwrap(),
            "tmux select-window -t 'api:2' ; exec tmux attach -t api"
        );
    }

    #[test]
    fn interactive_attach_remote_psmux_uses_attach_plan_over_ssh() {
        // A REMOTE psmux host is enumerated/attached the generic way (its registry lives
        // on the far side); the attach argv still comes from Backend::attach_plan
        // (`new-session -A -s`) and is `exec`d over `ssh -t`.
        let rem = src("prod", "psmux", true, "linux", "");
        let got = rem.interactive_attach_command("api", None);
        assert_eq!(got[0], "ssh");
        assert!(got.iter().any(|s| s == "-t"), "{got:?}");
        assert_eq!(got.last().unwrap(), "exec psmux new-session -A -s api");
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
