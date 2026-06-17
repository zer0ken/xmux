//! A one-shot cross-server jump handoff. The in-mux popup cannot switch-client
//! across mux servers, so for a cross-server pick it records the chosen target
//! here and detaches; the home loop, regaining control, consumes the record and
//! attaches that target directly — making a cross-server pick a single action
//! rather than detach-then-re-pick.
//!
//! The record is consumed at most once and only when fresh: a stale file left by
//! a popup whose home loop never ran (the cockpit precondition was not met) must
//! not hijack a much-later `xmux` launch into an unexpected attach.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::session::{self, Session};

/// A pending jump older than this is treated as stale and ignored. The
/// popup→detach→home-loop handoff is near-instant, so a real jump is always far
/// fresher than this.
const FRESH_WINDOW: Duration = Duration::from_secs(10);

/// The handoff file path within the xmux dir.
pub fn pending_path(xmux_dir: &Path) -> PathBuf {
    xmux_dir.join("pending-jump")
}

/// Records the target the home loop should attach on its next iteration.
pub fn write_pending(xmux_dir: &Path, target: &Session) -> std::io::Result<()> {
    std::fs::create_dir_all(xmux_dir)?;
    std::fs::write(pending_path(xmux_dir), target.address())
}

/// Consumes a pending jump if present, parsing it back into a target. Always
/// removes the file (even when stale or malformed) so it fires at most once;
/// returns `None` when there is none, it is stale, or it does not parse.
pub fn take_pending(xmux_dir: &Path) -> Option<Session> {
    let path = pending_path(xmux_dir);
    let content = std::fs::read_to_string(&path).ok()?;
    let fresh = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|m| m.elapsed().ok())
        .is_some_and(|age| age < FRESH_WINDOW);
    let _ = std::fs::remove_file(&path); // consume at most once, fresh or not
    if !fresh {
        return None;
    }
    session::parse_target(content.trim()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("xmux-jump-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn target(source: &str, name: &str) -> Session {
        Session {
            source: source.into(),
            name: name.into(),
            ..Default::default()
        }
    }

    #[test]
    fn write_then_take_round_trips() {
        let dir = tmp("roundtrip");
        write_pending(&dir, &target("prod", "api")).unwrap();
        let got = take_pending(&dir).expect("a fresh pending jump");
        assert_eq!(got.source, "prod");
        assert_eq!(got.name, "api");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn take_consumes_so_it_fires_once() {
        let dir = tmp("once");
        write_pending(&dir, &target("prod", "api")).unwrap();
        assert!(take_pending(&dir).is_some());
        assert!(take_pending(&dir).is_none(), "second take must be empty");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn take_none_when_absent() {
        let dir = tmp("absent");
        std::fs::create_dir_all(&dir).unwrap();
        assert!(take_pending(&dir).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_pending_is_ignored_and_removed() {
        let dir = tmp("stale");
        write_pending(&dir, &target("prod", "api")).unwrap();
        // Backdate the file well beyond the freshness window.
        let path = pending_path(&dir);
        let old = std::time::SystemTime::now() - Duration::from_secs(3600);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(old)
            .unwrap();
        assert!(take_pending(&dir).is_none(), "stale jump must be ignored");
        assert!(!path.exists(), "stale jump must still be consumed (removed)");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
