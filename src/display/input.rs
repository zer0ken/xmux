//! Terminal-focus input handling. When the terminal view has focus every byte is
//! forwarded raw to the session's active pane (so a real program - vim, a pager -
//! sees exact input), EXCEPT a prefix (default `C-g`) followed by a command key,
//! which is intercepted: `prefix Left|Up|Tab` returns focus to the nav,
//! `prefix Right|Down` keeps focus on the (already-focused) terminal view (an arrow
//! points at the view it focuses), `prefix q` quits, `prefix ?` toggles
//! the keys help, `prefix h`/`l` and `prefix Ctrl+←/→` resize the nav, `prefix t`
//! toggles auto-hide-nav mode, `prefix n`/`R`/`r` run the nav actions
//! (new / rename / re-scan) on the displayed session, `prefix x` kills the ACTIVE pane
//! of the displayed session (tmux `prefix x` parity - distinct from nav focus, where
//! `prefix x` kills the selected node), and a doubled
//! prefix sends one literal prefix byte to the pane. Apart from `prefix x`, the command set matches
//! nav focus, so those commands behave identically regardless of which view holds
//! focus. The prefix is a C0
//! control byte, so it cannot collide with a UTF-8 continuation byte or appear mid-CSI;
//! bracketed paste is respected so a prefix pasted as data is never intercepted.
use crate::display::dispatch::Action;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub struct TermInput {
    prefix: u8,
    armed: bool,
    in_paste: bool,
    paste_scan: Vec<u8>,
}

const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

impl TermInput {
    pub fn new(prefix: u8) -> Self {
        Self {
            prefix,
            armed: false,
            in_paste: false,
            paste_scan: Vec::new(),
        }
    }

    /// Whether a prefix is armed awaiting its command key. The app checks this so
    /// its resize-repeat intercept does not skip a read while a prefix sequence is mid-flight
    /// (which would leave the prefix armed and mis-read the following key as a command).
    pub fn is_armed(&self) -> bool {
        self.armed
    }

    /// Drops a pending prefix. A prefix waits for the NEXT input, and a mouse action
    /// is input - but mouse bytes are scanned out of the stream before `feed` ever
    /// sees them, so the mouse path says so here instead of leaving the chord
    /// half-open.
    pub fn disarm(&mut self) {
        self.armed = false;
    }

    fn track_paste(&mut self, byte: u8) {
        self.paste_scan.push(byte);
        if self.paste_scan.len() > PASTE_START.len().max(PASTE_END.len()) {
            self.paste_scan.remove(0);
        }
        if !self.in_paste && self.paste_scan.ends_with(PASTE_START) {
            self.in_paste = true;
        } else if self.in_paste && self.paste_scan.ends_with(PASTE_END) {
            self.in_paste = false;
        }
    }

