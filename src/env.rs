//! The resolved runtime: the source list and the lookups the commands share,
//! built once per process from config + ssh-config. Owns the scan (concurrent
//! reachability probe, used by `ls`) and the switcher's side-effecting [`Ops`]
//! over the live mux — including the per-source/per-session probes the event
//! loop streams in.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::config::{self, Config};
use crate::discovery;
use crate::manage;
use crate::session::{Session, WindowPanes};
use crate::source::{self, Source};
use crate::ui::switcher::Ops;
use crate::ui::tree::{self, Group};

const SCAN_CONCURRENCY: usize = 8;
const SCAN_TIMEOUT: Duration = Duration::from_secs(6); // must exceed the ssh connect timeout (5s)
const DETAIL_TIMEOUT: Duration = Duration::from_secs(6);

/// The resolved runtime.
pub struct Env {
    pub cfg: Config,
    pub cfg_warnings: Vec<String>,
    pub srcs: Vec<Source>,
    pub by_alias: HashMap<String, Source>,
    pub local_bin: String,
    pub ui_prefix: String,
    pub xmux_dir: PathBuf,
    /// The ssh-config host aliases discovered at startup (a config-assembly product).
    /// `Hosts::build` reruns `Config::host_specs` over these to seed the runtime host
    /// registry, so the registry is built from config, not by re-reading `srcs`.
    pub ssh_aliases: Vec<String>,
    /// The local mux server socket parsed from `$TMUX` (`-S` target), threaded into
    /// the local host's transport by `Hosts::build`. `None` on the default socket.
    pub local_socket: Option<String>,
}

/// Pure fallback decision: a resolved home is returned unflagged; an unresolved
/// home falls back to the current directory (`.`) and flags it `true` so the caller
/// can warn. Split out so the fallback is unit-tested without touching the real HOME.
fn home_or_cwd(home: Option<PathBuf>) -> (PathBuf, bool) {
    match home {
        Some(p) => (p, false),
        None => (PathBuf::from("."), true),
    }
}

fn home_dir() -> PathBuf {
    let (dir, fell_back) = home_or_cwd(dirs::home_dir());
    if fell_back {
        tracing::warn!("could not resolve a home directory; falling back to the current directory for config, ~/.xmux state, sockets, and logs");
    }
    dir
}

fn config_path() -> PathBuf {
    home_dir().join(".config").join("xmux").join("config.toml")
}

pub(crate) fn ssh_config_path() -> PathBuf {
    home_dir().join(".ssh").join("config")
}

pub(crate) fn xmux_dir_path() -> PathBuf {
    home_dir().join(".xmux")
}

fn current_os() -> &'static str {
    std::env::consts::OS
}

/// The local mux server socket parsed from `$TMUX` (`<socket>,<pid>,<session>`),
/// so xmux running inside a non-default mux (e.g. `tmux -L work`) targets that
/// server rather than the default socket. `None` when not inside a mux — then
/// the default socket is used.
fn local_socket(tmux: Option<&str>) -> Option<String> {
    let path = tmux?.split(',').next()?;
    (!path.is_empty()).then(|| path.to_string())
}

/// Loads config and assembles the sources. The returned error is the config-parse
/// error (non-`None` for a malformed config); the [`Env`] is still usable with
/// defaults so `doctor` can report the problem instead of dying on it.
pub fn build_env() -> (Env, Option<anyhow::Error>) {
    let (cfg, mut cfg_warnings, cfg_err) = match config::load_verbose(&config_path()) {
        Ok((c, w)) => (c, w, None),
        Err(e) => (Config::default(), Vec::new(), Some(e)),
    };
    // Value-level advisories (an unrecognized `mux` typo) alongside the unknown-KEY
    // warnings `load_verbose` already produced. On the parse-error branch cfg is a
    // default, so this is a no-op there.
    cfg_warnings.extend(cfg.value_warnings());
    let os = current_os();
    let aliases = config::ssh_host_aliases(&ssh_config_path());
    let xmux_dir = xmux_dir_path();
    let local_socket = local_socket(std::env::var("TMUX").ok().as_deref());
    let srcs = source::build(&cfg, &aliases, os, &xmux_dir, local_socket);
    let by_alias = srcs.iter().map(|s| (s.alias.clone(), s.clone())).collect();
    let local_bin = cfg.local_bin(os);
    let ui_prefix = cfg.ui_prefix().to_string();
    // The local host's `-S` socket, read back from the assembled local source so the
    // host registry (`Hosts::build`) targets the same server the source list does.
    let host_local_socket = srcs
        .iter()
        .find(|s| s.alias == crate::session::LOCAL_SOURCE)
        .and_then(|s| s.local_socket());
    (
        Env {
            cfg,
            cfg_warnings,
            srcs,
            by_alias,
            local_bin,
            ui_prefix,
            xmux_dir,
            ssh_aliases: aliases,
            local_socket: host_local_socket,
        },
        cfg_err,
    )
}

