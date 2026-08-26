use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};

// SGR mouse tracking: button press/release (1000h) + drag (1002h) + any-motion
// (1003h) + SGR encoding (1006h). 1003h is on so the app sees idle moves for the
// view border hover cue; it CONSUMES idle motion (never forwards it to the mux), so the
// flood 1003h would otherwise push onto the child / a remote link does not happen.
#[cfg(windows)]
const SGR_MOUSE_ON: &[u8] = b"\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h";
#[cfg(windows)]
const SGR_MOUSE_OFF: &[u8] = b"\x1b[?1006l\x1b[?1003l\x1b[?1002l\x1b[?1000l";

/// RAII guard owning the terminal for the app's lifetime: enables raw mode,
/// enters the alternate screen, and enables SGR mouse capture on construction, then
/// on drop disables mouse capture, leaves the alternate screen, and disables raw mode.
/// Restores the user's pre-launch screen on normal return AND on a panic (release
/// builds unwind; see Cargo.toml `panic`).
pub struct TermGuard;

impl TermGuard {
    pub fn enter() -> anyhow::Result<Self> {
        enable_raw_mode()?;
        execute!(std::io::stdout(), EnterAlternateScreen, EnableMouseCapture)?;
        #[cfg(windows)]
        windows_mouse::enable()?;
        // crossterm's EnableMouseCapture (non-Windows) enables button-event motion
        // (1002h) but not any-motion (1003h); add it so idle moves arrive for the
        // view border hover cue. The Windows path already includes 1003h in SGR_MOUSE_ON.
        #[cfg(not(windows))]
        {
            use std::io::Write;
            let mut out = std::io::stdout();
            out.write_all(b"\x1b[?1003h")?;
            out.flush()?;
        }
        Ok(TermGuard)
    }
}

impl Drop for TermGuard {
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            use std::io::Write;
            let mut out = std::io::stdout();
            let _ = out.write_all(SGR_MOUSE_OFF);
            let _ = out.flush();
        }
        #[cfg(not(windows))]
        {
            use std::io::Write;
            let mut out = std::io::stdout();
            let _ = out.write_all(b"\x1b[?1003l");
            let _ = out.flush();
        }
        let _ = execute!(std::io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
        let _ = disable_raw_mode();
    }
}

/// Windows mouse setup the app needs but crossterm doesn't provide. crossterm's
/// `EnableMouseCapture` takes the WinAPI path on Windows (its `is_ansi_code_supported`
/// is false): it sets the legacy `ENABLE_MOUSE_INPUT` console flag but neither enables
/// virtual-terminal input nor emits the SGR mouse-tracking DECSET. The app reads
/// raw stdin and parses SGR mouse sequences, so without VT input ConPTY delivers mouse
/// as legacy INPUT_RECORDs (a `read()` never sees them), and without the DECSET the
/// terminal keeps its native drag-to-select. This adds both.
#[cfg(windows)]
mod windows_mouse {
    use std::io::Write;
    use windows_sys::Win32::System::Console::{
        GetConsoleMode, GetStdHandle, SetConsoleMode, ENABLE_ECHO_INPUT, ENABLE_EXTENDED_FLAGS,
        ENABLE_LINE_INPUT, ENABLE_MOUSE_INPUT, ENABLE_PROCESSED_INPUT, ENABLE_QUICK_EDIT_MODE,
        ENABLE_VIRTUAL_TERMINAL_INPUT, STD_INPUT_HANDLE,
    };

    pub(super) fn enable() -> anyhow::Result<()> {
        set_conin_mode();
        let mut out = std::io::stdout();
        out.write_all(super::SGR_MOUSE_ON)?;
        out.flush()?;
        Ok(())
    }

    /// Re-asserts only the CONIN input-mode bits, without touching the terminal output.
    /// Called at a ConPTY child spawn (the mutation source), from the worker thread, where
    /// writing SGR to stdout would interleave with the main loop's draw.
    pub(super) fn reassert_conin_mode() -> anyhow::Result<()> {
        set_conin_mode();
        Ok(())
    }

    /// Applies the desired CONIN input-mode bits: sets VT-input, mouse-input, extended
    /// flags; clears quick-edit, line-input, echo-input, processed-input.
    fn set_conin_mode() {
        // SAFETY: standard console handle; the calls are read-then-write of a mode flag.
        unsafe {
            let h = GetStdHandle(STD_INPUT_HANDLE);
            let mut mode = 0u32;
            if GetConsoleMode(h, &mut mode) != 0 {
                let want = (mode
                    | ENABLE_VIRTUAL_TERMINAL_INPUT
                    | ENABLE_MOUSE_INPUT
                    | ENABLE_EXTENDED_FLAGS)
                    & !ENABLE_QUICK_EDIT_MODE
                    & !ENABLE_LINE_INPUT
                    & !ENABLE_ECHO_INPUT
                    & !ENABLE_PROCESSED_INPUT;
                SetConsoleMode(h, want);
            }
        }
    }
}

/// Reads the current console INPUT (CONIN) mode for diagnostics; 0 if unavailable.
/// Used to detect whether the mouse / VT-input bits get cleared during operation.
#[cfg(windows)]
pub fn conin_mode() -> u32 {
    use windows_sys::Win32::System::Console::{GetConsoleMode, GetStdHandle, STD_INPUT_HANDLE};
    // SAFETY: standard handle; a plain mode read.
    unsafe {
        let h = GetStdHandle(STD_INPUT_HANDLE);
        let mut m = 0u32;
        if GetConsoleMode(h, &mut m) != 0 {
            m
        } else {
            0
        }
    }
}