    /// Processes one stdin read. Forwarded bytes are coalesced; an intercepted
    /// prefix sequence produces FocusNav/Quit/help/resize/… actions. The command key
    /// after a prefix is resolved at the byte level and consumes ONLY its own
    /// byte(s), so any trailing bytes in the same read resume as normal input.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Action> {
        let mut out = Vec::new();
        let mut fwd: Vec<u8> = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            // A kitty key event (a release, or Windows Terminal's repeat/release
            // hybrid) is resolved BEFORE the command-key logic: a release or a repeat
            // must never be read as a command key, and a held prefix must not disarm.
            if let Some((end, ev)) = crate::display::decode::parse_kitty_seq(bytes, i) {
                i = end;
                self.handle_kitty(&ev, &mut fwd);
                continue;
            }
            if self.armed {
                let b0 = bytes[i];
                if b0 == self.prefix {
                    // A repeated prefix byte on a terminal without kitty event kinds
                    // is indistinguishable from a held key's autorepeat, so it is
                    // swallowed: ready stays put (no flicker) and no literal reaches
                    // the pane. The doubled-prefix literal fires on the kitty path,
                    // where a held key is a Repeat and a second tap is a fresh Press.
                    i += 1;
                    continue;
                }
                // Any key while ready CONSUMES the prefix (even a no-op like focusing
                // the already-focused view): ready clears, the bar hides.
                self.armed = false;
                // prefix ? / h / l keep terminal-view focus (help toggle, nav resize), so the
                // rest of the read still forwards to the pane - flush, emit, continue.
                if b0 == b'?' {
                    if !fwd.is_empty() {
                        out.push(Action::Forward(std::mem::take(&mut fwd)));
                    }
                    out.push(Action::ShowHelp);
                    i += 1;
                    continue;
                }
                if b0 == b'h' || b0 == b'l' {
                    if !fwd.is_empty() {
                        out.push(Action::Forward(std::mem::take(&mut fwd)));
                    }
                    out.push(Action::Width(if b0 == b'l' { 1 } else { -1 }));
                    i += 1;
                    continue;
                }
                // prefix t → toggle auto-hide-nav; keeps terminal-view focus, so the rest of
                // the read still forwards to the pane.
                if b0 == b't' {
                    if !fwd.is_empty() {
                        out.push(Action::Forward(std::mem::take(&mut fwd)));
                    }
                    out.push(Action::ToggleAutoHide);
                    i += 1;
                    continue;
                }
                // prefix n/r and prefix <digit> → the nav actions (new session, re-scan,
                // card jump), so they are reachable from the terminal view too, not only
                // nav focus. Emitted as a NavKey the caller hands to
                // Switcher::handle_key: n opens the new-session input, r kicks a re-scan,
                // a digit opens the jump popup. Focus stays on the terminal view (the modal
                // draws over it and owns the NEXT read), so the rest of THIS read still
                // forwards to the pane, same shape as prefix ?/t above.
                if matches!(b0, b'n' | b'r') || b0.is_ascii_digit() {
                    if !fwd.is_empty() {
                        out.push(Action::Forward(std::mem::take(&mut fwd)));
                    }
                    out.push(Action::NavKey(KeyEvent::new(
                        KeyCode::Char(b0 as char),
                        KeyModifiers::NONE,
                    )));
                    i += 1;
                    continue;
                }
                // prefix Ctrl-arrow (ESC [ 1 ; 5 A/B/C/D) → resize. ←/→ (D/C) the WIDTH,
                // ↑/↓ (A/B) the HEIGHT. Matched before the plain ESC/arrow focus handling
                // below so the Ctrl-arrow is not read as Esc.
                if b0 == 0x1b
                    && bytes[i..].len() >= 6
                    && bytes[i + 1] == b'['
                    && &bytes[i + 2..i + 5] == b"1;5"
                    && matches!(bytes[i + 5], b'A' | b'B' | b'C' | b'D')
                {
                    if !fwd.is_empty() {
                        out.push(Action::Forward(std::mem::take(&mut fwd)));
                    }
                    out.push(match bytes[i + 5] {
                        b'C' => Action::Width(1),
                        b'D' => Action::Width(-1),
                        b'B' => Action::Height(1),
                        _ => Action::Height(-1), // b'A'
                    });
                    i += 6;
                    continue;
                }
                // Tab → leave the terminal for the nav. Focus is switching away, so the
                // remainder of this read belongs to the new focus and is delivered on
                // the next read; flush what was forwarded and stop here.
                if b0 == b'\t' {
                    if !fwd.is_empty() {
                        out.push(Action::Forward(std::mem::take(&mut fwd)));
                    }
                    out.push(Action::FocusNav(bytes[i + 1..].to_vec()));
                    break;
                }
                // An ESC sequence (an arrow) → leave the terminal for the nav; a bare Esc
                // is not a prefix command, so it falls through to the unrecognized-key
                // arm below, which ends the chord and swallows the key.
                if b0 == 0x1b {
                    // Consume the WHOLE arrow (ESC [ A/B/C/D), so its tail isn't replayed
                    // as stray nav input.
                    let arrow = bytes[i..].len() >= 3
                        && bytes[i + 1] == b'['
                        && matches!(bytes[i + 2], b'A' | b'B' | b'C' | b'D');
                    if arrow {
                        // An arrow points AT the view it focuses: the terminal is right of the
                        // nav in Side and below it in Top, so prefix → (C) and prefix ↓ (B) both
                        // name the view that already has focus here. Swallow them and stay; the
                        // rest of the read resumes as mux input. prefix ← (D) and prefix ↑ (A)
                        // name the nav and fall through to the focus switch below.
                        if matches!(bytes[i + 2], b'C' | b'B') {
                            i += 3;
                            continue;
                        }
                        if !fwd.is_empty() {
                            out.push(Action::Forward(std::mem::take(&mut fwd)));
                        }
                        // Hand any bytes AFTER the command to the nav (focus switching).
                        out.push(Action::FocusNav(bytes[i + 3..].to_vec()));
                        break;
                    }
                    // A bare Esc falls through to the unrecognized-key arm below.
                }
                if b0 == b'q' {
                    if !fwd.is_empty() {
                        out.push(Action::Forward(std::mem::take(&mut fwd)));
                    }
                    out.push(Action::Quit);
                    break;
                }
                // Unrecognized single-byte follow-up: command mode swallows just this
                // key; the rest of the read resumes as normal input. Ready is already
                // consumed.
                i += 1;
                continue;
            }

