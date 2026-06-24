//! A first-class host (`Host`) — the single owner of one machine's transport, mux,
//! inventory, display BOOKKEEPING, captured display tty, and liveness. Replaces
//! everything previously tied together by an alias string across `Source` +
//! `HostInventory` + `HostClient` + the supervisor's host_session/in_flight/
//! reaped_ids maps. The live PTYs stay in `AttachRegistry`/`DisplayWorker`; this
//! owns only the bookkeeping of which session each attachment shows.

use std::collections::HashMap;

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

#[cfg(test)]
mod tests {
    use super::*;

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