/// Converts scan results to display groups, sorting sessions by recency.
fn to_groups(results: Vec<discovery::ScanResult>) -> Vec<Group> {
    results
        .into_iter()
        .map(|r| {
            let mut sessions = r.sessions;
            tree::sort_by_recency(&mut sessions);
            Group {
                source: r.source,
                err: r.err,
                sessions,
            }
        })
        .collect()
}

impl Env {
    /// Probes every source and returns the merged, recency-sorted host/session
    /// groups (used by `ls`, which needs no window/pane detail).
    pub async fn scan(&self) -> Vec<Group> {
        let results = discovery::scan_all(&self.srcs, SCAN_TIMEOUT, SCAN_CONCURRENCY).await;
        to_groups(results)
    }

    /// Builds the switcher's side-effecting actions over the live mux. A shared
    /// semaphore bounds the concurrent probes (`list-sessions`/`list-panes`) the
    /// event loop streams through these ops.
    pub fn ops(self: &Arc<Self>) -> Arc<dyn Ops> {
        Arc::new(EnvOps {
            env: self.clone(),
            sem: Arc::new(tokio::sync::Semaphore::new(SCAN_CONCURRENCY)),
        })
    }
}

/// Renders scan groups for `xmux ls`: one `<source>/<name>` line per reachable
/// session, an unreachable line per dead source, and whether EVERY source is
/// unreachable (a reachable mux with zero sessions is empty, not failed).
pub fn ls_lines(groups: &[Group]) -> (Vec<String>, Vec<String>, bool) {
    let mut lines = Vec::new();
    let mut unreachable = Vec::new();
    let mut reachable = 0;
    for g in groups {
        if let Some(err) = &g.err {
            unreachable.push(format!("{}\t(unreachable: {})", g.source, err));
            continue;
        }
        reachable += 1;
        for s in &g.sessions {
            lines.push(format!(
                "{}\t{}w\tattached={}",
                s.address(),
                s.windows,
                s.attached
            ));
        }
    }
    let all_unreachable = reachable == 0 && !groups.is_empty();
    (lines, unreachable, all_unreachable)
}

/// The live [`Ops`] implementation over a [`Env`].
struct EnvOps {
    env: Arc<Env>,
    /// Bounds the in-flight probes so a fan-out of ssh connects stays capped.
    sem: Arc<tokio::sync::Semaphore>,
}

impl EnvOps {
    fn source(&self, alias: &str) -> anyhow::Result<Source> {
        self.env
            .by_alias
            .get(alias)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("unknown source {alias:?}"))
    }
}

async fn with_timeout<T>(
    timeout: Duration,
    fut: impl std::future::Future<Output = Result<T, source::RunError>>,
) -> anyhow::Result<T> {
    match tokio::time::timeout(timeout, fut).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(e.into()),
        Err(_) => Err(anyhow::anyhow!("timed out after {}s", timeout.as_secs())),
    }
}

#[async_trait::async_trait]
impl Ops for EnvOps {
    fn sources(&self) -> Vec<String> {
        self.env.srcs.iter().map(|s| s.alias.clone()).collect()
    }

    async fn list_sessions(&self, source: &str) -> anyhow::Result<Vec<Session>> {
        let src = self.source(source)?;
        let _permit = self.sem.acquire().await?;
        with_timeout(SCAN_TIMEOUT, async move {
            let mut host = src.host();
            host.enumerate_with(src.run_with())
                .await
                .map(|()| host.inventory.sessions)
        })
        .await
    }

    async fn new_session(&self, source: &str, name: &str) -> anyhow::Result<Session> {
        let src = self.source(source)?;
        let host = src.host();
        let assigned =
            with_timeout(DETAIL_TIMEOUT, manage::create(&host, src.run_with(), name)).await?;
        Ok(Session {
            source: source.to_string(),
            name: assigned,
            windows: 1,
            ..Default::default()
        })
    }

