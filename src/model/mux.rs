//! One mux backend per mux. `Box<dyn Mux>` lives inside a `Host`. The method set is
//! exactly what the supervisor + control reader + manage layer call — no feature
//! catalogue, and NO session-level lifecycle (no end-to-end caller in this plan).
//! The backend owns its binary name and `ServerModel`, so nothing above threads a
//! `bin: &str` or branches on a `remote` bool to pick the model. Every method is
//! transport-blind except `enumerate` (which runs a probe).

use async_trait::async_trait;

use crate::model::plan::{DeathSignal, EventSource, SwitchPlan};
use crate::model::server_model::ServerModel;
use crate::model::transport::Transport;
use crate::mux;
use crate::session::Session;
use crate::source::{is_no_sessions, ExecRunner, RunError, Runner};

/// One mux backend. Methods are the EXACT set the supervisor + control reader +
/// manage layer call. `enumerate` takes `&Transport` because the per-session model
/// runs a probe (registry read + one list-sessions); the shared model runs one
/// command. Every other method is transport-blind: `switch_plan` returns intent,
/// the `Transport` lowers it.
#[async_trait]
pub trait Mux: Send + Sync {
    /// The canonical mux identity, for backend comparison and diagnostics.
    fn kind(&self) -> &str;

    /// The binary name to invoke on this host.
    fn bin(&self) -> &str;

    /// Per-session vs shared. The supervisor reads this instead of `remote`.
    fn server_model(&self) -> ServerModel;

    /// Lists this host's sessions over `transport`. A reachable empty mux =>
    /// `Ok(vec![])`; unreachable => `Err`.
    async fn enumerate(&self, transport: &Transport) -> Result<Vec<Session>, RunError>;

    /// The interactive attach argv (`argv[0]` = binary). The window is selected
    /// separately via `select_window_plan`; the transport folds it for a remote
    /// attach when composing the final connection.
    fn attach_plan(&self, session: &str, window: Option<i64>) -> Vec<String>;

    /// TRANSPORT-BLIND intent: how (or whether) to move the host's ONE shared
    /// attachment to `session`. `Shared` => `Switch { session }`; `PerSession` =>
    /// `PerSessionNoOp`. Names NO transport — the `Transport` lowers it.
    fn switch_plan(&self, session: &str) -> SwitchPlan;

    /// The mux's own `switch-client` argv given the captured display tty + target.
    /// The supervisor closes over the host's display tty and hands this to
    /// `Transport::lower_switch`. The ONLY mux method that names the tty; it does NOT
    /// decide local-vs-ssh.
    fn switch_client_argv(&self, display_tty: &str, session: &str) -> Vec<String>;

    /// The control argv for a `-CC` metadata channel. `None` for a mux with no
    /// host-level control stream (it is polled).
    fn control_argv(&self) -> Option<Vec<String>>;

    /// How this host learns a session/attachment died.
    fn death_signal(&self) -> DeathSignal;

    /// The change/event channel for this mux.
    fn event_source(&self) -> EventSource;

    fn list_panes_plan(&self, session: &str) -> Vec<String>;
    fn new_window_plan(&self, session: &str, name: &str) -> Vec<String>;
    fn split_window_plan(&self, target: &str, vertical: bool) -> Vec<String>;
    fn select_window_plan(&self, target: &str) -> Vec<String>;
    fn kill_window_plan(&self, target: &str) -> Vec<String>;
    fn rename_window_plan(&self, target: &str, new: &str) -> Vec<String>;
}

/// Reports whether `err` means "reachable but no sessions" rather than
/// "unreachable" (the `is_no_sessions` rule). Shared by the backends' `enumerate`.
fn benign_empty(err: &RunError) -> bool {
    is_no_sessions(err)
}

/// The `-CC` control argv `[bin, -CC, attach]`, shared by the backends.
fn mux_control_argv(bin: &str) -> Vec<String> {
    vec![bin.to_string(), "-CC".to_string(), "attach".to_string()]
}

