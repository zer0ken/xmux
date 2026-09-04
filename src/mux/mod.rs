//! One mux mux per mux. `Box<dyn Mux>` lives inside a `Host`. The method set is
//! exactly what the supervisor + control reader + manage layer call - no feature
//! catalogue. It covers both window operations and session lifecycle (create / kill /
//! rename), so the manage layer routes every mux argv through the mux rather than
//! building it off a bare binary name. The mux owns its binary name and
//! `ServerModel`, so nothing above threads a `bin: &str` or branches on a `remote` bool
//! to pick the model. Every method is transport-blind except `enumerate` (which runs a
//! probe).

use async_trait::async_trait;

use crate::link::HostEvent;
use crate::model::plan::{DeathSignal, EventSource};
use crate::model::server_model::ServerModel;
use crate::model::source::{RunError, Runner};
use crate::mux::vocab as mux;
use crate::session::Session;
use crate::transport::Transport;

mod abduco;
mod control;
mod psmux;
mod screen;
mod tmux;
pub mod vocab;
mod zellij;

pub use abduco::{Abduco, AbducoDriver};
pub use control::{ControlProtocol, Line, Notif};
pub use psmux::Psmux;
pub use screen::Screen;
pub use tmux::{Tmux, TmuxControl};
pub use zellij::Zellij;
// Re-export the pure mux builders at the crate::mux root so `crate::mux::<fn>`
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
    // never a healthy-but-empty mux - a broken host must not be hidden as empty.
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
/// LOCAL-psmux behavior - `~/.psmux` has no remote awareness).
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

/// True when `text` is ssh's canonical AUTH-failure line (`Permission denied (…`
/// with the rejected-methods list), meaning the host was REACHED but refused the
/// credentials: the locked state, distinct from unreachable. The `(` after
/// "Permission denied" is ssh's own signature; a generic mux permission error or
/// a reach failure ("Connection refused" / "Host key verification failed") does not
/// carry it. Conservative on purpose: a false positive invites a password entry on
/// a host that is merely down.
pub(crate) fn is_locked(text: &str) -> bool {
    text.contains("Permission denied (")
}

/// The per-command budget [`ExecRunner`] applies to itself, so a command that never
/// answers is torn down cleanly (kill → drain → wait) rather than the sweep's
/// cancellation dropping pipe reads in flight (which crashes on Windows - see
/// `source.rs`). Exceeds the ssh connect timeout (5s) so a slow remote is not mistaken
/// for a hung one.
pub(crate) const POLL_CMD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(6);

/// The sweep-level backstop around `runner.run`, one second longer than
/// [`POLL_CMD_TIMEOUT`] so a real command's own clean teardown always finishes before
/// this fires. It exists to bound a runner that does NOT self-limit - a fake runner
/// under test, or a future runner that forgets its own budget - so a hung command can
/// never freeze the whole poll sweep.
const POLL_SWEEP_BUDGET: std::time::Duration =
    std::time::Duration::from_secs(POLL_CMD_TIMEOUT.as_secs() + 1);

/// Runs `fut` under [`POLL_SWEEP_BUDGET`], mapping a timeout to a [`RunError`] naming
/// the budget it blew.
async fn within_poll_budget<T>(
    what: &str,
    fut: impl std::future::Future<Output = Result<T, RunError>>,
) -> Result<T, RunError> {
    match tokio::time::timeout(POLL_SWEEP_BUDGET, fut).await {
        Ok(r) => r,
        Err(_) => Err(RunError::Other(format!(
            "{what} did not answer within {}s",
            POLL_SWEEP_BUDGET.as_secs()
        ))),
    }
}

/// An opaque, mux-authored plan for an in-place display-client switch. The driver runs
/// it BLIND through the host's transport and never inspects which variant it is - the
/// variant↔dispatch mapping is `run_switch_plan`'s job, not the driver's. Each variant
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
/// manage layer + detection call. `enumerate` takes `&Transport` because the per-session model
/// runs a probe (registry read + one list-sessions); the shared model runs one
/// command. Every other method is transport-blind.
#[async_trait]
pub trait Mux: Send + Sync {
    /// The canonical mux identity, for mux comparison and diagnostics.
    fn kind(&self) -> &str;

    /// The binary name to invoke on this host.
    fn bin(&self) -> &str;

