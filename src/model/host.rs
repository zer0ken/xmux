//! A first-class host (`Host`) — the single owner of one machine's transport, mux,
//! inventory, display BOOKKEEPING, captured display tty, and liveness. The rest of
//! the system addresses a machine through its `Host`, never through a bare alias
//! string. The live PTYs stay in `AttachRegistry`/`DisplayWorker`; this owns only
//! the bookkeeping of which session each attachment shows.

use std::collections::HashMap;

use crate::host::HostInventory;
use crate::machine::Transport;
use crate::model::DisplayTty;
use crate::mux::Mux;
use crate::source::Runner;

/// Connecting / live / unreachable — replaces the loose `connecting` AtomicBool
/// (host.rs:334) and the supervisor's `connected: HashSet` tracking (app.rs:1048).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Liveness {
    Connecting,
    Live,
    Unreachable,
}

/// The per-host display BOOKKEEPING. The
/// `AttachRegistry`/`Attachment`/`DisplayWorker` PTY MECHANISM OWNS the PTYs; this is
/// only the record of WHICH session each display_key currently shows and what spawn
/// is in flight, so it can never disagree with `display_key`.
#[derive(Default)]
pub struct HostDisplay {
    /// display_key -> the session it currently shows. `Shared`: one entry keyed by
    /// the host id. `PerSession`: one per `source/session`.
    pub current: HashMap<String, String>,
    /// display_key -> in-flight spawn seq.
    pub in_flight: HashMap<String, u64>,
    /// Spawned attachment ids whose PTY EOF'd BEFORE their off-loop Ready arrived (the
    /// Exited-raced-Ready case). The Ready arm tears the attachment down instead of
    /// inserting a dead pane.
    pub reaped_ids: std::collections::HashSet<u64>,
    /// In-flight attachment id → its display key, recorded at request time so a
    /// pre-Ready Exited (registry has no id yet) can be attributed to THIS host's
    /// reaped_ids. Cleared when the attachment registers (Ready) or fails.
    pub pending: std::collections::HashMap<u64, String>,
}

impl HostDisplay {
    /// The session `key`'s attachment currently shows, if any.
    pub fn shows(&self, key: &str) -> Option<&str> {
        self.current.get(key).map(String::as_str)
    }
    /// Record that `key`'s attachment now shows `session`.
    pub fn set_shows(&mut self, key: &str, session: &str) {
        self.current.insert(key.to_string(), session.to_string());
    }
    /// Record an in-flight spawn `seq` for `key`.
    pub fn mark_in_flight(&mut self, key: &str, seq: u64) {
        self.in_flight.insert(key.to_string(), seq);
    }
    /// Forget everything about `key` (its attachment closed/reaped), including any
    /// in-flight attach id recorded for it — so a spawn whose `key` is cleared while
    /// still in flight cannot leave a `pending` id that grows the map for the session's
    /// lifetime (its orphaned `Ready` would otherwise tear down without forgetting it).
    pub fn clear(&mut self, key: &str) {
        self.current.remove(key);
        self.in_flight.remove(key);
        self.pending.retain(|_, k| k != key);
    }
}

/// A first-class host: one machine reachable by one transport, running one mux,
/// owning its inventory, its display BOOKKEEPING, its captured display tty, and its
/// liveness — the single owner of all per-machine state, keyed by a stable host id
/// rather than a bare alias string. The PTYs are NOT here — they live in
/// `AttachRegistry`/`DisplayWorker`; `Host` owns only the bookkeeping.
pub struct Host {
    pub transport: Box<dyn Transport>,
    pub mux: Box<dyn Mux>,
    /// Live session/window inventory.
    pub inventory: HostInventory,
    /// Which session each display_key shows + what spawn is in flight.
    pub display: HostDisplay,
    /// xmux's own display-client tty, captured in memory. Read by the driver's in-place
    /// switch to build `mux.switch_client_argv(tty, session)`.
    pub display_tty: DisplayTty,
    pub liveness: Liveness,
    pub(crate) detected: bool,
}

impl Host {
    /// Builds a host from a transport + mux. Replaces `source::build`'s per-source
    /// construction (source.rs:460), one host at a time.
    pub fn new(transport: Box<dyn Transport>, mux: Box<dyn Mux>) -> Self {
        Host {
            transport,
            mux,
            inventory: HostInventory::new(),
            display: HostDisplay::default(),
            display_tty: DisplayTty::default(),
            liveness: Liveness::Connecting,
            detected: false,
        }
    }

