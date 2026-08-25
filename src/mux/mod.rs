//! One mux mux per mux. `Box<dyn Mux>` lives inside a `Host`. The method set is
//! exactly what the supervisor + control reader + manage layer call — no feature
//! catalogue. It covers both window operations and session lifecycle (create / kill /
//! rename), so the manage layer routes every mux argv through the mux rather than
//! building it off a bare binary name. The mux owns its binary name and
//! `ServerModel`, so nothing above threads a `bin: &str` or branches on a `remote` bool
//! to pick the model. Every method is transport-blind except `enumerate` (which runs a
//! probe).

use async_trait::async_trait;

use crate::link::HostEvent;
use crate::transport::Transport;
use crate::model::plan::{DeathSignal, EventSource};
use crate::model::server_model::ServerModel;
use crate::mux::vocab as mux;
use crate::session::{Session, WindowPanes};
use crate::source::{RunError, Runner};

mod control;
mod psmux;
mod tmux;
pub mod vocab;
mod zellij;

pub use control::{ControlProtocol, Line, Notif};
pub use psmux::Psmux;
pub use tmux::{Tmux, TmuxControl};
pub use zellij::Zellij;
// Re-export the pure mux vocabulary at the crate::mux root so `crate::mux::<fn>`
// call sites resolve unchanged whether the item is the Mux trait/factory or a
// vocab builder/parser.
pub use vocab::*;

