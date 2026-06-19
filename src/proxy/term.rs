use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};

/// RAII guard owning the terminal for the cockpit's lifetime: enables raw mode
/// and enters the alternate screen on construction (so pre-launch shell output
/// never bleeds under the UI — issue #1), and on drop leaves the alternate
/// screen + disables raw mode, restoring the user's pre-launch screen on normal
/// return AND on a panic (release builds unwind; see Cargo.toml `panic`).
pub struct TermGuard;

impl TermGuard {
    pub fn enter() -> anyhow::Result<Self> {
        enable_raw_mode()?;
        execute!(std::io::stdout(), EnterAlternateScreen)?;
        Ok(TermGuard)
    }
}

impl Drop for TermGuard {
    fn drop(&mut self) {
        let _ = execute!(std::io::stdout(), LeaveAlternateScreen);
        let _ = disable_raw_mode();
    }
}

/// Parse an `XMUX_PREFIX`-style spec into a C0 control byte.
///
/// Recognised forms:
/// - `C-<letter>` / `c-<letter>` → 0x01..0x1a
/// - `C-Space` / `c-space`       → 0x00
/// - anything else               → default 0x07 (`C-g`)
pub fn parse_prefix(spec: Option<&str>) -> u8 {
    let s = match spec {
        Some(s) => s.trim(),
        None => return 0x07,
    };
    let rest = s.strip_prefix("C-").or_else(|| s.strip_prefix("c-"));
    match rest {
        Some("Space") | Some("space") => 0x00,
        Some(c) if c.len() == 1 => {
            let ch = c.as_bytes()[0].to_ascii_lowercase();
            if ch.is_ascii_lowercase() {
                ch - b'a' + 1
            } else {
                0x07
            }
        }
        _ => 0x07,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_prefix_recognises_specs_and_defaults() {
        assert_eq!(parse_prefix(Some("C-g")), 0x07);
        assert_eq!(parse_prefix(Some("C-Space")), 0x00);
        assert_eq!(parse_prefix(Some("c-a")), 0x01);
        assert_eq!(parse_prefix(None), 0x07);
        assert_eq!(parse_prefix(Some("garbage")), 0x07);
    }

    /// Human visual gate: verifies that `TermGuard` enters the alternate screen
    /// and restores the user's pre-launch screen on drop.  Requires a real
    /// console — raw-mode toggling is not available in the test harness.
    #[test]
    #[ignore]
    fn term_guard_enters_and_restores() {
        let _guard = TermGuard::enter().expect("TermGuard::enter failed");
        // drop restores LeaveAlternateScreen + disable_raw_mode
    }
}
