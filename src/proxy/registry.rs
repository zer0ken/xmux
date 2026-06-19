//! The AttachRegistry: an `Address → Attachment` map bounded by the keep cap.
//! When attaching would exceed the cap it evicts the least-recently-used
//! attachment that is neither the foreground nor the current cursor session.
//! A master-EOF reap removes the dead attachment. All blocking PTY work lives on
//! each Attachment's control/pump threads (Task 4), so registry methods never
//! block the event loop.

use std::collections::HashMap;
use std::time::Instant;

use crate::proxy::run::{spawn_attachment, Attachment, LiveOwner};

pub struct AttachRegistry {
    map: HashMap<String, Attachment>,
    cap: usize,
    next_id: u64,
    live: LiveOwner,
    eof_tx: tokio::sync::mpsc::UnboundedSender<u64>,
}

impl AttachRegistry {
    pub fn new(
        cap: usize,
        live: LiveOwner,
        eof_tx: tokio::sync::mpsc::UnboundedSender<u64>,
    ) -> Self {
        AttachRegistry {
            map: HashMap::new(),
            cap: cap.max(2),
            next_id: 1,
            live,
            eof_tx,
        }
    }

    pub fn contains(&self, addr: &str) -> bool {
        self.map.contains_key(addr)
    }

    /// The number of currently-kept attachments (for the status bar).
    pub fn kept(&self) -> usize {
        self.map.len()
    }

    /// Push the Passthrough status-bar bytes into `addr`'s attachment so its owner
    /// pump can re-emit them after a full-screen clear.
    pub fn set_status_bar(&self, addr: &str, bytes: Vec<u8>) {
        if let Some(att) = self.map.get(addr) {
            att.set_status_bar(bytes);
        }
    }

    pub fn get(&self, addr: &str) -> Option<&Attachment> {
        self.map.get(addr)
    }

    pub fn id_of(&self, addr: &str) -> Option<u64> {
        self.map.get(addr).map(Attachment::id)
    }

    pub fn touch(&mut self, addr: &str) {
        if let Some(att) = self.map.get_mut(addr) {
            att.last_used = Instant::now();
        }
    }

    /// The address of the least-recently-used attachment that is not protected,
    /// or `None` when every attachment is protected.
    fn lru_victim(&self, protect: &[&str]) -> Option<String> {
        self.map
            .iter()
            .filter(|(addr, _)| !protect.contains(&addr.as_str()))
            .min_by_key(|(_, att)| att.last_used)
            .map(|(addr, _)| addr.clone())
    }

    /// Ensures `addr` is attached, evicting an unprotected LRU entry first when at
    /// cap. Returns the attachment's id. `protect` lists addresses that must not be
    /// evicted (the foreground + the current cursor session).
    pub fn ensure(
        &mut self,
        addr: &str,
        argv: &[String],
        cols: u16,
        rows: u16,
        protect: &[&str],
    ) -> anyhow::Result<u64> {
        if let Some(att) = self.map.get_mut(addr) {
            att.last_used = Instant::now();
            return Ok(att.id());
        }
        while self.map.len() >= self.cap {
            match self.lru_victim(protect) {
                Some(victim) => {
                    if let Some(att) = self.map.remove(&victim) {
                        att.teardown();
                    }
                }
                None => break, // everything protected; allow a transient over-cap
            }
        }
        let id = self.next_id;
        self.next_id += 1;
        let att = spawn_attachment(
            argv,
            cols,
            rows,
            id,
            self.live.clone(),
            std::io::stdout(),
            self.eof_tx.clone(),
        )?;
        self.map.insert(addr.to_string(), att);
        Ok(id)
    }

    /// Removes the attachment whose id == `id` (its master hit EOF), tearing it
    /// down. A no-op if it was already evicted.
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

    /// Resizes every kept attachment to `cols×rows` (one PtyCmd::Resize each, off
    /// the loop via their control threads).
    pub fn resize_all(&mut self, cols: u16, rows: u16) {
        for att in self.map.values_mut() {
            att.resize(cols, rows);
            att.size = (cols, rows);
        }
    }

    /// Tears down every attachment (on quit). Each `teardown` signals its control
    /// thread and returns immediately; the threads drop their masters off the loop.
    pub fn teardown_all(self) {
        for (_addr, att) in self.map {
            att.teardown();
        }
    }
}

#[cfg(test)]
impl AttachRegistry {
    /// Test-only: insert a fake entry without a real PTY, to exercise the LRU /
    /// protect / reap selection in isolation. The fake Attachment's child + threads
    /// are dummies that are never driven.
    fn insert_fake(&mut self, addr: &str, id: u64, last_used: std::time::Instant) {
        let att = crate::proxy::run::fake_attachment(id, last_used);
        self.map.insert(addr.to_string(), att);
    }

    fn lru_victim_pub(&self, protect: &[&str]) -> Option<String> {
        self.lru_victim(protect)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    // A registry whose map we populate directly with fake entries (no PTY), to
    // test the LRU/protect selection in isolation. spawn_attachment is live-only
    // (Task 14).
    fn empty_registry(cap: usize) -> AttachRegistry {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<u64>();
        AttachRegistry::new(cap, crate::proxy::run::LiveOwner::new(), tx)
    }

    #[test]
    fn lru_victim_picks_oldest_unprotected() {
        let mut reg = empty_registry(3);
        let now = Instant::now();
        reg.insert_fake("local/a", 1, now - Duration::from_secs(30));
        reg.insert_fake("jupiter06/b", 2, now - Duration::from_secs(10));
        reg.insert_fake("jupiter06/c", 3, now - Duration::from_secs(20));
        // a is oldest, but protect it: the next-oldest unprotected (c) is the victim.
        assert_eq!(
            reg.lru_victim_pub(&["local/a"]).as_deref(),
            Some("jupiter06/c")
        );
        // With nothing protected, the oldest (a) is the victim.
        assert_eq!(reg.lru_victim_pub(&[]).as_deref(), Some("local/a"));
    }

    #[test]
    fn lru_victim_none_when_all_protected() {
        let mut reg = empty_registry(2);
        let now = Instant::now();
        reg.insert_fake("local/a", 1, now);
        reg.insert_fake("jupiter06/b", 2, now);
        assert!(reg.lru_victim_pub(&["local/a", "jupiter06/b"]).is_none());
    }

    #[test]
    fn reap_removes_by_id() {
        let mut reg = empty_registry(3);
        let now = Instant::now();
        reg.insert_fake("jupiter06/b", 2, now);
        assert!(reg.contains("jupiter06/b"));
        reg.reap(2);
        assert!(!reg.contains("jupiter06/b"), "reap removes the EOF'd attachment");
    }
}