    async fn new_window(&self, source: &str, session: &str, name: &str) -> anyhow::Result<()> {
        let src = self.source(source)?;
        let host = src.host();
        with_timeout(
            DETAIL_TIMEOUT,
            manage::new_window(&host, src.run_with(), session, name),
        )
        .await
    }

    async fn split_window(&self, source: &str, target: &str, vertical: bool) -> anyhow::Result<()> {
        let src = self.source(source)?;
        let host = src.host();
        with_timeout(
            DETAIL_TIMEOUT,
            manage::split_window(&host, src.run_with(), target, vertical),
        )
        .await
    }

    async fn kill(&self, s: &Session) -> anyhow::Result<()> {
        let src = self.source(&s.source)?;
        let host = src.host();
        with_timeout(DETAIL_TIMEOUT, manage::kill(&host, src.run_with(), &s.name)).await
    }

    async fn rename(&self, s: &Session, new_name: &str) -> anyhow::Result<()> {
        let src = self.source(&s.source)?;
        let host = src.host();
        with_timeout(
            DETAIL_TIMEOUT,
            manage::rename(&host, src.run_with(), &s.name, new_name),
        )
        .await
    }

    async fn panes(&self, s: &Session) -> anyhow::Result<Vec<WindowPanes>> {
        let src = self.source(&s.source)?;
        let host = src.host();
        let _permit = self.sem.acquire().await?;
        with_timeout(
            DETAIL_TIMEOUT,
            manage::panes(&host, src.run_with(), &s.name),
        )
        .await
    }

    async fn kill_window(&self, source: &str, target: &str) -> anyhow::Result<()> {
        let src = self.source(source)?;
        let host = src.host();
        with_timeout(
            DETAIL_TIMEOUT,
            manage::kill_window(&host, src.run_with(), target),
        )
        .await
    }

    async fn border_styles(&self, source: &str) -> anyhow::Result<(String, String)> {
        let src = self.source(source)?;
        let _permit = self.sem.acquire().await?;
        let host = src.host();
        // A failed / timed-out query degrades to "" so the view border falls back to
        // the configured override, then the stock default (never an error to the caller).
        let active = with_timeout(
            DETAIL_TIMEOUT,
            manage::show_option(&host, src.run_with(), "pane-active-border-style"),
        )
        .await
        .unwrap_or_default();
        let inactive = with_timeout(
            DETAIL_TIMEOUT,
            manage::show_option(&host, src.run_with(), "pane-border-style"),
        )
        .await
        .unwrap_or_default();
        Ok((active, inactive))
    }

    async fn rename_window(
        &self,
        source: &str,
        target: &str,
        new_name: &str,
    ) -> anyhow::Result<()> {
        let src = self.source(source)?;
        let host = src.host();
        with_timeout(
            DETAIL_TIMEOUT,
            manage::rename_window(&host, src.run_with(), target, new_name),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::session::Session;
    use crate::source::{RunError, Runner};

    #[test]
    fn env_carries_configured_prefix() {
        let mut cfg = Config::default();
        cfg.ui.prefix = "C-a".into();
        assert_eq!(cfg.ui_prefix(), "C-a");
    }

    /// Returns canned list-sessions output, ignoring the command.
    struct StaticRunner(Vec<u8>);

    #[async_trait::async_trait]
    impl Runner for StaticRunner {
        async fn run(&self, _name: &str, _args: &[String]) -> Result<Vec<u8>, RunError> {
            Ok(self.0.clone())
        }
    }

    fn runner(line: &str) -> std::sync::Arc<dyn Runner> {
        std::sync::Arc::new(StaticRunner(line.as_bytes().to_vec()))
    }

    fn test_source(alias: &str, remote: bool, line: &str) -> Source {
        let kind = if remote {
            crate::machine::MachineKind::Ssh {
                alias: alias.into(),
                control_path: String::new(),
                os: "linux".into(),
            }
        } else {
            crate::machine::MachineKind::Local { socket: None }
        };
        Source {
            alias: alias.into(),
            binary: "tmux".into(),
            kind,
            runner: Some(runner(line)),
        }
    }

    #[tokio::test]
    async fn list_sessions_probes_one_source() {
        // EnvOps::list_sessions probes a single source by alias, returning its
        // sessions (the per-host streaming probe the event loop fans out).
        let env = Arc::new(Env {
            cfg: Config::default(),
            cfg_warnings: Vec::new(),
            srcs: vec![test_source("local", false, "2\t1\t100\teditor\n")],
            by_alias: [(
                "local".to_string(),
                test_source("local", false, "2\t1\t100\teditor\n"),
            )]
            .into_iter()
            .collect(),
            local_bin: "tmux".into(),
            ui_prefix: "C-g".into(),
            xmux_dir: PathBuf::from("."),
            ssh_aliases: Vec::new(),
            local_socket: None,
        });
        let ops = env.ops();
        assert_eq!(ops.sources(), vec!["local".to_string()]);
        let sessions = ops.list_sessions("local").await.unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].name, "editor");
        assert_eq!(sessions[0].source, "local");
    }

