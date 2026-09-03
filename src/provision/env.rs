//! The resolved runtime: the source list and the lookups the commands share,
//! resolved from config + the roster providers at launch and again on every re-scan.
//! Owns the scan (concurrent
//! reachability probe, used by `ls`) and the switcher's side-effecting [`Ops`]
//! over the live mux — including the per-source/per-session probes the event
//! loop streams in.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::link::manage;
use crate::model::source::{self, Source};
use crate::provision::config::{self, Config};
use crate::provision::discovery;
use crate::session::Session;
use crate::ui::switcher::Ops;
use crate::ui::tree::{self, Group};

use tokio::sync::mpsc;

const SCAN_CONCURRENCY: usize = 8;
const SCAN_TIMEOUT: Duration = Duration::from_secs(6); // must exceed the ssh connect timeout (5s)
const DETAIL_TIMEOUT: Duration = Duration::from_secs(6);
/// The unlock's whole budget: ssh connect (5s) + host-key/password answering + a
/// margin for a slow login prompt. Bounds the PTY exchange so it cannot hang the
/// off-loop task that runs it.
const UNLOCK_TIMEOUT_SECS: u64 = 20;

/// Everything a config resolution decides about WHICH sources exist.
///
/// One value because every field answers the same question from the same read of config
/// plus the roster providers. A re-scan resolves a FRESH one and swaps it in, so a config
/// edit, or a tailnet peer coming online, lands without a restart.
#[derive(Default)]
pub struct Roster {
    pub cfg: Config,
    pub cfg_warnings: Vec<String>,
    /// Every source this process knows: the ones config named, plus the ones async mux
    /// discovery adds while the app runs. A source missing from this list is invisible to
    /// every off-loop op, so a discovered mux could be enumerated and painted but not
    /// created on, its panes never read.
    pub sources: Vec<Source>,
    pub local_muxes: Vec<String>,
    /// Which provider put each host on the roster, keyed by HOST name (the machine half
    /// of a source id). Read only to be SHOWN: the unreachable host screen names it, so
    /// a host that fails is traceable to the thing that offered it. See
    /// [`crate::provision::roster::Provider`].
    pub roster_providers: HashMap<String, crate::provision::roster::Provider>,
    /// The ssh-config host aliases this resolution offered (a config-assembly product).
    /// `Hosts::build` reruns `Config::host_specs` over these to seed the runtime host
    /// registry, so the registry is built from config, not by re-reading `sources`.
    pub ssh_aliases: Vec<String>,
    /// The WSL distributions this resolution listed, as MACHINE names. Held for the same
    /// reason as `ssh_aliases`: `Hosts::build` reruns `Config::wsl_specs` over them, so
    /// the host registry and the source list are built from one answer rather than two.
    pub wsl_distros: Vec<String>,
}

