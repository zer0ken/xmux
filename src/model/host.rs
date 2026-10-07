//! A first-class host (`Host`) — the single owner of one machine's transport, mux,
//! inventory, display BOOKKEEPING, captured display tty, and liveness. The rest of
//! the system addresses a machine through its `Host`, never through a bare alias
//! string. The live PTYs stay in `AttachRegistry`/`DisplayWorker`; this owns only
//! the bookkeeping of which session each attachment shows.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::link::HostInventory;
use crate::model::host_def::Runner;
use crate::model::DisplayTty;
use crate::mux::Mux;
use crate::transport::Transport;

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
    /// the host id. `PerSession`: one per `host/session`.
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
    /// Fresh attachments held off-screen until their grid has painted enough to replace
    /// the live stale frame. The PTYs themselves remain owned by the display registry.
    painting: HashMap<String, PendingPaint>,
    /// display_key -> the attachment its latest current `Ready` delivered and when, so
    /// an exit can tell an attachment that ended right after starting.
    started: HashMap<String, (u64, Instant)>,
}

/// Quiet time after visible output that lets a fresh attachment finish one visual burst
/// before replacing the stale frame. Output counts only once the fresh grid shows
/// something, since a client that clears the screen and waits on its own terminal
/// queries has sent bytes but no frame.
pub(crate) const PAINT_SETTLE: Duration = Duration::from_millis(50);

/// Maximum time continuous output may postpone a swap, counted from the first visible
/// output, so a chatty client cannot keep the stale frame indefinitely.
pub(crate) const PAINT_HARD_CAP: Duration = Duration::from_millis(400);

/// Maximum time a fresh attachment may go without a visible frame before it replaces the
/// stale frame, so a silent or stalled client cannot freeze the old session indefinitely.
pub(crate) const PAINT_NO_OUTPUT_CAP: Duration = Duration::from_secs(3);

/// How soon after its `Ready` an attachment's end counts as early. A client a mux drops
/// right after attaching ends within tens of milliseconds of its first paint, and a
/// fresh client paints well inside a second, so two seconds covers a loaded machine
/// without reaching a session the user has been working in.
pub const EARLY_END: Duration = Duration::from_secs(2);

#[derive(Debug)]
struct PendingPaint {
    id: u64,
    shown: String,
    ready_at: Instant,
    first_output: Option<Instant>,
    last_output: Option<Instant>,
}

/// A fresh attachment whose paint gate has opened and may replace the stale attachment.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct PendingInstall {
    pub(crate) key: String,
    pub(crate) id: u64,
    pub(crate) shown: String,
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
    /// This reply is current, but a live attachment still supplies the key's visible
    /// frame. Park the fresh attachment until it paints. `replaced` identifies an older
    /// parked attachment superseded under the same key.
    Hold {
        shown: String,
        replaced: Option<u64>,
    },
    /// A newer attach superseded this seq — tear it down without touching the key's
    /// in-flight seq (the newer request owns it).
    TearDownStale,
}

impl HostDisplay {
    /// The session each attachment of this host is on, with its key and whether it is a
    /// fresh attachment parked under that key until it paints.
    pub fn attachments(&self) -> impl Iterator<Item = (&str, &str, bool)> {
        self.current
            .iter()
            .map(|(key, shown)| (key.as_str(), shown.as_str(), false))
            .chain(
                self.painting
                    .iter()
                    .map(|(key, paint)| (key.as_str(), paint.shown.as_str(), true)),
            )
    }

