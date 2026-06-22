//! The `AttachRegistry`: an `Address → Attachment` map holding one live PTY-attached
//! mux client per session. Sessions are added on discovery (`ensure`) and removed on
//! close (`remove`) or master EOF (`reap`); the user mandate is to keep EVERY session
//! attached and alive, so there is no cap or LRU eviction — the map size tracks the
//! live session count. All blocking PTY work lives on each `Attachment`'s control and
//! pump threads, so registry methods never block the event loop.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::proxy::run::{spawn_attachment, Attachment, PtyEvent};
use crate::proxy::screen::Grid;

/// How the registry spawns a new attachment. Defaults to `spawn_attachment` (the
/// live PTY path); a test injects a fake via `with_spawner` to exercise `ensure`'s
/// dedup/id bookkeeping without a real PTY.
type AttachmentSpawner = Box<
    dyn Fn(
            &[String],
            u16,
            u16,
            u64,
            tokio::sync::mpsc::UnboundedSender<PtyEvent>,
        ) -> anyhow::Result<Attachment>
        + Send
        + Sync,
>;

pub struct AttachRegistry {
    /// Keyed by `Session::address()` (`source/session`).
    map: HashMap<String, Attachment>,
    next_id: u64,
    events: tokio::sync::mpsc::UnboundedSender<PtyEvent>,
    spawner: AttachmentSpawner,
}

impl AttachRegistry {
    pub fn new(events: tokio::sync::mpsc::UnboundedSender<PtyEvent>) -> Self {
        Self::with_spawner(events, Box::new(spawn_attachment))
    }

    pub fn with_spawner(
        events: tokio::sync::mpsc::UnboundedSender<PtyEvent>,
        spawner: AttachmentSpawner,
    ) -> Self {
        AttachRegistry {
            map: HashMap::new(),
            next_id: 1,
            events,
            spawner,
        }
    }

    pub fn contains(&self, addr: &str) -> bool {
        self.map.contains_key(addr)
    }

    /// The number of currently-kept attachments.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn get(&self, addr: &str) -> Option<&Attachment> {
        self.map.get(addr)
    }

    /// The grid Arc for `addr`'s attachment, so the cockpit can render it.
    pub fn grid(&self, addr: &str) -> Option<Arc<Mutex<Grid>>> {
        self.map.get(addr).map(|a| a.grid.clone())
    }

    /// Wipes `addr`'s grid to blank (a no-op if not attached). Called when the
    /// displayed session/window switches so the previous content's cells do not
    /// linger as residue behind the mux's fresh repaint.
    pub fn clear_grid(&self, addr: &str) {
        if let Some(att) = self.map.get(addr) {
            if let Ok(mut g) = att.grid.lock() {
                g.clear();
            }
        }
    }

    /// Whether `addr`'s attach is still establishing (drives the spinner). `true`
    /// for an absent address (nothing attached yet ⇒ still "connecting").
    pub fn connecting(&self, addr: &str) -> bool {
        match self.map.get(addr) {
            Some(att) => att.connecting.load(std::sync::atomic::Ordering::Acquire),
            None => true,
        }
    }

    /// Queue input bytes to `addr`'s child (a no-op if it is not attached).
    pub fn input(&self, addr: &str, bytes: Vec<u8>) {
        if let Some(att) = self.map.get(addr) {
            att.input(bytes);
        }
    }

    /// Clear every attachment's output-coalescing flag after a redraw, so each
    /// pump may signal its next chunk. Bounds the event channel to ≤1 pending
    /// Output per attachment between draws (see `Attachment::pending`).
    pub fn clear_all_pending(&self) {
        for att in self.map.values() {
            att.clear_pending();
        }
    }

    /// The set of currently-attached addresses, for diffing against the inventory.
    pub fn addresses(&self) -> Vec<String> {
        self.map.keys().cloned().collect()
    }

    /// Issues the next attachment id WITHOUT spawning — for the off-loop path where the
    /// worker spawns and the cockpit inserts the finished attachment under this id.
    pub fn alloc_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Inserts a finished attachment under its address key (the off-loop handoff: the
    /// worker spawned it on its OS thread, the cockpit stores it here so `grid`/`input`/
    /// `reap` reach it). Overwrites any prior attachment at `addr`.
    pub fn insert(&mut self, addr: &str, att: Attachment) {
        self.map.insert(addr.to_string(), att);
    }

    /// Ensures `addr` is attached, spawning a real `attach` child on a PTY if absent.
    /// `argv` is the attach argv (`Source::attach_command`). A no-op (returns the
    /// existing id) when already attached. Returns the attachment's id.
    pub fn ensure(
        &mut self,
        addr: &str,
        argv: &[String],
        cols: u16,
        rows: u16,
    ) -> anyhow::Result<u64> {
        if let Some(att) = self.map.get(addr) {
            return Ok(att.id());
        }
        let id = self.next_id;
        self.next_id += 1;
        let att = (self.spawner)(argv, cols, rows, id, self.events.clone())?;
        self.map.insert(addr.to_string(), att);
        Ok(id)
    }

    /// Tears down and removes `addr`'s attachment (its session closed). A no-op if
    /// it is not attached.
    pub fn remove(&mut self, addr: &str) {
        if let Some(att) = self.map.remove(addr) {
            att.teardown();
        }
    }

    /// Removes the attachment whose id == `id` (its master hit EOF), tearing it down.
    /// A no-op if it was already removed.
    pub fn reap(&mut self, id: u64) {
        let addr = self
            .map
            .iter()
            .find(|(_, att)| att.id() == id)
            .map(|(addr, _)| addr.clone());
        if let Some(addr) = addr {
            if let Some(att) = self.map.remove(&addr) {
                att.teardown();
            }
        }
    }

    /// Resizes every kept attachment to `cols×rows` (one `PtyCmd::Resize` each, off
    /// the loop via their control threads).
    pub fn resize_all(&mut self, cols: u16, rows: u16) {
        for att in self.map.values_mut() {
            att.resize(cols, rows);
        }
    }

    /// Tears down every attachment (on quit). Each `teardown` signals its control
    /// thread and returns at once; the threads drop their masters off the loop.
    pub fn teardown_all(self) {
        for (_addr, att) in self.map {
            att.teardown();
        }
    }
}

