//! Terminal-focus input handling. When the terminal view has focus every byte is
//! forwarded raw to the session's active pane (so a real program - vim, a pager -
//! sees exact input), EXCEPT a prefix (default `C-g`) followed by a command key,
//! which is intercepted: `prefix Left|Up|Tab` returns focus to the nav,
//! `prefix Right|Down` keeps focus on the (already-focused) terminal view (an arrow
//! pair facing the terminal's side names it - with the nav on the right or below the
//! pair flips), `prefix q` quits, `prefix ?` toggles
//! the keys help, `prefix h`/`l` and `prefix Ctrl+←/→` resize the nav width,
//! `prefix Ctrl+↑/↓` the nav height, `prefix t`
//! toggles auto-hide-nav mode, and `prefix n`/`r`/`/` and `prefix <digit>` run the nav
//! actions (new session / re-scan / filter / card jump) on the displayed session. A doubled
//! prefix sends one literal prefix byte. The command set matches
//! nav focus, so those commands behave identically regardless of which view holds
//! focus. The prefix is a C0
//! control byte, so it cannot collide with a UTF-8 continuation byte or appear mid-CSI,
//! and a paste never reaches this path: pastes are taken out of the stream before it.
use crate::display::dispatch::Action;
use crate::model::keys::{prefix_command, Chord, KeyCommand};
use crate::model::NavPosition;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub struct TermInput {
    prefix: u8,
    armed: bool,
}

