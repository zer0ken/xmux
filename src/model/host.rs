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

/// Connecting / live / unreachable — the single per-host reachability state the
/// supervisor and the tree read (no separate `connecting` flag or `connected` set).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Liveness {
    Connecting,
    Live,
    Unreachable,
}

impl Liveness {
    /// Projects a scan/ls outcome's optional error into reachability: `Some` ⇒
    /// `Unreachable`, `None` ⇒ `Live`. The scan path has no "connecting" state, so
    /// this is a two-way projection; the failure message itself is kept alongside
    /// (`Liveness` is `Copy` and holds none).
    pub fn from_scan_err(err: &Option<String>) -> Liveness {
        if err.is_some() {
            Liveness::Unreachable
        } else {
            Liveness::Live
        }
    }
}

/// The per-host display BOOKKEEPING. The
/// `AttachRegistry`/`Attachment`/`DisplayWorker` PTY MECHANISM OWNS the PTYs; this is
/// only the record of WHICH session each display_key currently shows and what spawn
/// is in flight, so it can never disagree with `display_key`.
#[derive(Default)]
pub struct HostDisplay {
    /// display_key -> the session it currently shows. `Shared`: one entry keyed by
    /// the host id. `PerSession`: one per `source/session`.
    current: HashMap<String, String>,
    /// display_key -> in-flight spawn seq.
    in_flight: HashMap<String, u64>,
    /// Spawned attachment ids whose PTY EOF'd BEFORE their off-loop Ready arrived (the
    /// Exited-raced-Ready case). The Ready arm tears the attachment down instead of
    /// inserting a dead pane.
    reaped_ids: std::collections::HashSet<u64>,
    /// In-flight attachment id → its display key, recorded at request time so a
    /// pre-Ready Exited (registry has no id yet) can be attributed to THIS host's
    /// reaped_ids. Cleared when the attachment registers (Ready) or fails.
    pending: std::collections::HashMap<u64, String>,
}