    /// The argvs that ask this mux to name itself, in the order to ask them. Each argv
    /// runs as one probe over the host's transport (`argv[0]` the binary), and the
    /// collected answers go to [`Mux::classify_identity`]. Detection is per implementation:
    /// there is no central sequence of shared stages, so an implementation owns WHICH commands identify it
    /// and in what order - tmux asks `help` before `-V` because a psmux alias of tmux
    /// mimics the version line, while abduco and screen ask only their `-v`.
    ///
    /// Deliberately has NO default. A default probe is a rank: it would make one
    /// implementation's commands the question every other implementation is guessed with, exactly what
    /// exactly what the central stage sequence this replaced did.
    fn identity_probes(&self) -> Vec<Vec<String>>;

    /// Which mux the collected probe outputs name, as a registry kind
    /// ([`known_muxes`]). The outputs align one-to-one with
    /// [`Mux::identity_probes`], `None` where a probe errored (an absent command, a
    /// rejected flag, an unreachable host).
    ///
    /// `Some(kind)` may name ANOTHER mux: a `tmux` whose `help` names psmux is the
    /// psmux alias, and the source corrects to psmux with the invoked binary
    /// preserved. `None` is inconclusive - the caller keeps its current mux and
    /// retries on a later scan; it is never decoded to a fallback kind.
    fn classify_identity(&self, outputs: &[Option<String>]) -> Option<&'static str>;

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

    /// Whether this mux ASSIGNS the name of a session it creates: given an empty name
    /// it auto-names, and its create plan prints the final name to stdout. The two are
    /// one capability, because a printed name is the only way an auto-assigned name can
    /// be learned. The manage layer reads this to decide both sides of a create: whether
    /// stdout is a name to parse (a mux whose create prints nothing can still have
    /// stdout NOISE - a shell banner, an motd - and trusting it invents a session that
    /// does not exist) and whether an empty name must be filled in by xmux before the
    /// plan is built.
    ///
    /// Deliberately has NO default, for the same reason as
    /// [`takes_server_socket`](Mux::takes_server_socket): a tmux-compatible default is
    /// silently wrong for a silent-create mux, and the failure it causes (stdout noise
    /// adopted as the new session's name) surfaces far from the mux that inherited it.
    fn assigns_new_session_name(&self) -> bool;

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

    /// The mux's own display driver - the per-host orchestration of which PTY to
    /// attach and whether to `switch-client` or reattach on a session change. Each
    /// mux constructs ITS OWN driver, so mux selection lives in the mux implementation
    /// (never a central `match server_model()`). The driver is zero-sized; the per-host
    /// display state lives on `host.display`/`AttachRegistry`, borrowed through
    /// `DriverCtx`, so a fresh value per call is free.
    fn driver(&self) -> Box<dyn crate::driver::MuxDriver>;

    /// Clones into a fresh box - a spawned poll task needs an owned mux, and a trait
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

    /// The interactive attach argv (`argv[0]` = binary).
    fn attach_plan(&self, session: &str) -> Vec<String>;

    /// An opaque plan that moves xmux's OWN display client to `session` IN PLACE (no
    /// teardown). The driver runs the returned [`SwitchPlan`] blind through the transport,
    /// never inspecting the variant; the `tty >file` / read-back mechanism a shared mux
    /// uses stays inside the mux implementation, not on this boundary. `display_tty` is the
    /// captured tty of xmux's display client - psmux targets it directly; tmux ignores it
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

    /// The environment variable this mux's own CLIENT carries the session it is attached
    /// to in, and rewrites in place when the mux moves it. `None` (the default) for a mux
    /// whose client does not, so nothing outside can read where its client went.
    ///
    /// It exists because a mux can move its client between sessions INSIDE the client
    /// process: it detaches from one session's server and the same process reconnects to
    /// another, keeping its pid and its original argv. No server sees that move, so a
    /// control channel cannot report it and the argv still names the session the client
    /// left. The live client's own environment is the only source of truth, and this names the
    /// one variable to read. The read itself works only for a client on THIS machine, so
    /// answering here is not a promise that an answer is available - the caller gates on
    /// the transport as well.
    fn display_session_env(&self) -> Option<&str> {
        None
    }

    /// The control argv for a `-CC` metadata channel. `None` for a mux with no
    /// host-level control stream (it is polled).
    fn control_argv(&self) -> Option<Vec<String>>;