/// tmux: one aggregate server (`ServerModel::Shared`), a `-CC` control stream, and
/// a `switch-client` move of one host attachment.
pub struct Tmux {
    pub bin: String,
}

#[async_trait]
impl Mux for Tmux {
    fn kind(&self) -> &str { "tmux" }

    fn bin(&self) -> &str { &self.bin }

    fn server_model(&self) -> ServerModel { ServerModel::Shared }

    async fn enumerate(&self, transport: &Transport) -> Result<Vec<Session>, RunError> {
        let (name, args) = transport.exec_argv(false, &mux::list_sessions(&self.bin));
        match ExecRunner.run(&name, &args).await {
            Ok(out) => Ok(mux::parse_sessions(transport.host_id(), &String::from_utf8_lossy(&out))),
            Err(e) if benign_empty(&e) => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    fn attach_plan(&self, session: &str, _window: Option<i64>) -> Vec<String> {
        mux::attach(&self.bin, session)
    }

    fn switch_plan(&self, session: &str) -> SwitchPlan {
        SwitchPlan::Switch { session: session.to_string() }
    }

    fn switch_client_argv(&self, display_tty: &str, session: &str) -> Vec<String> {
        vec![
            self.bin.clone(),
            "switch-client".to_string(),
            "-c".to_string(),
            display_tty.to_string(),
            "-t".to_string(),
            mux::quote_target(session),
        ]
    }

    fn control_argv(&self) -> Option<Vec<String>> {
        Some(mux_control_argv(&self.bin))
    }

    fn death_signal(&self) -> DeathSignal { DeathSignal::ControlNotice }

    fn event_source(&self) -> EventSource { EventSource::Control }

    fn list_panes_plan(&self, session: &str) -> Vec<String> { mux::list_panes(&self.bin, session) }
    fn new_window_plan(&self, session: &str, name: &str) -> Vec<String> { mux::new_window(&self.bin, session, name) }
    fn split_window_plan(&self, target: &str, vertical: bool) -> Vec<String> { mux::split_window(&self.bin, target, vertical) }
    fn select_window_plan(&self, target: &str) -> Vec<String> { mux::select_window(&self.bin, target) }
    fn kill_window_plan(&self, target: &str) -> Vec<String> { mux::kill_window(&self.bin, target) }
    fn rename_window_plan(&self, target: &str, new: &str) -> Vec<String> { mux::rename_window(&self.bin, target, new) }
}

/// The local-psmux poll cadence (psmux is one-server-per-session with no event
/// push, so changes are discovered by re-enumeration). Mirrors the supervisor's
/// loop constant; held here so the supervisor reads it off the mux, not a literal.
const PSMUX_POLL_MS: u64 = 1500;

/// psmux: one server per session (`ServerModel::PerSession`), enumerated from the
/// filesystem registry, polled for change, each session keeping its own attachment.
pub struct Psmux {
    pub bin: String,
}

#[async_trait]
impl Mux for Psmux {
    fn kind(&self) -> &str { "psmux" }

    fn bin(&self) -> &str { &self.bin }

    fn server_model(&self) -> ServerModel { ServerModel::PerSession }

    async fn enumerate(&self, transport: &Transport) -> Result<Vec<Session>, RunError> {
        // The registry (`~/.psmux/<name>.port`) is the authoritative existence set;
        // one list-sessions supplies display detail (empty on a default-route miss).
        let names = crate::source::read_psmux_registry_dir(&crate::source::psmux_registry_dir());
        let (name, args) = transport.exec_argv(false, &mux::list_sessions(&self.bin));
        let detail = match ExecRunner.run(&name, &args).await {
            Ok(out) => mux::parse_sessions(transport.host_id(), &String::from_utf8_lossy(&out)),
            Err(_) => Vec::new(),
        };
        Ok(crate::source::merge_psmux_sessions(transport.host_id(), names, detail))
    }

    fn attach_plan(&self, session: &str, _window: Option<i64>) -> Vec<String> {
        mux::attach(&self.bin, session)
    }

    fn switch_plan(&self, _session: &str) -> SwitchPlan {
        SwitchPlan::PerSessionNoOp
    }

    fn switch_client_argv(&self, display_tty: &str, session: &str) -> Vec<String> {
        // PerSession never lowers a switch (switch_plan is PerSessionNoOp, so
        // lower_switch returns None before this is reached). The trait is total, so
        // the argv is defined for completeness and uses the psmux binary.
        vec![
            self.bin.clone(),
            "switch-client".to_string(),
            "-c".to_string(),
            display_tty.to_string(),
            "-t".to_string(),
            mux::quote_target(session),
        ]
    }

    fn control_argv(&self) -> Option<Vec<String>> { None }

    fn death_signal(&self) -> DeathSignal { DeathSignal::PathStat { dir_is_psmux_registry: true } }

    fn event_source(&self) -> EventSource { EventSource::Poll { interval_ms: PSMUX_POLL_MS } }

    fn list_panes_plan(&self, session: &str) -> Vec<String> { mux::list_panes(&self.bin, session) }
    fn new_window_plan(&self, session: &str, name: &str) -> Vec<String> { mux::new_window(&self.bin, session, name) }
    fn split_window_plan(&self, target: &str, vertical: bool) -> Vec<String> { mux::split_window(&self.bin, target, vertical) }
    fn select_window_plan(&self, target: &str) -> Vec<String> { mux::select_window(&self.bin, target) }
    fn kill_window_plan(&self, target: &str) -> Vec<String> { mux::kill_window(&self.bin, target) }
    fn rename_window_plan(&self, target: &str, new: &str) -> Vec<String> { mux::rename_window(&self.bin, target, new) }
}

struct MuxKind {
    name: &'static str,
    make: fn(String) -> Box<dyn Mux>,
}

// `name` is the canonical identity, help-output marker, and conventional binary
// name. tmux is the implicit fallback because tmux has no positive help signal.
fn known_muxes() -> &'static [MuxKind] {
    &[MuxKind { name: "psmux", make: |bin| Box::new(Psmux { bin }) }]
}

/// Picks a mux backend by conventional binary name. tmux is the fallback, matching
/// the default in `Config::local_bin` / `host_specs`.
pub fn for_binary(bin: &str) -> Box<dyn Mux> {
    for k in known_muxes() {
        if k.name == bin {
            return (k.make)(bin.to_string());
        }
    }
    Box::new(Tmux { bin: bin.to_string() })
}

/// Builds a backend by canonical identity while preserving the binary used to
/// reach it.
pub fn for_kind(kind: &str, bin: &str) -> Box<dyn Mux> {
    for k in known_muxes() {
        if k.name == kind {
            return (k.make)(bin.to_string());
        }
    }
    Box::new(Tmux { bin: bin.to_string() })
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
pub async fn detect_backend(transport: &Transport, bin: &str, runner: &dyn Runner) -> Option<Box<dyn Mux>> {
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
        return Some(Box::new(Tmux { bin: bin.to_string() }));
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
        Psmux { bin: "psmux".into() }
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
        assert_eq!(m.attach_plan("api", None), argv(&["tmux", "attach", "-t", "api"]));
        // The window is selected separately (select_window_plan); attach stays plain.
        assert_eq!(m.attach_plan("api", Some(2)), argv(&["tmux", "attach", "-t", "api"]));
    }

    #[test]
    fn tmux_switch_plan_is_transport_blind_intent() {
        // Shared => Switch{session}. It names NO transport (codex C2): the Transport
        // lowers it. A psmux-style NotShared is impossible to produce here.
        assert_eq!(tmux().switch_plan("api"), SwitchPlan::Switch { session: "api".into() });
    }

    #[test]
    fn tmux_switch_client_argv_targets_the_captured_tty() {
        // The argv the supervisor closes over (with the captured DisplayTty) and
        // hands to Transport::lower_switch. It names the tty + session, never ssh.
        let m = tmux();
        assert_eq!(
            m.switch_client_argv("/dev/pts/3", "api"),
            argv(&["tmux", "switch-client", "-c", "/dev/pts/3", "-t", "api"])
        );
        // A session with control-mode metacharacters is quote_target-safe.
        assert_eq!(
            m.switch_client_argv("/dev/pts/3", "my proj"),
            argv(&["tmux", "switch-client", "-c", "/dev/pts/3", "-t", "'my proj'"])
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
        assert_eq!(m.select_window_plan("api:2"), mux::select_window("tmux", "api:2"));
        assert_eq!(m.new_window_plan("work", "logs"), mux::new_window("tmux", "work", "logs"));
        assert_eq!(m.split_window_plan("work:1", true), mux::split_window("tmux", "work:1", true));
        assert_eq!(m.kill_window_plan("api:2"), mux::kill_window("tmux", "api:2"));
        assert_eq!(m.rename_window_plan("api:2", "logs"), mux::rename_window("tmux", "api:2", "logs"));
    }

    // LIVE: enumerate over a real local tmux server. `#[ignore]` (needs tmux + a
    // server). Run on demand:
    //   cargo test --lib model::mux::tests::tmux_enumerate_live -- --ignored --nocapture
    #[ignore = "live: needs a running local tmux server"]
    #[tokio::test]
    async fn tmux_enumerate_live() {
        let t = Transport::Local { socket: None };
        let sessions = tmux().enumerate(&t).await.expect("reachable tmux (empty is Ok)");
        eprintln!("local tmux sessions: {:?}", sessions.iter().map(|s| &s.name).collect::<Vec<_>>());
    }

    use crate::model::plan::SwitchPlan;

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
    fn psmux_has_no_shared_attachment_to_switch() {
        // PerSession keeps one attachment PER SESSION — there is nothing to switch.
        assert_eq!(psmux().switch_plan("work"), SwitchPlan::PerSessionNoOp);
    }

    #[test]
    fn psmux_polls_and_dies_on_registry_stat() {
        // No host-level control stream: it is polled at the LOCAL_POLL_MS cadence
        // (cockpit.rs:48 = 1500). Death is the per-session registry stat.
        let m = psmux();
        assert_eq!(m.control_argv(), None);
        assert_eq!(m.event_source(), EventSource::Poll { interval_ms: 1500 });
        assert_eq!(m.death_signal(), DeathSignal::PathStat { dir_is_psmux_registry: true });
    }

    #[test]
    fn psmux_attach_plan_is_plain_attach() {
        assert_eq!(psmux().attach_plan("work", None), argv(&["psmux", "attach", "-t", "work"]));
    }

    #[test]
    fn psmux_window_plans_use_the_psmux_binary() {
        let m = psmux();
        assert_eq!(m.list_panes_plan("work"), mux::list_panes("psmux", "work"));
        assert_eq!(m.select_window_plan("work:1"), mux::select_window("psmux", "work:1"));
        assert_eq!(m.new_window_plan("work", "logs"), mux::new_window("psmux", "work", "logs"));
    }

    #[test]
    fn psmux_behavior_is_decoupled_from_invoked_binary() {
        let m = Psmux { bin: "tmux".into() };
        assert_eq!(m.attach_plan("api", None), argv(&["tmux", "attach", "-t", "api"]));
        assert_eq!(m.server_model(), ServerModel::PerSession);
        assert_eq!(m.kind(), "psmux");
    }

    #[test]
    fn tmux_behavior_is_decoupled_from_invoked_binary() {
        let m = Tmux { bin: "psmux".into() };
        assert_eq!(m.attach_plan("api", None), argv(&["psmux", "attach", "-t", "api"]));
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
        assert_eq!(got.attach_plan("api", None), argv(&["tmux", "attach", "-t", "api"]));
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
    //   cargo test --lib model::mux::tests::detect_backend_live -- --ignored --nocapture
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
}
