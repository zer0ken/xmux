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
const SCAN_TIMEOUT: Duration = crate::mux::POLL_SWEEP_BUDGET;
const DETAIL_TIMEOUT: Duration = crate::mux::POLL_SWEEP_BUDGET;
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
    /// The address a provider reported for a host, keyed by HOST name. Only a provider
    /// that knows one contributes; an ssh-config alias has no address of its own. Read
    /// only to be OFFERED: it seeds the login pane, because a host named by a label this
    /// machine cannot resolve is reachable only by the address the provider knew.
    pub host_addresses: HashMap<String, String>,
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
    remote_shells: source::RemoteShells,
    pub ui_prefix: String,
    pub xmux_dir: PathBuf,
    /// The [`crate::session::Address`] of the session xmux is ITSELF running in
    /// (`local:psmux` + `xmus`), or `None` when it is not inside a mux or the session
    /// could not be named. The one session the terminal view refuses to mirror; see
    /// [`crate::display::attach::own_mux_session`]. Fixed for the run: the environment
    /// that names it cannot change under one.
    pub own_session: Option<crate::session::Address>,
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
    let (neighbors, wsl_distros, installed) = tokio::join!(
        async {
            if cfg.discovery.neighbors {
                crate::provision::neighbor::neighbors().await
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
    let host_addresses: HashMap<String, String> = neighbors
        .iter()
        .filter_map(|(alias, addr)| addr.clone().map(|a| (alias.clone(), a)))
        .collect();
    let neighbors: Vec<String> = neighbors.into_iter().map(|(alias, _)| alias).collect();
    let offered = crate::provision::roster::merge(&[
        (crate::provision::roster::Provider::SshConfig, ssh_aliases),
        (crate::provision::roster::Provider::Neighbor, neighbors),
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
            host_addresses,
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

/// The [`crate::session::Address`] of the session xmux is running in, resolved against
/// the LOCAL sources.
///
/// The mux names the session; this pairs it with the source id that mux answers as on
/// this machine, because the refusal has to match the card exactly. A mux xmux does not
/// serve here leaves it unresolved, which blocks nothing - the same as not being inside
/// a mux at all.
fn own_session_address(srcs: &[Source]) -> Option<crate::session::Address> {
    let (kind, session) = crate::display::attach::own_mux_session()?;
    Some(crate::session::Address::new(
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
        mut roster: Roster,
        ui_prefix: String,
        xmux_dir: PathBuf,
        own_session: Option<crate::session::Address>,
        local_socket: Option<String>,
    ) -> Self {
        let remote_shells = source::RemoteShells::default();
        for source in &mut roster.sources {
            source.remote_shells = remote_shells.clone();
        }
        Env {
            roster: std::sync::RwLock::new(roster),
            remote_shells,
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

    /// Records the shell family a machine's probe read, for every source this
    /// environment holds.
    pub(crate) fn record_remote_shell(
        &self,
        machine: &str,
        shell: crate::transport::vocab::RemoteShell,
    ) {
        self.remote_shells.record(machine, shell);
    }

    /// Registers a source found after launch (async mux discovery). Idempotent: false
    /// when one already answers as that alias.
    pub fn add_source(&self, mut src: Source) -> bool {
        let mut r = self.roster.write().expect("roster lock");
        if r.sources.iter().any(|s| s.alias == src.alias) {
            return false;
        }
        src.remote_shells = self.remote_shells.clone();
        r.sources.push(src);
        true
    }

    /// Carries the machines a PROBE offered into a freshly resolved roster that lost them.
    ///
    /// A roster names a machine from one of two kinds of evidence, and absence means
    /// opposite things for the two. A RECORD that no longer names it (`~/.ssh/config`,
    /// `[[hosts]]`, the wsl list) is someone having removed it, so the machine must go. A
    /// PROBE that did not answer is one round trip that was too slow, and says nothing
    /// about whether the machine exists - a neighbour is offered by connecting to port 22
    /// inside a 700ms budget, which a tunnel hop misses without anything being wrong.
    ///
    /// Reaping on a missed probe tears down the card the user is working in and paints it
    /// again on the next scan. A machine that really did leave still shows: it keeps its
    /// card and reports itself unreachable, which is what a machine named by a record
    /// does when it goes offline, so both kinds behave the same way.
    ///
    /// Everything the machine had is carried, not just its name: the sources (async mux
    /// discovery's included), the provider its card names, and the address the login pane
    /// offers - all of which came from the answer that is now missing.
    pub fn carry_probed(&self, fresh: &mut Roster) {
        use crate::provision::roster::Provider;
        let cur = self.roster.read().expect("roster lock");
        let lost: Vec<String> = cur
            .roster_providers
            .iter()
            .filter(|(machine, provider)| {
                **provider == Provider::Neighbor && !fresh.roster_providers.contains_key(*machine)
            })
            .map(|(machine, _)| machine.clone())
            .collect();
        for machine in lost {
            fresh.sources.extend(
                cur.sources
                    .iter()
                    .filter(|s| crate::session::machine_of(&s.alias) == machine)
                    .cloned(),
            );
            if let Some(addr) = cur.host_addresses.get(&machine) {
                fresh.host_addresses.insert(machine.clone(), addr.clone());
            }
            fresh
                .roster_providers
                .insert(machine.clone(), Provider::Neighbor);
            fresh.ssh_aliases.push(machine);
        }
    }

    /// Swaps in a freshly resolved roster, CARRYING OVER the sources async mux discovery
    /// added on machines the fresh roster still names.
    ///
    /// The roster names MACHINES; which muxes a machine serves is answered by probing the
    /// machine, and resolving a roster probes nothing remote. A machine that leaves its
    /// muxes to xmux has no source in the fresh roster at all, so it is named by the host
    /// list rather than by its sources. Dropping a carried source would make every re-scan
    /// tear a discovered mux card down and re-find it a moment later.
    pub fn replace_roster(&self, mut fresh: Roster) {
        let mut cur = self.roster.write().expect("roster lock");
        let auto = fresh.cfg.auto_hosts(&fresh.ssh_aliases, &fresh.wsl_distros);
        let machines: HashSet<&str> = fresh
            .sources
            .iter()
            .map(|s| crate::session::machine_of(&s.alias))
            .chain(auto.iter().map(String::as_str))
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
        for source in &mut fresh.sources {
            source.remote_shells = self.remote_shells.clone();
        }
        *cur = fresh;
    }

    /// Asks each host on the roster that leaves its muxes to xmux which of them it
    /// serves, and registers a source for every mux that answered, named the way a
    /// written list names them. `only` narrows the question to one host. Returns the
    /// hosts that gained no source, in roster order.
    ///
    /// The app asks this of a host once it connects; a CLI command has no connected host
    /// to wait on, so it asks here, as part of the one request it is, and in the same
    /// order: the host's reachability probe first, and its muxes only once it connected.
    /// Hosts are asked concurrently, and each host one command at a time.
    pub async fn discover_hosts(&self, only: Option<&str>) -> Vec<Unanswered> {
        let machines: Vec<String> = {
            let r = self.roster();
            r.cfg
                .auto_hosts(&r.ssh_aliases, &r.wsl_distros)
                .into_iter()
                .filter(|m| only.is_none_or(|o| o == m))
                .filter(|m| {
                    !r.sources
                        .iter()
                        .any(|s| crate::session::machine_of(&s.alias) == m)
                })
                .collect()
        };
        let sem = Arc::new(tokio::sync::Semaphore::new(SCAN_CONCURRENCY));
        let mut set = tokio::task::JoinSet::new();
        for (i, machine) in machines.iter().cloned().enumerate() {
            let sem = sem.clone();
            let remote_shells = self.remote_shells.clone();
            let transport = crate::transport::kind_for(
                &machine,
                machine.clone(),
                current_os(),
                &self.xmux_dir,
                None,
            )
            .transport();
            set.spawn(async move {
                let _permit = sem.acquire().await.expect("semaphore not closed");
                (i, ask_host(&machine, transport, remote_shells).await)
            });
        }
        let mut answers: Vec<(usize, Result<Vec<String>, String>)> = set.join_all().await;
        answers.sort_by_key(|(i, _)| *i);
        let mut unanswered = Vec::new();
        for (i, answer) in answers {
            let machine = &machines[i];
            let found = match answer {
                Ok(found) => found,
                Err(reason) => {
                    unanswered.push(Unanswered {
                        host: machine.clone(),
                        reason: Some(reason),
                    });
                    continue;
                }
            };
            if found.is_empty() {
                unanswered.push(Unanswered {
                    host: machine.clone(),
                    reason: None,
                });
            }
            for spec in config::host_specs_for(machine, &found) {
                self.add_source(source::for_machine_mux(
                    machine,
                    &spec.bin,
                    spec.id,
                    current_os(),
                    &self.xmux_dir,
                    None,
                ));
            }
        }
        unanswered
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
        .map(|s| s.address().display().len())
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
                s.address().display(),
                window_word(s.windows),
                s.attached
            )
        })
        .collect();
    (lines, None)
}

/// A host [`Env::discover_hosts`] registered no source for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unanswered {
    pub host: String,
    /// Why the host could not be asked, or `None` when it answered and no mux xmux
    /// supports is installed there.
    pub reason: Option<String>,
}

/// One host's reachability probe, then, once it connected, its mux discovery over the
/// shell family the probe read. `Err` carries the reason the host could not be asked.
async fn ask_host(
    machine: &str,
    mut transport: Box<dyn crate::transport::Transport>,
    remote_shells: source::RemoteShells,
) -> Result<Vec<String>, String> {
    use crate::model::source::Runner;
    if transport.is_remote() {
        if let Some(argv) = transport.raw_shell_argv(crate::transport::vocab::SHELL_PROBE) {
            let out = source::ExecRunner
                .run(&argv[0], &argv[1..])
                .await
                .map_err(|e| e.to_string())?;
            let shell = crate::transport::vocab::RemoteShell::from_probe(&out);
            remote_shells.record(machine, shell);
            transport.set_remote_shell(shell);
        }
    }
    crate::mux::host_muxes(&*transport, &source::ExecRunner).await
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
        let mut host = with_timeout(SCAN_TIMEOUT, src.host_for_op()).await?;
        with_timeout(SCAN_TIMEOUT, async {
            host.enumerate_with(src.run_with())
                .await
                .map(|()| host.inventory.sessions)
        })
        .await
    }

    async fn new_session(&self, source: &str, name: &str) -> anyhow::Result<Session> {
        let src = self.source(source)?;
        let host = with_timeout(DETAIL_TIMEOUT, src.host_for_op()).await?;
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

    fn login_argv(&self, source: &str, login: &crate::transport::Login) -> Option<Vec<String>> {
        // A login authenticates the MACHINE, which may serve no source yet: a host whose
        // muxes xmux asks for has none until it answers, and it cannot answer until the
        // login lets xmux in.
        let machine = crate::session::machine_of(source);
        crate::transport::kind_for(
            machine,
            machine.to_string(),
            current_os(),
            &self.env.xmux_dir,
            None,
        )
        .transport()
        .login_argv(login)
    }

    fn login_remote(&self, register_key: bool) -> String {
        if !register_key {
            // The connection itself is the work; the command only has to exit.
            return EXIT_OK.to_string();
        }
        match authorized_keys_command() {
            // Registering ends in the same report an empty login gives, so the verdict
            // stays a verdict on the AUTHENTICATION. A key that did not land leaves the
            // host asking for a password on the next probe, which is the truth about it -
            // and which the user reads on the card, rather than as a login that looks
            // like the password was wrong.
            Ok(cmd) => format!("{cmd}; {EXIT_OK}"),
            // A key that cannot be read or made registers nothing, and the login is still
            // worth having: it accepts the host key and carries the values.
            Err(_) => EXIT_OK.to_string(),
        }
    }

    async fn login_follow_ups(
        &self,
        source: &str,
        login: &crate::transport::Login,
        write_config: bool,
    ) -> Vec<String> {
        let mut notes = Vec::new();
        if write_config {
            if let Err(e) = write_ssh_config_stanza(crate::session::machine_of(source), login) {
                notes.push(format!("ssh config not written: {e}"));
            }
        }
        notes
    }
}

/// Writes the xmux-managed stanza for `machine` into `~/.ssh/config`.
///
/// The file is rewritten whole, from the text just read, so a stanza the user edited
/// between the read and the write is never resurrected from a stale copy. Only lines
/// xmux marked as its own are replaced; everything else is carried across untouched.
fn write_ssh_config_stanza(
    machine: &str,
    login: &crate::transport::Login,
) -> Result<(), std::io::Error> {
    let path = ssh_config_path();
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let next = crate::provision::config::upsert_managed_stanza(&text, machine, login);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, next)
}

/// The remote command that reports a login worked, and nothing else.
///
/// The login's verdict IS this command's exit code, so what it runs must answer one
/// question: did the authentication succeed. `exit 0` is that answer in every shell
/// family, which is what this position needs - the family of a LOCKED host is unknown by
/// construction, because the probe that would have read it never got past the refusal
/// that locked the card. A POSIX-only word here (`true`) is a command a PowerShell remote
/// does not have, so it exits nonzero and an accepted password reads as a refused one.
const EXIT_OK: &str = "exit 0";

/// The remote command that puts this machine's public key in the host's
/// `authorized_keys`, for the login to carry.
///
/// It runs inside the session the user authenticates, which is what makes it work on a
/// platform that keeps no connection afterwards - and what makes it the thing worth doing
/// there, since the key it leaves turns a host that wanted a password into one that wants
/// nothing. Idempotent: the key is added only when that exact line is absent, so a second
/// login changes nothing.
fn authorized_keys_command() -> Result<String, std::io::Error> {
    let key = public_key_line()?;
    // Single-quoted for the remote shell, with the key's own quotes made impossible by
    // the reject below, so nothing in it can end the quoting.
    if key.contains('\'') || key.contains('\n') {
        return Err(std::io::Error::other("the public key is not a plain line"));
    }
    Ok(format!(
        "umask 077; mkdir -p ~/.ssh; touch ~/.ssh/authorized_keys; \
         grep -qxF '{key}' ~/.ssh/authorized_keys || printf '%s\\n' '{key}' >> ~/.ssh/authorized_keys"
    ))
}

/// This machine's public key line, generating an ed25519 pair when it has none.
///
/// The first existing public key wins, in the order ssh itself prefers, so a machine
/// that already has a key registers THAT one rather than growing a second identity.
fn public_key_line() -> Result<String, std::io::Error> {
    // The home SSH itself reads `~` from, so the key xmux sends is the key ssh would
    // offer. See `ssh_home`.
    let dir = ssh_home().join(".ssh");
    for name in ["id_ed25519.pub", "id_ecdsa.pub", "id_rsa.pub"] {
        if let Ok(text) = std::fs::read_to_string(dir.join(name)) {
            let line = text.trim().to_string();
            if !line.is_empty() {
                return Ok(line);
            }
        }
    }
    std::fs::create_dir_all(&dir)?;
    let key = dir.join("id_ed25519");
    let status = std::process::Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(&key)
        .stdin(std::process::Stdio::null())
        .status()?;
    if !status.success() {
        return Err(std::io::Error::other("ssh-keygen failed"));
    }
    Ok(std::fs::read_to_string(key.with_extension("pub"))?
        .trim()
        .to_string())
}

#[cfg(test)]
mod tests {
    /// Serializes the tests that point `$HOME` at a scratch directory. `ssh_home` reads
    /// that variable, so two tests setting it concurrently would hand each other the
    /// other's scratch path.
    static HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// A public key line a scratch HOME holds for the login tests. It never
    /// authenticates anything; it only has to look like a key for the login command
    /// builder.
    const KNOWN_PUBLIC_KEY: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIMwVQxmuxTestKeyNeverUsed xmux@test";

    /// The login's verdict is its remote command's exit code, so whatever the command
    /// does it must end by saying the AUTHENTICATION worked. `exit 0` is that word in
    /// every shell family, which is the requirement here: a locked host's family is
    /// unknown, because the probe that reads it never got past the refusal that locked
    /// the card. A POSIX-only word makes an accepted password read as a refused one on a
    /// PowerShell remote.
    #[test]
    fn every_login_command_ends_by_reporting_the_authentication() {
        let ops = Arc::new(env_with(&["prod"])).ops();
        assert_eq!(
            ops.login_remote(false),
            "exit 0",
            "a login with nothing to carry reports the authentication and stops"
        );
        // A HOME that already holds a key makes the key registration deterministic:
        // the login reads that key instead of asking the machine's ssh-keygen, so the
        // check never depends on the runner's own key state.
        let _guard = HOME_LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!("xmux-env-login-key-{}", std::process::id()));
        let ssh = home.join(".ssh");
        std::fs::create_dir_all(&ssh).unwrap();
        std::fs::write(ssh.join("id_ed25519.pub"), KNOWN_PUBLIC_KEY).unwrap();
        let saved = std::env::var_os("HOME");
        std::env::set_var("HOME", &home);
        let with_key = ops.login_remote(true);
        match saved {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        std::fs::remove_dir_all(&home).ok();
        assert!(
            with_key.ends_with("; exit 0") && with_key.contains(KNOWN_PUBLIC_KEY),
            "registering a key does not get to fail the login: {with_key}"
        );
    }

    /// The key registration is a REMOTE COMMAND the login carries, not a connection
    /// opened afterwards. That is what makes it work where there is no afterwards, and it
    /// must be idempotent, because a second login runs it again.
    #[test]
    fn the_key_command_adds_the_line_only_when_it_is_absent() {
        let Ok(cmd) = authorized_keys_command() else {
            return; // this machine has no key and cannot make one; nothing to check
        };
        assert!(
            cmd.contains("grep -qxF") && cmd.contains(">> ~/.ssh/authorized_keys"),
            "it appends only what is not already there: {cmd}"
        );
        assert!(
            cmd.starts_with("umask 077"),
            "the file it may create is not readable by others: {cmd}"
        );
    }

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

    #[test]
    fn operation_timeouts_leave_the_runner_cleanup_margin() {
        assert!(SCAN_TIMEOUT > crate::mux::POLL_CMD_TIMEOUT);
        assert!(DETAIL_TIMEOUT > crate::mux::POLL_CMD_TIMEOUT);
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

    struct RecordingRunner {
        answers: std::sync::Mutex<std::collections::VecDeque<Vec<u8>>>,
        commands: std::sync::Mutex<Vec<(String, Vec<String>)>>,
    }

    impl RecordingRunner {
        fn new(answers: &[&str]) -> std::sync::Arc<Self> {
            std::sync::Arc::new(Self {
                answers: std::sync::Mutex::new(
                    answers
                        .iter()
                        .map(|answer| answer.as_bytes().to_vec())
                        .collect(),
                ),
                commands: std::sync::Mutex::new(Vec::new()),
            })
        }

        fn commands(&self) -> Vec<(String, Vec<String>)> {
            self.commands.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl Runner for RecordingRunner {
        async fn run(&self, name: &str, args: &[String]) -> Result<Vec<u8>, RunError> {
            self.commands
                .lock()
                .unwrap()
                .push((name.to_string(), args.to_vec()));
            Ok(self.answers.lock().unwrap().pop_front().unwrap_or_default())
        }
    }

    fn remote_psmux(runner: std::sync::Arc<dyn Runner>) -> Source {
        remote_psmux_as("prod", runner)
    }

    fn remote_psmux_as(alias: &str, runner: std::sync::Arc<dyn Runner>) -> Source {
        Source {
            alias: alias.into(),
            binary: "psmux".into(),
            kind: crate::transport::MachineKind::Ssh {
                id: alias.into(),
                alias: "prod".into(),
                control_path: String::new(),
                os: "windows".into(),
            },
            runner: Some(runner),
            remote_shells: Default::default(),
        }
    }

    struct SlowProbeRunner {
        probes: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl Runner for SlowProbeRunner {
        async fn run(&self, _name: &str, args: &[String]) -> Result<Vec<u8>, RunError> {
            if args.last().map(String::as_str) == Some(crate::transport::vocab::SHELL_PROBE) {
                self.probes
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(20)).await;
                Ok(b"\n".to_vec())
            } else {
                Ok(b"api\n".to_vec())
            }
        }
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
            remote_shells: Default::default(),
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
    fn replace_roster_carries_what_a_host_that_writes_no_mux_answered() {
        // A host that leaves its muxes to xmux has no source in any fresh roster: every
        // source it has came from its own answer. The roster still names the HOST, so
        // those sources are carried rather than dropped on every re-scan.
        let env = env_with(&["win"]);
        env.replace_roster(Roster {
            ssh_aliases: vec!["win".to_string()],
            ..Default::default()
        });
        assert_eq!(aliases_of(&env), vec!["win".to_string()]);
    }

    #[test]
    fn a_host_with_no_source_yet_can_be_logged_into() {
        // A login authenticates the machine, and a host whose muxes xmux asks for has no
        // source until it answers, which a locked host cannot do before the login.
        let env = Arc::new(Env::new(
            Roster {
                ssh_aliases: vec!["win".to_string()],
                ..Default::default()
            },
            "C-g".into(),
            PathBuf::from("."),
            None,
            None,
        ));
        assert!(env.source("win").is_none(), "precondition");
        let argv = env
            .ops()
            .login_argv("win", &crate::transport::Login::default())
            .expect("an ssh host has a login");
        assert!(argv.iter().any(|a| a == "win"), "{argv:?}");
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

    /// A neighbour is offered by a 700ms round trip. When it does not answer, the
    /// machine is not gone: everything it had is carried back in, so a card the user is
    /// working in survives a probe that was merely slow.
    #[test]
    fn a_machine_only_a_probe_offered_survives_a_probe_that_missed_it() {
        use crate::provision::roster::Provider;
        let env = Env::new(
            Roster {
                sources: vec![
                    test_source("prod", true, ""),
                    test_source("prod:zellij", true, ""),
                ],
                ssh_aliases: vec!["prod".into()],
                roster_providers: [("prod".to_string(), Provider::Neighbor)].into(),
                host_addresses: [("prod".to_string(), "100.87.27.26".to_string())].into(),
                ..Default::default()
            },
            "C-g".into(),
            PathBuf::from("."),
            None,
            None,
        );
        // The probe answered for nothing this time.
        let mut fresh = Roster::default();
        env.carry_probed(&mut fresh);

        assert_eq!(
            fresh.ssh_aliases,
            vec!["prod".to_string()],
            "the machine is named again, so every registry built from this roster keeps it"
        );
        assert_eq!(
            fresh
                .sources
                .iter()
                .map(|s| s.alias.clone())
                .collect::<Vec<_>>(),
            vec!["prod".to_string(), "prod:zellij".to_string()],
            "the mux discovery found on it is carried too, not just the machine's name"
        );
        assert_eq!(
            fresh.roster_providers.get("prod"),
            Some(&Provider::Neighbor),
            "the card still names what offered it"
        );
        assert_eq!(
            fresh.host_addresses.get("prod").map(String::as_str),
            Some("100.87.27.26"),
            "the login pane still offers the address the probe had found"
        );
    }

    /// A record is the opposite evidence: someone wrote the machine down, so a roster
    /// that no longer names it is someone having removed it, and it must go.
    #[test]
    fn a_machine_a_record_named_is_not_carried_when_the_record_stops_naming_it() {
        use crate::provision::roster::Provider;
        let env = Env::new(
            Roster {
                sources: vec![test_source("prod", true, "")],
                ssh_aliases: vec!["prod".into()],
                roster_providers: [("prod".to_string(), Provider::SshConfig)].into(),
                ..Default::default()
            },
            "C-g".into(),
            PathBuf::from("."),
            None,
            None,
        );
        let mut fresh = Roster::default();
        env.carry_probed(&mut fresh);
        assert!(
            fresh.ssh_aliases.is_empty() && fresh.sources.is_empty(),
            "the stanza is gone, so the machine is gone: {:?}",
            fresh.ssh_aliases
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

    #[tokio::test]
    async fn create_uses_the_recorded_non_posix_shell() {
        let runner = RecordingRunner::new(&["api\n"]);
        let env = Arc::new(Env::new(
            Roster {
                sources: vec![remote_psmux(runner.clone())],
                ..Default::default()
            },
            "C-g".into(),
            PathBuf::from("."),
            None,
            None,
        ));
        env.record_remote_shell("prod", crate::transport::vocab::RemoteShell::Other);

        env.ops().new_session("prod", "api").await.unwrap();

        let commands = runner.commands();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].0, "ssh");
        assert_eq!(
            commands[0].1.last().map(String::as_str),
            Some("psmux new-session -A -d -P -F '#{session_name}' -s api")
        );
    }

    #[tokio::test]
    async fn create_probes_an_unrecorded_remote_before_the_command() {
        let runner = RecordingRunner::new(&["\n", "api\n"]);
        let env = Arc::new(Env::new(
            Roster {
                sources: vec![remote_psmux(runner.clone())],
                ..Default::default()
            },
            "C-g".into(),
            PathBuf::from("."),
            None,
            None,
        ));

        env.ops().new_session("prod", "api").await.unwrap();

        let commands = runner.commands();
        assert_eq!(commands.len(), 2);
        assert_eq!(
            commands[0].1.last().map(String::as_str),
            Some(crate::transport::vocab::SHELL_PROBE)
        );
        assert_eq!(
            commands[1].1.last().map(String::as_str),
            Some("psmux new-session -A -d -P -F '#{session_name}' -s api")
        );
    }

    #[tokio::test]
    async fn concurrent_first_operations_share_one_shell_probe_per_machine() {
        let runner = Arc::new(SlowProbeRunner {
            probes: std::sync::atomic::AtomicUsize::new(0),
        });
        let env = Arc::new(Env::new(
            Roster {
                sources: vec![
                    remote_psmux_as("prod:psmux", runner.clone()),
                    remote_psmux_as("prod:tmux", runner.clone()),
                ],
                ..Default::default()
            },
            "C-g".into(),
            PathBuf::from("."),
            None,
            None,
        ));
        let ops = env.ops();

        let (first, second) = tokio::join!(
            ops.new_session("prod:psmux", "api"),
            ops.new_session("prod:tmux", "api")
        );

        first.unwrap();
        second.unwrap();
        assert_eq!(runner.probes.load(std::sync::atomic::Ordering::SeqCst), 1);
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
            ("kyla".to_string(), Provider::Neighbor),
        ];
        let got = roster_providers(&cfg, &offered, &["wsl.Ubuntu-24.04".to_string()]);

        assert_eq!(got.get("jupiter00"), Some(&Provider::SshConfig));
        assert_eq!(got.get("kyla"), Some(&Provider::Neighbor));
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
        let _guard = HOME_LOCK.lock().unwrap();
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