/// Reports whether `err` means "the mux is reachable but has no sessions" rather
/// than "the host is unreachable". tmux exits non-zero with a "no server
/// running" message when idle, so this distinguishes an empty-but-alive mux from
/// a dead one. Only a real command exit (carrying stderr) can be benign; a
/// missing binary or a connect failure is always unreachable.
pub(crate) fn is_no_sessions(err: &RunError) -> bool {
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

/// The aggregate-server enumeration shared by every mux that has a real
/// `list-sessions`: run `<bin> list-sessions -F …` over `transport` via `runner`,
/// parse the rows (tagged with the host id), and classify an error as a
/// reachable-but-empty mux (`Ok(vec![])`) versus an unreachable host (`Err`). tmux
/// always uses it; psmux uses it for a REMOTE host (the local-registry merge is a
/// LOCAL-psmux behavior — `~/.psmux` has no remote awareness).
pub(crate) async fn enumerate_via_list_sessions(
    bin: &str,
    kind: &str,
    transport: &dyn Transport,
    runner: &dyn Runner,
) -> Result<Vec<Session>, RunError> {
    let (name, args) = transport.exec_argv(false, &mux::list_sessions(bin));
    match runner.run(&name, &args).await {
        Ok(out) => Ok(mux::parse_sessions(
            transport.host_id(),
            kind,
            &String::from_utf8_lossy(&out),
        )),
        Err(e) if is_no_sessions(&e) => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}

/// True when `text` (a mux error / exit reason) means "reachable but no server /
/// no sessions" rather than a real transport failure. The control-mode path gets a
/// plain string (the `%exit` / `%error` reason), not a [`RunError`], so it calls
/// this directly. Matches the marker as a line PREFIX so a login banner / MOTD line
/// like "you have no sessions pending" cannot masquerade as the idle mux.
pub(crate) fn reason_is_no_sessions(text: &str) -> bool {
    text.to_lowercase().split('\n').any(|line| {
        let line = line.trim();
        line.starts_with("no server running")
            || line.starts_with("no sessions")
            // zellij's idle message, on stderr with a plain non-zero exit.
            || line.starts_with("no active zellij sessions")
    })
}

/// The per-command budget for one poll sweep. The poll loop's ticker only advances
/// after `poll_once` RETURNS, so a single mux command that never answers freezes that
/// host's whole inventory: every card stays on its loading spinner and no later sweep
/// ever runs. A command that outlives this is abandoned (the child is killed on drop)
/// and retried on the next sweep. Exceeds the ssh connect timeout (5s) so a slow remote
/// is not mistaken for a hung one.
pub(crate) const POLL_CMD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(6);

/// Runs `fut` under [`POLL_CMD_TIMEOUT`], mapping a timeout to a [`RunError`] naming
/// the budget it blew.
async fn within_poll_budget<T>(
    what: &str,
    fut: impl std::future::Future<Output = Result<T, RunError>>,
) -> Result<T, RunError> {
    match tokio::time::timeout(POLL_CMD_TIMEOUT, fut).await {
        Ok(r) => r,
        Err(_) => Err(RunError::Other(format!(
            "{what} did not answer within {}s",
            POLL_CMD_TIMEOUT.as_secs()
        ))),
    }
}

/// An opaque, mux-authored plan for an in-place display-client switch. The driver runs
/// it BLIND through the host's transport and never inspects which variant it is — the
/// variant↔lowering mapping is `run_switch_plan`'s job, not the driver's. Each variant
/// lowers 1:1 to a [`crate::transport::LoweredSwitch`].
pub enum SwitchPlan {
    /// Mux argv(s) to run non-interactively in order via the exec path (psmux:
    /// `switch-client` then `refresh-client`).
    Exec(Vec<Vec<String>>),
    /// A raw shell command to run in the host shell (tmux: read the recorded tty file
    /// and switch+refresh in one shell). A machine with no host shell cannot run it, so
    /// the driver falls back to a reattach.
    Shell(String),
}

/// One mux mux. Methods are the EXACT set the supervisor + control reader +
/// manage layer call. `enumerate` takes `&Transport` because the per-session model
/// runs a probe (registry read + one list-sessions); the shared model runs one
/// command. Every other method is transport-blind.
#[async_trait]
pub trait Mux: Send + Sync {
    /// The canonical mux identity, for mux comparison and diagnostics.
    fn kind(&self) -> &str;

    /// The binary name to invoke on this host.
    fn bin(&self) -> &str;

    /// Per-session vs shared. The supervisor reads this instead of `remote`.
    fn server_model(&self) -> ServerModel;

    /// Whether this mux takes a SERVER SOCKET flag (`-S <path>`), which is what decides
    /// whether a machine may address it over one.
    ///
    /// Deliberately has NO default. A tmux-compatible default is silently wrong for a
    /// mux that refuses tmux's flags outright: zellij exits on an unexpected `-S` before
    /// it reads the verb, so the source it serves can never answer at all. A mux added
    /// later has to answer this itself rather than inherit an answer that breaks it.
    fn takes_server_socket(&self) -> bool;

    /// The mux argv this mux enumerates its sessions with, `argv[0]` the binary.
    ///
    /// Exists to be SHOWN - the unreachable screen states the command behind a failed
    /// scan - so it must be the argv `enumerate` really issues, not a plausible one. The
    /// default is the shared `list-sessions` listing, which is what every mux built on
    /// `enumerate_via_list_sessions` runs; a mux that lists its sessions another way
    /// overrides this beside its own `enumerate`, so the two cannot drift.
    fn list_sessions_plan(&self) -> Vec<String> {
        mux::list_sessions(self.bin())
    }

    /// The mux's own display driver — the per-host orchestration of which PTY to
    /// attach and whether to `switch-client` or reattach on a session change. Each
    /// mux constructs ITS OWN driver, so mux selection lives in the mux family
    /// (never a central `match server_model()`). The driver is zero-sized; the per-host
    /// display state lives on `host.display`/`AttachRegistry`, borrowed through
    /// `DriverCtx`, so a fresh value per call is free.
    fn driver(&self) -> Box<dyn crate::driver::MuxDriver>;

    /// Clones into a fresh box — a spawned poll task needs an owned mux, and a trait
    /// object cannot derive `Clone`. Symmetric with `Transport::clone_box`; each mux
    /// deep-copies itself (identity + invoked binary preserved).
    fn clone_box(&self) -> Box<dyn Mux>;

    /// Lists this host's sessions over `transport`, executing its probe via
    /// `runner` (the real [`ExecRunner`] in production; an injected fake under test).
    /// A reachable empty mux => `Ok(vec![])`; unreachable => `Err`.
    async fn enumerate(
        &self,
        transport: &dyn Transport,
        runner: &dyn Runner,
    ) -> Result<Vec<Session>, RunError>;

    /// The interactive attach argv (`argv[0]` = binary). The window is selected
    /// separately via `select_window_plan`; the transport folds it for a remote
    /// attach when composing the final connection.
    fn attach_plan(&self, session: &str) -> Vec<String>;

    /// An opaque plan that moves xmux's OWN display client to `session` IN PLACE (no
    /// teardown). The driver runs the returned [`SwitchPlan`] blind through the transport,
    /// never inspecting the variant; the `tty >file` / read-back mechanism a shared mux
    /// uses stays inside the mux family, not on this boundary. `display_tty` is the
    /// captured tty of xmux's display client — psmux targets it directly; tmux ignores it
    /// (it reads the tty its attach recorded to a per-host file). `None` (the default) for
    /// a mux that supports no in-place switch, so the driver reattaches instead.
    fn switch_in_place(
        &self,
        _host_key: &str,
        _session: &str,
        _display_tty: Option<&str>,
    ) -> Option<SwitchPlan> {
        None
    }

    /// The control argv for a `-CC` metadata channel. `None` for a mux with no
    /// host-level control stream (it is polled).
    fn control_argv(&self) -> Option<Vec<String>>;

    /// The control-mode wire protocol (line classification + notification→event policy
    /// + command-line builders) the host reader drives this `-CC` channel with. `None`
    /// for a mux with no host-level control stream (it is polled), matching `control_argv`.
    /// The protocol is stateless, so the reference is `'static` (a shared unit struct) —
    /// the host reader/writer threads borrow it for their whole lifetime.
    fn control_protocol(&self) -> Option<&'static dyn ControlProtocol> {
        None
    }

    /// How this host learns a session/attachment died.
    fn death_signal(&self) -> DeathSignal;

    /// The change/event channel for this mux.
    fn event_source(&self) -> EventSource;

    /// One poll sweep for a POLL host: enumerate sessions, then enumerate each
    /// session's panes, emitting a [`HostEvent::Sessions`] followed by one
    /// [`HostEvent::Panes`] per session — the same payloads and order a control
    /// client's metadata path produces. Built from the existing trait methods
    /// (`enumerate`, `list_panes_plan`) plus `parse_panes`, so it is mux-blind and
    /// needs no per-impl override: tmux is control-driven and never calls it; psmux
    /// uses this default. The host manager owns the ticker/cancel lifecycle and calls
    /// this once per tick; `emit` is its sink onto the shared event bus.
    async fn poll_once(
        &self,
        source: &str,
        transport: &dyn Transport,
        runner: &dyn Runner,
        emit: &mut (dyn FnMut(HostEvent) + Send),
    ) {
        let (sessions, err) =
            match within_poll_budget("list-sessions", self.enumerate(transport, runner)).await {
                Ok(s) => (s, None),
                Err(e) => (Vec::new(), Some(e.to_string())),
            };
        let names: Vec<(String, String)> = sessions
            .iter()
            .map(|s| (s.name.clone(), s.address()))
            .collect();
        emit(HostEvent::Sessions {
            source: source.to_string(),
            sessions,
            err,
        });
        for (name, address) in names {
            let argv = self.list_panes_plan(&name);
            let (cmd, args) = transport.exec_argv(false, &argv);
            // A session whose window list cannot be read still gets a `Panes` event, with
            // no windows in it. Emitting nothing would leave the card on its loading
            // spinner forever; an empty answer is the truth (xmux could not read them)
            // and the nav shows the session without a window row.
            let panes = match within_poll_budget("list-panes", runner.run(&cmd, &args)).await {
                Ok(out) => self.parse_panes(&String::from_utf8_lossy(&out)),
                Err(e) => {
                    tracing::warn!(host = %source, session = %name, error = %e, "panes_unreadable");
                    Vec::new()
                }
            };
            emit(HostEvent::Panes { address, panes });
        }
    }

    // The command-plan verbs are tmux-compatible argv builders over `self.bin()`, so
    // every tmux-compatible mux inherits them for free (the north-star additivity). A mux
    // whose argv diverges overrides only the verb it differs on.
    fn list_panes_plan(&self, session: &str) -> Vec<String> {
        mux::list_panes(self.bin(), session)
    }

    /// Reads the output of [`Self::list_panes_plan`] as windows-and-panes. A plan and
    /// the shape of what it prints are one decision, so they are overridden together: a
    /// mux whose argv diverges from tmux's usually prints something else too (zellij
    /// answers in JSON). The default is the tmux tab-delimited [`mux::PANE_FORMAT`]
    /// parser, which every tmux-compatible mux inherits.
    fn parse_panes(&self, out: &str) -> Vec<WindowPanes> {
        mux::parse_panes(out)
    }

    fn select_window_plan(&self, target: &str) -> Vec<String> {
        mux::select_window(self.bin(), target)
    }

    /// The `new-session` argv that creates-or-attaches a DETACHED session (auto-named
    /// when `name` is empty) and prints its assigned name. The manage layer runs it via
    /// the host's `Transport` and reads back the assigned name.
    fn new_session_plan(&self, name: &str) -> Vec<String> {
        mux::new_session(self.bin(), name)
    }

    /// How this mux writes one of its own windows, for a reader who knows the mux and
    /// not xmux. The default is tmux's `{index}:{name}`, which is what tmux's own status
    /// line and `list-windows` print, so every tmux-compatible mux inherits it; a mux
    /// that names its windows differently overrides this and nothing else.
    fn window_label(&self, index: i64, name: &str) -> String {
        format!("{index}:{name}")
    }
}

struct MuxKind {
    name: &'static str,
    make: fn(String) -> Box<dyn Mux>,
}

// `name` is the canonical identity, help-output marker, and conventional binary
// name. tmux is the implicit fallback because tmux has no positive help signal.
fn known_muxes() -> &'static [MuxKind] {
    &[
        MuxKind {
            name: "psmux",
            make: |bin| Box::new(Psmux { bin }),
        },
        MuxKind {
            name: "zellij",
            make: |bin| Box::new(Zellij { bin }),
        },
    ]
}