/// The resolved runtime: a [`Roster`] that a re-scan can replace, plus the values that
/// are fixed for the life of the process.
pub struct Env {
    /// Behind a lock because a re-scan swaps it. Read it through [`Env::roster`] and the
    /// accessors over it; never hold the guard across an await.
    roster: std::sync::RwLock<Roster>,
    pub ui_prefix: String,
    pub xmux_dir: PathBuf,
    /// The ADDRESS of the session xmux is ITSELF running in (`local:psmux/xmus`), or
    /// `None` when it is not inside a mux or the session could not be named. The one
    /// session the terminal view refuses to mirror; see [`crate::display::attach::own_mux_session`].
    /// Fixed for the run: the environment that names it cannot change under one.
    pub own_session: Option<String>,
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

pub(crate) fn config_path() -> PathBuf {
    home_dir().join(".config").join("xmux").join("config.toml")
}

/// The home the shell ssh reads `~` and its config from: `$HOME` when set, else
/// the platform home. OpenSSH resolves `~` off `$HOME`, and on Windows Git
/// Bash/msys sets `$HOME` to a path that can differ from `USERPROFILE`, so
/// preferring `$HOME` keeps the config xmux reads identical to the one the
/// user's ssh actually reads.
pub(crate) fn ssh_home() -> PathBuf {
    match std::env::var_os("HOME") {
        Some(h) => PathBuf::from(h),
        None => dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")),
    }
}

pub(crate) fn ssh_config_path() -> PathBuf {
    ssh_home().join(".ssh").join("config")
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

/// Resolves the ROSTER: reads config, runs the roster providers, and assembles the
/// source list. The returned error is the config-parse error (non-`None` for a
/// malformed config); the [`Roster`] is still usable with defaults so `doctor` can
/// report the problem instead of dying on it.
///
/// A launch and a re-scan both come from this ONE answer, so neither can disagree with
/// the other about which machines exist.
pub async fn resolve_roster(
    xmux_dir: &std::path::Path,
    local_socket: Option<String>,
) -> (Roster, Option<anyhow::Error>) {
    let (cfg, mut cfg_warnings, cfg_err) = match config::load_verbose(&config_path()) {
        Ok((c, w)) => (c, w, None),
        Err(e) => (Config::default(), Vec::new(), Some(e)),
    };
    // Value-level advisories (an unrecognized `mux` typo) alongside the unknown-KEY
    // warnings `load_verbose` already produced. On the parse-error branch cfg is a
    // default, so this is a no-op there.
    cfg_warnings.extend(cfg.value_warnings());
    let os = current_os();
    // The ROSTER: which machines xmux offers. `~/.ssh/config` first, so a hand-written
    // alias keeps the position the user gave it; then each network provider the config
    // enables. See `crate::provision::roster`.
    //
    // The providers that spawn a process (tailscale, wsl.exe, and the local mux probe)
    // run CONCURRENTLY over the async runner, so the roster build is not serialized on
    // however long each one takes, and none of them blocks the single-threaded runtime.
    // The `~/.ssh/config` read is local file I/O (fast), so it stays inline.
    let ssh_aliases = if cfg.discovery.ssh_config {
        config::ssh_host_aliases(&ssh_config_path())
    } else {
        Vec::new()
    };
    let (tailscale, wsl_distros, installed) = tokio::join!(
        async {
            if cfg.discovery.tailscale {
                crate::provision::roster::tailscale_aliases().await
            } else {
                Vec::new()
            }
        },
        async {
            if cfg.discovery.wsl {
                crate::transport::wsl::distros().await
            } else {
                Vec::new()
            }
        },
        async {
            // The local mux list, resolved ONCE here and threaded on: `auto` (the default)
            // asks this machine which of the muxes xmux supports it actually has, so a zellij
            // you just installed shows up without being written down. Probed over a
            // socket-LESS local transport, because "is this mux here" has nothing to do
            // with which server socket a session lives on - and a `-S <socket>` injection
            // is a flag zellij would refuse.
            if cfg.local.mux.is_auto() {
                crate::mux::installed_muxes(
                    &*crate::transport::local(None),
                    &crate::model::source::ExecRunner,
                )
                .await
            } else {
                Vec::new()
            }
        },
    );
    let offered = crate::provision::roster::merge(&[
        (crate::provision::roster::Provider::SshConfig, ssh_aliases),
        (crate::provision::roster::Provider::Tailscale, tailscale),
    ]);
    let aliases: Vec<String> = offered.iter().map(|(name, _)| name.clone()).collect();
    let local_muxes = cfg.local_muxes(os, &installed);
    let srcs = source::build(
        &cfg,
        &aliases,
        &wsl_distros,
        os,
        &local_muxes,
        xmux_dir,
        local_socket.clone(),
    );
    let roster_providers = roster_providers(&cfg, &offered, &wsl_distros);
    (
        Roster {
            cfg,
            cfg_warnings,
            sources: srcs,
            local_muxes,
            ssh_aliases: aliases,
            wsl_distros,
            roster_providers,
        },
        cfg_err,
    )
}

/// Loads the process-wide runtime: a resolved roster plus the values that are fixed for
/// the life of the process. The returned error is the config-parse error.
pub async fn build_env() -> (Env, Option<anyhow::Error>) {
    let xmux_dir = xmux_dir_path();
    // The local server socket this machine named, handed on RAW: the host registry filters
    // it per mux exactly as the source list does, so both derive one answer from one
    // value. Reading it back off an assembled source would instead make it depend on
    // WHICH local mux happens to be first, and a first source that takes no socket
    // (zellij) would drop it for every host behind it.
    let local_socket = local_socket(std::env::var("TMUX").ok().as_deref());
    let (roster, cfg_err) = resolve_roster(&xmux_dir, local_socket.clone()).await;
    let ui_prefix = roster.cfg.ui_prefix().to_string();
    let own_session = own_session_address(&roster.sources);
    (
        Env::new(roster, ui_prefix, xmux_dir, own_session, local_socket),
        cfg_err,
    )
}

/// The address of the session xmux is running in, resolved against the LOCAL sources.
///
/// The mux names the session; this pairs it with the source id that mux answers as on
/// this machine, because an address is a source and a session and the refusal has to match
/// the card exactly. A mux xmux does not serve here leaves it unresolved, which blocks
/// nothing - the same as not being inside a mux at all.
fn own_session_address(srcs: &[Source]) -> Option<String> {
    let (kind, session) = crate::display::attach::own_mux_session()?;
    Some(crate::session::address_of(
        own_source_id(srcs, &kind)?,
        &session,
    ))
}

/// The LOCAL source id serving mux `kind`, as the mux named itself.
///
/// A source spells the binary the user CONFIGURED, which need not be what the mux calls
/// itself: psmux answers to `tmux` as well, so a box whose config says `tmux` is served
/// by psmux all the same. The spelling is matched first, then the tmux-compatible
/// kinds, which is unambiguous while the box serves one of them. Neither matching leaves the
/// session unresolved, and an unresolved session blocks nothing.
fn own_source_id<'a>(srcs: &'a [Source], kind: &str) -> Option<&'a str> {
    let local: Vec<&Source> = srcs
        .iter()
        .filter(|s| crate::session::is_local_source(&s.alias))
        .collect();
    if let Some(s) = local.iter().find(|s| s.binary == kind) {
        return Some(&s.alias);
    }
    let tmux_compatible = |b: &str| b == "tmux" || b == "psmux";
    if !tmux_compatible(kind) {
        return None;
    }
    let mut it = local.iter().filter(|s| tmux_compatible(&s.binary));
    let only = it.next()?;
    it.next().is_none().then_some(only.alias.as_str())
}

