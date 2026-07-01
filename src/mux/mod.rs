//! One mux backend per mux. `Box<dyn Backend>` lives inside a `Host`. The method set is
//! exactly what the supervisor + control reader + manage layer call — no feature
//! catalogue. It covers both window operations and session lifecycle (create / kill /
//! rename), so the manage layer routes every mux argv through the backend rather than
//! building it off a bare binary name. The backend owns its binary name and
//! `ServerModel`, so nothing above threads a `bin: &str` or branches on a `remote` bool
//! to pick the model. Every method is transport-blind except `enumerate` (which runs a
//! probe).

use async_trait::async_trait;

use crate::host::HostEvent;
use crate::model::plan::{DeathSignal, EventSource};
use crate::model::server_model::ServerModel;
use crate::model::transport::Transport;
use crate::mux::vocab as mux;
use crate::session::Session;
use crate::source::{RunError, Runner};

mod control;
mod psmux;
mod tmux;
pub mod vocab;

pub use control::{ControlProtocol, Line, Notif};
pub use psmux::Psmux;
pub use tmux::{Tmux, TmuxControl};
// Re-export the pure mux vocabulary at the crate::mux root so `crate::mux::<fn>`
// call sites resolve unchanged whether the item is the Backend trait/factory or a
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
    transport: &Transport,
    runner: &dyn Runner,
) -> Result<Vec<Session>, RunError> {
    let (name, args) = transport.exec_argv(false, &mux::list_sessions(bin));
    match runner.run(&name, &args).await {
        Ok(out) => Ok(mux::parse_sessions(
            transport.host_id(),
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
        line.starts_with("no server running") || line.starts_with("no sessions")
    })
}

/// One mux backend. Methods are the EXACT set the supervisor + control reader +
/// manage layer call. `enumerate` takes `&Transport` because the per-session model
/// runs a probe (registry read + one list-sessions); the shared model runs one
/// command. Every other method is transport-blind.
#[async_trait]
pub trait Backend: Send + Sync {
    /// The canonical mux identity, for backend comparison and diagnostics.
    fn kind(&self) -> &str;

    /// The binary name to invoke on this host.
    fn bin(&self) -> &str;

    /// Per-session vs shared. The supervisor reads this instead of `remote`.
    fn server_model(&self) -> ServerModel;

    /// The mux's own display driver — the per-host orchestration of which PTY to
    /// attach and whether to `switch-client` or reattach on a session change. Each
    /// backend constructs ITS OWN driver, so mux selection lives in the mux family
    /// (never a central `match server_model()`). The driver is zero-sized; the per-host
    /// display state lives on `host.display`/`AttachRegistry`, borrowed through
    /// `DriverCtx`, so a fresh value per call is free.
    fn driver(&self) -> Box<dyn crate::driver::MuxDriver>;

    /// Lists this host's sessions over `transport`, executing its probe via
    /// `runner` (the real [`ExecRunner`] in production; an injected fake under test).
    /// A reachable empty mux => `Ok(vec![])`; unreachable => `Err`.
    async fn enumerate(
        &self,
        transport: &Transport,
        runner: &dyn Runner,
    ) -> Result<Vec<Session>, RunError>;

    /// The interactive attach argv (`argv[0]` = binary). The window is selected
    /// separately via `select_window_plan`; the transport folds it for a remote
    /// attach when composing the final connection.
    fn attach_plan(&self, session: &str, window: Option<i64>) -> Vec<String>;

    /// The mux's own `switch-client` argv given the captured display tty + target.
    /// The driver closes over the host's display tty to move xmux's own display client
    /// to `session` (the psmux in-place switch). The ONLY mux method that names the tty;
    /// it does NOT decide local-vs-ssh. The default builds the standard
    /// `switch-client -c <tty> -t <session>` form; a mux with a divergent verb overrides it.
    fn switch_client_argv(&self, display_tty: &str, session: &str) -> Vec<String> {
        vec![
            self.bin().to_string(),
            "switch-client".to_string(),
            "-c".to_string(),
            display_tty.to_string(),
            "-t".to_string(),
            mux::quote_target(session),
        ]
    }

    /// The shell prefix the mux's display attach prepends to its remote command so the
    /// attach shell records its OWN controlling tty before exec'ing the attach — the
    /// value a later `switch-client -c <tty>` targets, provably xmux's own display
    /// client and never the user's own attached client. A shared mux (tmux) writes it
    /// to a per-host file (`tty >FILE`); the read-back side is [`display_tty_read_argv`].
    /// Out-of-band (a file, not the pty stream) so the Windows ConPTY cannot consume it.
    /// `None` for a mux that identifies its display client another way (psmux correlates
    /// by the session the client shows).
    ///
    /// [`display_tty_read_argv`]: Backend::display_tty_read_argv
    fn display_tty_record_prefix(&self, _host_key: &str) -> Option<String> {
        None
    }

    /// A self-contained shell command that moves the mux's display client to `session`
    /// by READING the tty it recorded via [`display_tty_record_prefix`] — so the switch
    /// targets xmux's OWN display client and never the user's own attached client (the
    /// failure of identifying the client by `list-clients`, where both look alike). Reads
    /// the file in-shell at switch time, so the value is always the live attach's current
    /// tty (no stale capture). Run as a remote raw command; `None` for a mux that uses no
    /// recorded-tty strategy.
    ///
    /// [`display_tty_record_prefix`]: Backend::display_tty_record_prefix
    fn switch_via_recorded_tty_cmd(&self, _host_key: &str, _session: &str) -> Option<String> {
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
        transport: &Transport,
        runner: &dyn Runner,
        emit: &mut (dyn FnMut(HostEvent) + Send),
    ) {
        let (sessions, err) = match self.enumerate(transport, runner).await {
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
            if let Ok(out) = runner.run(&cmd, &args).await {
                let panes = mux::parse_panes(&String::from_utf8_lossy(&out));
                emit(HostEvent::Panes { address, panes });
            }
        }
    }

    fn list_panes_plan(&self, session: &str) -> Vec<String>;
    fn new_window_plan(&self, session: &str, name: &str) -> Vec<String>;
    fn split_window_plan(&self, target: &str, vertical: bool) -> Vec<String>;
    fn select_window_plan(&self, target: &str) -> Vec<String>;
    fn kill_window_plan(&self, target: &str) -> Vec<String>;
    fn rename_window_plan(&self, target: &str, new: &str) -> Vec<String>;

    /// The `new-session` argv that creates-or-attaches a DETACHED session (auto-named
    /// when `name` is empty) and prints its assigned name. The manage layer runs it via
    /// the host's `Transport` and reads back the assigned name.
    fn new_session_plan(&self, name: &str) -> Vec<String>;
    /// The `kill-session` argv for session `name`.
    fn kill_session_plan(&self, name: &str) -> Vec<String>;
    /// The `rename-session` argv moving `old` to `new`.
    fn rename_session_plan(&self, old: &str, new: &str) -> Vec<String>;
}

struct MuxKind {
    name: &'static str,
    make: fn(String) -> Box<dyn Backend>,
}

// `name` is the canonical identity, help-output marker, and conventional binary
// name. tmux is the implicit fallback because tmux has no positive help signal.
fn known_muxes() -> &'static [MuxKind] {
    &[MuxKind {
        name: "psmux",
        make: |bin| Box::new(Psmux { bin }),
    }]
}