/// Every mux xmux can drive, by the binary name each is conventionally invoked as.
/// tmux leads because it is the fallback identity (and the conventional mux on every
/// OS but Windows); the rest follow in registry order. This is the CANDIDATE SET for
/// discovery: xmux only ever looks for a mux it already knows how to drive.
pub fn supported_muxes() -> Vec<&'static str> {
    let mut v = vec!["tmux"];
    v.extend(known_muxes().iter().map(|k| k.name));
    v
}

/// The per-probe budget for discovery. Discovery runs BEFORE the first paint, so a
/// binary that never answers must not hold the screen. Far shorter than
/// [`POLL_CMD_TIMEOUT`]: this asks a local binary to print its help, which takes
/// milliseconds when it is there and fails at once when it is not.
const DETECT_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(1500);

/// Which mux is installed on the machine `transport` reaches, out of the ones xmux can
/// drive ([`supported_muxes`]).
///
/// Each candidate is asked with the SAME identity probe a configured mux gets
/// ([`detect_backend`]), and counts as installed only when the binary answers AS the
/// mux it was probed for. That second half matters: a binary that carries a mux's name
/// while being another mux would otherwise become a source whose every command is aimed
/// at the wrong mux.
///
/// PSMUX SHADOWS TMUX. psmux installs a `tmux` alias of itself, and that alias names
/// itself by the name it was invoked under, so no probe can tell it from a real tmux.
/// Where psmux answers, a `tmux` that also answers is therefore that same alias, and
/// taking it would hand a PER-SESSION mux to tmux's SHARED-server driver - so tmux is
/// dropped from that machine's list. This is decided from what ANSWERED, never from the
/// machine's OS, which xmux does not know for a remote (`Ssh.os` is the LOCAL platform,
/// gating ControlMaster). A machine that really serves both writes them in the config,
/// where a name is taken verbatim.
pub async fn installed_muxes(transport: &dyn Transport, runner: &dyn Runner) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for name in supported_muxes() {
        let probe = detect_backend(transport, name, runner);
        if let Ok(Some(mux)) = tokio::time::timeout(DETECT_TIMEOUT, probe).await {
            if mux.kind() == name {
                found.push(name.to_string());
            }
        }
    }
    if found.iter().any(|m| m == "psmux") {
        found.retain(|m| m != "tmux");
    }
    found
}

/// The implicit tmux fallback — the single place that names tmux as the mux any
/// binary/kind decodes to when it matches no `known_muxes()` entry. tmux has no
/// positive help signal, so it cannot be a registry entry; this explicit helper is
/// the one site that materialises it, preserving the invoked binary.
fn tmux_fallback(bin: &str) -> Box<dyn Mux> {
    Box::new(Tmux {
        bin: bin.to_string(),
    })
}

/// Picks a mux by conventional binary name. tmux is the fallback, the same default
/// `[local] mux` and the host specs take.
pub fn for_binary(bin: &str) -> Box<dyn Mux> {
    for k in known_muxes() {
        if k.name == bin {
            return (k.make)(bin.to_string());
        }
    }
    tmux_fallback(bin)
}

/// The server socket to address the mux binary `bin` over: the one this box named, or
/// `None` for a mux that takes no socket flag.
///
/// The two composition sites (the source list and the host registry) call this before
/// handing a socket to the machine axis, so a socket only ever reaches a mux that
/// understands it. The machine axis cannot make this call itself: it names no mux by
/// design, so it injects the socket it is GIVEN and asks nothing about it.
pub fn server_socket_for(bin: &str, socket: Option<String>) -> Option<String> {
    socket.filter(|_| for_binary(bin).takes_server_socket())
}

/// Builds a mux by canonical identity while preserving the binary used to
/// reach it.
pub fn for_kind(kind: &str, bin: &str) -> Box<dyn Mux> {
    for k in known_muxes() {
        if k.name == kind {
            return (k.make)(bin.to_string());
        }
    }
    tmux_fallback(bin)
}