    /// The stable host id (`transport.host_id()`).
    pub fn id(&self) -> &str {
        self.transport.host_id()
    }

    pub(crate) async fn detect_and_correct(&mut self, runner: &dyn Runner) {
        if self.detected {
            return;
        }
        let bin = self.mux.bin().to_string();
        let Some(mux) = crate::mux::detect_backend(&self.transport, &bin, runner).await else {
            return;
        };
        if mux.kind() != self.mux.kind() {
            self.mux = mux;
        }
        self.detected = true;
    }

    /// Re-enumerate this host's sessions through the mux, updating `inventory` and
    /// `liveness`. `Ok` (possibly empty) ⇒ `Live`; `Err` ⇒ `Unreachable` (and the
    /// error propagates). The single owner of this host's session list and reachability.
    pub async fn enumerate(&mut self) -> Result<(), crate::source::RunError> {
        self.detect_and_correct(&crate::source::ExecRunner).await;
        match self
            .mux
            .enumerate(&self.transport, &crate::source::ExecRunner)
            .await
        {
            Ok(sessions) => {
                self.inventory.sessions = sessions;
                self.liveness = Liveness::Live;
                Ok(())
            }
            Err(e) => {
                self.liveness = Liveness::Unreachable;
                Err(e)
            }
        }
    }

    /// Record xmux's display-client tty for this host, captured in memory from the
    /// PTY marker (no `/tmp` file). The supervisor reads it to build a
    /// `switch-client -c <tty>` (via `mux.switch_client_argv`) that targets xmux's
    /// own display client only.
    pub fn record_display_tty(&mut self, tty: Option<String>) {
        self.display_tty = DisplayTty(tty);
    }

    /// Forget the display tty when the attachment dies, so no later `switch-client`
    /// is aimed at a detached/dead client (the blank-pane class).
    pub fn clear_display_tty(&mut self) {
        self.display_tty = DisplayTty(None);
    }

    /// True when `client` (a `%client-detached` client tty) is xmux's OWN display
    /// client under this mux's death signal. Delegates to the free
    /// `matches_display_tty` so the filter logic has one home.
    pub fn matches_display_tty(&self, client: &str) -> bool {
        crate::model::death::matches_display_tty(
            &self.mux.death_signal(),
            client,
            &self.display_tty,
        )
    }