    /// The control-mode wire protocol (line classification + notification→event policy
    /// + command-line builders) the host reader drives this `-CC` channel with. `None`
    /// for a mux with no host-level control stream (it is polled), matching `control_argv`.
    /// The protocol is stateless, so the reference is `'static` (a shared unit struct) -
    /// the host reader/writer threads borrow it for their whole lifetime.
    fn control_protocol(&self) -> Option<&'static dyn ControlProtocol> {
        None
    }

    /// How this host learns a session/attachment died.
    fn death_signal(&self) -> DeathSignal;

    /// The change/event channel for this mux.
    fn event_source(&self) -> EventSource;

    /// One poll sweep for a POLL host: enumerate sessions, emitting a
    /// [`HostEvent::Sessions`] - the same payload a control client's metadata path
    /// produces. Built from the existing trait method (`enumerate`), so it is
    /// mux-blind and needs no per-impl override: tmux is control-driven and never
    /// calls it; psmux uses this default. The host manager owns the ticker/cancel
    /// lifecycle and calls this once per tick; `emit` is its sink onto the shared
    /// event bus.
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
        emit(HostEvent::Sessions {
            source: source.to_string(),
            sessions,
            err,
        });
    }

    // The command-plan verbs are tmux-compatible argv builders over `self.bin()`, so
    // every tmux-compatible mux inherits them for free. A mux
    // whose argv diverges overrides only the verb it differs on.
    /// The `new-session` argv that creates-or-attaches a DETACHED session (auto-named
    /// when `name` is empty) and prints its assigned name. The manage layer runs it via
    /// the host's `Transport` and reads back the assigned name.
    fn new_session_plan(&self, name: &str) -> Vec<String> {
        mux::new_session(self.bin(), name)
    }
}

struct MuxKind {
    name: &'static str,
    make: fn(String) -> Box<dyn Mux>,
}

// `name` is the canonical identity, the marker the implementation's classify reads its probe
// outputs for, and the conventional binary name. Every mux xmux drives is an entry;
// none is a fallback for another.
fn known_muxes() -> &'static [MuxKind] {
    &[
        MuxKind {
            name: "tmux",
            make: |bin| Box::new(Tmux { bin }),
        },
        MuxKind {
            name: "abduco",
            make: |bin| Box::new(Abduco { bin }),
        },
        MuxKind {
            name: "psmux",
            make: |bin| Box::new(Psmux { bin }),
        },
        MuxKind {
            name: "zellij",
            make: |bin| Box::new(Zellij { bin }),
        },
        MuxKind {
            name: "screen",
            make: |bin| Box::new(Screen { bin }),
        },
    ]
}

/// Every mux xmux can drive, in registry order. This is the CANDIDATE SET for
/// discovery: xmux only ever looks for a mux it already knows how to drive, and each
/// candidate resolves to a mux of its own kind.
pub fn supported_muxes() -> Vec<&'static str> {
    known_muxes().iter().map(|k| k.name).collect()
}

/// The per-probe budget for LOCAL discovery. A probe asks a local binary to print its
/// help, which takes milliseconds when it is there and fails at once when it is not -
/// but discovery also shares the machine with the rescan burst (remote probes, poll
/// respawns), so a probe that would answer in milliseconds when idle can take a couple
/// of seconds to be serviced under load. The budget is wide enough that a healthy probe
/// always lands inside it, and a binary genuinely absent still fails at once. Off the
/// loop and async, so nothing on screen waits on it.
const DETECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// The per-probe budget for a REMOTE machine. A probe there is one ssh round trip per
/// command, and a mux like screen falls through three probes (`help`, `-V`, `-v`)
/// before it answers, so its whole detect must fit one budget. Longer than
/// [`DETECT_TIMEOUT`] because the round trips dominate; remote discovery is async and
/// off the loop, so nothing waits on it. The detection LOGIC is the same as local - a
/// slower link just gets more time, never a different question.
const DETECT_TIMEOUT_REMOTE: std::time::Duration = std::time::Duration::from_secs(6);