/// Which provider put each host on the roster, keyed by HOST name.
///
/// `offered` is what the roster providers answered, already deduped in precedence order.
/// The two implementations that never pass through those providers are added behind it: a WSL
/// distribution `wsl.exe` listed, and a host the CONFIG named outright, which is a host
/// no provider offered and `host_specs` / `wsl_specs` append. First entry wins
/// throughout, so a host that a provider listed keeps that provider even when a
/// `[[hosts]]` entry also names it - the entry overrides its mux, it did not put it on
/// the roster.
fn roster_providers(
    cfg: &Config,
    offered: &[(String, crate::provision::roster::Provider)],
    wsl_distros: &[String],
) -> HashMap<String, crate::provision::roster::Provider> {
    use crate::provision::roster::Provider;
    let mut out: HashMap<String, Provider> = HashMap::new();
    for (name, provider) in offered {
        out.entry(name.clone()).or_insert(*provider);
    }
    for machine in wsl_distros {
        out.entry(machine.clone()).or_insert(Provider::Wsl);
    }
    for h in &cfg.hosts {
        out.entry(h.ssh.clone()).or_insert(Provider::Config);
    }
    for w in &cfg.wsl {
        if w.distro.is_empty() {
            continue;
        }
        out.entry(format!("{}{}", crate::session::WSL_PREFIX, w.distro))
            .or_insert(Provider::Config);
    }
    // This box is on the roster without anything offering it.
    out.insert(crate::session::LOCAL_SOURCE.to_string(), Provider::Local);
    out
}

/// Converts scan results to display groups, sessions ordered by name.
fn to_groups(results: Vec<discovery::ScanResult>) -> Vec<Group> {
    results
        .into_iter()
        .map(|r| {
            let mut sessions = r.sessions;
            tree::sort_by_name(&mut sessions);
            Group {
                source: r.source,
                err: r.err,
                sessions,
            }
        })
        .collect()
}

impl Env {
    /// Assembles the runtime around an already-resolved roster. The roster is the only
    /// part a re-scan replaces; everything else here is fixed for the life of the process.
    pub fn new(
        roster: Roster,
        ui_prefix: String,
        xmux_dir: PathBuf,
        own_session: Option<String>,
        local_socket: Option<String>,
    ) -> Self {
        Env {
            roster: std::sync::RwLock::new(roster),
            ui_prefix,
            xmux_dir,
            own_session,
            local_socket,
        }
    }

