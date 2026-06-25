//! A first-class host (`Host`) — the single owner of one machine's transport, mux,
//! inventory, display BOOKKEEPING, captured display tty, and liveness. The rest of
//! the system addresses a machine through its `Host`, never through a bare alias
//! string. The live PTYs stay in `AttachRegistry`/`DisplayWorker`; this owns only
//! the bookkeeping of which session each attachment shows.

use std::collections::HashMap;

use crate::host::HostInventory;
use crate::model::{DisplayTty, Mux, ServerModel, Transport};

/// Connecting / live / unreachable — replaces the loose `connecting` AtomicBool
/// (host.rs:334) and the supervisor's `connected: HashSet` tracking (cockpit.rs:1048).
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
    pub transport: Transport,
    pub mux: Box<dyn Mux>,
    /// Live session/window inventory.
    pub inventory: HostInventory,
    /// Which session each display_key shows + what spawn is in flight.
    pub display: HostDisplay,
    /// xmux's own display-client tty, captured in memory. Read by the supervisor to
    /// build `mux.switch_client_argv(tty, session)` for `Transport::lower_switch`.
    pub display_tty: DisplayTty,
    pub liveness: Liveness,
    control: Option<crate::host::HostClient>,
}

impl Host {
    /// Builds a host from a transport + mux. Replaces `source::build`'s per-source
    /// construction (source.rs:460), one host at a time.
    pub fn new(transport: Transport, mux: Box<dyn Mux>) -> Self {
        Host {
            transport,
            mux,
            inventory: HostInventory::new(),
            display: HostDisplay::default(),
            display_tty: DisplayTty::default(),
            liveness: Liveness::Connecting,
            control: None,
        }
    }

    /// The stable host id (`transport.host_id()`).
    pub fn id(&self) -> &str {
        self.transport.host_id()
    }

    /// The `AttachRegistry` key for `address` under this host's model — the SINGLE
    /// definition, replacing the free `display_key` fn (cockpit.rs:245).
    pub fn display_key(&self, address: &str) -> String {
        self.mux.server_model().display_key(self.id(), address)
    }

    /// Re-enumerate this host's sessions through the mux, updating `inventory` and
    /// `liveness`. `Ok` (possibly empty) ⇒ `Live`; `Err` ⇒ `Unreachable` (and the
    /// error propagates). Replaces `Source::list_sessions` + `HostClient::list_sessions`
    /// + the supervisor `connected` bookkeeping.
    pub async fn enumerate(&mut self) -> Result<(), crate::source::RunError> {
        match self.mux.enumerate(&self.transport).await {
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
        crate::model::death::matches_display_tty(&self.mux.death_signal(), client, &self.display_tty)
    }

    /// True when `session` is still live under this mux's death signal. PerSession
    /// (psmux) ⇒ the `.port` stat; any other model ⇒ always live (death arrives by
    /// EOF/ControlNotice, not a port file).
    pub fn psmux_session_live(&self, session: &str) -> bool {
        match self.mux.death_signal() {
            crate::model::DeathSignal::PathStat { dir_is_psmux_registry: true } => {
                crate::model::death::psmux_session_is_live(session)
            }
            _ => true,
        }
    }

    /// Ensures this host's `-CC` control client exists, spawning + owning it lazily.
    /// `Ok(true)` on a fresh spawn; `Ok(false)` if already present or if this host's
    /// `event_source()` is `Poll` (psmux has no host-level control stream). Moves the
    /// mechanism `HostManager::ensure` (host.rs:570) held onto the Host.
    pub fn ensure_control_client(
        &mut self,
        cols: u16,
        rows: u16,
        events: &tokio::sync::mpsc::UnboundedSender<crate::host::HostEvent>,
    ) -> anyhow::Result<bool> {
        if !matches!(self.mux.event_source(), crate::model::EventSource::Control) {
            return Ok(false);
        }
        if self.control.is_some() {
            return Ok(false);
        }
        let Some(mux_control) = self.mux.control_argv() else {
            return Ok(false);
        };
        let argv = self.transport.control_argv(&mux_control);
        let client = crate::host::HostClient::spawn(
            self.id(),
            &argv,
            cols,
            rows,
            events.clone(),
            &[],
        )?;
        self.control = Some(client);
        Ok(true)
    }
}

/// One reconciliation step the supervisor executes through the KEPT DisplayWorker /
/// AttachRegistry. `Host::sync` returns these instead of the supervisor branching on
/// `remote` to fan out attaches/reaps (cockpit.rs:401/420).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyncAction {
    /// Spawn (or warm) the attachment for `key`, landing on `session`.
    Attach { key: String, session: String },
    /// Tear down `key`'s attachment (its session/host closed).
    Reap { key: String },
}