/// Which mux is installed on the machine `transport` reaches, out of the ones xmux can
/// drive ([`supported_muxes`]).
///
/// No implementation is the fallback for another. Each candidate is
/// asked with ITS OWN identity probe (the [`Mux::identity_probes`] of the mux its
/// conventional name builds) and counts as installed only when the binary answers AS
/// that candidate ([`Mux::classify_identity`]). A binary that carries one mux's name
/// while answering as another is simply not this candidate: the psmux alias of tmux
/// never counts as tmux, because tmux's own help probe reads the alias's self-naming
/// help. No classification reads ANOTHER mux's name as its own identity, and the one
/// name a stage may drop is one that stage itself has a reason to skip.
pub async fn installed_muxes(transport: &dyn Transport, runner: &dyn Runner) -> Vec<String> {
    // Probe every candidate mux CONCURRENTLY (each with the same `DETECT_TIMEOUT`
    // budget) rather than one after another, so a machine's mux set resolves in ~one
    // probe-time instead of the sum of them. A remote machine gets the longer remote
    // budget: its probes are ssh round trips, and an implementation that asks several commands
    // would otherwise time out before its last one answers.
    let names = supported_muxes();
    let budget = if transport.is_remote() {
        DETECT_TIMEOUT_REMOTE
    } else {
        DETECT_TIMEOUT
    };
    let futures = names.iter().map(|name| async {
        let Some(mux) = for_binary(name) else {
            return false;
        };
        let probe = probe_identity(transport, mux.as_ref(), runner);
        match tokio::time::timeout(budget, probe).await {
            Ok(Some(kind)) => kind == *name,
            _ => false,
        }
    });
    let hits: Vec<bool> = futures::future::join_all(futures).await;
    names
        .iter()
        .zip(hits)
        .filter(|(_, hit)| *hit)
        .map(|(name, _)| (*name).to_string())
        .collect()
}

/// The mux whose name `text` contains, in registry order. `skip` drops one kind's
/// name from the search. tmux's help stage skips itself: real tmux has no `help`
/// command, so a successful help naming a mux names ANOTHER mux - the
/// psmux-behind-a-tmux-alias correction. psmux's help stage skips tmux: psmux's
/// own help output mentions tmux while presenting psmux as a tmux alternative, so
/// those mentions never name the mux.
pub(crate) fn named_mux_excluding(text: &str, skip: &str) -> Option<&'static str> {
    known_muxes()
        .iter()
        .map(|k| k.name)
        .find(|n| *n != skip && text.contains(n))
}

/// The mux whose name `text` contains, in registry order. Pure over the text; the
/// implementations' [`Mux::classify_identity`] read their probe outputs through this.
pub(crate) fn named_mux(text: &str) -> Option<&'static str> {
    named_mux_excluding(text, "")
}

/// Picks a mux by conventional binary name; `None` for a name no kind owns. A
/// written name that resolves to `None` is a config error warned at load, never a
/// silent decode to another implementation.
pub fn for_binary(bin: &str) -> Option<Box<dyn Mux>> {
    let k = known_muxes().iter().find(|k| k.name == bin)?;
    Some((k.make)(bin.to_string()))
}

/// The server socket to address the mux binary `bin` over: the one this machine named, or
/// `None` for a mux that takes no socket flag or a name no kind owns.
///
/// The two composition sites (the source list and the host registry) call this before
/// handing a socket to the transport axis, so a socket only ever reaches a mux that
/// understands it. The transport axis cannot make this call itself: it names no mux by
/// design, so it injects the socket it is GIVEN and asks nothing about it.
pub fn server_socket_for(bin: &str, socket: Option<String>) -> Option<String> {
    socket.filter(|_| for_binary(bin).is_some_and(|m| m.takes_server_socket()))
}

/// Builds a mux by canonical identity while preserving the binary used to
/// reach it; `None` for an unknown kind.
pub fn for_kind(kind: &str, bin: &str) -> Option<Box<dyn Mux>> {
    let k = known_muxes().iter().find(|k| k.name == kind)?;
    Some((k.make)(bin.to_string()))
}

/// True when `name` names a mux xmux actually recognizes: an entry of
/// [`known_muxes`], which holds every mux xmux drives. Config validation warns on a
/// written name this refuses, and the source build drops it - a name no kind owns
/// is never decoded to one that does.
pub fn is_recognized(name: &str) -> bool {
    known_muxes().iter().any(|k| k.name == name)
}