#[cfg(test)]
impl AttachRegistry {
    /// Test-only: insert a fake entry without a real PTY, to exercise membership /
    /// removal / reap in isolation. `spawn_attachment` is live-only (human gate).
    fn insert_fake(&mut self, addr: &str, id: u64) {
        self.map
            .insert(addr.to_string(), crate::proxy::run::fake_attachment(id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_registry() -> AttachRegistry {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<PtyEvent>();
        AttachRegistry::new(tx)
    }

    #[test]
    fn contains_and_remove() {
        let mut reg = empty_registry();
        reg.insert_fake("jupiter06/api", 1);
        assert!(reg.contains("jupiter06/api"));
        assert_eq!(reg.len(), 1);
        reg.remove("jupiter06/api");
        assert!(!reg.contains("jupiter06/api"), "remove tears down + drops the entry");
        assert!(reg.is_empty());
    }

    #[test]
    fn reap_removes_by_id() {
        let mut reg = empty_registry();
        reg.insert_fake("jupiter06/b", 2);
        assert!(reg.contains("jupiter06/b"));
        reg.reap(2);
        assert!(!reg.contains("jupiter06/b"), "reap removes the EOF'd attachment");
    }

    #[test]
    fn reap_unknown_id_is_noop() {
        let mut reg = empty_registry();
        reg.insert_fake("local/work", 1);
        reg.reap(999);
        assert!(reg.contains("local/work"), "reaping an unknown id leaves the map intact");
    }

    #[test]
    fn addresses_lists_every_attachment() {
        let mut reg = empty_registry();
        reg.insert_fake("local/a", 1);
        reg.insert_fake("jupiter06/b", 2);
        let mut got = reg.addresses();
        got.sort();
        assert_eq!(got, vec!["jupiter06/b".to_string(), "local/a".to_string()]);
    }

    #[test]
    fn connecting_true_for_absent_and_fresh() {
        let mut reg = empty_registry();
        assert!(reg.connecting("nope"), "an absent address is still 'connecting'");
        reg.insert_fake("local/a", 1);
        assert!(reg.connecting("local/a"), "a fresh fake attachment starts connecting");
    }

    #[test]
    fn grid_returns_arc_for_attached() {
        let mut reg = empty_registry();
        reg.insert_fake("local/a", 1);
        assert!(reg.grid("local/a").is_some());
        assert!(reg.grid("absent").is_none());
    }

    #[test]
    fn input_to_absent_is_noop() {
        let reg = empty_registry();
        reg.input("absent", b"x".to_vec()); // must not panic
    }

    #[test]
    fn clear_grid_blanks_then_noop_for_absent() {
        let mut reg = empty_registry();
        reg.insert_fake("local/a", 1);
        // Put content into the grid, then clear it through the registry.
        if let Some(g) = reg.grid("local/a") {
            g.lock().unwrap().feed(b"stale residue");
            assert!(!g.lock().unwrap().is_blank());
        }
        reg.clear_grid("local/a");
        assert!(reg.grid("local/a").unwrap().lock().unwrap().is_blank(), "clear_grid wipes the grid");
        reg.clear_grid("absent"); // must not panic
    }

    #[test]
    fn registry_ensure_uses_injected_spawner() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };

        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_spawner = calls.clone();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<crate::proxy::run::PtyEvent>();
        let mut registry = AttachRegistry::with_spawner(
            tx,
            Box::new(move |_argv, _cols, _rows, id, _events| {
                calls_for_spawner.fetch_add(1, Ordering::SeqCst);
                Ok(crate::proxy::run::fake_attachment(id))
            }),
        );

        let argv = vec!["cmd.exe".to_string(), "/c".to_string(), "rem".to_string()];
        assert_eq!(registry.ensure("local/a", &argv, 80, 24).unwrap(), 1);
        assert_eq!(registry.ensure("local/a", &argv, 80, 24).unwrap(), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn alloc_id_increments_and_insert_registers_attachment() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<PtyEvent>();
        let mut reg = AttachRegistry::new(tx);
        let id0 = reg.alloc_id();
        let id1 = reg.alloc_id();
        assert_eq!((id0, id1), (1, 2), "ids are issued sequentially from 1");
        reg.insert("local/a", crate::proxy::run::fake_attachment(id0));
        assert!(reg.contains("local/a"));
        assert!(reg.grid("local/a").is_some(), "inserted attachment exposes its grid");
    }

    #[test]
    fn synchronous_ensure_with_slow_spawner_exceeds_responsiveness_budget() {
        use std::time::{Duration, Instant};

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<crate::proxy::run::PtyEvent>();
        let mut registry = AttachRegistry::with_spawner(
            tx,
            Box::new(move |_argv, _cols, _rows, id, _events| {
                std::thread::sleep(Duration::from_millis(100));
                Ok(crate::proxy::run::fake_attachment(id))
            }),
        );

        let argv = vec!["cmd.exe".to_string(), "/c".to_string(), "rem".to_string()];
        let started = Instant::now();
        let _ = registry.ensure("local/slow", &argv, 80, 24).unwrap();
        assert!(
            started.elapsed() >= Duration::from_millis(100),
            "current synchronous ensure blocks the caller"
        );
    }
}