/// How the mux named `kind` writes the window `(index, name)` on screen. The nav holds
/// a session's mux as a kind string, not a [`Mux`], so this is the one call it needs;
/// the convention itself stays with the mux, in [`Mux::window_label`]. An unstamped
/// session (its mux not yet known) reads as tmux, the same fallback every other
/// kind-keyed lookup takes.
pub fn window_label(kind: &str, index: i64, name: &str) -> String {
    for_kind(kind, kind).window_label(index, name)
}

/// True when `name` names a mux xmux actually recognizes — tmux (the implicit
/// fallback) or any of the [`known_muxes`]. A narrower advisory predicate than
/// [`for_binary`]/[`for_kind`], which always fall back to tmux; this lets config
/// validation flag a value that decodes but names no real mux. Reuses
/// `known_muxes()` so a future mux is covered automatically.
pub fn is_recognized(name: &str) -> bool {
    name == "tmux" || known_muxes().iter().any(|k| k.name == name)
}

/// Probes a server's true identity over `transport`, independent of its binary name
/// and `-V` (psmux mimics tmux's `-V`, reporting a fake `tmux 3.3.6`). Two stages:
///
/// 1. `<bin> help` — psmux names itself here (its reliable positive signal). A real
///    tmux has no `help` command (`tmux help` exits non-zero), so a known-mux marker
///    in the output means that mux.
/// 2. `<bin> -V` — reached only when stage 1 carried no marker. A working `-V` is a
///    real tmux; psmux never reaches here because its `help` already matched.
///
/// `Some(mux)` means a probe was conclusive. `None` means BOTH probes failed
/// (unreachable host / missing binary), so the caller keeps its current mux and
/// retries on a later scan.
pub async fn detect_backend(
    transport: &dyn Transport,
    bin: &str,
    runner: &dyn Runner,
) -> Option<Box<dyn Mux>> {
    // psmux identifies itself in `help`; check it first because it lies in `-V`.
    let (name, args) = transport.exec_argv(false, &[bin.to_string(), "help".to_string()]);
    if let Ok(out) = runner.run(&name, &args).await {
        let low = String::from_utf8_lossy(&out).to_lowercase();
        for k in known_muxes() {
            if low.contains(k.name) {
                return Some((k.make)(bin.to_string()));
            }
        }
    }
    // No known-mux marker. A working `-V` is a real tmux (its only positive signal);
    // both probes failing is inconclusive (unreachable / not a mux) → retry later.
    let (name, args) = transport.exec_argv(false, &[bin.to_string(), "-V".to_string()]);
    if runner.run(&name, &args).await.is_ok() {
        return Some(tmux_fallback(bin));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    fn tmux() -> Tmux {
        Tmux { bin: "tmux".into() }
    }

    fn psmux() -> Psmux {
        Psmux {
            bin: "psmux".into(),
        }
    }

    #[test]
    fn window_label_follows_each_mux_own_convention() {
        // tmux prints `index:name` itself, in its status line and `list-windows`, and
        // every tmux-compatible mux inherits that.
        assert_eq!(window_label("tmux", 0, "bash"), "0:bash");
        assert_eq!(window_label("psmux", 2, "editor"), "2:editor");
        // zellij's tab bar shows names alone; the number a reader expects is already in
        // the name zellij gives a fresh tab.
        assert_eq!(window_label("zellij", 0, "Tab #1"), "Tab #1");
        assert_eq!(window_label("zellij", 2, "deploy"), "deploy");
        // A session whose mux is not stamped yet reads as tmux, like every other
        // kind-keyed lookup.
        assert_eq!(window_label("", 1, "bash"), "1:bash");
    }

    #[test]
    fn tmux_is_shared_and_named() {
        let m = tmux();
        assert_eq!(m.kind(), "tmux");
        assert_eq!(m.server_model(), ServerModel::Shared);
    }

    #[test]
    fn tmux_is_object_safe() {
        // The whole point: a Box<dyn Mux> must compile. If the trait gains a
        // non-dispatchable method this stops compiling.
        let _m: Box<dyn Mux> = Box::new(tmux());
    }

    #[test]
    fn tmux_attach_plan_is_plain_attach() {
        let m = tmux();
        // The window is selected separately (select_window_plan); attach stays plain.
        assert_eq!(m.attach_plan("api"), argv(&["tmux", "attach", "-t", "api"]));
    }

    #[test]
    fn tmux_control_attach_and_event_and_death() {
        let m = tmux();
        assert_eq!(m.control_argv(), Some(argv(&["tmux", "-CC", "attach"])));
        assert_eq!(m.event_source(), EventSource::Control);
        assert_eq!(m.death_signal(), DeathSignal::ControlNotice);
    }

    #[test]
    fn tmux_read_plans_match_mux_builders() {
        let m = tmux();
        assert_eq!(m.list_panes_plan("work"), mux::list_panes("tmux", "work"));
        assert_eq!(
            m.select_window_plan("api:2"),
            mux::select_window("tmux", "api:2")
        );
    }

    #[test]
    fn tmux_session_plans_match_mux_builders() {
        let m = tmux();
        assert_eq!(m.new_session_plan("dev"), mux::new_session("tmux", "dev"));
        assert_eq!(m.new_session_plan(""), mux::new_session("tmux", ""));
    }

    /// A minimal tmux-compatible mux that implements ONLY the required `Mux` methods
    /// — none of the command-plan verbs. It must still compile and get every command
    /// plan for free from the trait defaults (the north-star additivity: a new
    /// tmux-compatible mux = identity + a few methods, the verbs are free).
    struct BareMux {
        bin: String,
    }

    #[async_trait]
    impl Mux for BareMux {
        /// tmux-shaped, like the fake itself.
        fn takes_server_socket(&self) -> bool {
            true
        }

        fn kind(&self) -> &str {
            "bare"
        }
        fn bin(&self) -> &str {
            &self.bin
        }
        fn server_model(&self) -> ServerModel {
            ServerModel::Shared
        }
        fn driver(&self) -> Box<dyn crate::driver::MuxDriver> {
            Box::new(crate::mux::tmux::TmuxDriver)
        }
        fn clone_box(&self) -> Box<dyn Mux> {
            Box::new(BareMux {
                bin: self.bin.clone(),
            })
        }
        async fn enumerate(
            &self,
            transport: &dyn Transport,
            runner: &dyn Runner,
        ) -> Result<Vec<Session>, RunError> {
            enumerate_via_list_sessions(&self.bin, "bare", transport, runner).await
        }
        fn attach_plan(&self, session: &str) -> Vec<String> {
            mux::attach(&self.bin, session)
        }
        fn control_argv(&self) -> Option<Vec<String>> {
            None
        }
        fn death_signal(&self) -> DeathSignal {
            DeathSignal::ControlNotice
        }
        fn event_source(&self) -> EventSource {
            EventSource::Control
        }
    }

    #[test]
    fn bare_tmux_compatible_mux_gets_command_plans_for_free() {
        // The command-plan verbs are trait defaults over `self.bin()`, so a bare
        // tmux-compatible mux inherits byte-identical plans without overriding them.
        let m = BareMux { bin: "tmux".into() };
        assert_eq!(m.list_panes_plan("work"), mux::list_panes("tmux", "work"));
        assert_eq!(m.new_session_plan("dev"), mux::new_session("tmux", "dev"));
    }

    #[test]
    fn mux_clone_box_preserves_identity_and_binary() {
        // A poll task needs an owned mux; `clone_box` deep-copies a `Box<dyn Mux>`
        // preserving both identity and invoked binary (the `Transport::clone_box` idiom).
        let p: Box<dyn Mux> = psmux().clone_box();
        assert_eq!(p.kind(), "psmux");
        assert_eq!(p.bin(), "psmux");
        let t: Box<dyn Mux> = Tmux {
            bin: "custom".into(),
        }
        .clone_box();
        assert_eq!(t.kind(), "tmux");
        assert_eq!(t.bin(), "custom");
    }

    // LIVE: enumerate over a real local tmux server. `#[ignore]` (needs tmux + a
    // server). Run on demand:
    //   cargo test --lib mux::tests::tmux_enumerate_live -- --ignored --nocapture
    #[ignore = "live: needs a running local tmux server"]
    #[tokio::test]
    async fn tmux_enumerate_live() {
        let t = crate::transport::local(None);
        let sessions = tmux()
            .enumerate(&t, &crate::source::ExecRunner)
            .await
            .expect("reachable tmux (empty is Ok)");
        eprintln!(
            "local tmux sessions: {:?}",
            sessions.iter().map(|s| &s.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn psmux_is_per_session_and_named() {
        let m = psmux();
        assert_eq!(m.kind(), "psmux");
        assert_eq!(m.server_model(), ServerModel::PerSession);
    }

    #[test]
    fn psmux_is_object_safe() {
        let _m: Box<dyn Mux> = Box::new(psmux());
    }

    #[test]
    fn psmux_polls_and_dies_on_registry_stat() {
        // No host-level control stream: it is polled at its own `event_source` interval
        // (the manager's poll task uses this cadence). Death is the per-session registry stat.
        let m = psmux();
        assert_eq!(m.control_argv(), None);
        assert_eq!(m.event_source(), EventSource::Poll { interval_ms: 1500 });
        assert_eq!(
            m.death_signal(),
            DeathSignal::PathStat {
                dir_is_psmux_registry: true
            }
        );
    }

    #[test]
    fn psmux_attach_plan_routes_to_the_per_session_server() {
        // psmux is one-server-per-session, so the display attach must use
        // `new-session -A -s <name>` (routes to that session's own server) rather
        // than a bare `attach -t <name>` on the default socket (a warm clone).
        assert_eq!(
            psmux().attach_plan("work"),
            argv(&["psmux", "new-session", "-A", "-s", "work"])
        );
    }

    #[test]
    fn psmux_read_plans_use_the_psmux_binary() {
        let m = psmux();
        assert_eq!(m.list_panes_plan("work"), mux::list_panes("psmux", "work"));
        assert_eq!(
            m.select_window_plan("work:1"),
            mux::select_window("psmux", "work:1")
        );
    }

    #[test]
    fn psmux_session_plans_use_the_psmux_binary() {
        let m = psmux();
        assert_eq!(m.new_session_plan("dev"), mux::new_session("psmux", "dev"));
        assert_eq!(m.new_session_plan(""), mux::new_session("psmux", ""));
    }

    #[test]
    fn psmux_behavior_is_decoupled_from_invoked_binary() {
        let m = Psmux { bin: "tmux".into() };
        assert_eq!(
            m.attach_plan("api"),
            argv(&["tmux", "new-session", "-A", "-s", "api"])
        );
        assert_eq!(m.server_model(), ServerModel::PerSession);
        assert_eq!(m.kind(), "psmux");
    }

    #[test]
    fn tmux_behavior_is_decoupled_from_invoked_binary() {
        let m = Tmux {
            bin: "psmux".into(),
        };
        assert_eq!(
            m.attach_plan("api"),
            argv(&["psmux", "attach", "-t", "api"])
        );
        assert_eq!(m.server_model(), ServerModel::Shared);
        assert_eq!(m.kind(), "tmux");
    }

    /// Answers the two detection probes (`help` and `-V`) independently so a test can
    /// model a real tmux (help fails, `-V` succeeds), a psmux (help names itself), or
    /// an unreachable host (both fail). `None` for a probe ⇒ that probe errors.
    struct ProbeRunner {
        help: Option<Vec<u8>>,
        version: Option<Vec<u8>>,
    }

    impl ProbeRunner {
        fn new(help: Option<&str>, version: Option<&str>) -> Self {
            ProbeRunner {
                help: help.map(|s| s.as_bytes().to_vec()),
                version: version.map(|s| s.as_bytes().to_vec()),
            }
        }
    }

    #[async_trait]
    impl Runner for ProbeRunner {
        async fn run(&self, _name: &str, args: &[String]) -> Result<Vec<u8>, RunError> {
            // The `-V` probe's arg is `-V` (local) or `<bin> -V` (ssh-wrapped); anything
            // else is the `help` probe.
            let probe = if args.iter().any(|a| a.contains("-V")) {
                &self.version
            } else {
                &self.help
            };
            probe.clone().ok_or_else(|| RunError::Other("down".into()))
        }
    }

    /// Answers the identity probes as a machine that HAS exactly `present`, with
    /// `help_marker` overriding what a present binary names itself as (the shape of a
    /// binary that carries a mux's name but is really another mux). An absent binary
    /// fails both probes, the way a missing one does.
    struct MachineWith {
        present: Vec<&'static str>,
        help_marker: Option<&'static str>,
    }

    impl MachineWith {
        fn new(present: &[&'static str]) -> Self {
            MachineWith {
                present: present.to_vec(),
                help_marker: None,
            }
        }
    }

    #[async_trait]
    impl Runner for MachineWith {
        async fn run(&self, name: &str, args: &[String]) -> Result<Vec<u8>, RunError> {
            if !self.present.contains(&name) {
                return Err(RunError::Other("no such binary".into()));
            }
            if args.iter().any(|a| a == "-V") {
                return Ok(format!("{name} 1.2.3").into_bytes());
            }
            // `help`: a real tmux has no such command, which is why it needs `-V`.
            let says = self.help_marker.unwrap_or(name);
            if says == "tmux" {
                return Err(RunError::Other("usage: tmux [command]".into()));
            }
            Ok(format!("{says} - a terminal workspace").into_bytes())
        }
    }

    #[test]
    fn discovery_only_looks_for_muxes_xmux_can_drive() {
        // The candidate set IS the supported set, so discovery can never turn up a name
        // xmux has no family for: every candidate resolves to a mux of its own kind.
        let names = supported_muxes();
        assert_eq!(names, vec!["tmux", "psmux", "zellij"]);
        for name in names {
            assert_eq!(for_binary(name).kind(), name, "{name} must be drivable");
        }
    }

    #[tokio::test]
    async fn a_machine_offers_every_supported_mux_it_actually_has() {
        // The reported case: zellij is up on this box but nothing in the config says so.
        // Discovery asks each supported mux whether it is here, in the supported order.
        let t = crate::transport::local(None);
        let got = installed_muxes(&t, &MachineWith::new(&["tmux", "zellij"])).await;
        assert_eq!(got, vec!["tmux", "zellij"]);
    }

    #[tokio::test]
    async fn where_psmux_answers_a_tmux_is_its_own_alias() {
        // psmux installs a `tmux` alias of itself, and that alias names ITSELF by the name
        // it was invoked under, so the identity probe reads it as a real tmux. Taking it
        // would drive a per-session mux through tmux's shared-server driver. Decided from
        // what ANSWERED, not from the OS - which xmux does not know for a remote.
        let t = crate::transport::local(None);
        assert_eq!(
            installed_muxes(&t, &MachineWith::new(&["tmux", "psmux"])).await,
            vec!["psmux"]
        );
        // No psmux, so a tmux that answers is a tmux.
        assert_eq!(
            installed_muxes(&t, &MachineWith::new(&["tmux", "zellij"])).await,
            vec!["tmux", "zellij"]
        );
    }

    #[tokio::test]
    async fn a_machine_with_no_mux_installed_discovers_none() {
        // Nothing answered, so discovery reports nothing; the CONVENTIONAL fallback is
        // the config layer's job (`Config::local_muxes`), not this probe's.
        let t = crate::transport::local(None);
        assert!(installed_muxes(&t, &MachineWith::new(&[])).await.is_empty());
    }

    #[tokio::test]
    async fn a_binary_that_answers_as_another_mux_is_not_that_mux() {
        // A `tmux` on the PATH that is really psmux (psmux mimics tmux's `-V`, so only
        // `help` tells them apart): counting it as tmux would create a source whose every
        // command is aimed at the wrong mux. Present-but-lying is not installed.
        let t = crate::transport::local(None);
        let runner = MachineWith {
            present: vec!["tmux"],
            help_marker: Some("psmux"),
        };
        assert!(installed_muxes(&t, &runner).await.is_empty());
    }

    /// Answers `list-sessions` and then NEVER answers the pane query - the shape a
    /// hung mux command takes (a zellij session whose server is gone, an ssh that
    /// stalls after the handshake).
    struct HangingPanesRunner;

    #[async_trait]
    impl Runner for HangingPanesRunner {
        async fn run(&self, _name: &str, args: &[String]) -> Result<Vec<u8>, RunError> {
            if args.iter().any(|a| a.contains("list-panes")) {
                std::future::pending::<()>().await;
            }
            Ok(b"1	0	0	api
"
            .to_vec())
        }
    }

    /// Never answers anything.
    struct HangingRunner;

    #[async_trait]
    impl Runner for HangingRunner {
        async fn run(&self, _name: &str, _args: &[String]) -> Result<Vec<u8>, RunError> {
            std::future::pending::<()>().await;
            unreachable!()
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_hung_pane_query_ends_the_sweep_with_an_empty_answer() {
        // The poll loop's ticker only advances after a sweep RETURNS, so a command that
        // never answers would freeze the host's whole inventory. The sweep must end, and
        // the session must still get a `Panes` event - an empty one - because emitting
        // nothing leaves the card on a loading spinner no later sweep resolves.
        // Reaching the assertions at all is the proof that the sweep returned.
        let m = tmux();
        let transport = crate::transport::local(None);
        let mut events = Vec::new();
        m.poll_once("local", &transport, &HangingPanesRunner, &mut |ev| {
            events.push(ev)
        })
        .await;
        assert_eq!(events.len(), 2, "one Sessions, one Panes");
        match &events[0] {
            HostEvent::Sessions { sessions, err, .. } => {
                assert_eq!(sessions.len(), 1);
                assert!(err.is_none(), "the listing answered");
            }
            _ => panic!("want Sessions first"),
        }
        match &events[1] {
            HostEvent::Panes { address, panes } => {
                assert_eq!(address, "local/api");
                assert!(panes.is_empty(), "no windows could be read");
            }
            _ => panic!("want Panes second"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_hung_listing_ends_the_sweep_as_an_error_not_a_freeze() {
        // Same budget on the other half of the sweep: an unanswered listing surfaces as
        // the host's error, which the nav shows as unreachable, instead of stalling the
        // loop forever with nothing on screen.
        let m = tmux();
        let transport = crate::transport::local(None);
        let mut events = Vec::new();
        m.poll_once("local", &transport, &HangingRunner, &mut |ev| {
            events.push(ev)
        })
        .await;
        assert_eq!(events.len(), 1, "no session to ask panes for");
        match &events[0] {
            HostEvent::Sessions { sessions, err, .. } => {
                assert!(sessions.is_empty());
                let err = err.as_deref().unwrap_or_default();
                assert!(
                    err.contains("list-sessions") && err.contains("did not answer"),
                    "the error names what went quiet: {err:?}"
                );
            }
            _ => panic!("want Sessions"),
        }
    }

    #[tokio::test]
    async fn detect_backend_classifies_psmux_by_help_marker() {
        let transport = crate::transport::local(None);
        // psmux names itself in `help`; `-V` is never reached (it would lie "tmux 3.3.6").
        let runner = ProbeRunner::new(Some("usage: PsMuX help"), Some("tmux 3.3.6"));
        let got = detect_backend(&transport, "tmux", &runner).await.unwrap();
        assert_eq!(got.kind(), "psmux");
        assert_eq!(got.server_model(), ServerModel::PerSession);
        assert_eq!(
            got.attach_plan("api"),
            argv(&["tmux", "new-session", "-A", "-s", "api"])
        );
    }

    #[tokio::test]
    async fn detect_backend_classifies_zellij_by_help_marker() {
        // zellij names itself in its help banner, the same positive signal psmux gives,
        // so it is identified without ever reaching the `-V` tmux fallback.
        let transport = crate::transport::local(None);
        let runner = ProbeRunner::new(
            Some(
                "A terminal workspace with batteries included

Usage: zellij [OPTIONS]",
            ),
            Some("tmux 3.5a"),
        );
        let got = detect_backend(&transport, "zellij", &runner).await.unwrap();
        assert_eq!(got.kind(), "zellij");
        assert_eq!(got.server_model(), ServerModel::PerSession);
        assert_eq!(got.attach_plan("api"), argv(&["zellij", "attach", "api"]));
    }

    #[tokio::test]
    async fn detect_backend_classifies_real_tmux_via_version_when_help_errors() {
        // Regression: real tmux has no `help` command (`tmux help` exits non-zero), so
        // the help probe errors. The `-V` fallback must still identify it as tmux —
        // otherwise a correctly-configured tmux host never gets detected/connected.
        let transport = crate::transport::local(None);
        let runner = ProbeRunner::new(None, Some("tmux 3.5a"));
        let got = detect_backend(&transport, "tmux", &runner).await.unwrap();
        assert_eq!(got.kind(), "tmux");
        assert_eq!(got.server_model(), ServerModel::Shared);
    }

    #[tokio::test]
    async fn detect_backend_classifies_tmux_when_help_lacks_marker() {
        // A `help` that succeeds without a known-mux marker still falls through to `-V`.
        let transport = crate::transport::local(None);
        let runner = ProbeRunner::new(Some("usage: tmux commands"), Some("tmux 3.5a"));
        let got = detect_backend(&transport, "tmux", &runner).await.unwrap();
        assert_eq!(got.kind(), "tmux");
        assert_eq!(got.server_model(), ServerModel::Shared);
    }

    // LIVE: probe the REAL detect_backend against the configured hosts. `#[ignore]`
    // (needs ssh jupiter00 + a local psmux). Run on demand:
    //   cargo test --lib mux::tests::detect_backend_live -- --ignored --nocapture
    #[ignore = "live: needs ssh jupiter00 and local psmux"]
    #[tokio::test]
    async fn detect_backend_live() {
        use crate::source::ExecRunner;
        let ssh = crate::transport::ssh("jupiter00".into(), String::new(), "windows".into());
        let got = detect_backend(&ssh, "tmux", &ExecRunner).await;
        eprintln!(
            "DETECT jupiter00/tmux -> {:?}",
            got.as_ref().map(|m| (m.kind(), m.server_model()))
        );
        let local = crate::transport::local(None);
        let got = detect_backend(&local, "psmux", &ExecRunner).await;
        eprintln!(
            "DETECT local/psmux -> {:?}",
            got.as_ref().map(|m| (m.kind(), m.server_model()))
        );
    }

    #[tokio::test]
    async fn detect_backend_both_probes_fail_is_inconclusive() {
        // Unreachable host / missing binary: both probes error ⇒ None (retry later).
        let transport = crate::transport::local(None);
        let runner = ProbeRunner::new(None, None);
        assert!(detect_backend(&transport, "tmux", &runner).await.is_none());
    }

    #[test]
    fn for_binary_picks_psmux_else_tmux() {
        assert_eq!(for_binary("psmux").kind(), "psmux");
        assert_eq!(for_binary("psmux").server_model(), ServerModel::PerSession);
        assert_eq!(for_binary("tmux").kind(), "tmux");
        assert_eq!(for_binary("tmux").server_model(), ServerModel::Shared);
        // Any non-psmux binary defaults to tmux (matches Config::local_bin's default).
        assert_eq!(for_binary("").kind(), "tmux");
        assert_eq!(for_binary("some-fork-of-tmux").kind(), "tmux");
    }

    #[test]
    fn for_kind_preserves_identity_and_invoked_binary() {
        let p = for_kind("psmux", "tmux");
        assert_eq!(p.kind(), "psmux");
        assert_eq!(p.bin(), "tmux");
        assert_eq!(p.event_source(), EventSource::Poll { interval_ms: 1500 });

        let t = for_kind("tmux", "psmux");
        assert_eq!(t.kind(), "tmux");
        assert_eq!(t.bin(), "psmux");
        assert_eq!(t.event_source(), EventSource::Control);
    }

    #[test]
    fn fallback_preserves_the_invoked_binary() {
        // The tmux fallback (a binary that matches no known mux) keeps its invoked
        // binary while reporting the tmux identity — pinned so folding the three
        // fallback sites into one shared helper stays byte-identical.
        assert_eq!(for_binary("some-fork").kind(), "tmux");
        assert_eq!(for_binary("some-fork").bin(), "some-fork");
        assert_eq!(for_kind("nope", "tcustom").kind(), "tmux");
        assert_eq!(for_kind("nope", "tcustom").bin(), "tcustom");
    }

    #[test]
    fn zellij_resolves_by_binary_name_and_by_kind() {
        // The registry is what makes a mux reachable from config: a `mux = "zellij"`
        // entry and a `zellij` binary must both land on the zellij impl, and the
        // invoked binary is preserved either way.
        assert_eq!(for_binary("zellij").kind(), "zellij");
        assert_eq!(for_kind("zellij", "zellij-nightly").kind(), "zellij");
        assert_eq!(for_kind("zellij", "zellij-nightly").bin(), "zellij-nightly");
    }

    #[test]
    fn is_recognized_covers_tmux_and_known_muxes() {
        assert!(is_recognized("tmux"));
        assert!(is_recognized("psmux"));
        assert!(is_recognized("zellij"));
        assert!(!is_recognized("byobu"));
        assert!(!is_recognized(""));
    }

    #[test]
    fn reason_is_no_sessions_matches_line_prefix_markers() {
        assert!(reason_is_no_sessions("no sessions"));
        assert!(reason_is_no_sessions(
            "no server running on /tmp/tmux-1000/default"
        ));
        assert!(!reason_is_no_sessions("connection timed out"));
        // Not a line prefix → not the idle mux (a MOTD must not masquerade).
        assert!(!reason_is_no_sessions("you have no sessions pending"));
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

    /// Always errors — models an unreachable poll host (ssh connect failure).
    struct FailRunner;

    #[async_trait]
    impl Runner for FailRunner {
        async fn run(&self, _name: &str, _args: &[String]) -> Result<Vec<u8>, RunError> {
            Err(RunError::Other("ssh: connect to host down".into()))
        }
    }

    /// A poll sweep whose enumeration ERRORS must still surface `err` on the emitted
    /// `Sessions` event: the event payload is the signal that a transient failure
    /// happened (the tree shows it, attachments are kept), not just a debug-log line. A
    /// remote psmux enumerates via list-sessions over ssh, so a failed run becomes
    /// `Sessions { err: Some(_) }`.
    #[tokio::test]
    async fn poll_once_surfaces_enumeration_error_on_sessions_event() {
        use crate::link::HostEvent;
        let transport = crate::transport::ssh("down-host".into(), String::new(), "linux".into());
        let mut events: Vec<HostEvent> = Vec::new();
        psmux()
            .poll_once("down-host", &transport, &FailRunner, &mut |e| {
                events.push(e)
            })
            .await;
        // Exactly the Sessions event fires (no panes — enumeration returned nothing),
        // and it carries the error so a transient poll failure stays observable.
        let sessions_ev = events
            .iter()
            .find(|e| matches!(e, HostEvent::Sessions { .. }))
            .expect("poll_once emits a Sessions event");
        let HostEvent::Sessions {
            source,
            sessions,
            err,
        } = sessions_ev
        else {
            unreachable!()
        };
        assert_eq!(source, "down-host");
        assert!(
            sessions.is_empty(),
            "a failed enumeration yields no sessions"
        );
        assert!(
            err.is_some(),
            "the error must surface on the Sessions event"
        );
        // No session names ⇒ no per-session Panes follow-up.
        assert!(!events.iter().any(|e| matches!(e, HostEvent::Panes { .. })));
    }

    /// A SUCCESSFUL poll sweep emits `Sessions { err: None }` then one `Panes` per
    /// session — the order and payloads a control client's metadata path produces.
    #[tokio::test]
    async fn poll_once_emits_sessions_then_panes_on_success() {
        use crate::link::HostEvent;

        /// Answers list-sessions with one session, then list-panes with one pane.
        struct OkRunner;
        #[async_trait]
        impl Runner for OkRunner {
            async fn run(&self, _name: &str, args: &[String]) -> Result<Vec<u8>, RunError> {
                let joined = args.join(" ");
                if joined.contains("list-panes") {
                    // win_idx, win_active, pane_idx, pane_active, command, win_name.
                    Ok(b"0\t1\t0\t1\tbash\twork\n".to_vec())
                } else {
                    // session row parsed by mux::parse_sessions.
                    Ok(b"1\t1\t1700000000\twork\n".to_vec())
                }
            }
        }

        let transport = crate::transport::ssh("host".into(), String::new(), "linux".into());
        let mut events: Vec<HostEvent> = Vec::new();
        psmux()
            .poll_once("host", &transport, &OkRunner, &mut |e| events.push(e))
            .await;
        // Sessions first, then Panes — same order as today.
        match &events[0] {
            HostEvent::Sessions {
                source,
                sessions,
                err,
            } => {
                assert_eq!(source, "host");
                assert_eq!(sessions.len(), 1);
                assert!(err.is_none());
            }
            _ => panic!("first event must be Sessions"),
        }
        assert!(matches!(events.get(1), Some(HostEvent::Panes { .. })));
    }
    #[test]
    fn a_socket_reaches_only_a_mux_that_takes_one() {
        // The whole point of asking: zellij exits on an unexpected `-S` before it reads
        // the verb, so a socket handed to it makes its source permanently unreachable.
        let sock = || Some("/tmp/psmux/default".to_string());
        assert_eq!(server_socket_for("tmux", sock()), sock());
        assert_eq!(server_socket_for("psmux", sock()), sock());
        assert_eq!(server_socket_for("zellij", sock()), None);
        // An unknown binary is reached the tmux way, and takes a socket the same way.
        assert_eq!(server_socket_for("mux-of-the-future", sock()), sock());
        // No socket to hand on stays no socket, whoever the mux is.
        assert_eq!(server_socket_for("tmux", None), None);
        assert_eq!(server_socket_for("zellij", None), None);
    }
}