    /// The roster as it stands. Never hold the guard across an await; from an async
    /// caller use [`Env::with_roster`], which cannot leak it.
    pub fn roster(&self) -> std::sync::RwLockReadGuard<'_, Roster> {
        self.roster.read().expect("roster lock")
    }

    /// Reads the roster and returns whatever the closure takes from it. The guard cannot
    /// escape the call, which is what makes this safe to use from an async fn.
    pub fn with_roster<T>(&self, f: impl FnOnce(&Roster) -> T) -> T {
        f(&self.roster())
    }

    /// A snapshot of every known source, in order.
    pub fn source_list(&self) -> Vec<Source> {
        self.roster().sources.clone()
    }

    /// The source answering as `alias`, if this process knows one.
    pub fn source(&self, alias: &str) -> Option<Source> {
        self.roster()
            .sources
            .iter()
            .find(|s| s.alias == alias)
            .cloned()
    }

    /// Registers a source found after launch (async mux discovery). Idempotent: false
    /// when one already answers as that alias.
    pub fn add_source(&self, src: Source) -> bool {
        let mut r = self.roster.write().expect("roster lock");
        if r.sources.iter().any(|s| s.alias == src.alias) {
            return false;
        }
        r.sources.push(src);
        true
    }

    /// Swaps in a freshly resolved roster, CARRYING OVER the sources async mux discovery
    /// added on machines the fresh roster still names.
    ///
    /// The roster names MACHINES; which muxes a machine serves is answered by probing the
    /// machine, and resolving a roster probes nothing remote. Dropping a carried source
    /// would make every re-scan tear a discovered mux card down and re-find it a moment
    /// later.
    pub fn replace_roster(&self, mut fresh: Roster) {
        let mut cur = self.roster.write().expect("roster lock");
        let machines: HashSet<&str> = fresh
            .sources
            .iter()
            .map(|s| crate::session::machine_of(&s.alias))
            .collect();
        let named: HashSet<&str> = fresh.sources.iter().map(|s| s.alias.as_str()).collect();
        let carried: Vec<Source> = cur
            .sources
            .iter()
            .filter(|s| {
                !named.contains(s.alias.as_str())
                    && machines.contains(crate::session::machine_of(&s.alias))
            })
            .cloned()
            .collect();
        drop(machines);
        drop(named);
        fresh.sources.extend(carried);
        *cur = fresh;
    }

    /// Probes every source and returns the merged, name-ordered host/session
    /// groups (used by `ls`, which needs no window/pane detail).
    pub async fn scan(&self) -> Vec<Group> {
        let srcs = self.source_list();
        let results = discovery::scan_all(&srcs, SCAN_TIMEOUT, SCAN_CONCURRENCY).await;
        to_groups(results)
    }

    /// Probes every source and streams each host/session group the moment its
    /// probe resolves, in completion order. Used by `ls` so it can print a source
    /// as soon as it answers instead of appearing frozen while a dead host is
    /// still timing out. The receiver closes after the last probe resolves.
    pub async fn scan_stream(&self) -> mpsc::Receiver<Group> {
        let srcs = self.source_list();
        let mut rx = discovery::scan_stream(&srcs, SCAN_TIMEOUT, SCAN_CONCURRENCY).await;
        let (tx, out) = mpsc::channel(srcs.len().max(1));
        tokio::spawn(async move {
            while let Some(r) = rx.recv().await {
                let mut sessions = r.sessions;
                tree::sort_by_name(&mut sessions);
                let _ = tx
                    .send(Group {
                        source: r.source,
                        err: r.err,
                        sessions,
                    })
                    .await;
            }
        });
        out
    }

    /// Builds the switcher's side-effecting actions over the live mux. A shared
    /// semaphore bounds the concurrent probes (`list-sessions`) the
    /// event loop streams through these ops.
    pub fn ops(self: &Arc<Self>) -> Arc<dyn Ops> {
        Arc::new(EnvOps {
            env: self.clone(),
            sem: Arc::new(tokio::sync::Semaphore::new(SCAN_CONCURRENCY)),
        })
    }
}