            let b = bytes[i];
            self.track_paste(b);
            if !self.in_paste && b == self.prefix {
                // A prefix byte arms ready (idempotent: a held key's legacy autorepeat
                // is another prefix byte, so it stays armed instead of toggling; a kitty
                // Repeat is already handled in handle_kitty and never re-arms a consumed
                // ready).
                if !fwd.is_empty() {
                    out.push(Action::Forward(std::mem::take(&mut fwd)));
                }
                self.armed = true;
            } else {
                fwd.push(b);
            }
            i += 1;
        }
        if !fwd.is_empty() {
            out.push(Action::Forward(fwd));
        }
        out
    }

    /// Applies one parsed kitty key event to the prefix state and the forward buffer.
    /// A prefix press arms ready; a second prefix press while ready is the
    /// doubled-prefix literal (one prefix byte forwarded, ready cleared); a held
    /// prefix's Repeat never re-arms a consumed ready, so a held key never spams a
    /// literal or flickers; a release is the key-up side of a press and is swallowed,
    /// so it cannot end the chord (a release between two taps would otherwise make
    /// the doubled-prefix literal unreachable). Non-prefix events forward their
    /// legacy bytes (a release forwards nothing, because a legacy program has no
    /// key-up event).
    fn handle_kitty(&mut self, ev: &crate::display::decode::KittyEvent, fwd: &mut Vec<u8>) {
        // A key is the prefix when its code is the configured control byte (kitty
        // presses and repeats, legacy bytes), or when a Ctrl chord reports the base
        // key instead (Windows Terminal sends the letter, e.g. C-g → 'g', on releases
        // and some repeats). The Ctrl check keeps a plain 'g' repeat from being
        // misread as the prefix.
        let is_prefix = ev.code == self.prefix as u32
            || (ev.modifiers & 4 != 0
                && ev.code == crate::display::decode::kitty_prefix_base(self.prefix));
        match ev.kind {
            3 => {
                // release: the key is up. It is the key-up side of the press that
                // armed ready and has no command of its own: swallowing it (rather
                // than cancelling) keeps ready live until the next key, a focus
                // switch, or a mouse action, which is what lets the doubled-prefix
                // literal fire on a following press.
            }
            2 => {
                // repeat: the same key is still down. A held prefix's repeat is
                // swallowed and never re-arms a consumed ready, so the bar stays
                // hidden once consumed and a held key never sends a literal prefix.
                // A non-prefix repeat forwards its legacy bytes unless a command is
                // mid-flight (a repeat is not a command).
                if !is_prefix && !self.armed {
                    self.forward_legacy(ev, fwd);
                }
            }
            _ => {
                // press: a prefix press arms ready; a second prefix press while
                // ready is the doubled-prefix command (one literal prefix byte
                // reaches the pane) and clears ready. A non-prefix press mid-command
                // consumes the prefix; otherwise it forwards its legacy bytes.
                if is_prefix {
                    if self.armed {
                        fwd.push(self.prefix);
                        self.armed = false;
                    } else {
                        self.armed = true;
                    }
                } else if self.armed {
                    self.armed = false;
                } else {
                    self.forward_legacy(ev, fwd);
                }
            }
        }
    }

    fn forward_legacy(&mut self, ev: &crate::display::decode::KittyEvent, fwd: &mut Vec<u8>) {
        let legacy = kitty_to_legacy(ev);
        fwd.extend_from_slice(&legacy);
    }
}

