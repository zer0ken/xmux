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
}

fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

fn config_path() -> PathBuf {
    home_dir().join(".config").join("xmux").join("config.toml")
}

fn ssh_config_path() -> PathBuf {
    home_dir().join(".ssh").join("config")
}

fn xmux_dir_path() -> PathBuf {
    home_dir().join(".xmux")
}

fn current_os() -> &'static str {
    std::env::consts::OS
}

/// The local mux server socket parsed from `$TMUX` (`<socket>,<pid>,<session>`),
/// so a popup launched inside a non-default mux (e.g. `tmux -L work`) targets
/// that server rather than the default socket. `None` when not inside a mux (the
/// home loop) — then the default socket is used.
fn local_socket(tmux: Option<&str>) -> Option<String> {
    let path = tmux?.split(',').next()?;
    (!path.is_empty()).then(|| path.to_string())
}

/// Loads config and assembles the sources. The returned error is the config-parse
/// error (non-`None` for a malformed config); the [`Env`] is still usable with
/// defaults so `doctor` can report the problem instead of dying on it.
pub fn build_env() -> (Env, Option<anyhow::Error>) {
    let (cfg, cfg_warnings, cfg_err) = match config::load_verbose(&config_path()) {
        Ok((c, w)) => (c, w, None),
        Err(e) => (Config::default(), Vec::new(), Some(e)),
    };
    let os = current_os();
    let aliases = config::ssh_host_aliases(&ssh_config_path());
    let xmux_dir = xmux_dir_path();
    let local_socket = local_socket(std::env::var("TMUX").ok().as_deref());
    let srcs = source::build(&cfg, &aliases, os, &xmux_dir, local_socket);
    let by_alias = srcs.iter().map(|s| (s.alias.clone(), s.clone())).collect();
    let local_bin = cfg.local_bin(os);
    let ui_prefix = cfg.ui_prefix().to_string();
    (
        Env {
            cfg,
            cfg_warnings,
            srcs,
            by_alias,
            local_bin,
            ui_prefix,
            xmux_dir,
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
        with_timeout(SCAN_TIMEOUT, src.list_sessions()).await
    }

    async fn new_session(&self, source: &str, name: &str) -> anyhow::Result<Session> {
        let src = self.source(source)?;
        let assigned = with_timeout(DETAIL_TIMEOUT, manage::create(&src, name)).await?;
        Ok(Session {
            source: source.to_string(),
            name: assigned,
            windows: 1,
            ..Default::default()
        })
    }

    async fn kill(&self, s: &Session) -> anyhow::Result<()> {
        let src = self.source(&s.source)?;
        with_timeout(DETAIL_TIMEOUT, manage::kill(&src, &s.name)).await
    }

    async fn rename(&self, s: &Session, new_name: &str) -> anyhow::Result<()> {
        let src = self.source(&s.source)?;
        with_timeout(DETAIL_TIMEOUT, manage::rename(&src, &s.name, new_name)).await
    }

    async fn panes(&self, s: &Session) -> anyhow::Result<Vec<WindowPanes>> {
        let src = self.source(&s.source)?;
        let _permit = self.sem.acquire().await?;
        with_timeout(DETAIL_TIMEOUT, manage::panes(&src, &s.name)).await
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
        Source {
            alias: alias.into(),
            binary: "tmux".into(),
            remote,
            control_path: String::new(),
            os: "linux".into(),
            socket: None,
            runner: Some(runner(line)),
        }
    }

    fn source_with(alias: &str, r: std::sync::Arc<dyn Runner>) -> Source {
        Source {
            alias: alias.into(),
            binary: "tmux".into(),
            remote: false,
            control_path: String::new(),
            os: "linux".into(),
            socket: None,
            runner: Some(r),
        }
    }

    fn env_of(src: Source) -> Arc<Env> {
        Arc::new(Env {
            cfg: Config::default(),
            cfg_warnings: Vec::new(),
            by_alias: [(src.alias.clone(), src.clone())].into_iter().collect(),
            srcs: vec![src],
            local_bin: "tmux".into(),
            ui_prefix: "C-g".into(),
            xmux_dir: PathBuf::from("."),
        })
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