    /// True when `session` is still live under this mux's death signal. PerSession
    /// (psmux) ⇒ the `.port` stat; any other model ⇒ always live (death arrives by
    /// EOF/ControlNotice, not a port file).
    pub fn psmux_session_live(&self, session: &str) -> bool {
        match self.mux.death_signal() {
            crate::model::DeathSignal::PathStat {
                dir_is_psmux_registry: true,
            } => crate::model::death::psmux_session_is_live(session),
            _ => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{DeathSignal, EventSource, ServerModel};
    use crate::mux::Mux;
    use crate::session::Session;
    use crate::source::{RunError, Runner};

    /// A minimal in-test mux: only `server_model` is exercised in these tests. The other
    /// methods return trivially since they wire no I/O — including the window and session
    /// lifecycle plans, which these tests never invoke.
    struct StubMux(ServerModel);

    #[async_trait::async_trait]
    impl Mux for StubMux {
        fn kind(&self) -> &str {
            "stub"
        }
        fn bin(&self) -> &str {
            "stub"
        }
        fn server_model(&self) -> ServerModel {
            self.0
        }
        fn driver(&self) -> Box<dyn crate::driver::MuxDriver> {
            Box::new(StubDriver)
        }
        async fn enumerate(
            &self,
            _t: &dyn Transport,
            _r: &dyn crate::source::Runner,
        ) -> Result<Vec<Session>, RunError> {
            Ok(vec![])
        }
        fn attach_plan(&self, _s: &str, _w: Option<i64>) -> Vec<String> {
            vec![]
        }
        fn control_argv(&self) -> Option<Vec<String>> {
            None
        }
        fn death_signal(&self) -> DeathSignal {
            DeathSignal::Eof
        }
        fn event_source(&self) -> EventSource {
            EventSource::Poll { interval_ms: 1500 }
        }
        fn list_panes_plan(&self, _s: &str) -> Vec<String> {
            vec![]
        }
        fn new_window_plan(&self, _s: &str, _n: &str) -> Vec<String> {
            vec![]
        }
        fn split_window_plan(&self, _t: &str, _v: bool) -> Vec<String> {
            vec![]
        }
        fn select_window_plan(&self, _t: &str) -> Vec<String> {
            vec![]
        }
        fn kill_window_plan(&self, _t: &str) -> Vec<String> {
            vec![]
        }
        fn rename_window_plan(&self, _t: &str, _n: &str) -> Vec<String> {
            vec![]
        }
        fn new_session_plan(&self, _n: &str) -> Vec<String> {
            vec![]
        }
        fn kill_session_plan(&self, _n: &str) -> Vec<String> {
            vec![]
        }
        fn rename_session_plan(&self, _o: &str, _n: &str) -> Vec<String> {
            vec![]
        }
    }

    /// A no-op display driver for `StubMux`: these tests exercise only host domain
    /// state, never display orchestration, so every method wires no I/O.
    struct StubDriver;

    impl crate::driver::MuxDriver for StubDriver {
        fn kind(&self) -> &str {
            "stub"
        }
        fn show(
            &mut self,
            _sel: &crate::app::runtime::Selection,
            _ctx: &mut crate::driver::DriverCtx,
        ) -> bool {
            false
        }
        fn grid(
            &self,
            _sel: &crate::app::runtime::Selection,
            _ctx: &crate::driver::DriverCtx,
        ) -> Option<std::sync::Arc<std::sync::Mutex<crate::display::grid::Grid>>> {
            None
        }
        fn input(
            &mut self,
            _sel: &crate::app::runtime::Selection,
            _bytes: Vec<u8>,
            _ctx: &crate::driver::DriverCtx,
        ) {
        }
        fn sync(
            &mut self,
            _source: &str,
            _sessions: &[crate::session::Session],
            _ctx: &mut crate::driver::DriverCtx,
        ) {
        }
    }

    #[test]
    fn host_id_is_the_transport_host_id() {
        let h = Host::new(
            crate::machine::local(None),
            Box::new(StubMux(ServerModel::Shared)),
        );
        assert_eq!(h.id(), "local");
        let r = Host::new(
            crate::machine::ssh("jup".into(), String::new(), "linux".into()),
            Box::new(StubMux(ServerModel::Shared)),
        );
        assert_eq!(r.id(), "jup");
    }

    #[test]
    fn new_host_starts_connecting_with_empty_inventory_and_tty() {
        let h = Host::new(
            crate::machine::local(None),
            Box::new(StubMux(ServerModel::PerSession)),
        );
        assert_eq!(h.liveness, Liveness::Connecting);
        assert!(h.inventory.sessions.is_empty());
        assert!(h.display_tty.0.is_none());
    }

    #[test]
    fn host_display_tracks_current_session_per_key() {
        let mut d = HostDisplay::default();
        assert_eq!(d.shows("jup"), None, "nothing shown until set");
        d.set_shows("jup", "api");
        assert_eq!(d.shows("jup"), Some("api"));
        d.set_shows("jup", "build");
        assert_eq!(
            d.shows("jup"),
            Some("build"),
            "set overwrites the shown session"
        );
    }

    #[test]
    fn host_display_clear_forgets_current_in_flight_and_pending() {
        let mut d = HostDisplay::default();
        d.set_shows("local/work", "work");
        d.mark_in_flight("local/work", 7);
        d.pending.insert(7, "local/work".into());
        assert_eq!(d.in_flight.get("local/work"), Some(&7));
        d.clear("local/work");
        assert_eq!(
            d.shows("local/work"),
            None,
            "clear forgets the shown session"
        );
        assert_eq!(
            d.in_flight.get("local/work"),
            None,
            "clear forgets the in-flight seq"
        );
        assert!(
            d.pending.is_empty(),
            "clear forgets the key's pending id so a dead attach cannot leak it forever"
        );
    }

    #[test]
    fn host_display_tracks_reaped_and_pending() {
        let mut d = HostDisplay::default();
        assert!(d.reaped_ids.is_empty(), "reaped_ids defaults empty");
        assert!(d.pending.is_empty(), "pending defaults empty");
        // A pre-Ready Exited records the dead id; its Ready later removes it.
        d.pending.insert(7, "jup".into());
        d.reaped_ids.insert(7);
        assert_eq!(
            d.pending.get(&7),
            Some(&"jup".to_string()),
            "pending maps id -> key"
        );
        assert!(d.reaped_ids.contains(&7), "reaped_ids holds the dead id");
        d.reaped_ids.remove(&7);
        d.pending.remove(&7);
        assert!(
            d.reaped_ids.is_empty() && d.pending.is_empty(),
            "round-trips back to empty"
        );
    }

    #[test]
    fn liveness_is_copy_and_comparable() {
        let l = Liveness::Connecting;
        assert_eq!(l, Liveness::Connecting);
        assert_ne!(Liveness::Live, Liveness::Unreachable);
    }

    struct EnumMux {
        model: ServerModel,
        result: std::sync::Mutex<Option<Result<Vec<Session>, RunError>>>,
    }
    impl EnumMux {
        fn ok(model: ServerModel, names: &[&str]) -> Self {
            let sessions = names
                .iter()
                .map(|n| Session {
                    source: "h".into(),
                    name: (*n).into(),
                    windows: 1,
                    attached: false,
                    last_attached: 0,
                })
                .collect();
            EnumMux {
                model,
                result: std::sync::Mutex::new(Some(Ok(sessions))),
            }
        }
        fn err(model: ServerModel) -> Self {
            EnumMux {
                model,
                result: std::sync::Mutex::new(Some(Err(RunError::Other("down".into())))),
            }
        }
    }
    #[async_trait::async_trait]
    impl Mux for EnumMux {
        fn kind(&self) -> &str {
            "enum"
        }
        fn bin(&self) -> &str {
            "enum"
        }
        fn server_model(&self) -> ServerModel {
            self.model
        }
        fn driver(&self) -> Box<dyn crate::driver::MuxDriver> {
            Box::new(StubDriver)
        }
        async fn enumerate(
            &self,
            _t: &dyn Transport,
            _r: &dyn crate::source::Runner,
        ) -> Result<Vec<Session>, RunError> {
            self.result.lock().unwrap().take().unwrap_or(Ok(vec![]))
        }
        fn attach_plan(&self, _s: &str, _w: Option<i64>) -> Vec<String> {
            vec![]
        }
        fn control_argv(&self) -> Option<Vec<String>> {
            None
        }
        fn death_signal(&self) -> DeathSignal {
            DeathSignal::Eof
        }
        fn event_source(&self) -> EventSource {
            EventSource::Poll { interval_ms: 1500 }
        }
        fn list_panes_plan(&self, _s: &str) -> Vec<String> {
            vec![]
        }
        fn new_window_plan(&self, _s: &str, _n: &str) -> Vec<String> {
            vec![]
        }
        fn split_window_plan(&self, _t: &str, _v: bool) -> Vec<String> {
            vec![]
        }
        fn select_window_plan(&self, _t: &str) -> Vec<String> {
            vec![]
        }
        fn kill_window_plan(&self, _t: &str) -> Vec<String> {
            vec![]
        }
        fn rename_window_plan(&self, _t: &str, _n: &str) -> Vec<String> {
            vec![]
        }
        fn new_session_plan(&self, _n: &str) -> Vec<String> {
            vec![]
        }
        fn kill_session_plan(&self, _n: &str) -> Vec<String> {
            vec![]
        }
        fn rename_session_plan(&self, _o: &str, _n: &str) -> Vec<String> {
            vec![]
        }
    }

    #[tokio::test]
    async fn enumerate_ok_fills_inventory_and_goes_live() {
        let mut h = Host::new(
            crate::machine::local(None),
            Box::new(EnumMux::ok(ServerModel::PerSession, &["work", "build"])),
        );
        h.enumerate().await.unwrap();
        assert_eq!(h.liveness, Liveness::Live);
        let names: Vec<&str> = h
            .inventory
            .sessions
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(names, vec!["work", "build"]);
    }

    #[tokio::test]
    async fn enumerate_empty_is_live_not_unreachable() {
        // A reachable mux with zero sessions is Live (the "(empty)" case), not Unreachable.
        let mut h = Host::new(
            crate::machine::local(None),
            Box::new(EnumMux::ok(ServerModel::Shared, &[])),
        );
        h.enumerate().await.unwrap();
        assert_eq!(h.liveness, Liveness::Live);
        assert!(h.inventory.sessions.is_empty());
    }

    #[tokio::test]
    async fn enumerate_err_marks_unreachable_and_propagates() {
        let mut h = Host::new(
            crate::machine::local(None),
            Box::new(EnumMux::err(ServerModel::Shared)),
        );
        assert!(h.enumerate().await.is_err());
        assert_eq!(h.liveness, Liveness::Unreachable);
    }

    #[test]
    fn record_and_clear_display_tty_round_trips() {
        let mut h = Host::new(
            crate::machine::ssh("jup".into(), String::new(), "linux".into()),
            Box::new(StubMux(ServerModel::Shared)),
        );
        assert!(h.display_tty.0.is_none(), "starts with no tty");
        h.record_display_tty(Some("/dev/pts/3".into()));
        assert_eq!(h.display_tty.0.as_deref(), Some("/dev/pts/3"));
        // The display attachment died: the tty is cleared so no later switch-client targets it.
        h.clear_display_tty();
        assert!(
            h.display_tty.0.is_none(),
            "clear forgets the dead client's tty"
        );
    }

    #[test]
    fn matches_display_tty_only_for_our_own_client_under_control_notice() {
        use crate::model::DisplayTty;
        let mut h = Host::new(
            crate::machine::ssh("jup".into(), String::new(), "linux".into()),
            crate::mux::for_binary("tmux"), // Shared → DeathSignal::ControlNotice
        );
        assert!(
            !h.matches_display_tty("/dev/pts/3"),
            "no captured tty → inert"
        );
        h.display_tty = DisplayTty(Some("/dev/pts/3".into()));
        assert!(
            h.matches_display_tty("/dev/pts/3"),
            "our own client's tty matches"
        );
        assert!(
            !h.matches_display_tty("/dev/pts/9"),
            "an unrelated client never matches"
        );
    }

    #[test]
    fn psmux_host_session_liveness_uses_the_port_stat() {
        let h = Host::new(
            crate::machine::local(None),
            crate::mux::for_binary("psmux"), // PerSession → DeathSignal::PathStat
        );
        let name = format!("xmux-hostlive-{}", std::process::id());
        let path = crate::model::death::psmux_port_path(&name);
        let _ = std::fs::create_dir_all(path.parent().unwrap());
        std::fs::write(&path, b"40001").unwrap();
        assert!(h.psmux_session_live(&name), "a present .port ⇒ live");
        std::fs::remove_file(&path).unwrap();
        assert!(!h.psmux_session_live(&name), "a vanished .port ⇒ not live");
    }

    #[test]
    fn tmux_host_session_is_always_live_by_port_stat() {
        let h = Host::new(
            crate::machine::ssh("jup".into(), String::new(), "linux".into()),
            crate::mux::for_binary("tmux"), // Shared → not PathStat
        );
        // A Shared host never dies by a .port file — liveness here is unconditionally true.
        assert!(h.psmux_session_live("anything"));
    }

    struct DetectRunner {
        result: std::sync::Mutex<Result<Vec<u8>, RunError>>,
        calls: std::sync::atomic::AtomicUsize,
    }

    impl DetectRunner {
        fn ok(out: &str) -> Self {
            DetectRunner {
                result: std::sync::Mutex::new(Ok(out.as_bytes().to_vec())),
                calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }

        fn err() -> Self {
            DetectRunner {
                result: std::sync::Mutex::new(Err(RunError::Other("down".into()))),
                calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl Runner for DetectRunner {
        async fn run(&self, _name: &str, _args: &[String]) -> Result<Vec<u8>, RunError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            match &*self.result.lock().unwrap() {
                Ok(out) => Ok(out.clone()),
                Err(RunError::Exit { stderr, code }) => Err(RunError::Exit {
                    stderr: stderr.clone(),
                    code: *code,
                }),
                Err(RunError::Other(e)) => Err(RunError::Other(e.clone())),
            }
        }
    }

    #[tokio::test]
    async fn detect_and_correct_replaces_behavior_and_preserves_bin() {
        let mut h = Host::new(crate::machine::local(None), crate::mux::for_binary("tmux"));
        let runner = DetectRunner::ok("psmux command help");
        h.detect_and_correct(&runner).await;
        assert_eq!(h.mux.kind(), "psmux");
        assert_eq!(h.mux.server_model(), ServerModel::PerSession);
        assert_eq!(
            h.mux.attach_plan("api", None),
            vec!["tmux", "new-session", "-A", "-s", "api"]
        );
        assert!(h.detected);

        h.detect_and_correct(&runner).await;
        assert_eq!(runner.calls(), 1);
    }

    #[tokio::test]
    async fn detect_and_correct_retries_after_inconclusive_probe() {
        let mut h = Host::new(crate::machine::local(None), crate::mux::for_binary("tmux"));
        let runner = DetectRunner::err();
        h.detect_and_correct(&runner).await;
        assert_eq!(h.mux.kind(), "tmux");
        assert_eq!(h.mux.server_model(), ServerModel::Shared);
        assert!(!h.detected);
    }
}