/// The legacy bytes to forward for a parsed kitty/hybrid key event. A release
/// forwards nothing (a legacy program has no key-up). A hybrid functional key
/// (letter final) becomes its legacy CSI/SS3 form; a codepoint becomes its text
/// bytes (an alt modifier prefixes ESC).
fn kitty_to_legacy(ev: &crate::display::decode::KittyEvent) -> Vec<u8> {
    if ev.kind == 3 {
        return Vec::new();
    }
    let mut out = Vec::new();
    match ev.final_byte {
        b'A' | b'B' | b'C' | b'D' | b'H' | b'F' => {
            out.extend_from_slice(b"\x1b[");
            if ev.modifiers != 0 {
                out.extend_from_slice(format!("1;{}", ev.modifiers + 1).as_bytes());
            }
            out.push(ev.final_byte);
        }
        b'P' => return b"\x1bOP".to_vec(),
        b'Q' => return b"\x1bOQ".to_vec(),
        b'R' => return b"\x1bOR".to_vec(),
        b'S' => return b"\x1bOS".to_vec(),
        _ => {}
    }
    if !out.is_empty() {
        return out;
    }
    match ev.code {
        0..=31 | 127 => {
            if ev.modifiers & 2 != 0 {
                out.push(0x1b);
            }
            out.push(ev.code as u8);
        }
        c => {
            if ev.modifiers & 2 != 0 {
                out.push(0x1b);
            }
            if let Some(ch) = char::from_u32(c) {
                let mut b = [0u8; 4];
                out.extend_from_slice(ch.encode_utf8(&mut b).as_bytes());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m() -> TermInput {
        TermInput::new(0x07)
    }
    fn fwd(a: &[Action]) -> Vec<u8> {
        a.iter()
            .flat_map(|x| match x {
                Action::Forward(b) => b.clone(),
                _ => vec![],
            })
            .collect()
    }

    #[test]
    fn plain_bytes_forward() {
        let mut t = m();
        assert_eq!(fwd(&t.feed(b"ab")), b"ab");
    }

    #[test]
    fn held_prefix_does_not_toggle_or_forward_literal() {
        let mut t = m();
        // A tap: the press arms, and the release is the key-up side of that press,
        // not a cancel, so ready stays live until the next key.
        t.feed(&[0x07]); // press
        assert!(t.is_armed());
        t.feed(b"\x1b[7;5:3u"); // release is swallowed
        assert!(t.is_armed(), "the release must not cancel ready");
        // Now hold the prefix on a legacy terminal: a press arms, repeated prefix
        // bytes are swallowed (no literal reaches the pane, no flicker), and the
        // release changes nothing.
        let mut t2 = m();
        t2.feed(&[0x07]);
        assert!(t2.is_armed());
        assert_eq!(
            fwd(&t2.feed(&[0x07])),
            Vec::<u8>::new(),
            "a legacy repeat forwards nothing"
        );
        assert!(t2.is_armed(), "a hold-repeat must not disarm");
        assert_eq!(
            fwd(&t2.feed(&[0x07])),
            Vec::<u8>::new(),
            "more repeats stay swallowed"
        );
        assert!(t2.is_armed());
        assert_eq!(
            t2.feed(b"\x1b[7;5:3u"),
            Vec::<Action>::new(),
            "release is swallowed"
        );
        assert!(t2.is_armed(), "the release does not cancel ready");
    }

    #[test]
    fn kitty_releases_are_never_forwarded() {
        let mut t = m();
        // 'a' press (legacy), then its release as CSI u: only the press forwards.
        assert_eq!(fwd(&t.feed(b"a\x1b[97;1:3u")), b"a");
        // A WT hybrid release of Up is swallowed too.
        assert_eq!(t.feed(b"\x1b[1;1:3A"), Vec::<Action>::new());
    }

    #[test]
    fn a_windows_terminal_release_is_swallowed() {
        // WT's getKittyBaseKey strips Ctrl and reports the letter for the release
        // (C-g → CSI 103;5:3u, code 103 = 'g'), not the control byte. It is the
        // key-up side of the press and must not end the chord: ready stays live so
        // the bar stays up and the doubled-prefix literal stays reachable.
        let mut t = m();
        t.feed(&[0x07]); // press
        assert!(t.is_armed());
        assert_eq!(
            t.feed(b"\x1b[103;5:3u"),
            Vec::<Action>::new(),
            "the release is swallowed"
        );
        assert!(
            t.is_armed(),
            "the WT base-key release does not cancel ready"
        );
    }

    #[test]
    fn kitty_hybrid_repeat_forwarded_as_legacy() {
        let mut t = m();
        // Up repeat (hybrid) forwards as the legacy CSI A.
        assert_eq!(fwd(&t.feed(b"\x1b[1;1:2A")), b"\x1b[A");
    }

    #[test]
    fn a_command_consumes_ready_and_a_repeat_never_rearms_it() {
        // A command key CONSUMES the prefix: ready clears (the bar hides). A held
        // prefix's kitty Repeat (event kind 2) never re-arms the consumed ready, so
        // the bar stays hidden while the key is held. Resize continuation after the
        // first arrow is the RUNTIME repeat window (bare Ctrl-arrows), not a re-armed
        // prefix, so a plain `h` after consumption is ordinary input again.
        let mut t = m();
        t.feed(&[0x07]); // prefix press
        assert!(t.is_armed());
        assert_eq!(t.feed(b"h"), vec![Action::Width(-1)], "the arrow resizes");
        assert!(!t.is_armed(), "a key while ready consumes ready");
        // The held prefix's repeat (kitty kind 2) must NOT re-arm ready.
        assert_eq!(t.feed(b"\x1b[7;5:2u"), Vec::<Action>::new());
        assert!(!t.is_armed(), "a repeat never re-arms a consumed ready");
        assert_eq!(
            fwd(&t.feed(b"h")),
            b"h",
            "after consumption a plain key is ordinary input, not a command"
        );
        t.feed(b"\x1b[7;5:3u"); // the release is a no-op
        assert!(!t.is_armed(), "consumed ready stays consumed");
    }

    #[test]
    fn prefix_then_tab_focuses_nav() {
        let mut t = m();
        assert!(t.feed(&[0x07]).is_empty(), "prefix alone is held");
        assert_eq!(t.feed(b"\t"), vec![Action::FocusNav(vec![])]);
    }

    #[test]
    fn prefix_then_left_or_up_focuses_nav_esc_is_not_a_command() {
        // Left/Up each name the nav (left of the terminal in Side, above it in Top),
        // consumed whole so the replay tail is empty. A bare Esc after the prefix is
        // NOT a prefix command: it is treated like any unrecognized key, ending the
        // chord and swallowing the key (no focus switch, nothing reaches the pane).
        for seq in [&b"\x1b[D"[..], &b"\x1b[A"[..]] {
            let mut t = m();
            t.feed(&[0x07]);
            assert_eq!(
                t.feed(seq),
                vec![Action::FocusNav(vec![])],
                "seq {seq:?} → nav"
            );
        }
        let mut t = m();
        t.feed(&[0x07]);
        assert_eq!(
            t.feed(b"\x1b"),
            Vec::<Action>::new(),
            "prefix Esc is not a command: the chord ends, the key is swallowed"
        );
    }

    #[test]
    fn prefix_then_right_or_down_stays_in_terminal_and_consumes() {
        // prefix → and prefix ↓ both name the terminal view, which already has focus:
        // swallowed, no FocusNav, and any trailing bytes resume as forwarded input. The
        // no-op still CONSUMES the prefix, so the bar hides and the next key is bare.
        for seq in [&b"\x1b[C"[..], &b"\x1b[B"[..]] {
            let mut t = m();
            t.feed(&[0x07]);
            assert!(
                t.feed(seq).is_empty(),
                "seq {seq:?} produces no action (stays in mux)"
            );
            assert!(
                !t.is_armed(),
                "seq {seq:?} is a no-op but still consumes the prefix"
            );
        }
        let mut t2 = m();
        t2.feed(&[0x07]);
        assert_eq!(
            fwd(&t2.feed(b"\x1b[Cabc")),
            b"abc",
            "trailing input after prefix → forwards"
        );
        let mut t3 = m();
        t3.feed(&[0x07]);
        assert_eq!(
            fwd(&t3.feed(b"\x1b[Babc")),
            b"abc",
            "trailing input after prefix ↓ forwards too"
        );
    }

    #[test]
    fn prefix_then_arrow_in_one_read_consumes_the_whole_arrow() {
        // `C-g Left` in one read leaves to nav with NO replay tail (the `[D` of the
        // arrow must not leak as stray nav input).
        let mut t = m();
        assert_eq!(t.feed(b"\x07\x1b[D"), vec![Action::FocusNav(vec![])]);
        // With trailing input after the arrow, only that trailing input is replayed.
        let mut t2 = m();
        assert_eq!(
            t2.feed(b"\x07\x1b[Dabc"),
            vec![Action::FocusNav(b"abc".to_vec())]
        );
    }

    #[test]
    fn prefix_then_tab_then_trailing_goes_to_nav() {
        // `C-g Tab abc` in one read: focus leaves to the nav carrying `abc` (no
        // byte loss - the trailing input belongs to the new focus).
        let mut t = m();
        assert_eq!(
            t.feed(b"\x07\tabc"),
            vec![Action::FocusNav(b"abc".to_vec())]
        );
    }

    #[test]
    fn prefix_then_q_quits() {
        let mut t = m();
        t.feed(&[0x07]);
        assert_eq!(t.feed(b"q"), vec![Action::Quit]);
    }

    #[test]
    fn prefix_then_question_toggles_help() {
        let mut t = m();
        t.feed(&[0x07]);
        assert_eq!(t.feed(b"?"), vec![Action::ShowHelp]);
    }

    #[test]
    fn prefix_then_t_toggles_auto_hide() {
        // Keeps terminal-view focus, so trailing bytes in the same read still forward.
        let mut t = m();
        assert_eq!(
            t.feed(b"\x07tabc"),
            vec![Action::ToggleAutoHide, Action::Forward(b"abc".to_vec())]
        );
    }

    #[test]
    fn prefix_then_nav_action_emits_nav_key() {
        // prefix n/R/r each emit a NavKey the caller routes to Switcher::handle_key,
        // so the nav actions work from terminal focus too. (prefix x is separate - it
        // kills the active pane; see prefix_then_x_kills_active_pane.)
        for (b, c) in [(b'n', 'n'), (b'r', 'r')] {
            let mut t = m();
            t.feed(&[0x07]);
            assert_eq!(
                t.feed(&[b]),
                vec![Action::NavKey(KeyEvent::new(
                    KeyCode::Char(c),
                    KeyModifiers::NONE
                ))],
                "prefix {c} emits a nav key"
            );
        }
    }

    #[test]
    fn prefix_nav_action_keeps_focus_and_forwards_rest() {
        // Like prefix ?/t: the action keeps terminal-view focus, so trailing bytes in the
        // same read still forward to the pane (the opened modal owns the NEXT read).
        let mut t = m();
        assert_eq!(
            t.feed(b"\x07nabc"),
            vec![
                Action::NavKey(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE)),
                Action::Forward(b"abc".to_vec()),
            ]
        );
    }

    #[test]
    fn prefix_then_h_or_l_resizes() {
        let mut t = m();
        t.feed(&[0x07]);
        assert_eq!(t.feed(b"h"), vec![Action::Width(-1)], "h narrows");
        let mut t2 = m();
        t2.feed(&[0x07]);
        assert_eq!(t2.feed(b"l"), vec![Action::Width(1)], "l widens");
    }

    #[test]
    fn prefix_then_ctrl_arrow_resizes() {
        let mut t = m();
        t.feed(&[0x07]);
        assert_eq!(
            t.feed(b"\x1b[1;5C"),
            vec![Action::Width(1)],
            "Ctrl-Right widens"
        );
        let mut t2 = m();
        t2.feed(&[0x07]);
        assert_eq!(
            t2.feed(b"\x1b[1;5D"),
            vec![Action::Width(-1)],
            "Ctrl-Left narrows"
        );
        // Ctrl+↑/↓ resize the HEIGHT (vertical axis, Top layout); ↓ grows.
        let mut t3 = m();
        t3.feed(&[0x07]);
        assert_eq!(
            t3.feed(b"\x1b[1;5B"),
            vec![Action::Height(1)],
            "Ctrl-Down grows height"
        );
        let mut t4 = m();
        t4.feed(&[0x07]);
        assert_eq!(
            t4.feed(b"\x1b[1;5A"),
            vec![Action::Height(-1)],
            "Ctrl-Up shrinks height"
        );
    }

    #[test]
    fn prefix_command_keeps_focus_and_forwards_rest() {
        // help/resize keep terminal-view focus, so trailing bytes in the same read still forward.
        let mut t = m();
        assert_eq!(
            t.feed(b"\x07?abc"),
            vec![Action::ShowHelp, Action::Forward(b"abc".to_vec())]
        );
        // Bytes before the prefix flush first, preserving order around the command.
        let mut t2 = m();
        assert_eq!(
            t2.feed(b"ab\x07lcd"),
            vec![
                Action::Forward(b"ab".to_vec()),
                Action::Width(1),
                Action::Forward(b"cd".to_vec()),
            ]
        );
    }

    #[test]
    fn a_second_prefix_press_sends_a_literal_prefix_byte() {
        // The release does not cancel ready, so a second prefix press while ready is
        // the doubled-prefix command: one literal prefix byte reaches the pane and
        // ready clears. A held key sends its Repeat, never a fresh Press, so holding
        // the prefix cannot fire this.
        let mut t = m();
        t.feed(&[0x07]); // press (legacy arm)
        t.feed(b"\x1b[7;5:3u"); // release is swallowed
        assert_eq!(
            fwd(&t.feed(b"\x1b[7;5:1u")),
            vec![0x07],
            "the doubled-prefix forwards one literal byte"
        );
        assert!(!t.is_armed(), "the doubled-prefix consumes ready");
        // A held prefix's Repeat never sends a literal, even while ready.
        let mut t2 = m();
        t2.feed(&[0x07]); // press
        assert_eq!(t2.feed(b"\x1b[7;5:2u"), Vec::<Action>::new());
        assert_eq!(
            fwd(&t2.feed(b"\x1b[7;5:2u")),
            Vec::<u8>::new(),
            "a held prefix's repeat forwards no literal"
        );
        assert!(t2.is_armed(), "ready stays put while held");
    }

    #[test]
    fn a_configured_prefix_uses_its_own_byte() {
        // The prefix is configurable (`[ui] prefix`), default C-g. A non-default
        // prefix (C-b = 0x02) must arm, stay live across a release, send its own
        // byte on the doubled-prefix, and resolve its commands like the default.
        let mut t = TermInput::new(0x02);
        t.feed(&[0x02]); // press arms
        assert!(t.is_armed());
        t.feed(b"\x1b[2;5:3u"); // release is a no-op
        assert!(t.is_armed());
        assert_eq!(
            fwd(&t.feed(b"\x1b[2;5:1u")),
            vec![0x02],
            "the doubled-prefix forwards the configured byte"
        );
        assert!(!t.is_armed(), "the doubled-prefix consumes ready");
        // The default prefix's byte is ordinary input to a differently-configured app.
        let mut t2 = TermInput::new(0x02);
        assert_eq!(fwd(&t2.feed(&[0x07])), vec![0x07], "0x07 forwards");
        t2.feed(&[0x02]);
        assert!(t2.is_armed());
        assert_eq!(
            t2.feed(b"\t"),
            vec![Action::FocusNav(vec![])],
            "the configured prefix command resolves"
        );
    }

    #[test]
    fn prefix_then_other_key_is_swallowed() {
        let mut t = m();
        t.feed(&[0x07]);
        // `z` is not a command key (unlike q/?/h/l/t/n/R/x/r), so it is swallowed.
        let out = t.feed(b"z");
        assert!(
            out.is_empty(),
            "unrecognised follow-up is swallowed: {out:?}"
        );
    }

    #[test]
    fn prefix_then_release_then_prefix_then_trailing_forwards_rest() {
        // `C-g` (release) `C-g abc`: the release is a no-op, so on a legacy terminal
        // the second prefix byte is swallowed as a held key's repeat, and `abc`'s
        // leading byte is then consumed as its command key (unrecognized), so only
        // the trailing `bc` forwards.
        let mut t = m();
        assert_eq!(
            fwd(&t.feed(b"\x07\x1b[7;5:3u\x07abc")),
            b"bc",
            "a repeated prefix byte swallows; the next key is its command key"
        );
        let mut t2 = m();
        assert_eq!(
            fwd(&t2.feed(b"\x07\x07abc")),
            b"bc",
            "a hold-repeat forwards nothing; the key after it is the (still-armed)\
             command key and is swallowed, so only the trailing input forwards"
        );
    }

    #[test]
    fn prefix_then_unknown_then_trailing_forwards_rest() {
        // `C-g z abc`: z (not a command key) is swallowed as command mode; abc still forwards.
        let mut t = m();
        assert_eq!(fwd(&t.feed(b"\x07zabc")), b"abc");
    }

    #[test]
    fn bytes_before_prefix_forward_then_intercept() {
        let mut t = m();
        let out = t.feed(b"hi\x07\t");
        assert_eq!(
            out,
            vec![Action::Forward(b"hi".to_vec()), Action::FocusNav(vec![])]
        );
    }

    #[test]
    fn prefix_inside_bracketed_paste_is_literal() {
        let mut t = m();
        for b in b"\x1b[200~" {
            let _ = t.feed(&[*b]);
        }
        // a 0x07 inside the paste forwards literally, never arms
        assert_eq!(fwd(&t.feed(&[0x07])), vec![0x07]);
        for b in b"\x1b[201~" {
            let _ = t.feed(&[*b]);
        }
        // after the paste the prefix arms again
        assert!(t.feed(&[0x07]).is_empty());
        assert_eq!(t.feed(b"\t"), vec![Action::FocusNav(vec![])]);
    }
}
