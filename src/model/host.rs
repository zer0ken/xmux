//! A first-class host (`Host`) — the single owner of one machine's transport, mux,
//! inventory, display BOOKKEEPING, captured display tty, and liveness. Replaces
//! everything previously tied together by an alias string across `Source` +
//! `HostInventory` + `HostClient` + the supervisor's host_session/in_flight/
//! reaped_ids maps. The live PTYs stay in `AttachRegistry`/`DisplayWorker`; this
//! owns only the bookkeeping of which session each attachment shows.

use std::collections::HashMap;

use crate::host::HostInventory;
use crate::model::{DisplayTty, Mux, Transport};

/// Connecting / live / unreachable — replaces the loose `connecting` AtomicBool
/// (host.rs:334) and the supervisor's `connected: HashSet` tracking (cockpit.rs:1048).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Liveness {
    Connecting,
    Live,
    Unreachable,
}

/// The per-host display BOOKKEEPING previously split across `AttachRegistry` keys +
/// `host_session` (cockpit.rs:1052) + `in_flight` (cockpit.rs:996). The
/// `AttachRegistry`/`Attachment`/`DisplayWorker` PTY MECHANISM is KEPT and OWNS the
/// PTYs; this is only the record of WHICH session each display_key currently shows
/// and what spawn is in flight, so it can never disagree with `display_key`.
#[derive(Default)]
pub struct HostDisplay {
    /// display_key -> the session it currently shows. `Shared`: one entry keyed by
    /// the host id. `PerSession`: one per `source/session`.
    pub current: HashMap<String, String>,
    /// display_key -> in-flight spawn seq (was `in_flight`, cockpit.rs:996).
    pub in_flight: HashMap<String, u64>,
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
    /// Forget everything about `key` (its attachment closed/reaped).
    pub fn clear(&mut self, key: &str) {
        self.current.remove(key);
        self.in_flight.remove(key);
    }
}

/// A first-class host: one machine reachable by one transport, running one mux,
/// owning its inventory, its display BOOKKEEPING, its captured display tty, and its
/// liveness. The single owner of the state previously tied together by the alias
/// string across `Source` + `HostInventory` + `HostClient` + the supervisor's
/// host_session/in_flight/reaped_ids maps. The PTYs are NOT here — they live in
/// `AttachRegistry`/`DisplayWorker`; `Host` owns only the bookkeeping.
pub struct Host {
    pub transport: Transport,
    pub mux: Box<dyn Mux>,
    /// Live session/window inventory (was `HostInventory`, host.rs:17).
    pub inventory: HostInventory,
    /// Which session each display_key shows + what spawn is in flight (Task 2.2).
    pub display: HostDisplay,
    /// xmux's own display-client tty, captured in memory (replaces the
    /// `/tmp/.xmux-cli-<alias>` file). Read by the supervisor to build
    /// `mux.switch_client_argv(tty, session)`.
    pub display_tty: DisplayTty,
    pub liveness: Liveness,
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
        }
    }

    /// The stable host id (`transport.host_id()`) — was `Source::alias`.
    pub fn id(&self) -> &str {
        self.transport.host_id()
    }

    /// The `AttachRegistry` key for `address` under this host's model — the SINGLE
    /// definition, replacing the free `display_key` fn (cockpit.rs:245).
    pub fn display_key(&self, address: &str) -> String {
        self.mux.server_model().display_key(self.id(), address)
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
    fn host_display_clears_both_maps_for_a_key() {
        let mut d = HostDisplay::default();
        d.set_shows("local/work", "work");
        d.mark_in_flight("local/work", 7);
        assert_eq!(d.in_flight.get("local/work"), Some(&7));
        d.clear("local/work");
        assert_eq!(d.shows("local/work"), None, "clear forgets the shown session");
        assert_eq!(d.in_flight.get("local/work"), None, "clear forgets the in-flight seq");
    }

    #[test]
    fn liveness_is_copy_and_comparable() {
        let l = Liveness::Connecting;
        assert_eq!(l, Liveness::Connecting);
        assert_ne!(Liveness::Live, Liveness::Unreachable);
    }
}
