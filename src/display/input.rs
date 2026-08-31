//! Terminal-focus input handling. When the terminal view has focus every byte is
//! forwarded raw to the session's active pane (so a real program - vim, a pager -
//! sees exact input), EXCEPT a prefix (default `C-g`) followed by a command key,
//! which is intercepted: `prefix Left|Up|Tab` returns focus to the nav,
//! `prefix Right|Down` keeps focus on the (already-focused) terminal view (an arrow
//! pair facing the terminal's side names it - with the nav on the right or below the
//! pair flips), `prefix q` quits, `prefix ?` toggles
//! the keys help, `prefix h`/`l` and `prefix Ctrl+←/→` resize the nav width,
//! `prefix Ctrl+↑/↓` the nav height, `prefix t`
//! toggles auto-hide-nav mode, and `prefix n`/`r` and `prefix <digit>` run the nav
//! actions (new session / re-scan / card jump) on the displayed session. A doubled
//! prefix sends one literal prefix byte. The command set matches
//! nav focus, so those commands behave identically regardless of which view holds
//! focus. The prefix is a C0
//! control byte, so it cannot collide with a UTF-8 continuation byte or appear mid-CSI;
//! bracketed paste is respected so a prefix pasted as data is never intercepted.
use crate::display::dispatch::Action;
use crate::ui::switcher::NavPosition;
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
    /// `nav_position` decides which arrow pair names the terminal (the pair facing
    /// the terminal's side, flipped when the nav rides right or below).
    pub fn feed(&mut self, bytes: &[u8], nav_position: NavPosition) -> Vec<Action> {
        let mut out = Vec::new();
        let mut fwd: Vec<u8> = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            if self.armed {
                let b0 = bytes[i];
                if b0 == self.prefix {
                    // A doubled prefix sends one literal prefix byte to the pane and
                    // ends the chord (tmux `send-prefix` parity). A terminal reports no
                    // key-up, so a held prefix's autorepeat is byte-identical to a second
                    // tap and takes this path too: holding the prefix streams literals and
                    // blinks the hint bar. That is the accepted cost of keeping the input
                    // path free of the kitty keyboard protocol.
                    fwd.push(self.prefix);
                    self.armed = false;
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
                // prefix p → cycle the nav position; same shape: applied on the input path,
                // terminal-view focus kept, the rest of the read still forwards.
                if b0 == b'p' {
                    if !fwd.is_empty() {
                        out.push(Action::Forward(std::mem::take(&mut fwd)));
                    }
                    out.push(Action::CycleNavPosition);
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
                        // The arrow PAIR facing the terminal's side names the terminal, so
                        // with the nav on the left or above (the default) →/↓ keep terminal
                        // focus (swallowed; the rest of the read resumes as mux input) and
                        // ←/↑ name the nav and fall through to the focus switch below. With
                        // the nav on the right or below the whole pair flips.
                        let forward = nav_position.forward_arrows_face_terminal();
                        if matches!(bytes[i + 2], b'C' | b'B') == forward {
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
                // A prefix byte arms ready. A second one while already armed is the
                // doubled-prefix literal, handled above, so this only ever arms.
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
        assert_eq!(fwd(&t.feed(b"ab", NavPosition::Left)), b"ab");
    }

    #[test]
    fn a_doubled_prefix_forwards_one_literal_and_ends_the_chord() {
        let mut t = m();
        t.feed(&[0x07], NavPosition::Left);
        assert!(t.is_armed());
        assert_eq!(
            fwd(&t.feed(&[0x07], NavPosition::Left)),
            vec![0x07],
            "a second prefix sends one literal prefix byte to the pane"
        );
        assert!(!t.is_armed(), "the literal ends the chord");
        // A terminal reports no key-up, so a held prefix's autorepeat is byte-identical
        // to repeated taps and streams literals. Accepted: see the doubled-prefix comment
        // in `feed`.
        let mut t2 = m();
        assert_eq!(
            fwd(&t2.feed(&[0x07, 0x07, 0x07, 0x07], NavPosition::Left)),
            vec![0x07, 0x07]
        );
        assert!(!t2.is_armed());
    }

    #[test]
    fn a_command_consumes_ready() {
        // A command key CONSUMES the prefix: ready clears (the bar hides). Resize
        // continuation after the first arrow is the RUNTIME repeat window (bare
        // Ctrl-arrows), not a re-armed prefix, so a plain `h` after consumption is
        // ordinary input again.
        let mut t = m();
        t.feed(&[0x07], NavPosition::Left);
        assert!(t.is_armed());
        assert_eq!(
            t.feed(b"h", NavPosition::Left),
            vec![Action::Width(-1)],
            "the key resizes"
        );
        assert!(!t.is_armed(), "a key while ready consumes ready");
        assert_eq!(
            fwd(&t.feed(b"h", NavPosition::Left)),
            b"h",
            "after consumption a plain key is ordinary input, not a command"
        );
    }

    #[test]
    fn prefix_then_p_cycles_position_and_forwards_the_rest() {
        // Same shape as prefix t: the cycle applies on the input path, terminal-view
        // focus is kept, and the rest of the read still forwards to the pane.
        let mut t = m();
        t.feed(&[0x07], NavPosition::Left);
        assert_eq!(
            t.feed(b"p", NavPosition::Left),
            vec![Action::CycleNavPosition]
        );
        let mut t2 = m();
        t2.feed(&[0x07], NavPosition::Right);
        assert_eq!(
            fwd(&t2.feed(b"pabc", NavPosition::Right)),
            b"abc",
            "trailing input after prefix p forwards"
        );
    }

    #[test]
    fn prefix_then_tab_focuses_nav() {
        let mut t = m();
        assert!(
            t.feed(&[0x07], NavPosition::Left).is_empty(),
            "prefix alone is held"
        );
        assert_eq!(
            t.feed(b"\t", NavPosition::Left),
            vec![Action::FocusNav(vec![])]
        );
    }

    #[test]
    fn prefix_then_left_or_up_focuses_nav_esc_is_not_a_command() {
        // Left/Up each name the nav (left of the terminal in a column, above it in a
        // band), consumed whole so the replay tail is empty. A bare Esc after the prefix
        // is NOT a prefix command: it is treated like any unrecognized key, ending the
        // chord and swallowing the key (no focus switch, nothing reaches the pane).
        for seq in [&b"\x1b[D"[..], &b"\x1b[A"[..]] {
            let mut t = m();
            t.feed(&[0x07], NavPosition::Left);
            assert_eq!(
                t.feed(seq, NavPosition::Left),
                vec![Action::FocusNav(vec![])],
                "seq {seq:?} → nav"
            );
        }
        let mut t = m();
        t.feed(&[0x07], NavPosition::Left);
        assert_eq!(
            t.feed(b"\x1b", NavPosition::Left),
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
            t.feed(&[0x07], NavPosition::Left);
            assert!(
                t.feed(seq, NavPosition::Left).is_empty(),
                "seq {seq:?} produces no action (stays in mux)"
            );
            assert!(
                !t.is_armed(),
                "seq {seq:?} is a no-op but still consumes the prefix"
            );
        }
        let mut t2 = m();
        t2.feed(&[0x07], NavPosition::Left);
        assert_eq!(
            fwd(&t2.feed(b"\x1b[Cabc", NavPosition::Left)),
            b"abc",
            "trailing input after prefix → forwards"
        );
        let mut t3 = m();
        t3.feed(&[0x07], NavPosition::Left);
        assert_eq!(
            fwd(&t3.feed(b"\x1b[Babc", NavPosition::Left)),
            b"abc",
            "trailing input after prefix ↓ forwards too"
        );
    }

    #[test]
    fn the_arrow_pair_flips_with_the_nav_on_the_right() {
        // The pair facing the terminal's side names the terminal. At the default Left
        // placement `C-g →` stays in the terminal (swallowed) and `C-g ←` leaves to the
        // nav; pinned Right the whole pair mirrors.
        let mut left = m();
        left.feed(&[0x07], NavPosition::Left);
        assert!(
            left.feed(b"\x1b[C", NavPosition::Left).is_empty(),
            "→ stays"
        );
        let mut left2 = m();
        left2.feed(&[0x07], NavPosition::Left);
        assert_eq!(
            left2.feed(b"\x1b[D", NavPosition::Left),
            vec![Action::FocusNav(vec![])],
            "← leaves to nav"
        );
        let mut right = m();
        right.feed(&[0x07], NavPosition::Right);
        assert_eq!(
            right.feed(b"\x1b[C", NavPosition::Right),
            vec![Action::FocusNav(vec![])],
            "→ leaves to the nav, which now rides on the right"
        );
        let mut right2 = m();
        right2.feed(&[0x07], NavPosition::Right);
        assert!(
            right2.feed(b"\x1b[D", NavPosition::Right).is_empty(),
            "← stays"
        );
    }

    #[test]
    fn prefix_then_arrow_in_one_read_consumes_the_whole_arrow() {
        // `C-g Left` in one read leaves to nav with NO replay tail (the `[D` of the
        // arrow must not leak as stray nav input).
        let mut t = m();
        assert_eq!(
            t.feed(b"\x07\x1b[D", NavPosition::Left),
            vec![Action::FocusNav(vec![])]
        );
        // With trailing input after the arrow, only that trailing input is replayed.
        let mut t2 = m();
        assert_eq!(
            t2.feed(b"\x07\x1b[Dabc", NavPosition::Left),
            vec![Action::FocusNav(b"abc".to_vec())]
        );
    }

    #[test]
    fn prefix_then_tab_then_trailing_goes_to_nav() {
        // `C-g Tab abc` in one read: focus leaves to the nav carrying `abc` (no
        // byte loss - the trailing input belongs to the new focus).
        let mut t = m();
        assert_eq!(
            t.feed(b"\x07\tabc", NavPosition::Left),
            vec![Action::FocusNav(b"abc".to_vec())]
        );
    }

    #[test]
    fn prefix_then_q_quits() {
        let mut t = m();
        t.feed(&[0x07], NavPosition::Left);
        assert_eq!(t.feed(b"q", NavPosition::Left), vec![Action::Quit]);
    }

    #[test]
    fn prefix_then_question_toggles_help() {
        let mut t = m();
        t.feed(&[0x07], NavPosition::Left);
        assert_eq!(t.feed(b"?", NavPosition::Left), vec![Action::ShowHelp]);
    }

    #[test]
    fn prefix_then_t_toggles_auto_hide() {
        // Keeps terminal-view focus, so trailing bytes in the same read still forward.
        let mut t = m();
        assert_eq!(
            t.feed(b"\x07tabc", NavPosition::Left),
            vec![Action::ToggleAutoHide, Action::Forward(b"abc".to_vec())]
        );
    }

    #[test]
    fn prefix_then_nav_action_emits_nav_key() {
        // prefix n/r each emit a NavKey the caller routes to Switcher::handle_key,
        // so the nav actions work from terminal focus too.
        for (b, c) in [(b'n', 'n'), (b'r', 'r')] {
            let mut t = m();
            t.feed(&[0x07], NavPosition::Left);
            assert_eq!(
                t.feed(&[b], NavPosition::Left),
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
            t.feed(b"\x07nabc", NavPosition::Left),
            vec![
                Action::NavKey(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE)),
                Action::Forward(b"abc".to_vec()),
            ]
        );
    }

    #[test]
    fn prefix_then_h_or_l_resizes() {
        let mut t = m();
        t.feed(&[0x07], NavPosition::Left);
        assert_eq!(
            t.feed(b"h", NavPosition::Left),
            vec![Action::Width(-1)],
            "h narrows"
        );
        let mut t2 = m();
        t2.feed(&[0x07], NavPosition::Left);
        assert_eq!(
            t2.feed(b"l", NavPosition::Left),
            vec![Action::Width(1)],
            "l widens"
        );
    }

    #[test]
    fn prefix_then_ctrl_arrow_resizes() {
        let mut t = m();
        t.feed(&[0x07], NavPosition::Left);
        assert_eq!(
            t.feed(b"\x1b[1;5C", NavPosition::Left),
            vec![Action::Width(1)],
            "Ctrl-Right widens"
        );
        let mut t2 = m();
        t2.feed(&[0x07], NavPosition::Left);
        assert_eq!(
            t2.feed(b"\x1b[1;5D", NavPosition::Left),
            vec![Action::Width(-1)],
            "Ctrl-Left narrows"
        );
        // Ctrl+↑/↓ resize the HEIGHT (vertical axis, band layout); ↓ grows.
        let mut t3 = m();
        t3.feed(&[0x07], NavPosition::Left);
        assert_eq!(
            t3.feed(b"\x1b[1;5B", NavPosition::Left),
            vec![Action::Height(1)],
            "Ctrl-Down grows height"
        );
        let mut t4 = m();
        t4.feed(&[0x07], NavPosition::Left);
        assert_eq!(
            t4.feed(b"\x1b[1;5A", NavPosition::Left),
            vec![Action::Height(-1)],
            "Ctrl-Up shrinks height"
        );
    }

    #[test]
    fn prefix_command_keeps_focus_and_forwards_rest() {
        // help/resize keep terminal-view focus, so trailing bytes in the same read still forward.
        let mut t = m();
        assert_eq!(
            t.feed(b"\x07?abc", NavPosition::Left),
            vec![Action::ShowHelp, Action::Forward(b"abc".to_vec())]
        );
        // Bytes before the prefix flush first, preserving order around the command.
        let mut t2 = m();
        assert_eq!(
            t2.feed(b"ab\x07lcd", NavPosition::Left),
            vec![
                Action::Forward(b"ab".to_vec()),
                Action::Width(1),
                Action::Forward(b"cd".to_vec()),
            ]
        );
    }

    #[test]
    fn a_configured_prefix_uses_its_own_byte() {
        // The prefix is configurable (`[ui] prefix`), default C-g. A non-default
        // prefix (C-b = 0x02) must arm, send its own byte on the doubled-prefix, and
        // resolve its commands like the default.
        let mut t = TermInput::new(0x02);
        t.feed(&[0x02], NavPosition::Left);
        assert!(t.is_armed());
        assert_eq!(
            fwd(&t.feed(&[0x02], NavPosition::Left)),
            vec![0x02],
            "the doubled-prefix forwards the configured byte"
        );
        assert!(!t.is_armed(), "the doubled-prefix consumes ready");
        // The default prefix's byte is ordinary input to a differently-configured app.
        assert_eq!(fwd(&t.feed(&[0x07], NavPosition::Left)), vec![0x07]);
        // `z` is not a command key (unlike q/?/h/l/t/n/R/x/r), so it is swallowed.
        t.feed(&[0x02], NavPosition::Left);
        let out = t.feed(b"z", NavPosition::Left);
        assert!(
            out.is_empty(),
            "unrecognised follow-up is swallowed: {out:?}"
        );
    }

    #[test]
    fn a_doubled_prefix_mid_read_forwards_the_literal_then_the_rest() {
        // `C-g C-g abc`: the second prefix byte forwards one literal and ends the
        // chord, so `abc` is ordinary input again and follows it through.
        let mut t = m();
        assert_eq!(fwd(&t.feed(b"abc", NavPosition::Left)), b"abc");
        assert!(!t.is_armed());
    }

    #[test]
    fn prefix_then_unknown_then_trailing_forwards_rest() {
        // `C-g z abc`: z (not a command key) is swallowed as command mode; abc still forwards.
        let mut t = m();
        assert_eq!(fwd(&t.feed(b"\x07zabc", NavPosition::Left)), b"abc");
    }

    #[test]
    fn bytes_before_prefix_forward_then_intercept() {
        let mut t = m();
        let out = t.feed(b"hi\x07\t", NavPosition::Left);
        assert_eq!(
            out,
            vec![Action::Forward(b"hi".to_vec()), Action::FocusNav(vec![])]
        );
    }

    #[test]
    fn prefix_inside_bracketed_paste_is_literal() {
        let mut t = m();
        for b in b"\x1b[200~" {
            let _ = t.feed(&[*b], NavPosition::Left);
        }
        // a 0x07 inside the paste forwards literally, never arms
        assert_eq!(fwd(&t.feed(&[0x07], NavPosition::Left)), vec![0x07]);
        for b in b"\x1b[201~" {
            let _ = t.feed(&[*b], NavPosition::Left);
        }
        // after the paste the prefix arms again
        assert!(t.feed(&[0x07], NavPosition::Left).is_empty());
        assert_eq!(
            t.feed(b"\t", NavPosition::Left),
            vec![Action::FocusNav(vec![])]
        );
    }
}