#[cfg(not(windows))]
pub fn conin_mode() -> u32 {
    0
}

/// Whether a CONIN mode has drifted off the desired mouse-capture state and must be
/// re-asserted: mouse input cleared, quick-edit (drag-to-select) re-enabled, or VT input
/// cleared. Pure over the mode bits so it is unit-testable on any target.
fn should_reassert(mode: u32) -> bool {
    use windows_sys::Win32::System::Console::{
        ENABLE_MOUSE_INPUT, ENABLE_QUICK_EDIT_MODE, ENABLE_VIRTUAL_TERMINAL_INPUT,
    };
    mode & ENABLE_MOUSE_INPUT == 0
        || mode & ENABLE_QUICK_EDIT_MODE != 0
        || mode & ENABLE_VIRTUAL_TERMINAL_INPUT == 0
}

/// Re-asserts the CONIN mouse-capture bits only, without writing to the terminal. Called
/// synchronously at a ConPTY child spawn (the mutation source), from the worker thread,
/// where a stdout write would interleave with the main loop's draw. No-op off Windows.
#[cfg(windows)]
pub fn reassert_conin_mode() {
    let _ = windows_mouse::reassert_conin_mode();
}

#[cfg(not(windows))]
pub fn reassert_conin_mode() {}

/// Re-applies the mouse-capture console mode + SGR tracking when the console drifts
/// from the state `TermGuard` established. Spawning a `portable-pty` child (ConPTY) can
/// silently mutate the PARENT's CONIN mid-session: clearing `ENABLE_MOUSE_INPUT` kills
/// mouse capture (keyboard survives, only the mouse dies), and re-enabling
/// `ENABLE_QUICK_EDIT_MODE` lets the terminal's native drag-to-select text selection back
/// in. Both violate the no-selection invariant, so either bit drifting off the desired
/// state re-asserts the full mode. The app calls this each loop iteration; it is a cheap
/// mode read and re-applies only on drift. No-op off Windows.
#[cfg(windows)]
pub fn ensure_mouse_capture() {
    use std::sync::atomic::{AtomicU32, Ordering};
    use windows_sys::Win32::System::Console::{
        ENABLE_MOUSE_INPUT, ENABLE_QUICK_EDIT_MODE, ENABLE_VIRTUAL_TERMINAL_INPUT,
    };
    // Observe CONIN mode transitions: mouse capture on Windows is fragile (a portable-pty
    // child spawn can silently mutate the input bits), so tracing exactly which bit drifts —
    // vt-input / mouse-input / quick-edit — the instant it changes turns an intermittent
    // "mouse stopped working" / "text selected itself" report into a pinpointed cause.
    // Logged only on change (silent in steady state) AND at trace level: a ConPTY child
    // keeps mutating the parent's mouse bit, so this fires on (almost) every loop iteration
    // and would otherwise be the bulk of a day's log (one file was ~98% these lines).
    // Keep it trace: enable with `XMUX_LOG=xmux::display=debug` when hunting a capture bug.
    static LAST: AtomicU32 = AtomicU32::new(u32::MAX);
    let m = conin_mode();
    if LAST.swap(m, Ordering::Relaxed) != m {
        tracing::trace!(
            mode = format!("{m:#010x}"),
            vt_input = (m & ENABLE_VIRTUAL_TERMINAL_INPUT != 0),
            mouse_input = (m & ENABLE_MOUSE_INPUT != 0),
            quick_edit = (m & ENABLE_QUICK_EDIT_MODE != 0),
            "conin_mode_changed"
        );
    }
    // Re-assert when mouse capture is lost OR quick-edit (drag-to-select) is enabled OR
    // VT input is lost; `windows_mouse::enable` restores the desired bits and the SGR tracking.
    if should_reassert(m) {
        let _ = windows_mouse::enable();
    }
}

#[cfg(not(windows))]
pub fn ensure_mouse_capture() {}

/// Parse a prefix-key spec (the `[ui] prefix` config value) into a C0 control byte.
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
    use windows_sys::Win32::System::Console::{
        ENABLE_EXTENDED_FLAGS, ENABLE_MOUSE_INPUT, ENABLE_QUICK_EDIT_MODE,
        ENABLE_VIRTUAL_TERMINAL_INPUT,
    };

    #[test]
    fn should_reassert_healthy_mode_is_false() {
        // All desired bits set, quick-edit cleared: no re-assert.
        let mode = ENABLE_MOUSE_INPUT | ENABLE_VIRTUAL_TERMINAL_INPUT | ENABLE_EXTENDED_FLAGS;
        assert!(!should_reassert(mode));
    }

    #[test]
    fn should_reassert_when_mouse_input_cleared() {
        let mode = ENABLE_VIRTUAL_TERMINAL_INPUT | ENABLE_EXTENDED_FLAGS; // no MOUSE_INPUT
        assert!(should_reassert(mode));
    }

    #[test]
    fn should_reassert_when_quick_edit_re_enabled() {
        // A ConPTY child spawn re-enables drag-to-select (the selection symptom).
        let mode = ENABLE_MOUSE_INPUT | ENABLE_VIRTUAL_TERMINAL_INPUT | ENABLE_QUICK_EDIT_MODE;
        assert!(should_reassert(mode));
    }

    #[test]
    fn should_reassert_when_vt_input_cleared() {
        // A child stripping VT input kills SGR mouse delivery but leaves MOUSE_INPUT set
        // and quick-edit clear, so it is only caught by the VT-input check.
        let mode = ENABLE_MOUSE_INPUT | ENABLE_EXTENDED_FLAGS; // no VIRTUAL_TERMINAL_INPUT
        assert!(should_reassert(mode));
    }

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
