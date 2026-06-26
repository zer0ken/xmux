//! Free helpers for death-as-a-push, all over the Phase-1 `DeathSignal`/`DisplayTty`
//! (defined in `crate::model::plan`, NOT here): the tty-match detach filter, the
//! psmux `.port` liveness backing `DeathSignal::PathStat`, and the
//! identity-preserving display-tty capture (the marker our OWN attach shell
//! self-reports, plus the pump-side parser). No type is defined here.

use std::path::PathBuf;

use crate::model::{DeathSignal, DisplayTty};

/// The unique marker our attach shell wraps its self-reported tty in: an OSC-style
/// `ESC ] … BEL` sequence so it is inert as terminal output and unmistakable in the
/// byte stream. One token: `\x1b]XMUX-DISPLAY-TTY:<tty>\x07`.
const MARKER_OPEN: &str = "\x1b]XMUX-DISPLAY-TTY:";
const MARKER_CLOSE: u8 = 0x07; // BEL

/// True when `detached_tty` is xmux's OWN display client under a `ControlNotice`
/// death (tmux's `%client-detached`). Only `ControlNotice` is tty-filtered — `Eof`
/// and `PathStat` are per-session deaths that never arrive as a client detach. An
/// empty/unknown `display_tty` never matches, so an unrelated client's detach is
/// structurally inert.
pub fn matches_display_tty(
    signal: &DeathSignal,
    detached_tty: &str,
    display_tty: &DisplayTty,
) -> bool {
    matches!(signal, DeathSignal::ControlNotice) && display_tty.0.as_deref() == Some(detached_tty)
}

/// `~/.psmux/<session>.port` — psmux's per-session liveness marker (one server per
/// session over localhost TCP; this file is psmux's own discovery substrate).
pub fn psmux_port_path(session: &str) -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".psmux")
        .join(format!("{session}.port"))
}

/// True when the session's `.port` file exists. Its disappearance means the
/// per-session server is gone even if a stale PTY lingers (the PathStat death).
pub fn psmux_session_is_live(session: &str) -> bool {
    psmux_port_path(session).exists()
}

/// The shell prefix xmux prepends to its OWN attach argv so the attach shell prints
/// its tty — provably xmux's display client's tty — over the PTY before `exec`'ing
/// the real mux attach. The argv builder appends `exec <mux attach …>` after this.
/// Identity-preserving: only this attach shell runs the snippet, so the captured
/// value cannot be another client's tty (codex H3). `printf` (not `echo -e`) for
/// portable escape handling across remote POSIX shells; `$(tty)` is the attach
/// shell's own controlling terminal.
pub fn display_tty_marker_prefix() -> &'static str {
    "printf '\\033]XMUX-DISPLAY-TTY:%s\\007' \"$(tty)\" ;"
}

