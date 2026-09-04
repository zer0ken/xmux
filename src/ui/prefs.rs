//! Persists lightweight, best-effort UI preferences across runs (the last-selected
//! session address, the nav width and height, the auto-hide-nav mode). Every value is a hint
//! only - a stale, missing, or unparsable file falls back to the built-in default,
//! so xmux stays stateless about sessions themselves.

use std::path::Path;

use crate::session::Address;
use crate::ui::switcher::NavPosition;

/// The file under the xmux dir holding the last-selected session address.
const LAST_SESSION_FILE: &str = "last_session";

/// The file under the xmux dir holding the nav view width the user last set
/// with `prefix h`/`l`, so the next launch restores it instead of the default.
const NAV_WIDTH_FILE: &str = "nav_width";

/// The file under the xmux dir holding the nav height (portrait band layout) the
/// user last set by dragging the horizontal view border, so the next launch restores it.
const NAV_HEIGHT_FILE: &str = "nav_height";

/// The file under the xmux dir holding the auto-hide-nav mode the user last set
/// with `prefix t` ("1"/"0"), so the next launch restores it (overriding the
/// `auto-hide-nav` config default).
const AUTO_HIDE_NAV_FILE: &str = "auto_hide_nav";

/// The file under the xmux dir holding the nav placement the user last set with
/// `prefix p` (a side word, or "auto" when the key cycled back to following the
/// [ui] settings), so the next launch restores the pin (overriding the
/// `auto-nav-position` config default).
const NAV_POSITION_FILE: &str = "nav_position";

/// Reads the persisted auto-hide-nav mode. `None` when the file is absent or
/// unrecognised - the caller falls back to the config default.
pub fn load_auto_hide_nav(xmux_dir: &Path) -> Option<bool> {
    match std::fs::read_to_string(xmux_dir.join(AUTO_HIDE_NAV_FILE))
        .ok()?
        .trim()
    {
        "1" => Some(true),
        "0" => Some(false),
        _ => None,
    }
}

/// Persists the auto-hide-nav mode. Best-effort: a write failure only loses the
/// next launch's restore.
pub fn save_auto_hide_nav(xmux_dir: &Path, on: bool) {
    let _ = std::fs::write(
        xmux_dir.join(AUTO_HIDE_NAV_FILE),
        if on { "1" } else { "0" },
    );
}

/// Reads the persisted nav placement pin. `Some(side)` pins the nav to that side;
/// `None` (an absent, unreadable, "auto", or unparsable file) means follow the
/// [ui] settings.
pub fn load_nav_position(xmux_dir: &Path) -> Option<NavPosition> {
    let raw = std::fs::read_to_string(xmux_dir.join(NAV_POSITION_FILE)).ok()?;
    NavPosition::parse(&raw)
}

/// Persists the nav placement pin. `None` stores "auto" (no pin, follow the [ui]
/// settings). Best-effort: a write failure only loses the next launch's restore.
pub fn save_nav_position(xmux_dir: &Path, pinned: Option<NavPosition>) {
    let word = match pinned {
        Some(p) => p.word(),
        None => "auto",
    };
    let _ = std::fs::write(xmux_dir.join(NAV_POSITION_FILE), word);
}

/// Reads the persisted tree width. `None` when the file is absent, unreadable, or
/// not a `u16` - the caller falls back to the default width.
pub fn load_nav_width(xmux_dir: &Path) -> Option<u16> {
    let raw = std::fs::read_to_string(xmux_dir.join(NAV_WIDTH_FILE)).ok()?;
    raw.trim().parse::<u16>().ok()
}

/// Persists the tree width. Best-effort: a write failure only loses the next
/// launch's width restore.
pub fn save_nav_width(xmux_dir: &Path, width: u16) {
    let _ = std::fs::write(xmux_dir.join(NAV_WIDTH_FILE), width.to_string());
}