/// How a worker `Ready` reply resolves against a host's display bookkeeping — the
/// pure decision [`HostDisplay::resolve_ready`] makes, which the run loop turns into
/// the registry install / teardown it alone can perform.
#[derive(Debug, PartialEq, Eq)]
pub enum ReadyOutcome {
    /// A pre-Ready `Exited` raced ahead: the child already died, so tear the fresh
    /// attachment down instead of installing a dead pane.
    TearDownReaped,
    /// This reply is the latest in-flight request for its key — install it as the live
    /// grid. `shown` is the session it displays (the confirmed display truth).
    Install { shown: String },
    /// A newer attach superseded this seq — tear it down without touching the key's
    /// in-flight seq (the newer request owns it).
    TearDownStale,
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
    /// Record an in-flight attachment `id` → its display `key`, so a pre-Ready `Exited`
    /// (the registry has no id yet) can be attributed to this host via
    /// [`mark_reaped_if_pending`](Self::mark_reaped_if_pending).
    pub fn mark_pending(&mut self, id: u64, key: &str) {
        self.pending.insert(id, key.to_string());
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

    /// True when an attach is in flight for `key` (a spawn requested, its `Ready`/`Failed`
    /// not yet resolved). Reads the in-flight bookkeeping without exposing the map.
    pub fn in_flight_contains(&self, key: &str) -> bool {
        self.in_flight.contains_key(key)
    }

    /// True when NO attach is in flight for any key.
    pub fn in_flight_is_empty(&self) -> bool {
        self.in_flight.is_empty()
    }

    /// The in-flight spawn seq for `key`, if any.
    pub fn in_flight_seq(&self, key: &str) -> Option<u64> {
        self.in_flight.get(key).copied()
    }

    /// True when a worker `Ready`/`Failed` reply carrying `seq` is still the latest
    /// in-flight request for `key`. A stale reply (the key was re-requested after a reap,
    /// so a newer seq is in flight, or the key is no longer in flight) must not
    /// register or clear state.
    pub fn reply_is_current(&self, key: &str, seq: u64) -> bool {
        self.in_flight.get(key) == Some(&seq)
    }

    /// Resolves a worker `Ready(seq, id)` for `key` against the bookkeeping: a reaped-race
    /// (its `Exited` arrived first) tears down and clears in-flight + pending; the current
    /// seq installs (clears in-flight + pending, returns the shown session); a stale seq
    /// tears down (clears only this id's pending). The run loop performs the registry
    /// install/teardown the outcome names — this owns only the bookkeeping decision.
    pub fn resolve_ready(&mut self, key: &str, seq: u64, id: u64) -> ReadyOutcome {
        if self.reaped_ids.remove(&id) {
            self.in_flight.remove(key);
            self.pending.remove(&id);
            ReadyOutcome::TearDownReaped
        } else if self.reply_is_current(key, seq) {
            self.in_flight.remove(key);
            self.pending.remove(&id);
            let shown = self.current.get(key).cloned().unwrap_or_default();
            ReadyOutcome::Install { shown }
        } else {
            self.pending.remove(&id);
            ReadyOutcome::TearDownStale
        }
    }

    /// Resolves a worker `Failed(seq)` for `key`: when it is the current in-flight reply,
    /// clear the in-flight seq + every pending id mapped to the key and return `true`
    /// (the caller rearms recovery); a stale failure is a no-op returning `false`.
    pub fn resolve_failed(&mut self, key: &str, seq: u64) -> bool {
        if self.reply_is_current(key, seq) {
            self.in_flight.remove(key);
            self.pending.retain(|_, k| k != key);
            true
        } else {
            false
        }
    }

    /// Records a pre-Ready `Exited` id in `reaped_ids` IFF this host spawned it (its id is
    /// in `pending`), so the coming `Ready` tears the dead attachment down. Returns whether
    /// it was ours — the caller stops scanning hosts once one claims the id.
    pub fn mark_reaped_if_pending(&mut self, id: u64) -> bool {
        if self.pending.contains_key(&id) {
            self.reaped_ids.insert(id);
            true
        } else {
            false
        }
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
    /// xmux's own display-client tty, captured in memory. Passed to the mux's
    /// `switch_in_place` so its `SwitchPlan` targets xmux's own display client.
    pub display_tty: DisplayTty,
    pub liveness: Liveness,
    pub(crate) detected: bool,
}

impl Host {
    /// Builds a host from a transport + mux — the single per-host constructor, one host
    /// at a time.
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

    /// Re-enumerate this host's sessions through the mux with an injected runner,
    /// updating `inventory` and `liveness`. `Ok` (possibly empty) ⇒ `Live`; `Err` ⇒
    /// `Unreachable` (and the error propagates). The single owner of this host's session
    /// list and reachability; off-loop `Ops`/CLI inject a runner (via a value host they
    /// assemble from config) so the probe is testable without spawning processes. Mux
    /// detection is a separate concern (`detect_and_correct`), not folded in here.
    pub async fn enumerate_with(
        &mut self,
        runner: &dyn Runner,
    ) -> Result<(), crate::source::RunError> {
        match self.mux.enumerate(&self.transport, runner).await {
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

    /// [`enumerate_with`](Self::enumerate_with) over the real exec runner.
    pub async fn enumerate(&mut self) -> Result<(), crate::source::RunError> {
        self.enumerate_with(&crate::source::ExecRunner).await
    }

    /// The argv that hands the terminal over to attach this host's named session
    /// (over `ssh -t` for a remote).
    ///
    /// Composes the two axes: the MUX supplies the attach argv via `Mux::attach_plan`
    /// (so local psmux uses `new-session -A -s <name>`, routing to the session's OWN
    /// server, not a warm clone from a bare `attach -t`), and the MACHINE wraps it via
    /// `Transport::interactive_attach_argv` (local `-S` injection, or `ssh -t` with
    /// `[<select-window> ;] exec <attach>`). `window` is the window index to land on:
    /// for a REMOTE host the selection is folded into the SAME `ssh -t` connection so
    /// there is no second connection that could hang or be lost to interactive auth; a
    /// LOCAL host pre-selects it with a separate instant command, so the transport
    /// ignores it here.
    pub fn interactive_attach_command(&self, name: &str, window: Option<i64>) -> Vec<String> {
        let attach = self.mux.attach_plan(name);
        let pre_select = window.map(|w| {
            self.mux
                .select_window_plan(&crate::mux::window_target(name, w))
        });
        let (n, a) = self
            .transport
            .interactive_attach_argv(&attach, pre_select.as_deref());
        let mut v = vec![n];
        v.extend(a);
        v
    }

    /// Record xmux's display-client tty for this host, captured in memory from the
    /// PTY marker (no `/tmp` file). The driver passes it to the mux's `switch_in_place`
    /// so the resulting `SwitchPlan` targets xmux's own display client only.
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
    pub fn session_is_live(&self, session: &str) -> bool {
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
        fn clone_box(&self) -> Box<dyn Mux> {
            Box::new(StubMux(self.0))
        }
        async fn enumerate(
            &self,
            _t: &dyn Transport,
            _r: &dyn crate::source::Runner,
        ) -> Result<Vec<Session>, RunError> {
            Ok(vec![])
        }
        fn attach_plan(&self, _s: &str) -> Vec<String> {
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
            _sel: &crate::model::Selection,
            _ctx: &mut crate::driver::DriverCtx,
        ) -> bool {
            false
        }
        fn grid(
            &self,
            _sel: &crate::model::Selection,
            _ctx: &crate::driver::DriverCtx,
        ) -> Option<std::sync::Arc<std::sync::Mutex<crate::display::grid::Grid>>> {
            None
        }
        fn input(
            &mut self,
            _sel: &crate::model::Selection,
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
    fn host_display_reply_is_current_only_for_latest_seq() {
        let mut d = HostDisplay::default();
        d.mark_in_flight("k", 5);
        assert!(d.reply_is_current("k", 5));
        assert!(!d.reply_is_current("k", 4), "older seq is stale");
        assert!(
            !d.reply_is_current("absent", 5),
            "no in-flight request → stale"
        );
    }

    #[test]
    fn host_display_resolve_ready_reaped_race_tears_down() {
        let mut d = HostDisplay::default();
        d.mark_in_flight("local/w", 3);
        d.pending.insert(42, "local/w".into());
        d.reaped_ids.insert(42);
        // Exited raced ahead of Ready: tear the fresh attachment down and clear
        // the key's in-flight + this id's pending so nothing leaks.
        assert_eq!(
            d.resolve_ready("local/w", 3, 42),
            ReadyOutcome::TearDownReaped
        );
        assert!(!d.in_flight_contains("local/w"), "in-flight cleared");
        assert!(!d.pending.contains_key(&42), "pending id cleared");
        assert!(!d.reaped_ids.contains(&42), "reaped id consumed");
    }

    #[test]
    fn host_display_resolve_ready_current_seq_installs() {
        let mut d = HostDisplay::default();
        d.set_shows("local/w", "work");
        d.mark_in_flight("local/w", 3);
        d.pending.insert(42, "local/w".into());
        assert_eq!(
            d.resolve_ready("local/w", 3, 42),
            ReadyOutcome::Install {
                shown: "work".into()
            }
        );
        assert!(
            !d.in_flight_contains("local/w"),
            "in-flight cleared on install"
        );
        assert!(
            !d.pending.contains_key(&42),
            "pending id cleared on install"
        );
    }

    #[test]
    fn host_display_resolve_ready_stale_seq_tears_down() {
        let mut d = HostDisplay::default();
        d.mark_in_flight("local/w", 9); // a newer seq is in flight
        d.pending.insert(42, "local/w".into());
        assert_eq!(
            d.resolve_ready("local/w", 3, 42),
            ReadyOutcome::TearDownStale
        );
        assert!(
            d.in_flight_contains("local/w"),
            "stale reply must not clear the newer in-flight seq"
        );
        assert!(
            !d.pending.contains_key(&42),
            "stale reply forgets its pending id"
        );
    }

    #[test]
    fn host_display_resolve_failed_clears_when_current() {
        let mut d = HostDisplay::default();
        d.mark_in_flight("local/w", 3);
        d.pending.insert(42, "local/w".into());
        assert!(d.resolve_failed("local/w", 3), "current reply clears state");
        assert!(!d.in_flight_contains("local/w"));
        assert!(d.pending.is_empty());
        // A stale Failed (newer seq in flight) leaves state untouched.
        d.mark_in_flight("local/w", 9);
        assert!(!d.resolve_failed("local/w", 3));
        assert!(d.in_flight_contains("local/w"));
    }

    #[test]
    fn host_display_mark_reaped_only_when_pending() {
        let mut d = HostDisplay::default();
        d.pending.insert(7, "jup".into());
        assert!(d.mark_reaped_if_pending(7), "an id we spawned is recorded");
        assert!(d.reaped_ids.contains(&7));
        assert!(
            !d.mark_reaped_if_pending(99),
            "an id we never spawned is not ours"
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
        fn clone_box(&self) -> Box<dyn Mux> {
            Box::new(EnumMux {
                model: self.model,
                result: std::sync::Mutex::new(None),
            })
        }
        async fn enumerate(
            &self,
            _t: &dyn Transport,
            _r: &dyn crate::source::Runner,
        ) -> Result<Vec<Session>, RunError> {
            self.result.lock().unwrap().take().unwrap_or(Ok(vec![]))
        }
        fn attach_plan(&self, _s: &str) -> Vec<String> {
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

    /// Returns a single canned result (or empty on a second call), ignoring the
    /// command — so `enumerate_with`'s runner injection is exercised through a real
    /// mux (`tmux`), covering the aggregate-list parse and the reachable-vs-unreachable
    /// classification the mux owns.
    struct CannedRunner(std::sync::Mutex<Option<Result<Vec<u8>, RunError>>>);

    impl CannedRunner {
        fn ok(out: &str) -> Self {
            CannedRunner(std::sync::Mutex::new(Some(Ok(out.as_bytes().to_vec()))))
        }
        fn err(e: RunError) -> Self {
            CannedRunner(std::sync::Mutex::new(Some(Err(e))))
        }
    }

    #[async_trait::async_trait]
    impl Runner for CannedRunner {
        async fn run(&self, _name: &str, _args: &[String]) -> Result<Vec<u8>, RunError> {
            self.0
                .lock()
                .unwrap()
                .take()
                .unwrap_or_else(|| Ok(Vec::new()))
        }
    }

    #[tokio::test]
    async fn enumerate_with_runner_parses_sessions_and_goes_live() {
        // The aggregate-server path: a single list-sessions returns every session,
        // parsed into the host's inventory, with liveness Live.
        let mut h = Host::new(crate::machine::local(None), crate::mux::for_binary("tmux"));
        let r = CannedRunner::ok("3\t1\t1781246739\teditor\n1\t0\t\tbuild\n");
        h.enumerate_with(&r).await.unwrap();
        assert_eq!(h.liveness, Liveness::Live);
        let names: Vec<&str> = h
            .inventory
            .sessions
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(names, vec!["editor", "build"]);
        assert_eq!(h.inventory.sessions[0].windows, 3);
        assert!(h.inventory.sessions[0].attached);
        assert_eq!(h.inventory.sessions[0].source, "local");
    }

    #[tokio::test]
    async fn enumerate_with_benign_no_server_is_empty_not_error() {
        // A reachable mux with no server is empty (Live), not an error.
        let mut h = Host::new(
            crate::machine::ssh("prod".into(), String::new(), "linux".into()),
            crate::mux::for_binary("tmux"),
        );
        let r = CannedRunner::err(RunError::Exit {
            stderr: "no server running on /tmp/tmux-1000/default".into(),
            code: 1,
        });
        h.enumerate_with(&r).await.unwrap();
        assert!(h.inventory.sessions.is_empty());
        assert_eq!(h.liveness, Liveness::Live);
    }

    #[tokio::test]
    async fn enumerate_with_unreachable_is_error() {
        let mut h = Host::new(
            crate::machine::ssh("prod".into(), String::new(), "linux".into()),
            crate::mux::for_binary("tmux"),
        );
        let r = CannedRunner::err(RunError::Other(
            "ssh: connect to host prod port 22: Connection timed out".into(),
        ));
        assert!(h.enumerate_with(&r).await.is_err());
        assert_eq!(h.liveness, Liveness::Unreachable);
    }

    /// Builds a value host for the attach-argv tests: `binary` selects the mux,
    /// `remote` picks the ssh vs local transport.
    fn attach_host(binary: &str, remote: bool) -> Host {
        let transport = if remote {
            crate::machine::ssh("prod".into(), String::new(), "linux".into())
        } else {
            crate::machine::local(None)
        };
        Host::new(transport, crate::mux::for_binary(binary))
    }

    #[test]
    fn interactive_attach_local_psmux_routes_to_the_per_session_server() {
        // Local psmux must attach via `new-session -A -s <name>` (routing to that
        // session's OWN server), NOT a bare `attach -t <name>`. The mux axis
        // (Mux::attach_plan) supplies this; the local pre-select is a separate command,
        // so the window is ignored here.
        let loc = attach_host("psmux", false);
        assert_eq!(
            loc.interactive_attach_command("dev", None),
            vec!["psmux", "new-session", "-A", "-s", "dev"]
        );
        assert_eq!(
            loc.interactive_attach_command("dev", Some(3)),
            vec!["psmux", "new-session", "-A", "-s", "dev"]
        );
    }

    #[test]
    fn interactive_attach_local_tmux_is_a_plain_attach() {
        // A LOCAL tmux (Shared) attach stays `attach -t <name>`.
        let loc = attach_host("tmux", false);
        assert_eq!(
            loc.interactive_attach_command("dev", None),
            vec!["tmux", "attach", "-t", "dev"]
        );
    }

    #[test]
    fn interactive_attach_remote_tmux_without_window() {
        let rem = attach_host("tmux", true);
        let got = rem.interactive_attach_command("api", None);
        assert_eq!(got[0], "ssh");
        assert!(got.iter().any(|s| s == "-t"), "{got:?}");
        assert_eq!(got.last().unwrap(), "exec tmux attach -t api");
    }

    #[test]
    fn interactive_attach_remote_tmux_folds_window_into_one_connection() {
        // The window pre-selection and the attach run over a SINGLE `ssh -t`, so
        // there is no second connection to hang on a stalled remote or to lose the
        // selection to interactive auth.
        let rem = attach_host("tmux", true);
        let got = rem.interactive_attach_command("api", Some(2));
        assert_eq!(got[0], "ssh");
        assert!(got.iter().any(|s| s == "-t"), "{got:?}");
        assert_eq!(
            got.last().unwrap(),
            "tmux select-window -t 'api:2' ; exec tmux attach -t api"
        );
    }

    #[test]
    fn interactive_attach_remote_psmux_uses_attach_plan_over_ssh() {
        // A REMOTE psmux host is attached the generic way; the attach argv still comes
        // from Mux::attach_plan (`new-session -A -s`) and is `exec`d over `ssh -t`.
        let rem = attach_host("psmux", true);
        let got = rem.interactive_attach_command("api", None);
        assert_eq!(got[0], "ssh");
        assert!(got.iter().any(|s| s == "-t"), "{got:?}");
        assert_eq!(got.last().unwrap(), "exec psmux new-session -A -s api");
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
        assert!(h.session_is_live(&name), "a present .port ⇒ live");
        std::fs::remove_file(&path).unwrap();
        assert!(!h.session_is_live(&name), "a vanished .port ⇒ not live");
    }

    #[test]
    fn tmux_host_session_is_always_live_by_port_stat() {
        let h = Host::new(
            crate::machine::ssh("jup".into(), String::new(), "linux".into()),
            crate::mux::for_binary("tmux"), // Shared → not PathStat
        );
        // A Shared host never dies by a .port file — liveness here is unconditionally true.
        assert!(h.session_is_live("anything"));
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
            h.mux.attach_plan("api"),
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