/// Picks a mux backend by conventional binary name. tmux is the fallback, matching
/// the default in `Config::local_bin` / `host_specs`.
pub fn for_binary(bin: &str) -> Box<dyn Backend> {
    for k in known_muxes() {
        if k.name == bin {
            return (k.make)(bin.to_string());
        }
    }
    Box::new(Tmux {
        bin: bin.to_string(),
    })
}

/// Builds a backend by canonical identity while preserving the binary used to
/// reach it.
pub fn for_kind(kind: &str, bin: &str) -> Box<dyn Backend> {
    for k in known_muxes() {
        if k.name == kind {
            return (k.make)(bin.to_string());
        }
    }
    Box::new(Tmux {
        bin: bin.to_string(),
    })
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
/// `Some(backend)` means a probe was conclusive. `None` means BOTH probes failed
/// (unreachable host / missing binary), so the caller keeps its current backend and
/// retries on a later scan.
pub async fn detect_backend(
    transport: &Transport,
    bin: &str,
    runner: &dyn Runner,
) -> Option<Box<dyn Backend>> {
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
        return Some(Box::new(Tmux {
            bin: bin.to_string(),
        }));
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
    fn tmux_is_shared_and_named() {
        let m = tmux();
        assert_eq!(m.kind(), "tmux");
        assert_eq!(m.server_model(), ServerModel::Shared);
    }

    #[test]
    fn tmux_is_object_safe() {
        // The whole point: a Box<dyn Backend> must compile. If the trait gains a
        // non-dispatchable method this stops compiling.
        let _m: Box<dyn Backend> = Box::new(tmux());
    }

    #[test]
    fn tmux_attach_plan_is_plain_attach() {
        let m = tmux();
        assert_eq!(
            m.attach_plan("api", None),
            argv(&["tmux", "attach", "-t", "api"])
        );
        // The window is selected separately (select_window_plan); attach stays plain.
        assert_eq!(
            m.attach_plan("api", Some(2)),
            argv(&["tmux", "attach", "-t", "api"])
        );
    }

    #[test]
    fn tmux_control_attach_and_event_and_death() {
        let m = tmux();
        assert_eq!(m.control_argv(), Some(argv(&["tmux", "-CC", "attach"])));
        assert_eq!(m.event_source(), EventSource::Control);
        assert_eq!(m.death_signal(), DeathSignal::ControlNotice);
    }

    #[test]
    fn tmux_window_plans_match_mux_builders() {
        let m = tmux();
        assert_eq!(m.list_panes_plan("work"), mux::list_panes("tmux", "work"));
        assert_eq!(
            m.select_window_plan("api:2"),
            mux::select_window("tmux", "api:2")
        );
        assert_eq!(
            m.new_window_plan("work", "logs"),
            mux::new_window("tmux", "work", "logs")
        );
        assert_eq!(
            m.split_window_plan("work:1", true),
            mux::split_window("tmux", "work:1", true)
        );
        assert_eq!(
            m.kill_window_plan("api:2"),
            mux::kill_window("tmux", "api:2")
        );
        assert_eq!(
            m.rename_window_plan("api:2", "logs"),
            mux::rename_window("tmux", "api:2", "logs")
        );
    }

    #[test]
    fn tmux_session_plans_match_mux_builders() {
        let m = tmux();
        assert_eq!(m.new_session_plan("dev"), mux::new_session("tmux", "dev"));
        assert_eq!(m.new_session_plan(""), mux::new_session("tmux", ""));
        assert_eq!(m.kill_session_plan("old"), mux::kill_session("tmux", "old"));
        assert_eq!(
            m.rename_session_plan("old", "new"),
            mux::rename_session("tmux", "old", "new")
        );
    }

    // LIVE: enumerate over a real local tmux server. `#[ignore]` (needs tmux + a
    // server). Run on demand:
    //   cargo test --lib mux::tests::tmux_enumerate_live -- --ignored --nocapture
    #[ignore = "live: needs a running local tmux server"]
    #[tokio::test]
    async fn tmux_enumerate_live() {
        let t = Transport::Local { socket: None };
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
        let _m: Box<dyn Backend> = Box::new(psmux());
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
            psmux().attach_plan("work", None),
            argv(&["psmux", "new-session", "-A", "-s", "work"])
        );
    }

    #[test]
    fn psmux_window_plans_use_the_psmux_binary() {
        let m = psmux();
        assert_eq!(m.list_panes_plan("work"), mux::list_panes("psmux", "work"));
        assert_eq!(
            m.select_window_plan("work:1"),
            mux::select_window("psmux", "work:1")
        );
        assert_eq!(
            m.new_window_plan("work", "logs"),
            mux::new_window("psmux", "work", "logs")
        );
    }

    #[test]
    fn psmux_session_plans_use_the_psmux_binary() {
        let m = psmux();
        assert_eq!(m.new_session_plan("dev"), mux::new_session("psmux", "dev"));
        assert_eq!(m.new_session_plan(""), mux::new_session("psmux", ""));
        assert_eq!(
            m.kill_session_plan("old"),
            mux::kill_session("psmux", "old")
        );
        assert_eq!(
            m.rename_session_plan("old", "new"),
            mux::rename_session("psmux", "old", "new")
        );
    }

    #[test]
    fn psmux_behavior_is_decoupled_from_invoked_binary() {
        let m = Psmux { bin: "tmux".into() };
        assert_eq!(
            m.attach_plan("api", None),
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
            m.attach_plan("api", None),
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

    #[tokio::test]
    async fn detect_backend_classifies_psmux_by_help_marker() {
        let transport = Transport::Local { socket: None };
        // psmux names itself in `help`; `-V` is never reached (it would lie "tmux 3.3.6").
        let runner = ProbeRunner::new(Some("usage: PsMuX help"), Some("tmux 3.3.6"));
        let got = detect_backend(&transport, "tmux", &runner).await.unwrap();
        assert_eq!(got.kind(), "psmux");
        assert_eq!(got.server_model(), ServerModel::PerSession);
        assert_eq!(
            got.attach_plan("api", None),
            argv(&["tmux", "new-session", "-A", "-s", "api"])
        );
    }

    #[tokio::test]
    async fn detect_backend_classifies_real_tmux_via_version_when_help_errors() {
        // Regression: real tmux has no `help` command (`tmux help` exits non-zero), so
        // the help probe errors. The `-V` fallback must still identify it as tmux —
        // otherwise a correctly-configured tmux host never gets detected/connected.
        let transport = Transport::Local { socket: None };
        let runner = ProbeRunner::new(None, Some("tmux 3.5a"));
        let got = detect_backend(&transport, "tmux", &runner).await.unwrap();
        assert_eq!(got.kind(), "tmux");
        assert_eq!(got.server_model(), ServerModel::Shared);
    }

    #[tokio::test]
    async fn detect_backend_classifies_tmux_when_help_lacks_marker() {
        // A `help` that succeeds without a known-mux marker still falls through to `-V`.
        let transport = Transport::Local { socket: None };
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
        let ssh = Transport::Ssh {
            alias: "jupiter00".into(),
            control_path: String::new(),
            os: "windows".into(),
        };
        let got = detect_backend(&ssh, "tmux", &ExecRunner).await;
        eprintln!(
            "DETECT jupiter00/tmux -> {:?}",
            got.as_ref().map(|m| (m.kind(), m.server_model()))
        );
        let local = Transport::Local { socket: None };
        let got = detect_backend(&local, "psmux", &ExecRunner).await;
        eprintln!(
            "DETECT local/psmux -> {:?}",
            got.as_ref().map(|m| (m.kind(), m.server_model()))
        );
    }

    #[tokio::test]
    async fn detect_backend_both_probes_fail_is_inconclusive() {
        // Unreachable host / missing binary: both probes error ⇒ None (retry later).
        let transport = Transport::Local { socket: None };
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

    /// Regression (Phase Y5): a poll sweep whose enumeration ERRORS must still surface
    /// `err` on the emitted `Sessions` event. Before, the producer dropped the failure
    /// from the debug log; the event payload is the signal a transient failure happened
    /// (the tree shows it, attachments are kept). A remote psmux enumerates via
    /// list-sessions over ssh, so a failed run becomes `Sessions { err: Some(_) }`.
    #[tokio::test]
    async fn poll_once_surfaces_enumeration_error_on_sessions_event() {
        use crate::host::HostEvent;
        let transport = Transport::Ssh {
            alias: "down-host".into(),
            control_path: String::new(),
            os: "linux".into(),
        };
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
        use crate::host::HostEvent;

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

        let transport = Transport::Ssh {
            alias: "host".into(),
            control_path: String::new(),
            os: "linux".into(),
        };
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
}