impl TermInput {
    pub fn new(prefix: u8) -> Self {
        Self {
            prefix,
            armed: false,
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
                // Any key while ready CONSUMES the prefix (even a no-op like focusing
                // the already-focused view): ready clears, the bar hides. What the key
                // runs is read from the one key table, the same lookup the nav path makes.
                self.armed = false;
                let (chord, len) = Chord::from_bytes(&bytes[i..], self.prefix);
                let command = chord.and_then(|c| prefix_command(c, nav_position));
                let Some(command) = command else {
                    // An unrecognized follow-up: the chord swallows just this key and the
                    // rest of the read resumes as normal input. A bare Esc lands here.
                    i += len;
                    continue;
                };
                // A command that keeps terminal focus emits its action and the rest of the
                // read still forwards to the pane.
                let keep = match command {
                    KeyCommand::Help => Some(Action::ShowHelp),
                    KeyCommand::History => Some(Action::ShowHistory),
                    KeyCommand::Check => Some(Action::ShowCheck),
                    KeyCommand::Palette => Some(Action::ShowPalette),
                    KeyCommand::Width(d) => Some(Action::Width(d)),
                    KeyCommand::Height(d) => Some(Action::Height(d)),
                    KeyCommand::AutoHide => Some(Action::ToggleAutoHide),
                    KeyCommand::Collapse => Some(Action::ToggleCollapse),
                    KeyCommand::Position => Some(Action::CycleNavPosition),
                    // The nav actions (new session, both re-scans, filter, card jump)
                    // reach the nav executor as the key itself. Focus stays on the
                    // terminal view: the modal draws over it and owns the NEXT read.
                    KeyCommand::Jump
                    | KeyCommand::Filter
                    | KeyCommand::NewSession
                    | KeyCommand::Rescan
                    | KeyCommand::RescanMachine
                    | KeyCommand::Logout
                    | KeyCommand::HostInfo => Some(Action::NavKey(KeyEvent::new(
                        KeyCode::Char(bytes[i] as char),
                        KeyModifiers::NONE,
                    ))),
                    _ => None,
                };
                if let Some(action) = keep {
                    if !fwd.is_empty() {
                        out.push(Action::Forward(std::mem::take(&mut fwd)));
                    }
                    out.push(action);
                    i += len;
                    continue;
                }
                match command {
                    // A doubled prefix sends one literal prefix byte to the pane and ends
                    // the chord (tmux `send-prefix` parity). A terminal reports no key-up,
                    // so a held prefix's autorepeat is byte-identical to a second tap and
                    // takes this path too: holding the prefix streams literals and blinks
                    // the hint bar. That is the accepted cost of keeping the input path
                    // free of the kitty keyboard protocol: requesting key releases would
                    // bind behaviour to what the terminal, and every enclosing mux, chooses
                    // to pass through.
                    KeyCommand::LiteralPrefix => {
                        fwd.push(self.prefix);
                        i += len;
                    }
                    // The arrow pair naming the terminal names the view that already has
                    // the focus: swallowed, and the rest of the read resumes as mux input.
                    KeyCommand::FocusTerminal => i += len,
                    // Leaving the terminal for the nav: the remainder of this read belongs
                    // to the new focus and is delivered on the next read, so flush what was
                    // forwarded and stop here.
                    KeyCommand::FocusNav | KeyCommand::FocusToggle => {
                        if !fwd.is_empty() {
                            out.push(Action::Forward(std::mem::take(&mut fwd)));
                        }
                        out.push(Action::FocusNav(bytes[i + len..].to_vec()));
                        break;
                    }
                    KeyCommand::Quit => {
                        if !fwd.is_empty() {
                            out.push(Action::Forward(std::mem::take(&mut fwd)));
                        }
                        out.push(Action::Quit);
                        break;
                    }
                    _ => i += len,
                }
                continue;
            }

            let b = bytes[i];
            if b == self.prefix {
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

    const POSITIONS: [NavPosition; 4] = [
        NavPosition::Left,
        NavPosition::Top,
        NavPosition::Right,
        NavPosition::Bottom,
    ];

    /// The bytes a table chord arrives as on the terminal path.
    fn bytes_of(chord: Chord) -> Vec<u8> {
        match chord {
            Chord::Char(c) => vec![c as u8],
            Chord::Digit => b"5".to_vec(),
            Chord::Tab => b"\t".to_vec(),
            Chord::Arrow(a) => vec![0x1b, b'[', a.csi_final()],
            Chord::CtrlArrow(a) => [b"\x1b[1;5".as_slice(), &[a.csi_final()]].concat(),
            Chord::Prefix => vec![0x07],
        }
    }

    /// What terminal focus must do for each table command after `prefix` + `seq`, stated
    /// here rather than read from `feed` so the test checks `feed` against the table.
    fn term_expected(command: KeyCommand, seq: &[u8]) -> Vec<Action> {
        let key = || KeyEvent::new(KeyCode::Char(seq[0] as char), KeyModifiers::NONE);
        match command {
            KeyCommand::Quit => vec![Action::Quit],
            KeyCommand::Help => vec![Action::ShowHelp],
            KeyCommand::History => vec![Action::ShowHistory],
            KeyCommand::Check => vec![Action::ShowCheck],
            KeyCommand::Palette => vec![Action::ShowPalette],
            KeyCommand::AutoHide => vec![Action::ToggleAutoHide],
            KeyCommand::Collapse => vec![Action::ToggleCollapse],
            KeyCommand::Position => vec![Action::CycleNavPosition],
            KeyCommand::Width(d) => vec![Action::Width(d)],
            KeyCommand::Height(d) => vec![Action::Height(d)],
            KeyCommand::Jump
            | KeyCommand::Filter
            | KeyCommand::NewSession
            | KeyCommand::Rescan
            | KeyCommand::RescanMachine => vec![Action::NavKey(key())],
            KeyCommand::Logout => {
                vec![Action::NavKey(key())]
            }
            KeyCommand::HostInfo => vec![Action::NavKey(key())],
            KeyCommand::LiteralPrefix => vec![Action::Forward(vec![0x07])],
            KeyCommand::FocusTerminal => vec![],
            KeyCommand::FocusNav | KeyCommand::FocusToggle => vec![Action::FocusNav(vec![])],
        }
    }

    #[test]
    fn every_prefix_entry_in_the_key_table_dispatches_in_terminal_focus() {
        for position in POSITIONS {
            for entry in crate::model::keys::TABLE.iter().filter(|e| e.prefixed()) {
                for chord in entry.chords(position) {
                    let seq = bytes_of(chord);
                    let command = entry.command_for(chord, position).unwrap();
                    let mut t = m();
                    let got = t.feed(&[&[0x07], seq.as_slice()].concat(), position);
                    assert_eq!(
                        got,
                        term_expected(command, &seq),
                        "{position:?} prefix {chord:?} ({:?})",
                        entry.label
                    );
                    assert!(!t.is_armed(), "the command ends the chord");
                }
            }
        }
    }

    #[test]
    fn every_key_terminal_focus_dispatches_after_the_prefix_is_in_the_key_table() {
        let mut seqs: Vec<Vec<u8>> = (0x00u8..=0x7f).map(|b| vec![b]).collect();
        for fin in b'A'..=b'D' {
            seqs.push(vec![0x1b, b'[', fin]);
            seqs.push([b"\x1b[1;5".as_slice(), &[fin]].concat());
            seqs.push([b"\x1b[1;2".as_slice(), &[fin]].concat());
            seqs.push([b"\x1b[1;3".as_slice(), &[fin]].concat());
        }
        for extra in [b"\x1b[5~".as_slice(), b"\x1b[H", b"\x1bOP", b"\x1b[Z"] {
            seqs.push(extra.to_vec());
        }
        for position in POSITIONS {
            for seq in &seqs {
                let mut t = m();
                let got = t.feed(&[&[0x07], seq.as_slice()].concat(), position);
                let handled = got.iter().any(|a| match a {
                    Action::Forward(b) => b.first() == Some(&0x07),
                    _ => true,
                });
                let (chord, len) = Chord::from_bytes(seq, 0x07);
                let command = chord.and_then(|c| prefix_command(c, position));
                match command {
                    Some(command) => {
                        let want = term_expected(command, seq);
                        assert_eq!(got.first(), want.first(), "{position:?} prefix {seq:?}");
                    }
                    None => {
                        assert!(
                            !handled,
                            "{position:?}: prefix {seq:?} dispatches {got:?} but no table entry binds it"
                        );
                        // An unbound key is swallowed alone; the rest resumes as input.
                        assert_eq!(fwd(&got), seq[len..].to_vec(), "{position:?} {seq:?}");
                    }
                }
            }
        }
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
            vec![Action::ShowCheck],
            "the key opens the machine problems"
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
    fn prefix_then_question_or_m_toggles_help_or_history() {
        let mut t = m();
        t.feed(&[0x07], NavPosition::Left);
        assert_eq!(t.feed(b"?", NavPosition::Left), vec![Action::ShowHelp]);
        assert_eq!(
            t.feed(b"\x07m", NavPosition::Left),
            vec![Action::ShowHistory]
        );
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
        // prefix n, r, / each emit a NavKey the caller routes to Switcher::handle_key,
        // so the nav actions work from terminal focus too.
        for (b, c) in [(b'n', 'n'), (b'r', 'r'), (b'/', '/')] {
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
    fn prefix_then_h_opens_the_check_table_and_s_is_unbound() {
        let mut t = m();
        t.feed(&[0x07], NavPosition::Left);
        assert_eq!(t.feed(b"h", NavPosition::Left), vec![Action::ShowCheck]);
        let mut t2 = m();
        t2.feed(&[0x07], NavPosition::Left);
        assert_eq!(t2.feed(b"s", NavPosition::Left), Vec::<Action>::new());
        let mut t3 = m();
        t3.feed(&[0x07], NavPosition::Left);
        assert_eq!(
            t3.feed(b"R", NavPosition::Left),
            vec![Action::NavKey(KeyEvent::new(
                KeyCode::Char('R'),
                KeyModifiers::NONE
            ))]
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
            t2.feed(b"ab\x07hcd", NavPosition::Left),
            vec![
                Action::Forward(b"ab".to_vec()),
                Action::ShowCheck,
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
        // `y` is not a command key (unlike q/?/h/l/t/z/n/R/x/r), so it is swallowed.
        t.feed(&[0x02], NavPosition::Left);
        let out = t.feed(b"y", NavPosition::Left);
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
}