/// Renders one scan group for `xmux ls`: the `<source>/<name>` lines of a
/// reachable source, or a single unreachable line for a dead one. Tabs are not
/// used as column separators: a tab advances to the next tab stop, so a first
/// column (`<source>/<name>`) that varies in width pushes every later column
/// onto a different stop and the rows do not line up. Instead each column is
/// padded to the widest cell in the group, so the group's rows share one
/// vertical line regardless of the terminal's tab-stop configuration.
fn window_word(n: i64) -> String {
    if n == 1 {
        "1 window".to_string()
    } else {
        format!("{n} windows")
    }
}

pub fn ls_lines_one(g: &Group) -> (Vec<String>, Option<String>) {
    if let Some(err) = &g.err {
        return (
            Vec::new(),
            Some(format!("{}  (unreachable: {err})", g.source)),
        );
    }
    let addr_w = g
        .sessions
        .iter()
        .map(|s| s.address().len())
        .max()
        .unwrap_or(0);
    let nw_w = g
        .sessions
        .iter()
        .map(|s| window_word(s.windows).len())
        .max()
        .unwrap_or(0);
    let lines = g
        .sessions
        .iter()
        .map(|s| {
            format!(
                "{:<addr_w$}  {:<nw_w$}  attached={}",
                s.address(),
                window_word(s.windows),
                s.attached
            )
        })
        .collect();
    (lines, None)
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
            .source(alias)
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
        self.env
            .source_list()
            .iter()
            .map(|s| s.alias.clone())
            .collect()
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
            mux: host.mux.kind().to_string(),
            windows: 1,
            ..Default::default()
        })
    }

    async fn unlock(
        &self,
        source: &str,
        user: &str,
        password: &str,
    ) -> crate::link::unlock::UnlockOutcome {
        let Ok(src) = self.source(source) else {
            return crate::link::unlock::UnlockOutcome::Unavailable;
        };
        let host = src.host();
        crate::link::unlock::unlock_host(
            &*host.transport,
            user,
            password,
            std::time::Duration::from_secs(UNLOCK_TIMEOUT_SECS),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::source::{RunError, Runner};
    use crate::provision::config::Config;
    use crate::session::Session;

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
            crate::transport::MachineKind::Ssh {
                id: String::new(),
                alias: alias.into(),
                control_path: String::new(),
                os: "linux".into(),
            }
        } else {
            crate::transport::MachineKind::Local {
                id: String::new(),
                socket: None,
            }
        };
        Source {
            alias: alias.into(),
            binary: "tmux".into(),
            kind,
            runner: Some(runner(line)),
        }
    }

    fn env_with(aliases: &[&str]) -> Env {
        Env::new(
            Roster {
                sources: aliases.iter().map(|a| test_source(a, true, "")).collect(),
                ..Default::default()
            },
            "C-g".into(),
            PathBuf::from("."),
            None,
            None,
        )
    }

    fn aliases_of(env: &Env) -> Vec<String> {
        env.source_list().iter().map(|s| s.alias.clone()).collect()
    }

    #[test]
    fn replace_roster_carries_a_discovered_source_on_a_machine_that_survives() {
        // `prod:zellij` is there because a probe ANSWERED. Resolving a roster probes
        // nothing remote, so it cannot name it; carrying it over is what stops every
        // re-scan from dropping the card and re-finding it a moment later.
        let env = env_with(&["prod", "prod:zellij", "stage"]);
        env.replace_roster(Roster {
            sources: vec![
                test_source("prod", true, ""),
                test_source("stage", true, ""),
            ],
            ..Default::default()
        });
        assert_eq!(
            aliases_of(&env),
            vec![
                "prod".to_string(),
                "stage".to_string(),
                "prod:zellij".to_string()
            ],
            "the carried source keeps its place behind the ones the roster named"
        );
    }

    #[test]
    fn replace_roster_drops_a_source_whose_machine_is_gone() {
        let env = env_with(&["prod", "prod:zellij", "stage"]);
        env.replace_roster(Roster {
            sources: vec![test_source("stage", true, "")],
            ..Default::default()
        });
        assert_eq!(
            aliases_of(&env),
            vec!["stage".to_string()],
            "prod is off the roster, so every source it served goes with it"
        );
    }

    #[tokio::test]
    async fn list_sessions_probes_one_source() {
        // EnvOps::list_sessions probes a single source by alias, returning its
        // sessions (the per-host streaming probe the event loop fans out).
        let env = Arc::new(Env::new(
            Roster {
                sources: vec![test_source("local", false, "2\t1\teditor\n")],
                local_muxes: vec!["tmux".into()],
                ..Default::default()
            },
            "C-g".into(),
            PathBuf::from("."),
            None,
            None,
        ));
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
            mux: String::new(),
            windows,
            attached,
        }
    }

    #[test]
    fn ls_lines_one_renders_a_reachable_group_aligned() {
        let g = group(
            "local",
            None,
            vec![
                sess("local", "editor", 2, true),
                sess("local", "build", 1, false),
            ],
        );
        let (lines, unreachable) = ls_lines_one(&g);
        assert_eq!(
            lines,
            vec![
                "local/editor  2 windows  attached=true",
                "local/build   1 window   attached=false"
            ]
        );
        assert!(unreachable.is_none());
    }

    #[test]
    fn ls_lines_one_renders_an_unreachable_group() {
        let g = group("prod", Some("connection refused"), vec![]);
        let (lines, unreachable) = ls_lines_one(&g);
        assert!(lines.is_empty());
        assert_eq!(
            unreachable,
            Some("prod  (unreachable: connection refused)".to_string())
        );
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
    fn the_roster_records_what_offered_each_host() {
        use crate::provision::roster::Provider;
        // `jupiter00` is on the roster twice over: a provider listed it AND a `[[hosts]]`
        // entry names it. The entry overrides its mux, it did not put it on the roster.
        let cfg = Config {
            hosts: vec![
                crate::provision::config::HostConfig {
                    ssh: "written-down".into(),
                    mux: Default::default(),
                },
                crate::provision::config::HostConfig {
                    ssh: "jupiter00".into(),
                    mux: Default::default(),
                },
            ],
            ..Default::default()
        };
        let offered = vec![
            ("jupiter00".to_string(), Provider::SshConfig),
            ("kyla".to_string(), Provider::Tailscale),
        ];
        let got = roster_providers(&cfg, &offered, &["wsl.Ubuntu-24.04".to_string()]);

        assert_eq!(got.get("jupiter00"), Some(&Provider::SshConfig));
        assert_eq!(got.get("kyla"), Some(&Provider::Tailscale));
        assert_eq!(got.get("wsl.Ubuntu-24.04"), Some(&Provider::Wsl));
        // A host no provider listed is offered by the config that names it.
        assert_eq!(got.get("written-down"), Some(&Provider::Config));
        // This box is on the roster without anything offering it.
        assert_eq!(got.get("local"), Some(&Provider::Local));
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
    fn ssh_config_path_prefers_home() {
        // Windows Git Bash/msys can set `$HOME` to a path different from
        // `USERPROFILE`; the ssh config must follow `$HOME` so it matches what the
        // user's ssh reads.
        let saved = std::env::var_os("HOME");
        let tmp = std::env::temp_dir();
        std::env::set_var("HOME", &tmp);
        let got = ssh_config_path();
        assert_eq!(got, tmp.join(".ssh").join("config"));
        match saved {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn ls_lines_one_empty_reachable_group_has_no_lines() {
        // A reachable mux with zero sessions is empty, not failed.
        let g = group("local", None, vec![]);
        let (lines, unreachable) = ls_lines_one(&g);
        assert!(lines.is_empty());
        assert!(unreachable.is_none());
    }

    #[test]
    fn ls_lines_one_all_unreachable_returns_a_line() {
        let g = group("prod", Some("boom"), vec![]);
        let (lines, unreachable) = ls_lines_one(&g);
        assert!(lines.is_empty());
        assert!(unreachable.is_some());
    }

    #[tokio::test]
    async fn to_groups_sorts_sessions_by_name() {
        let results = vec![discovery::ScanResult {
            source: "local".into(),
            sessions: vec![
                Session {
                    source: "local".into(),
                    name: "old".into(),
                    ..Default::default()
                },
                Session {
                    source: "local".into(),
                    name: "new".into(),
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