    /// The session `key`'s attachment currently shows, if any.
    pub fn shows(&self, key: &str) -> Option<&str> {
        self.current.get(key).map(String::as_str)
    }
    /// Carry every record of `from` across to `to`: the session was renamed under the
    /// attachment, which still shows the same session.
    pub fn rename_session(&mut self, from: &str, to: &str) {
        for shown in self.current.values_mut() {
            if shown == from {
                *shown = to.to_string();
            }
        }
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
        self.painting.remove(key);
        self.started.remove(key);
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

    /// True while a fresh attachment for `key` is parked until its grid paints.
    pub(crate) fn pending_paint_contains(&self, key: &str) -> bool {
        self.painting.contains_key(key)
    }

    /// True when a worker `Ready`/`Failed` reply carrying `seq` is still the latest
    /// in-flight request for `key`. A stale reply (the key was re-requested after a reap,
    /// so a newer seq is in flight, or the key is no longer in flight) must not
    /// register or clear state.
    pub fn reply_is_current(&self, key: &str, seq: u64) -> bool {
        self.in_flight.get(key) == Some(&seq)
    }

    /// Resolves a worker `Ready(seq, id)` for `key` against the bookkeeping: a reaped-race
    /// (its `Exited` arrived first) tears down and clears this id's pending — and the
    /// in-flight slot too, UNLESS a newer attach has since taken it; the current seq
    /// installs (clears in-flight + pending, returns the shown session); a stale seq
    /// tears down (clears only this id's pending). The run loop performs the registry
    /// install/teardown the outcome names — this owns only the bookkeeping decision.
    pub fn resolve_ready(
        &mut self,
        key: &str,
        seq: u64,
        id: u64,
        hold_for_paint: bool,
        output_times: Option<(Instant, Instant)>,
        now: Instant,
    ) -> ReadyOutcome {
        if self.reaped_ids.remove(&id) {
            // Clear the in-flight slot only when THIS reaped reply is still the current
            // one. A reattach-flap can request a newer attach for the same key after the
            // reap; the dead id's late `Ready` must tear its own attachment down without
            // clobbering the newer in-flight seq — otherwise that live attach then
            // resolves as stale and blanks the pane until it self-heals.
            if self.reply_is_current(key, seq) {
                self.in_flight.remove(key);
                // No client reached the session this request recorded, so the record
                // no longer names what the key shows.
                self.current.remove(key);
            }
            self.pending.remove(&id);
            ReadyOutcome::TearDownReaped
        } else if self.reply_is_current(key, seq) {
            self.in_flight.remove(key);
            self.pending.remove(&id);
            self.started.insert(key.to_string(), (id, now));
            let shown = self.current.get(key).cloned().unwrap_or_default();
            if hold_for_paint {
                let replaced = self
                    .painting
                    .insert(
                        key.to_string(),
                        PendingPaint {
                            id,
                            shown: shown.clone(),
                            ready_at: now,
                            first_output: output_times.map(|(first, _)| first),
                            last_output: output_times.map(|(_, last)| last),
                        },
                    )
                    .map(|old| old.id);
                ReadyOutcome::Hold { shown, replaced }
            } else {
                ReadyOutcome::Install { shown }
            }
        } else {
            self.pending.remove(&id);
            ReadyOutcome::TearDownStale
        }
    }

    /// Resolves a worker `Failed(seq)` for `key`: when it is the current in-flight reply,
    /// clear the in-flight seq, every pending id mapped to the key, and the session the
    /// request recorded, which no client reached, and return `true`; a stale failure is
    /// a no-op returning `false`.
    pub fn resolve_failed(&mut self, key: &str, seq: u64) -> bool {
        if self.reply_is_current(key, seq) {
            self.in_flight.remove(key);
            self.pending.retain(|_, k| k != key);
            self.current.remove(key);
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

    /// Whether attachment `id`, the latest one delivered for `key`, ends within
    /// [`EARLY_END`] of its `Ready` when it ends at `now`.
    pub fn ended_early(&self, key: &str, id: u64, now: Instant) -> bool {
        self.started.get(key).is_some_and(|&(started_id, at)| {
            started_id == id && now.saturating_duration_since(at) < EARLY_END
        })
    }

    /// Records output from a parked attachment. Returns whether this host owns `id`.
    pub(crate) fn note_pending_output(&mut self, id: u64, now: Instant) -> bool {
        let Some(pending) = self.painting.values_mut().find(|pending| pending.id == id) else {
            return false;
        };
        pending.first_output.get_or_insert(now);
        pending.last_output = Some(now);
        true
    }

    /// Takes every parked attachment whose paint gate has opened at `now`.
    pub(crate) fn take_due_pending(&mut self, now: Instant) -> Vec<PendingInstall> {
        let due: Vec<String> = self
            .painting
            .iter()
            .filter(|(_, pending)| pending.is_due(now))
            .map(|(key, _)| key.clone())
            .collect();
        due.into_iter()
            .filter_map(|key| {
                self.painting.remove(&key).map(|pending| PendingInstall {
                    key,
                    id: pending.id,
                    shown: pending.shown,
                })
            })
            .collect()
    }

    /// Takes a parked attachment that exited before its paint gate opened.
    pub(crate) fn take_pending_exit(&mut self, id: u64) -> Option<PendingInstall> {
        let key = self
            .painting
            .iter()
            .find(|(_, pending)| pending.id == id)
            .map(|(key, _)| key.clone())?;
        let pending = self.painting.remove(&key)?;
        Some(PendingInstall {
            key,
            id: pending.id,
            shown: pending.shown,
        })
    }

    /// Cancels the parked attachment under `key` when a newer request supersedes it.
    pub(crate) fn cancel_pending_paint(&mut self, key: &str) -> Option<u64> {
        self.painting.remove(key).map(|pending| pending.id)
    }
}

impl PendingPaint {
    fn is_due(&self, now: Instant) -> bool {
        match (self.first_output, self.last_output) {
            (Some(first), Some(last)) => {
                now.saturating_duration_since(last) >= PAINT_SETTLE
                    || now.saturating_duration_since(first) >= PAINT_HARD_CAP
            }
            _ => now.saturating_duration_since(self.ready_at) >= PAINT_NO_OUTPUT_CAP,
        }
    }
}

/// A first-class host: one machine reachable by one transport, running one mux,
/// owning its inventory, its display BOOKKEEPING, its captured display tty, and its
/// liveness — the single owner of all per-host state, keyed by a stable host id
/// rather than a bare alias string. The PTYs are NOT here — they live in
/// `AttachRegistry`/`DisplayWorker`; `Host` owns only the bookkeeping.
///
/// A host carries no control client, no display-key derivation, and no attach or reap
/// plan: the live control client belongs to the host manager (`link::HostManager`),
/// the live warm and reap to the driver, and the display-key authority to the driver
/// capability port (`DriverCtx`).
pub struct Host {
    pub transport: Box<dyn Transport>,
    pub mux: Box<dyn Mux>,
    /// Session/window inventory — the single owner for the paths that read it: the
    /// control-mode `-CC` reader's fold (`ApplyInventory`) and the enumerate/CLI path
    /// (`Host::enumerate`). A poll host applies its enumeration straight to the tree and
    /// leaves this untouched; nothing in the live loop reads a poll host's inventory (the
    /// re-warm reader is gated on a live control client).
    pub inventory: HostInventory,
    /// Which session each display_key shows + what spawn is in flight.
    pub display: HostDisplay,
    /// xmux's own display-client tty, captured in memory. Passed to the mux's
    /// `switch_in_place` so its `SwitchPlan` targets xmux's own display client.
    pub display_tty: DisplayTty,
    /// The session each client the mux reported moving was last on, kept only while
    /// `display_tty` is unknown. A remote attach records its tty on the host before it
    /// execs the mux client, so a capture made as the attach starts can find no record
    /// yet; the mux's report of that client arriving is what proves the record exists,
    /// and the report has to wait here until the capture it prompts names which client
    /// is xmux's own.
    reported_sessions: HashMap<String, String>,
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
            reported_sessions: HashMap::new(),
            liveness: Liveness::Connecting,
            detected: false,
        }
    }

    /// The stable host id (`transport.host_id()`).
    pub fn id(&self) -> &str {
        self.transport.host_id()
    }

    /// Probes the configured mux on this host and, when another kind answers, corrects
    /// the mux to it (the psmux-behind-a-tmux-alias correction). Returns `None` when
    /// the host is already detected or resolves now; `Some(reason)` when detection
    /// failed, carrying the first probe error so the caller can settle the undetected
    /// card instead of leaving it scanning forever. The host stays undetected on
    /// failure, so a later retry recovers it.
    pub(crate) async fn detect_and_correct(&mut self, runner: &dyn Runner) -> Option<String> {
        if self.detected {
            return None;
        }
        let bin = self.mux.bin().to_string();
        let (mux, err) = crate::mux::detect_backend(&self.transport, &bin, runner).await;
        match mux {
            Some(mux) => {
                if mux.kind() != self.mux.kind() {
                    self.mux = mux;
                }
                self.detected = true;
                None
            }
            None => err,
        }
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
    ) -> Result<(), crate::model::host_def::RunError> {
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
    pub async fn enumerate(&mut self) -> Result<(), crate::model::host_def::RunError> {
        self.enumerate_with(&crate::model::host_def::ExecRunner)
            .await
    }

    /// The command a session listing spawns on this host, `argv[0]` first.
    ///
    /// The mux names the listing (a local psmux reads its registry first, then still runs
    /// it for the detail) and the machine wraps it, so it is what a failed scan ran. It
    /// exists to be SHOWN: the unreachable screen states it, which is what lets a user
    /// reproduce the failure outside xmux instead of taking the app's word for it.
    pub fn list_sessions_command(&self) -> crate::transport::CommandSpec {
        self.transport
            .exec_argv(false, &self.mux.list_sessions_plan())
    }

    /// The argv that hands the terminal over to attach this host's named session
    /// (over `ssh -t` for a remote).
    ///
    /// Composes the two axes: the MUX supplies the attach argv via `Mux::attach_plan`
    /// (so psmux uses `-f NUL attach -t <name>` to reach the session's own server without
    /// creating a missing one), and the MACHINE wraps it via
    /// `Transport::interactive_attach_argv` (local `-S` injection, or `ssh -t` with
    /// `exec <attach>`).
    pub fn interactive_attach_command(&self, name: &str) -> crate::transport::CommandSpec {
        let attach = self.mux.attach_plan(name);
        self.transport.interactive_attach_argv(&attach)
    }

    pub fn cli_attach_command(&self, name: &str) -> crate::transport::CommandSpec {
        let attach = self.mux.attach_plan(name);
        self.transport.cli_attach_argv(&attach)
    }

    /// Record xmux's display-client tty for this host, captured in memory from the
    /// PTY marker (no `/tmp` file). The driver passes it to the mux's `switch_in_place`
    /// so the resulting `SwitchPlan` targets xmux's own display client only.
    pub fn record_display_tty(&mut self, tty: Option<String>) {
        self.display_tty = DisplayTty(tty);
    }

    /// Remember that the mux moved `client` to `session` while xmux's own display tty is
    /// still unknown, so the move can be matched once the tty is captured.
    pub fn note_reported_session(&mut self, client: &str, session: &str) {
        self.reported_sessions
            .insert(client.to_string(), session.to_string());
    }

    /// The session the mux last reported xmux's own display client on, once its tty is
    /// known. Every other remembered client is someone else's and is dropped with it.
    pub fn take_reported_session(&mut self) -> Option<String> {
        let tty = self.display_tty.0.as_deref()?;
        let session = self.reported_sessions.remove(tty);
        self.reported_sessions.clear();
        session
    }

    /// Forget the display tty when the attachment dies, so no later `switch-client`
    /// is aimed at a detached/dead client (the blank-pane class). Moves reported for the
    /// dead attachment's clients go with it, so the next attachment's tty cannot claim one.
    pub fn clear_display_tty(&mut self) {
        self.display_tty = DisplayTty(None);
        self.reported_sessions.clear();
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

    /// True when `session` is still live under this mux's death signal. A psmux host
    /// in the local registry scope uses its `.port` file. Other hosts stay live here
    /// because death arrives through the attachment PTY or control channel.
    pub fn session_is_live(&self, session: &str) -> bool {
        match self.mux.death_signal() {
            crate::model::DeathSignal::PathStat {
                dir_is_psmux_registry: true,
            } if self.transport.local_registry_scope() => {
                crate::model::death::psmux_session_is_live(session)
            }
            _ => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::host_def::{RunError, Runner};
    use crate::model::{DeathSignal, EventSource, ServerModel};
    use crate::mux::Mux;
    use crate::session::Session;

    /// A minimal in-test mux: only `server_model` is exercised in these tests. The other
    /// methods return trivially since they wire no I/O — including the window and session
    /// lifecycle plans, which these tests never invoke.
    struct StubMux(ServerModel);

    #[async_trait::async_trait]
    impl Mux for StubMux {
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
            _r: &dyn crate::model::host_def::Runner,
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
            EventSource::Poll
        }
        fn new_session_plan(&self, _n: &str) -> Vec<String> {
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
        fn sync(
            &mut self,
            _host: &str,
            _sessions: &[crate::session::Session],
            _ctx: &mut crate::driver::DriverCtx,
        ) {
        }
    }

    #[test]
    fn host_id_is_the_transport_host_id() {
        let h = Host::new(
            crate::transport::local(None),
            Box::new(StubMux(ServerModel::Shared)),
        );
        assert_eq!(h.id(), "local");
        let r = Host::new(
            crate::transport::ssh("jup".into(), String::new(), "linux".into()),
            Box::new(StubMux(ServerModel::Shared)),
        );
        assert_eq!(r.id(), "jup");
    }

    #[test]
    fn new_host_starts_connecting_with_empty_inventory_and_tty() {
        let h = Host::new(
            crate::transport::local(None),
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
            d.resolve_ready("local/w", 3, 42, false, None, Instant::now()),
            ReadyOutcome::TearDownReaped
        );
        assert!(!d.in_flight_contains("local/w"), "in-flight cleared");
        assert!(!d.pending.contains_key(&42), "pending id cleared");
        assert!(!d.reaped_ids.contains(&42), "reaped id consumed");
    }

    #[test]
    fn host_display_resolve_ready_reaped_keeps_newer_in_flight() {
        let mut d = HostDisplay::default();
        // A reattach flap: id=42/seq=3 was reaped before its Ready, then a NEW attach
        // (id=99/seq=7) was requested for the same key, so seq=7 is now in flight.
        d.mark_in_flight("local/w", 7);
        d.pending.insert(42, "local/w".into());
        d.pending.insert(99, "local/w".into());
        d.reaped_ids.insert(42);
        // The late Ready for the DEAD id (seq=3) tears its own attachment down but must
        // NOT clear the newer in-flight seq=7, or the live attach resolves as stale.
        assert_eq!(
            d.resolve_ready("local/w", 3, 42, false, None, Instant::now()),
            ReadyOutcome::TearDownReaped
        );
        assert_eq!(
            d.in_flight_seq("local/w"),
            Some(7),
            "the newer in-flight seq survives the dead id's teardown"
        );
        assert!(!d.pending.contains_key(&42), "dead id's pending cleared");
        assert!(
            d.pending.contains_key(&99),
            "newer attach's pending untouched"
        );
        assert!(!d.reaped_ids.contains(&42), "reaped id consumed");
    }

    #[test]
    fn host_display_resolve_ready_current_seq_installs() {
        let mut d = HostDisplay::default();
        d.set_shows("local/w", "work");
        d.mark_in_flight("local/w", 3);
        d.pending.insert(42, "local/w".into());
        assert_eq!(
            d.resolve_ready("local/w", 3, 42, false, None, Instant::now()),
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
            d.resolve_ready("local/w", 3, 42, false, None, Instant::now()),
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
        d.set_shows("local/w", "w");
        assert!(d.resolve_failed("local/w", 3), "current reply clears state");
        assert!(!d.in_flight_contains("local/w"));
        assert!(d.pending.is_empty());
        assert_eq!(
            d.shows("local/w"),
            None,
            "no client reached the session the request recorded"
        );
        // A stale Failed (newer seq in flight) leaves state untouched.
        d.mark_in_flight("local/w", 9);
        assert!(!d.resolve_failed("local/w", 3));
        assert!(d.in_flight_contains("local/w"));
    }

    #[test]
    fn an_end_counts_as_early_only_for_the_latest_attachment_inside_the_window() {
        let mut d = HostDisplay::default();
        let t0 = Instant::now();
        d.mark_in_flight("local", 1);
        d.set_shows("local", "a");
        assert_eq!(
            d.resolve_ready("local", 1, 7, false, None, t0),
            ReadyOutcome::Install { shown: "a".into() }
        );
        assert!(d.ended_early("local", 7, t0 + Duration::from_millis(50)));
        assert!(
            !d.ended_early("local", 7, t0 + EARLY_END),
            "an end at the window's edge is a late end"
        );
        assert!(
            !d.ended_early("local", 8, t0),
            "another attachment under the key never started here"
        );
        d.clear("local");
        assert!(!d.ended_early("local", 7, t0), "a cleared key has no start");
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

    fn hold_ready(d: &mut HostDisplay, now: Instant, id: u64) -> ReadyOutcome {
        d.set_shows("local", "fresh");
        d.mark_in_flight("local", id);
        d.mark_pending(id, "local");
        d.resolve_ready("local", id, id, true, None, now)
    }

    #[test]
    fn host_display_holds_a_current_ready_for_paint() {
        let now = Instant::now();
        let mut d = HostDisplay::default();
        assert_eq!(
            hold_ready(&mut d, now, 7),
            ReadyOutcome::Hold {
                shown: "fresh".into(),
                replaced: None,
            }
        );
        assert!(!d.in_flight_contains("local"));
        assert!(d.take_due_pending(now + PAINT_SETTLE).is_empty());
    }

    #[test]
    fn pending_paint_swaps_after_output_settles() {
        let now = Instant::now();
        let mut d = HostDisplay::default();
        hold_ready(&mut d, now, 7);
        assert!(d.note_pending_output(7, now + Duration::from_millis(10)));
        assert!(d
            .take_due_pending(now + Duration::from_millis(59))
            .is_empty());
        assert_eq!(
            d.take_due_pending(now + Duration::from_millis(60)),
            vec![PendingInstall {
                key: "local".into(),
                id: 7,
                shown: "fresh".into(),
            }]
        );
    }

    #[test]
    fn output_that_precedes_ready_still_opens_the_settle_gate() {
        let now = Instant::now();
        let first = now - Duration::from_millis(100);
        let mut d = HostDisplay::default();
        d.set_shows("local", "fresh");
        d.mark_in_flight("local", 7);
        d.mark_pending(7, "local");
        assert!(matches!(
            d.resolve_ready("local", 7, 7, true, Some((first, now)), now),
            ReadyOutcome::Hold { .. }
        ));
        let just_before_cap = first + PAINT_HARD_CAP - Duration::from_millis(1);
        d.note_pending_output(7, just_before_cap);
        assert!(d.take_due_pending(just_before_cap).is_empty());
        assert_eq!(d.take_due_pending(first + PAINT_HARD_CAP)[0].id, 7);
    }

    #[test]
    fn continuous_output_swaps_at_the_hard_cap() {
        let now = Instant::now();
        let mut d = HostDisplay::default();
        hold_ready(&mut d, now, 7);
        d.note_pending_output(7, now + Duration::from_millis(10));
        for ms in [50, 100, 200, 300, 409] {
            d.note_pending_output(7, now + Duration::from_millis(ms));
            assert!(d
                .take_due_pending(now + Duration::from_millis(ms))
                .is_empty());
        }
        assert_eq!(
            d.take_due_pending(now + Duration::from_millis(410))[0].id,
            7
        );
    }

    #[test]
    fn silent_pending_swaps_at_the_no_output_cap() {
        let now = Instant::now();
        let mut d = HostDisplay::default();
        hold_ready(&mut d, now, 7);
        assert!(d
            .take_due_pending(now + PAINT_NO_OUTPUT_CAP - Duration::from_millis(1))
            .is_empty());
        assert_eq!(d.take_due_pending(now + PAINT_NO_OUTPUT_CAP)[0].id, 7);
    }

    #[test]
    fn newer_pending_replaces_the_older_and_exit_can_take_it() {
        let now = Instant::now();
        let mut d = HostDisplay::default();
        hold_ready(&mut d, now, 7);
        assert_eq!(
            hold_ready(&mut d, now + Duration::from_millis(1), 8),
            ReadyOutcome::Hold {
                shown: "fresh".into(),
                replaced: Some(7),
            }
        );
        assert!(d.take_pending_exit(7).is_none());
        assert_eq!(
            d.take_pending_exit(8),
            Some(PendingInstall {
                key: "local".into(),
                id: 8,
                shown: "fresh".into(),
            })
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
                    host: "h".into(),
                    name: (*n).into(),
                    mux: String::new(),
                    id: String::new(),
                    windows: 1,
                    clients: 0,
                    stopped: false,
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
            _r: &dyn crate::model::host_def::Runner,
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
            EventSource::Poll
        }
        fn new_session_plan(&self, _n: &str) -> Vec<String> {
            vec![]
        }
    }

    #[tokio::test]
    async fn enumerate_ok_fills_inventory_and_goes_live() {
        let mut h = Host::new(
            crate::transport::local(None),
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
            crate::transport::local(None),
            Box::new(EnumMux::ok(ServerModel::Shared, &[])),
        );
        h.enumerate().await.unwrap();
        assert_eq!(h.liveness, Liveness::Live);
        assert!(h.inventory.sessions.is_empty());
    }

    #[tokio::test]
    async fn enumerate_err_marks_unreachable_and_propagates() {
        let mut h = Host::new(
            crate::transport::local(None),
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
        crate::model::host_def::runner_spec_via_argv!();
        async fn run(&self, _name: &str, _args: &[String]) -> Result<Vec<u8>, RunError> {
            self.0
                .lock()
                .unwrap()
                .take()
                .unwrap_or_else(|| Ok(Vec::new()))
        }
    }

    #[test]
    fn list_sessions_command_is_the_listing_a_scan_runs() {
        // Shown on the unreachable screen, so it has to be the real command: the mux
        // binary, its listing verb, and the machine's own wrapping around them.
        let h = Host::new(
            crate::transport::local(None),
            crate::mux::for_binary("tmux").unwrap(),
        );
        let cmd = h.list_sessions_command();
        assert_eq!(cmd[0], "tmux");
        assert!(
            cmd.contains(&"list-sessions".to_string()),
            "the listing verb is in it: {cmd:?}"
        );
    }

    #[tokio::test]
    async fn enumerate_with_runner_parses_sessions_and_goes_live() {
        // The aggregate-server path: a single list-sessions returns every session,
        // parsed into the host's inventory, with liveness Live.
        let mut h = Host::new(
            crate::transport::local(None),
            crate::mux::for_binary("tmux").unwrap(),
        );
        let r = CannedRunner::ok("3:1::editor\n1:0::build\n");
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
        assert_eq!(h.inventory.sessions[0].clients, 1);
        assert_eq!(h.inventory.sessions[0].host, "local");
    }

    #[tokio::test]
    async fn enumerate_with_benign_no_server_is_empty_not_error() {
        // A reachable mux with no server is empty (Live), not an error.
        let mut h = Host::new(
            crate::transport::ssh("prod".into(), String::new(), "linux".into()),
            crate::mux::for_binary("tmux").unwrap(),
        );
        let r = CannedRunner::err(RunError::Exit {
            stderr: "no server running on /tmp/tmux-1000/default".into(),
            code: 1,
            stdout: Vec::new(),
        });
        h.enumerate_with(&r).await.unwrap();
        assert!(h.inventory.sessions.is_empty());
        assert_eq!(h.liveness, Liveness::Live);
    }

    #[tokio::test]
    async fn enumerate_with_unreachable_is_error() {
        let mut h = Host::new(
            crate::transport::ssh("prod".into(), String::new(), "linux".into()),
            crate::mux::for_binary("tmux").unwrap(),
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
            crate::transport::ssh("prod".into(), String::new(), "linux".into())
        } else {
            crate::transport::local(None)
        };
        Host::new(transport, crate::mux::for_binary(binary).unwrap())
    }

    #[test]
    fn interactive_attach_local_psmux_routes_to_the_per_session_server() {
        // The mux's explicit target reaches the session's own server.
        let loc = attach_host("psmux", false);
        assert_eq!(
            loc.interactive_attach_command("dev"),
            vec!["psmux", "-f", "NUL", "attach", "-t", "dev"]
        );
    }

    #[test]
    fn interactive_attach_local_tmux_is_a_plain_attach() {
        // A LOCAL tmux (Shared) attach stays `-f NUL attach -t <name>`.
        let loc = attach_host("tmux", false);
        assert_eq!(
            loc.interactive_attach_command("dev"),
            vec!["tmux", "attach", "-t", "dev"]
        );
    }

    #[test]
    fn interactive_attach_remote_tmux_execs_over_ssh_tty() {
        let rem = attach_host("tmux", true);
        let got = rem.interactive_attach_command("api");
        assert_eq!(got[0], "ssh");
        assert!(got.iter().any(|s| s == "-t"), "{got:?}");
        assert_eq!(
            got.last().unwrap(),
            "sh -lc '{ exec tmux attach -t api\n} 1>&3 2>&4 3>&- 4>&-' 3>&1 4>&2 1>/dev/null 2>/dev/null"
        );
    }

    #[test]
    fn interactive_attach_remote_psmux_uses_attach_plan_over_ssh() {
        // A REMOTE psmux host is attached the generic way; the attach argv still comes
        // from Mux::attach_plan (`-f NUL attach -t`) and is `exec`d through the login
        // shell over `ssh -t`.
        let rem = attach_host("psmux", true);
        let got = rem.interactive_attach_command("api");
        assert_eq!(got[0], "ssh");
        assert!(got.iter().any(|s| s == "-t"), "{got:?}");
        assert_eq!(
            got.last().unwrap(),
            "sh -lc '{ exec psmux -f NUL attach -t api\n} 1>&3 2>&4 3>&- 4>&-' 3>&1 4>&2 1>/dev/null 2>/dev/null"
        );
    }

    #[test]
    fn record_and_clear_display_tty_round_trips() {
        let mut h = Host::new(
            crate::transport::ssh("jup".into(), String::new(), "linux".into()),
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
            crate::transport::ssh("jup".into(), String::new(), "linux".into()),
            crate::mux::for_binary("tmux").unwrap(), // Shared → DeathSignal::ControlNotice
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
            crate::transport::local(None),
            crate::mux::for_binary("psmux").unwrap(), // PerSession → DeathSignal::PathStat
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
    fn remote_psmux_host_is_live_without_a_local_port() {
        let h = Host::new(
            crate::transport::ssh("jup".into(), String::new(), "windows".into()),
            crate::mux::for_binary("psmux").unwrap(),
        );
        let name = format!("xmux-remote-hostlive-{:?}", std::time::SystemTime::now());
        assert!(!crate::model::death::psmux_port_path(&name).exists());
        assert!(h.session_is_live(&name));
    }

    #[test]
    fn tmux_host_session_is_always_live_by_port_stat() {
        let h = Host::new(
            crate::transport::ssh("jup".into(), String::new(), "linux".into()),
            crate::mux::for_binary("tmux").unwrap(), // Shared → not PathStat
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
        crate::model::host_def::runner_spec_via_argv!();
        async fn run(&self, _name: &str, _args: &[String]) -> Result<Vec<u8>, RunError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            match &*self.result.lock().unwrap() {
                Ok(out) => Ok(out.clone()),
                Err(RunError::Exit { stderr, code, .. }) => Err(RunError::Exit {
                    stderr: stderr.clone(),
                    code: *code,
                    stdout: Vec::new(),
                }),
                Err(RunError::Other(e)) => Err(RunError::Other(e.clone())),
            }
        }
    }

    #[tokio::test]
    async fn detect_and_correct_replaces_behavior_and_preserves_bin() {
        let mut h = Host::new(
            crate::transport::local(None),
            crate::mux::for_binary("tmux").unwrap(),
        );
        let runner = DetectRunner::ok("psmux command help");
        assert_eq!(
            h.detect_and_correct(&runner).await,
            None,
            "a successful detection reports no reason"
        );
        assert_eq!(h.mux.kind(), "psmux");
        assert_eq!(h.mux.server_model(), ServerModel::PerSession);
        assert_eq!(
            h.mux.attach_plan("api"),
            vec!["tmux", "-f", "NUL", "attach", "-t", "api"]
        );
        assert!(h.detected);

        h.detect_and_correct(&runner).await;
        // tmux's probe pair (help, then -V) both run before the classify reads them;
        // the corrected host never probes again.
        h.detect_and_correct(&runner).await;
        assert_eq!(runner.calls(), 2);
    }

    #[tokio::test]
    async fn detect_and_correct_retries_after_inconclusive_probe() {
        let mut h = Host::new(
            crate::transport::local(None),
            crate::mux::for_binary("tmux").unwrap(),
        );
        let runner = DetectRunner::err();
        assert_eq!(
            h.detect_and_correct(&runner).await.as_deref(),
            Some("down"),
            "an inconclusive probe reports the probe error as the detection reason"
        );
        assert_eq!(h.mux.kind(), "tmux");
        assert_eq!(h.mux.server_model(), ServerModel::Shared);
        assert!(!h.detected);
    }
}