/// Extracts the tty from the FIRST whole `\x1b]XMUX-DISPLAY-TTY:<tty>\x07` marker in
/// `bytes`, or `None` if no whole marker is present. Identity-preserving: the marker
/// is emitted ONLY by xmux's own attach shell, so any other client tty elsewhere in
/// the stream (a `list-clients` dump, the user's terminal) is ignored — the value is
/// provably xmux's display client (codex H3). UTF-8-lossy on the tty bytes (a tty
/// path is ASCII in practice).
pub fn parse_display_tty_marker(bytes: &[u8]) -> Option<String> {
    let open = MARKER_OPEN.as_bytes();
    let start = bytes.windows(open.len()).position(|w| w == open)? + open.len();
    let end = bytes[start..].iter().position(|&b| b == MARKER_CLOSE)? + start;
    let tty = String::from_utf8_lossy(&bytes[start..end]).into_owned();
    (!tty.is_empty()).then_some(tty)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{DeathSignal, DisplayTty};

    // --- the tty-match filter (the detach side) ---

    #[test]
    fn matches_only_for_control_notice_and_only_our_tty() {
        let ours = DisplayTty(Some("/dev/pts/3".into()));
        // ControlNotice (tmux): matches ONLY our own tty.
        assert!(matches_display_tty(
            &DeathSignal::ControlNotice,
            "/dev/pts/3",
            &ours
        ));
        assert!(!matches_display_tty(
            &DeathSignal::ControlNotice,
            "/dev/pts/9",
            &ours
        ));
        // An unknown/empty display tty never matches → an unrelated detach is inert.
        assert!(!matches_display_tty(
            &DeathSignal::ControlNotice,
            "/dev/pts/3",
            &DisplayTty(None)
        ));
    }

    #[test]
    fn non_control_notice_signals_never_match_a_detach() {
        // psmux death is EOF/PathStat, never a %client-detached — a ControlNotice
        // filter must not fire for a PerSession mux even if the ttys are equal.
        let ours = DisplayTty(Some("/dev/pts/3".into()));
        assert!(!matches_display_tty(&DeathSignal::Eof, "/dev/pts/3", &ours));
        assert!(!matches_display_tty(
            &DeathSignal::PathStat {
                dir_is_psmux_registry: true
            },
            "/dev/pts/3",
            &ours
        ));
    }

    // --- the identity-preserving capture (the marker side) ---

    #[test]
    fn marker_prefix_self_reports_the_attach_shells_own_tty_before_exec() {
        // The prefix prints OUR attach shell's own tty (the value of `tty`), wrapped
        // in the unique marker, THEN the attach argv appends `exec <mux attach>`.
        // Only xmux's own attach shell runs this, so the captured value is ours.
        let p = display_tty_marker_prefix();
        assert!(p.contains("$(tty"), "self-reports its own tty: {p}");
        assert!(
            p.contains("XMUX-DISPLAY-TTY:"),
            "inside the unique marker: {p}"
        );
        assert!(
            p.trim_end().ends_with(';'),
            "a prefix the attach argv appends exec to: {p}"
        );
    }

    #[test]
    fn parse_picks_our_marked_tty_not_the_first_tty_in_a_multiclient_dump() {
        // The pump's byte stream may ALSO carry a list-clients-style dump with
        // several other clients' ttys (the user's own terminal, the -CC control
        // client). A naive "first tty-looking token" pick would choose /dev/pts/0.
        // The marker disambiguates: the captured tty is the ONE our attach shell
        // self-reported, regardless of what else is on screen.
        let stream = b"\
/dev/pts/0 user-shell\r\n\
/dev/pts/7 control-client\r\n\
\x1b]XMUX-DISPLAY-TTY:/dev/pts/3\x07\
some-banner /dev/pts/5\r\n";
        assert_eq!(
            parse_display_tty_marker(stream).as_deref(),
            Some("/dev/pts/3")
        );
    }

    #[test]
    fn parse_returns_none_without_the_marker() {
        // Ordinary output (no marker yet) yields nothing — the capture stays pending
        // rather than guessing a tty off unrelated bytes.
        assert_eq!(
            parse_display_tty_marker(b"/dev/pts/0 just some output\r\n"),
            None
        );
    }

    #[test]
    fn parse_finds_a_whole_marker_anywhere_in_the_buffer() {
        // The pump accumulates until it sees one, so a marker that arrives across
        // reads is whole by the time it is found.
        let stream = b"prompt$ \x1b]XMUX-DISPLAY-TTY:/dev/pts/12\x07rest";
        assert_eq!(
            parse_display_tty_marker(stream).as_deref(),
            Some("/dev/pts/12")
        );
    }

    // --- psmux .port liveness (PathStat death) ---

    #[test]
    fn psmux_port_path_is_under_registry_dir() {
        let p = psmux_port_path("editor");
        assert!(
            p.ends_with(std::path::Path::new(".psmux").join("editor.port")),
            "{p:?}"
        );
    }

    #[test]
    fn psmux_session_is_live_reflects_port_file() {
        let name = format!("xmux-live-{}", std::process::id());
        let path = psmux_port_path(&name);
        let _ = std::fs::create_dir_all(path.parent().unwrap());
        std::fs::write(&path, b"40000").unwrap();
        assert!(psmux_session_is_live(&name));
        std::fs::remove_file(&path).unwrap();
        assert!(!psmux_session_is_live(&name));
    }
}