    fn group(source: &str, err: Option<&str>, sessions: Vec<Session>) -> Group {
        Group {
            source: source.into(),
            err: err.map(|s| s.to_string()),
            sessions,
        }
    }

    fn sess(source: &str, name: &str, windows: i64, attached: bool) -> Session {
        Session {
            source: source.into(),
            name: name.into(),
            windows,
            attached,
            last_attached: 0,
        }
    }

    #[test]
    fn ls_lines_reachable_and_unreachable() {
        let groups = vec![
            group(
                "local",
                None,
                vec![
                    sess("local", "editor", 2, true),
                    sess("local", "build", 1, false),
                ],
            ),
            group("prod", Some("connection refused"), vec![]),
        ];
        let (lines, unreachable, all_unreachable) = ls_lines(&groups);
        assert_eq!(
            lines,
            vec![
                "local/editor\t2w\tattached=true",
                "local/build\t1w\tattached=false"
            ]
        );
        assert_eq!(unreachable, vec!["prod\t(unreachable: connection refused)"]);
        assert!(!all_unreachable);
    }

    #[test]
    fn local_socket_parses_tmux() {
        assert_eq!(
            local_socket(Some("/tmp/tmux-1000/default,1234,0")),
            Some("/tmp/tmux-1000/default".to_string())
        );
        assert_eq!(
            local_socket(Some("/private/tmp/work,99,2")),
            Some("/private/tmp/work".to_string())
        );
        assert_eq!(local_socket(None), None);
        assert_eq!(local_socket(Some("")), None);
    }

    #[test]
    fn home_or_cwd_flags_the_cwd_fallback() {
        // A resolved home is returned unflagged; an unresolved home falls back to the
        // current directory AND flags it so the caller can warn.
        assert_eq!(
            home_or_cwd(Some(PathBuf::from("/home/u"))),
            (PathBuf::from("/home/u"), false)
        );
        assert_eq!(home_or_cwd(None), (PathBuf::from("."), true));
    }

    #[test]
    fn ls_lines_all_unreachable() {
        let groups = vec![group("prod", Some("boom"), vec![])];
        let (lines, _unreachable, all_unreachable) = ls_lines(&groups);
        assert!(lines.is_empty());
        assert!(all_unreachable);
    }

    #[test]
    fn ls_lines_reachable_empty_is_not_all_unreachable() {
        // A reachable mux with zero sessions is empty, not failed.
        let groups = vec![group("local", None, vec![])];
        let (lines, _unreachable, all_unreachable) = ls_lines(&groups);
        assert!(lines.is_empty());
        assert!(!all_unreachable);
    }

    #[test]
    fn ls_lines_empty_groups_not_all_unreachable() {
        let (_l, _u, all_unreachable) = ls_lines(&[]);
        assert!(!all_unreachable);
    }

    #[tokio::test]
    async fn to_groups_sorts_sessions_by_recency() {
        let results = vec![discovery::ScanResult {
            source: "local".into(),
            sessions: vec![
                Session {
                    source: "local".into(),
                    name: "old".into(),
                    last_attached: 10,
                    ..Default::default()
                },
                Session {
                    source: "local".into(),
                    name: "new".into(),
                    last_attached: 99,
                    ..Default::default()
                },
            ],
            err: None,
        }];
        let groups = to_groups(results);
        assert_eq!(groups[0].sessions[0].name, "new");
        assert_eq!(groups[0].sessions[1].name, "old");
    }
}
