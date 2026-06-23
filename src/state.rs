//! Persists the last-selected session across runs so the next launch can preselect
//! it. (`session_last_attached` is not used: xmux's own pre-attaching and
//! cross-host clock differences make it an unreliable interaction signal.) This is
//! a best-effort hint only — a stale/missing value just falls back to the
//! local-first preselect, so xmux stays stateless about sessions themselves.

use std::path::Path;

/// The file under the xmux dir holding the last-selected session address.
const LAST_SESSION_FILE: &str = "last_session";

/// The file under the xmux dir holding the tree (sidebar) width the user last set
/// with `prefix h`/`l`, so the next launch restores it instead of the default.
const TREE_WIDTH_FILE: &str = "tree_width";

/// Reads the persisted tree width. `None` when the file is absent, unreadable, or
/// not a `u16` — the caller falls back to the default width.
pub fn load_tree_width(xmux_dir: &Path) -> Option<u16> {
    let raw = std::fs::read_to_string(xmux_dir.join(TREE_WIDTH_FILE)).ok()?;
    raw.trim().parse::<u16>().ok()
}

/// Persists the tree width. Best-effort: a write failure only loses the next
/// launch's width restore.
pub fn save_tree_width(xmux_dir: &Path, width: u16) {
    let _ = std::fs::write(xmux_dir.join(TREE_WIDTH_FILE), width.to_string());
}

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

    #[test]
    fn tree_width_save_then_load_round_trips() {
        let dir = temp_dir("tw-roundtrip");
        save_tree_width(&dir, 62);
        assert_eq!(load_tree_width(&dir), Some(62));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tree_width_load_missing_or_garbage_is_none() {
        let dir = temp_dir("tw-garbage");
        assert_eq!(load_tree_width(&dir), None, "absent file");
        std::fs::write(dir.join(TREE_WIDTH_FILE), "not-a-number").unwrap();
        assert_eq!(load_tree_width(&dir), None, "unparsable value");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