/// Runs the mux's own identity probes over `transport` and reads the answers. Each
/// argv is one probe run; a probe that errors (an absent command, a rejected flag, an
/// unreachable host) collects `None`, and the outputs aligned with
/// [`Mux::identity_probes`] go to [`Mux::classify_identity`]. Output is lowercased,
/// so a implementation's classify matches case-insensitively.
async fn probe_identity(
    transport: &dyn Transport,
    mux: &dyn Mux,
    runner: &dyn Runner,
) -> Option<&'static str> {
    let mut outs: Vec<Option<String>> = Vec::new();
    for argv in mux.identity_probes() {
        let (name, args) = transport.exec_argv(false, &argv);
        outs.push(
            runner
                .run(&name, &args)
                .await
                .ok()
                .map(|out| String::from_utf8_lossy(&out).to_lowercase()),
        );
    }
    mux.classify_identity(&outs)
}

/// Probes a server's true identity over `transport`, independent of its binary name
/// (psmux installs a `tmux` alias of itself and mimics tmux's `-V`). The kind the
/// binary conventionally belongs to supplies its OWN probes
/// ([`Mux::identity_probes`]) and reads the answers ([`Mux::classify_identity`]),
/// which may name ANOTHER mux: a `tmux` whose `help` names psmux is that alias, and
/// the source corrects to psmux with the invoked binary preserved.
///
/// `None` means the binary is unrecognized, the host unreachable, or every probe
/// inconclusive; the caller keeps its current mux and retries on a later scan.
pub async fn detect_backend(
    transport: &dyn Transport,
    bin: &str,
    runner: &dyn Runner,
) -> Option<Box<dyn Mux>> {
    let mux = for_binary(bin)?;
    let kind = probe_identity(transport, mux.as_ref(), runner).await?;
    for_kind(kind, bin)
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
    fn tmux_session_plans_match_mux_builders() {
        let m = tmux();
        assert_eq!(m.new_session_plan("dev"), mux::new_session("tmux", "dev"));
        assert_eq!(m.new_session_plan(""), mux::new_session("tmux", ""));
        assert!(
            m.assigns_new_session_name(),
            "new-session auto-names an empty request and -P -F prints the result"
        );
    }

    /// A minimal tmux-compatible mux that implements ONLY the required `Mux` methods
    /// and none of the command-plan verbs. It must still compile and get every command
    /// plan for free from the trait defaults (additivity: a new
    /// tmux-compatible mux = identity + a few methods, the verbs are free).
    struct BareMux {
        bin: String,
    }

    #[async_trait]
    impl Mux for BareMux {
        fn identity_probes(&self) -> Vec<Vec<String>> {
            Vec::new()
        }

        fn classify_identity(&self, _outputs: &[Option<String>]) -> Option<&'static str> {
            None
        }

        /// tmux-shaped, like the fake itself.
        fn takes_server_socket(&self) -> bool {
            true
        }

        /// tmux-shaped, like the fake itself.
        fn assigns_new_session_name(&self) -> bool {
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
            .enumerate(&t, &crate::model::source::ExecRunner)
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
    fn psmux_session_plans_use_the_psmux_binary() {
        let m = psmux();
        assert_eq!(m.new_session_plan("dev"), mux::new_session("psmux", "dev"));
        assert_eq!(m.new_session_plan(""), mux::new_session("psmux", ""));
        assert!(
            m.assigns_new_session_name(),
            "psmux runs tmux's new-session plan, prints included"
        );
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

    /// Answers the three detection probes (`help`, `-V`, `-v`) independently so a test
    /// can model a real tmux (help fails, `-V` succeeds), a psmux (help names itself),
    /// an abduco (`-V` fails, `-v` names itself), or an unreachable host (all fail).
    /// `None` for a probe ⇒ that probe errors.
    struct ProbeRunner {
        help: Option<Vec<u8>>,
        version: Option<Vec<u8>>,
        low_version: Option<Vec<u8>>,
    }

    impl ProbeRunner {
        fn new(help: Option<&str>, version: Option<&str>) -> Self {
            ProbeRunner {
                help: help.map(|s| s.as_bytes().to_vec()),
                version: version.map(|s| s.as_bytes().to_vec()),
                low_version: None,
            }
        }
        fn low_version(mut self, v: Option<&str>) -> Self {
            self.low_version = v.map(|s| s.as_bytes().to_vec());
            self
        }
    }

    #[async_trait]
    impl Runner for ProbeRunner {
        async fn run(&self, _name: &str, args: &[String]) -> Result<Vec<u8>, RunError> {
            // The `-V` probe's arg is `-V` (local) or `<bin> -V` (ssh-wrapped); `-v` is
            // abduco's lower-case version flag; anything else is the `help` probe.
            let probe = if args.iter().any(|a| a.contains("-V")) {
                &self.version
            } else if args.iter().any(|a| a.contains("-v")) {
                &self.low_version
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
        // xmux has no implementation for: every candidate resolves to a mux of its own kind.
        let names = supported_muxes();
        assert_eq!(names, vec!["tmux", "abduco", "psmux", "zellij", "screen"]);
        for name in names {
            assert_eq!(
                for_binary(name).unwrap().kind(),
                name,
                "{name} must be drivable"
            );
        }
    }

    #[tokio::test]
    async fn a_machine_offers_every_supported_mux_it_actually_has() {
        // The reported case: zellij is up on this machine but nothing in the config says so.
        // Discovery asks each supported mux whether it is here, in the supported order.
        let t = crate::transport::local(None);
        let got = installed_muxes(&t, &MachineWith::new(&["tmux", "zellij"])).await;
        assert_eq!(got, vec!["tmux", "zellij"]);
    }

    #[tokio::test]
    async fn where_psmux_answers_a_tmux_alias_is_never_tmux() {
        // psmux installs a `tmux` alias of itself whose `-V` mimics tmux's version line
        // while the alias's own help names it. tmux's OWN probe reads the help first, so
        // the alias answers psmux and the `tmux` candidate is simply not tmux - no name
        // is dropped for another's sake.
        let t = crate::transport::local(None);
        let alias = MachineWith {
            present: vec!["tmux", "psmux"],
            help_marker: Some("psmux"),
        };
        assert_eq!(installed_muxes(&t, &alias).await, vec!["psmux"]);
        // No psmux, so a tmux that answers is a tmux.
        assert_eq!(
            installed_muxes(&t, &MachineWith::new(&["tmux", "zellij"])).await,
            vec!["tmux", "zellij"]
        );
    }

    #[tokio::test]
    async fn a_machine_serving_tmux_and_psmux_is_offered_both() {
        // Equal treatment: a REAL tmux (help errors, as real tmux's does) next to a real
        // psmux is identified as tmux - detection asks each binary its own probe, so
        // both implementations are offered where both are installed.
        let t = crate::transport::local(None);
        assert_eq!(
            installed_muxes(&t, &MachineWith::new(&["tmux", "psmux"])).await,
            vec!["tmux", "psmux"]
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

    #[tokio::test]
    async fn a_psmux_whose_help_mentions_tmux_is_still_discovered() {
        // psmux's help banner names itself and mentions tmux (it presents itself as
        // a tmux alternative): the mentions are comparative, never an identity claim,
        // so the psmux candidate is confirmed by its own name in its own output.
        let t = crate::transport::local(None);
        let runner = MachineWith {
            present: vec!["psmux"],
            help_marker: Some("psmux v3.3.8 - terminal multiplexer for windows (tmux alternative)"),
        };
        assert_eq!(installed_muxes(&t, &runner).await, vec!["psmux"]);
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
    async fn detect_backend_classifies_a_psmux_binary_as_psmux_despite_tmux_mentions() {
        // A configured psmux source re-detects through the same classify: the tmux
        // mentions in psmux's help must not swap it onto a tmux mux over the psmux
        // binary.
        let transport = crate::transport::local(None);
        let runner = ProbeRunner::new(
            Some("psmux v3.3.8 - Terminal multiplexer for Windows (tmux alternative)"),
            Some("tmux 3.3.8"),
        );
        let got = detect_backend(&transport, "psmux", &runner).await.unwrap();
        assert_eq!(got.kind(), "psmux");
        assert_eq!(got.server_model(), ServerModel::PerSession);
        assert_eq!(
            got.attach_plan("api"),
            argv(&["psmux", "new-session", "-A", "-s", "api"])
        );
    }

    #[tokio::test]
    async fn detect_backend_classifies_zellij_by_help_marker() {
        // zellij names itself in its help banner, the same positive signal psmux
        // gives; one help question is the whole probe.
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
        // the help probe errors. The `-V` half of tmux's own probe pair must still
        // identify it as tmux - otherwise a correctly-configured tmux host never gets
        // detected/connected.
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
        use crate::model::source::ExecRunner;
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
    async fn detect_backend_classifies_abduco_by_low_version_marker() {
        // abduco rejects `-V` (uppercase, exit 1) and names itself in `-v` (lowercase)
        // output: `abduco-0.6 © …`. `-v` is the implementation's one probe and identifies it.
        let transport = crate::transport::local(None);
        let runner = ProbeRunner::new(None, None).low_version(Some("abduco-0.6 © 2013-2018"));
        let got = detect_backend(&transport, "abduco", &runner).await.unwrap();
        assert_eq!(got.kind(), "abduco");
        assert_eq!(got.server_model(), ServerModel::PerSession);
        assert_eq!(got.attach_plan("api"), argv(&["abduco", "-a", "api"]));
    }

    #[tokio::test]
    async fn detect_backend_does_not_classify_tmux_as_abduco() {
        // A real tmux answers `-V` ("tmux 3.5a") and must stay tmux: tmux's own
        // version stage reads the name in its output, and `-v` is never asked.
        let transport = crate::transport::local(None);
        let runner = ProbeRunner::new(None, Some("tmux 3.5a"));
        let got = detect_backend(&transport, "tmux", &runner).await.unwrap();
        assert_eq!(got.kind(), "tmux");
    }

    #[tokio::test]
    async fn detect_backend_both_probes_fail_is_inconclusive() {
        // Unreachable host / missing binary: both probes error ⇒ None (retry later).
        let transport = crate::transport::local(None);
        let runner = ProbeRunner::new(None, None);
        assert!(detect_backend(&transport, "tmux", &runner).await.is_none());
    }

    #[test]
    fn for_binary_resolves_registry_names_and_refuses_others() {
        assert_eq!(for_binary("psmux").unwrap().kind(), "psmux");
        assert_eq!(
            for_binary("psmux").unwrap().server_model(),
            ServerModel::PerSession
        );
        assert_eq!(for_binary("tmux").unwrap().kind(), "tmux");
        assert_eq!(
            for_binary("tmux").unwrap().server_model(),
            ServerModel::Shared
        );
        // No fallback: a name no kind owns resolves to nothing at all.
        assert!(for_binary("").is_none());
        assert!(for_binary("some-fork-of-tmux").is_none());
    }

    #[test]
    fn for_kind_preserves_identity_and_invoked_binary() {
        let p = for_kind("psmux", "tmux").unwrap();
        assert_eq!(p.kind(), "psmux");
        assert_eq!(p.bin(), "tmux");
        assert_eq!(p.event_source(), EventSource::Poll { interval_ms: 1500 });

        let t = for_kind("tmux", "psmux").unwrap();
        assert_eq!(t.kind(), "tmux");
        assert_eq!(t.bin(), "psmux");
        assert_eq!(t.event_source(), EventSource::Control);
    }

    #[test]
    fn an_unknown_name_or_kind_decodes_to_nothing() {
        // Equal treatment: a name or kind outside the registry is an error for the
        // caller to surface, never a silent decode to another kind.
        assert!(for_binary("some-fork").is_none());
        assert!(for_kind("nope", "tcustom").is_none());
    }

    #[test]
    fn zellij_resolves_by_binary_name_and_by_kind() {
        // The registry is what makes a mux reachable from config: a `mux = "zellij"`
        // entry and a `zellij` binary must both land on the zellij impl, and the
        // invoked binary is preserved either way.
        assert_eq!(for_binary("zellij").unwrap().kind(), "zellij");
        assert_eq!(
            for_kind("zellij", "zellij-nightly").unwrap().kind(),
            "zellij"
        );
        assert_eq!(
            for_kind("zellij", "zellij-nightly").unwrap().bin(),
            "zellij-nightly"
        );
    }

    #[test]
    fn is_recognized_covers_tmux_and_known_muxes() {
        assert!(is_recognized("tmux"));
        assert!(is_recognized("abduco"));
        assert!(is_recognized("psmux"));
        assert!(is_recognized("zellij"));
        assert!(is_recognized("screen"));
        assert!(!is_recognized("byobu"));
        assert!(!is_recognized(""));
    }

    #[test]
    fn screen_resolves_by_binary_name_and_by_kind() {
        // The registry is what makes a mux reachable from config: a `mux = "screen"`
        // entry and a `screen` binary must both land on the screen impl, and the
        // invoked binary is preserved either way.
        assert_eq!(for_binary("screen").unwrap().kind(), "screen");
        assert_eq!(
            for_kind("screen", "screen-custom").unwrap().kind(),
            "screen"
        );
        assert_eq!(
            for_kind("screen", "screen-custom").unwrap().bin(),
            "screen-custom"
        );
    }

    #[tokio::test]
    async fn detect_backend_classifies_screen_via_dash_v() {
        // screen's positive signal is `-v` ("Screen version ... (GNU)"), NOT `-V`
        // (which errors); `-v` is the implementation's one probe and identifies it.
        let transport = crate::transport::local(None);
        let runner = ProbeRunner::new(Some("Must be connected to a terminal."), None)
            .low_version(Some("Screen version 4.09.00 (GNU) 30-Jan-22"));
        let got = detect_backend(&transport, "screen", &runner).await.unwrap();
        assert_eq!(got.kind(), "screen");
        assert_eq!(got.server_model(), ServerModel::PerSession);
    }

    #[test]
    fn is_locked_matches_only_the_ssh_auth_failure_signature() {
        // The canonical ssh auth-failure line (locked), and the exact "(" after
        // "Permission denied" that distinguishes it from a generic mux permission error.
        assert!(is_locked(
            "pwtest@127.0.0.1: Permission denied (publickey,password)."
        ));
        assert!(is_locked(
            "command failed (exit 255): pwtest@127.0.0.1: Permission denied (publickey)."
        ));
        assert!(is_locked(
            "Permission denied (publickey,password,keyboard-interactive)."
        ));
        // Reach failures and non-ssh permission errors are NOT locked.
        assert!(!is_locked(
            "ssh: connect to host 192.0.2.1 port 22: Connection timed out"
        ));
        assert!(!is_locked("Host key verification failed."));
        assert!(!is_locked(
            "tmux: open /tmp/tmux-0/default: Permission denied"
        ));
        assert!(!is_locked("no server running on /tmp/tmux-1000/default"));
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

    /// Always errors - models an unreachable poll host (ssh connect failure).
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
        // Exactly the Sessions event fires (no panes - enumeration returned nothing),
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
    }

    /// A SUCCESSFUL poll sweep emits `Sessions { err: None }` - the same payload a
    /// control client's metadata path produces.
    #[tokio::test]
    async fn poll_once_emits_sessions_on_success() {
        use crate::link::HostEvent;

        /// Answers list-sessions with one session.
        struct OkRunner;
        #[async_trait]
        impl Runner for OkRunner {
            async fn run(&self, _name: &str, _args: &[String]) -> Result<Vec<u8>, RunError> {
                // session row parsed by mux::parse_sessions.
                Ok(b"1\t1\t1700000000\twork\n".to_vec())
            }
        }

        let transport = crate::transport::ssh("host".into(), String::new(), "linux".into());
        let mut events: Vec<HostEvent> = Vec::new();
        psmux()
            .poll_once("host", &transport, &OkRunner, &mut |e| events.push(e))
            .await;
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
        assert_eq!(events.len(), 1, "a sweep is Sessions alone");
    }
    #[test]
    fn a_socket_reaches_only_a_mux_that_takes_one() {
        // The whole point of asking: zellij exits on an unexpected `-S` before it reads
        // the verb, so a socket handed to it makes its source permanently unreachable.
        let sock = || Some("/tmp/psmux/default".to_string());
        assert_eq!(server_socket_for("tmux", sock()), sock());
        assert_eq!(server_socket_for("psmux", sock()), sock());
        assert_eq!(server_socket_for("zellij", sock()), None);
        assert_eq!(server_socket_for("abduco", sock()), None);
        // An unknown binary belongs to no kind, so no socket is composed for it.
        assert_eq!(server_socket_for("mux-of-the-future", sock()), None);
        // No socket to hand on stays no socket, whoever the mux is.
        assert_eq!(server_socket_for("tmux", None), None);
        assert_eq!(server_socket_for("zellij", None), None);
    }
}