impl Host {
    /// The attach/reap plan that reconciles this host's display set with its
    /// inventory under its `ServerModel`. Pure over `&self`: `Shared` keeps ONE
    /// attachment (host key) warmed on the first session and reaps it when empty;
    /// `PerSession` keeps one per session (address key) and reaps closed ones.
    /// Folds the per-host half of `sync_source_terminals` (cockpit.rs:384).
    pub fn sync(&self) -> Vec<SyncAction> {
        match self.mux.server_model() {
            ServerModel::Shared => {
                let key = self.id().to_string();
                match self.inventory.sessions.first() {
                    Some(first) if self.display.shows(&key).is_none() => {
                        vec![SyncAction::Attach { key, session: first.name.clone() }]
                    }
                    None if self.display.shows(&key).is_some() => {
                        vec![SyncAction::Reap { key }]
                    }
                    _ => Vec::new(),
                }
            }
            ServerModel::PerSession => {
                let mut actions = Vec::new();
                let mut desired = std::collections::HashSet::new();
                for s in &self.inventory.sessions {
                    let key = self.display_key(&s.address());
                    desired.insert(key.clone());
                    if self.display.shows(&key).is_none() {
                        actions.push(SyncAction::Attach { key, session: s.name.clone() });
                    }
                }
                for key in self.display.current.keys() {
                    if !desired.contains(key) {
                        actions.push(SyncAction::Reap { key: key.clone() });
                    }
                }
                // ponytail: reap order is non-deterministic (HashMap iteration); tests
                // assert a single reap so this is fine. Sort if multi-reap order matters.
                actions
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{DeathSignal, EventSource, Mux, ServerModel, SwitchPlan, Transport};
    use crate::session::Session;
    use crate::source::RunError;

    /// A minimal in-test mux: only `server_model` is exercised in the early tasks. The
    /// other methods return trivially since these tasks wire no I/O. Shaped to the
    /// REVISED `Mux` trait (switch_plan/switch_client_argv, ControlNotice, no lifecycle).
    struct StubMux(ServerModel);

    #[async_trait::async_trait]
    impl Mux for StubMux {
        fn kind(&self) -> &str { "stub" }
        fn server_model(&self) -> ServerModel { self.0 }
        async fn enumerate(&self, _t: &Transport) -> Result<Vec<Session>, RunError> { Ok(vec![]) }
        fn attach_plan(&self, _s: &str, _w: Option<i64>) -> Vec<String> { vec![] }
        fn switch_plan(&self, _s: &str) -> SwitchPlan { SwitchPlan::PerSessionNoOp }
        fn switch_client_argv(&self, _tty: &str, _s: &str) -> Vec<String> { vec![] }
        fn control_argv(&self) -> Option<Vec<String>> { None }
        fn death_signal(&self) -> DeathSignal { DeathSignal::Eof }
        fn event_source(&self) -> EventSource { EventSource::Poll { interval_ms: 1500 } }
        fn list_panes_plan(&self, _s: &str) -> Vec<String> { vec![] }
        fn new_window_plan(&self, _s: &str, _n: &str) -> Vec<String> { vec![] }
        fn split_window_plan(&self, _t: &str, _v: bool) -> Vec<String> { vec![] }
        fn select_window_plan(&self, _t: &str) -> Vec<String> { vec![] }
        fn kill_window_plan(&self, _t: &str) -> Vec<String> { vec![] }
        fn rename_window_plan(&self, _t: &str, _n: &str) -> Vec<String> { vec![] }
    }

    #[test]
    fn host_id_is_the_transport_host_id() {
        let h = Host::new(Transport::Local { socket: None }, Box::new(StubMux(ServerModel::Shared)));
        assert_eq!(h.id(), "local");
        let r = Host::new(
            Transport::Ssh { alias: "jup".into(), control_path: String::new(), os: "linux".into() },
            Box::new(StubMux(ServerModel::Shared)),
        );
        assert_eq!(r.id(), "jup");
    }

    #[test]
    fn display_key_shape_comes_from_the_mux_model_not_a_remote_bool() {
        // Shared (tmux) -> one PTY per HOST: key = host id, ignoring the address.
        let shared = Host::new(
            Transport::Ssh { alias: "jup".into(), control_path: String::new(), os: "linux".into() },
            Box::new(StubMux(ServerModel::Shared)),
        );
        assert_eq!(shared.display_key("jup/api"), "jup", "shared -> per-host key");
        // PerSession (psmux) -> one PTY per SESSION: key = the address.
        let per = Host::new(
            Transport::Local { socket: None },
            Box::new(StubMux(ServerModel::PerSession)),
        );
        assert_eq!(per.display_key("local/work"), "local/work", "per-session -> per-session key");
    }

    #[test]
    fn new_host_starts_connecting_with_empty_inventory_and_tty() {
        let h = Host::new(Transport::Local { socket: None }, Box::new(StubMux(ServerModel::PerSession)));
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
        assert_eq!(d.shows("jup"), Some("build"), "set overwrites the shown session");
    }

    #[test]
    fn host_display_clear_forgets_current_in_flight_and_pending() {
        let mut d = HostDisplay::default();
        d.set_shows("local/work", "work");
        d.mark_in_flight("local/work", 7);
        d.pending.insert(7, "local/work".into());
        assert_eq!(d.in_flight.get("local/work"), Some(&7));
        d.clear("local/work");
        assert_eq!(d.shows("local/work"), None, "clear forgets the shown session");
        assert_eq!(d.in_flight.get("local/work"), None, "clear forgets the in-flight seq");
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
        assert_eq!(d.pending.get(&7), Some(&"jup".to_string()), "pending maps id -> key");
        assert!(d.reaped_ids.contains(&7), "reaped_ids holds the dead id");
        d.reaped_ids.remove(&7);
        d.pending.remove(&7);
        assert!(d.reaped_ids.is_empty() && d.pending.is_empty(), "round-trips back to empty");
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
            let sessions = names.iter().map(|n| Session {
                source: "h".into(), name: (*n).into(), windows: 1, attached: false, last_attached: 0,
            }).collect();
            EnumMux { model, result: std::sync::Mutex::new(Some(Ok(sessions))) }
        }
        fn err(model: ServerModel) -> Self {
            EnumMux { model, result: std::sync::Mutex::new(Some(Err(RunError::Other("down".into())))) }
        }
    }
    #[async_trait::async_trait]
    impl Mux for EnumMux {
        fn kind(&self) -> &str { "enum" }
        fn server_model(&self) -> ServerModel { self.model }
        async fn enumerate(&self, _t: &Transport) -> Result<Vec<Session>, RunError> {
            self.result.lock().unwrap().take().unwrap_or(Ok(vec![]))
        }
        fn attach_plan(&self, _s: &str, _w: Option<i64>) -> Vec<String> { vec![] }
        fn switch_plan(&self, _s: &str) -> SwitchPlan { SwitchPlan::PerSessionNoOp }
        fn switch_client_argv(&self, _tty: &str, _s: &str) -> Vec<String> { vec![] }
        fn control_argv(&self) -> Option<Vec<String>> { None }
        fn death_signal(&self) -> DeathSignal { DeathSignal::Eof }
        fn event_source(&self) -> EventSource { EventSource::Poll { interval_ms: 1500 } }
        fn list_panes_plan(&self, _s: &str) -> Vec<String> { vec![] }
        fn new_window_plan(&self, _s: &str, _n: &str) -> Vec<String> { vec![] }
        fn split_window_plan(&self, _t: &str, _v: bool) -> Vec<String> { vec![] }
        fn select_window_plan(&self, _t: &str) -> Vec<String> { vec![] }
        fn kill_window_plan(&self, _t: &str) -> Vec<String> { vec![] }
        fn rename_window_plan(&self, _t: &str, _n: &str) -> Vec<String> { vec![] }
    }

    #[tokio::test]
    async fn enumerate_ok_fills_inventory_and_goes_live() {
        let mut h = Host::new(Transport::Local { socket: None }, Box::new(EnumMux::ok(ServerModel::PerSession, &["work", "build"])));
        h.enumerate().await.unwrap();
        assert_eq!(h.liveness, Liveness::Live);
        let names: Vec<&str> = h.inventory.sessions.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["work", "build"]);
    }

    #[tokio::test]
    async fn enumerate_empty_is_live_not_unreachable() {
        // A reachable mux with zero sessions is Live (the "(empty)" case), not Unreachable.
        let mut h = Host::new(Transport::Local { socket: None }, Box::new(EnumMux::ok(ServerModel::Shared, &[])));
        h.enumerate().await.unwrap();
        assert_eq!(h.liveness, Liveness::Live);
        assert!(h.inventory.sessions.is_empty());
    }

    #[tokio::test]
    async fn enumerate_err_marks_unreachable_and_propagates() {
        let mut h = Host::new(Transport::Local { socket: None }, Box::new(EnumMux::err(ServerModel::Shared)));
        assert!(h.enumerate().await.is_err());
        assert_eq!(h.liveness, Liveness::Unreachable);
    }

    #[test]
    fn record_and_clear_display_tty_round_trips() {
        let mut h = Host::new(
            Transport::Ssh { alias: "jup".into(), control_path: String::new(), os: "linux".into() },
            Box::new(StubMux(ServerModel::Shared)),
        );
        assert!(h.display_tty.0.is_none(), "starts with no tty");
        h.record_display_tty(Some("/dev/pts/3".into()));
        assert_eq!(h.display_tty.0.as_deref(), Some("/dev/pts/3"));
        // The display attachment died: the tty is cleared so no later switch-client targets it.
        h.clear_display_tty();
        assert!(h.display_tty.0.is_none(), "clear forgets the dead client's tty");
    }

    #[tokio::test]
    async fn ensure_control_client_is_a_noop_for_a_poll_host() {
        // A PerSession host has event_source() == Poll, so there is no -CC client to
        // ensure: ensure_control_client returns Ok(false) and spawns nothing.
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<crate::host::HostEvent>();
        let mut h = Host::new(
            Transport::Local { socket: None },
            Box::new(StubMux(ServerModel::PerSession)), // StubMux::event_source == Poll
        );
        assert!(!h.ensure_control_client(80, 24, &tx).unwrap());
    }

    fn sess(name: &str) -> Session {
        Session { source: "h".into(), name: name.into(), windows: 1, attached: false, last_attached: 0 }
    }

    #[test]
    fn sync_shared_attaches_the_first_session_once_then_nothing() {
        // Shared (tmux): ONE attachment per host, keyed by host id, warmed on the first
        // session. With nothing attached yet, sync asks to attach the first session.
        let mut h = Host::new(
            Transport::Ssh { alias: "jup".into(), control_path: String::new(), os: "linux".into() },
            Box::new(StubMux(ServerModel::Shared)),
        );
        h.inventory.sessions = vec![sess("api"), sess("build")];
        assert_eq!(
            h.sync(),
            vec![SyncAction::Attach { key: "jup".into(), session: "api".into() }],
            "shared: warm one PTY (host key) on the first session"
        );
        // Once the host key is shown, sync asks for nothing more (selection-driven
        // switch-client is the supervisor's job, not sync's keep-alive).
        h.display.set_shows("jup", "api");
        assert!(h.sync().is_empty(), "shared: already warmed -> no action");
    }

    #[test]
    fn sync_shared_with_no_sessions_reaps_the_host_pty() {
        let mut h = Host::new(
            Transport::Ssh { alias: "jup".into(), control_path: String::new(), os: "linux".into() },
            Box::new(StubMux(ServerModel::Shared)),
        );
        h.display.set_shows("jup", "api"); // a PTY is warm
        h.inventory.sessions = vec![]; // host went empty
        assert_eq!(h.sync(), vec![SyncAction::Reap { key: "jup".into() }]);
    }

    #[test]
    fn sync_per_session_attaches_each_missing_and_reaps_closed() {
        // PerSession (psmux): one attachment per session, keyed by address. The stub
        // sessions have source "h", so their addresses are "h/work" etc.; the local host
        // id is "local" but display_key for PerSession uses the address, so keys are
        // "h/work"/"h/build".
        let mut h = Host::new(Transport::Local { socket: None }, Box::new(StubMux(ServerModel::PerSession)));
        h.inventory.sessions = vec![sess("work"), sess("build")];
        h.display.set_shows("h/build", "build"); // build already attached
        let got = h.sync();
        assert_eq!(
            got,
            vec![SyncAction::Attach { key: "h/work".into(), session: "work".into() }],
            "per-session: attach the missing one only"
        );
        // A session that closed (shown but no longer in inventory) is reaped.
        let mut h2 = Host::new(Transport::Local { socket: None }, Box::new(StubMux(ServerModel::PerSession)));
        h2.inventory.sessions = vec![sess("work")];
        h2.display.set_shows("h/build", "build");
        h2.display.set_shows("h/work", "work");
        let reaps: Vec<_> = h2.sync().into_iter().filter(|a| matches!(a, SyncAction::Reap { .. })).collect();
        assert_eq!(reaps, vec![SyncAction::Reap { key: "h/build".into() }], "per-session: reap the closed session");
    }

    #[test]
    fn matches_display_tty_only_for_our_own_client_under_control_notice() {
        use crate::model::{DisplayTty, Transport};
        let mut h = Host::new(
            Transport::Ssh { alias: "jup".into(), control_path: String::new(), os: "linux".into() },
            crate::model::mux::for_binary("tmux"), // Shared → DeathSignal::ControlNotice
        );
        assert!(!h.matches_display_tty("/dev/pts/3"), "no captured tty → inert");
        h.display_tty = DisplayTty(Some("/dev/pts/3".into()));
        assert!(h.matches_display_tty("/dev/pts/3"), "our own client's tty matches");
        assert!(!h.matches_display_tty("/dev/pts/9"), "an unrelated client never matches");
    }

    #[test]
    fn psmux_host_session_liveness_uses_the_port_stat() {
        use crate::model::Transport;
        let h = Host::new(
            Transport::Local { socket: None },
            crate::model::mux::for_binary("psmux"), // PerSession → DeathSignal::PathStat
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
        use crate::model::Transport;
        let h = Host::new(
            Transport::Ssh { alias: "jup".into(), control_path: String::new(), os: "linux".into() },
            crate::model::mux::for_binary("tmux"), // Shared → not PathStat
        );
        // A Shared host never dies by a .port file — liveness here is unconditionally true.
        assert!(h.psmux_session_live("anything"));
    }
}
