//! Tiny cross-run UI state: the session the user last had selected. The next
//! launch restores it instead of guessing from `session_last_attached`, which
//! xmux's own pre-attaching (it keeps a live client per session) and cross-host
//! clock differences make an unreliable signal of true user interaction. This is a
//! best-effort hint only — a stale/missing value just falls back to the local-first
//! preselect, so xmux stays stateless about sessions themselves.

use std::path::Path;

/// The file under the xmux dir holding the last-selected session address.
const LAST_SESSION_FILE: &str = "last_session";

/// Reads the persisted last-selected session address (`source/session`). `None`
/// when the file is absent, unreadable, or blank.
pub fn load_last_session(xmux_dir: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(xmux_dir.join(LAST_SESSION_FILE)).ok()?;
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Persists `address` (`source/session`) as the last-selected session. Best-effort:
/// a write failure is ignored — it only degrades the next launch's preselect.
pub fn save_last_session(xmux_dir: &Path, address: &str) {
    let _ = std::fs::write(xmux_dir.join(LAST_SESSION_FILE), address);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("xmux-state-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn save_then_load_round_trips_the_address() {
        let dir = temp_dir("roundtrip");
        save_last_session(&dir, "jupiter00/infer");
        assert_eq!(load_last_session(&dir).as_deref(), Some("jupiter00/infer"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_missing_file_is_none() {
        let dir = std::env::temp_dir().join("xmux-state-absent-does-not-exist-zzz");
        assert_eq!(load_last_session(&dir), None);
    }

    #[test]
    fn load_blank_file_is_none() {
        let dir = temp_dir("blank");
        save_last_session(&dir, "   \n");
        assert_eq!(load_last_session(&dir), None, "a blank value is treated as absent");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