/// Reads the persisted band-layout tree height. `None` when absent or unparsable - the
/// caller falls back to the auto height (~40% of the body).
pub fn load_nav_height(xmux_dir: &Path) -> Option<u16> {
    let raw = std::fs::read_to_string(xmux_dir.join(NAV_HEIGHT_FILE)).ok()?;
    raw.trim().parse::<u16>().ok()
}

/// Persists the band-layout tree height. Best-effort: a write failure only loses the
/// next launch's height restore.
pub fn save_nav_height(xmux_dir: &Path, height: u16) {
    let _ = std::fs::write(xmux_dir.join(NAV_HEIGHT_FILE), height.to_string());
}

/// Persists the last-selected session as two fields (the source line, then the
/// session line), so a session name holding any character survives without a
/// delimiter grammar. Best-effort: a write failure is ignored, and so is the value
/// on the next run - the launch preselect is the first session to answer the scan
/// (FR-D5), so nothing reads this file back.
pub fn save_last_session(xmux_dir: &Path, address: &Address) {
    let _ = std::fs::write(
        xmux_dir.join(LAST_SESSION_FILE),
        format!("{}\n{}", address.source, address.session),
    );
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
    fn save_writes_the_two_fields() {
        let dir = temp_dir("roundtrip");
        save_last_session(&dir, &Address::new("jupiter00", "infer"));
        assert_eq!(
            std::fs::read_to_string(dir.join(LAST_SESSION_FILE)).unwrap(),
            "jupiter00\ninfer"
        );
        // A session name holding a space or a slash stays whole on its own line.
        save_last_session(&dir, &Address::new("prod", "my session"));
        assert_eq!(
            std::fs::read_to_string(dir.join(LAST_SESSION_FILE)).unwrap(),
            "prod\nmy session"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nav_width_save_then_load_round_trips() {
        let dir = temp_dir("tw-roundtrip");
        save_nav_width(&dir, 62);
        assert_eq!(load_nav_width(&dir), Some(62));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nav_width_load_missing_or_garbage_is_none() {
        let dir = temp_dir("tw-garbage");
        assert_eq!(load_nav_width(&dir), None, "absent file");
        std::fs::write(dir.join(NAV_WIDTH_FILE), "not-a-number").unwrap();
        assert_eq!(load_nav_width(&dir), None, "unparsable value");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn auto_hide_nav_save_then_load_round_trips() {
        let dir = temp_dir("ah-roundtrip");
        save_auto_hide_nav(&dir, true);
        assert_eq!(load_auto_hide_nav(&dir), Some(true));
        save_auto_hide_nav(&dir, false);
        assert_eq!(load_auto_hide_nav(&dir), Some(false));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn auto_hide_nav_load_missing_or_garbage_is_none() {
        let dir = temp_dir("ah-garbage");
        assert_eq!(load_auto_hide_nav(&dir), None, "absent file");
        std::fs::write(dir.join(AUTO_HIDE_NAV_FILE), "yes").unwrap();
        assert_eq!(load_auto_hide_nav(&dir), None, "unrecognised value");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nav_position_save_then_load_round_trips() {
        let dir = temp_dir("np-roundtrip");
        save_nav_position(&dir, Some(NavPosition::Right));
        assert_eq!(load_nav_position(&dir), Some(NavPosition::Right));
        save_nav_position(&dir, Some(NavPosition::Bottom));
        assert_eq!(load_nav_position(&dir), Some(NavPosition::Bottom));
        // None means follow the [ui] settings, stored as the word "auto".
        save_nav_position(&dir, None);
        assert_eq!(
            std::fs::read_to_string(dir.join(NAV_POSITION_FILE)).unwrap(),
            "auto"
        );
        assert_eq!(load_nav_position(&dir), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nav_position_load_missing_or_garbage_is_none() {
        let dir = temp_dir("np-garbage");
        assert_eq!(load_nav_position(&dir), None, "absent file");
        std::fs::write(dir.join(NAV_POSITION_FILE), "diagonal").unwrap();
        assert_eq!(load_nav_position(&dir), None, "unrecognised value");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
