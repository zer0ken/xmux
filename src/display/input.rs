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
//! prefix sends one literal prefix byte. Apart from `prefix x`, the command set matches
//! nav focus, so those commands behave identically regardless of which view holds
//! focus. The prefix is a C0
//! control byte, so it cannot collide with a UTF-8 continuation byte or appear mid-CSI;
//! bracketed paste is respected so a prefix pasted as data is never intercepted.
use crate::display::dispatch::Action;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub struct TermInput {
    prefix: u8,
    armed: bool,
    /// True while the prefix key is physically held down (set on the prefix press,
    /// cleared on the prefix release). While it is true a repeated prefix down is a
    /// hold-repeat and is ignored, so the hint bar and the auto-hide nav show stay
    /// put while the key is held. A terminal that reports releases (the kitty
    /// protocol) is what makes the release visible; one that does not leaves the
    /// hold latched until a command key or a mouse action resolves it.
    holding: bool,
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
            holding: false,
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

    /// Whether the prefix key is still physically held down. Together with `is_armed`
    /// this is the `ready || holding` prefix-active signal the hint bar and the
    /// auto-hide nav show read, so a held key keeps them steady.
    pub fn is_holding(&self) -> bool {
        self.holding
    }

    /// Drops a pending prefix and any hold. A prefix waits for the NEXT input, and a
    /// mouse action is input - but mouse bytes are scanned out of the stream before
    /// `feed` ever sees them, so the mouse path says so here instead of leaving the
    /// chord half-open.
    pub fn disarm(&mut self) {
        self.armed = false;
    }

    /// Drops the physical hold (the key-up side of a prefix gesture). The mouse path
    /// calls this alongside `disarm` so a mouse action ends a held chord completely.
    pub fn drop_hold(&mut self) {
        self.holding = false;
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
    /// prefix sequence produces FocusNav/Quit (or a literal prefix byte). The
    /// command key after a prefix is resolved at the byte level and consumes ONLY
    /// its own byte(s), so any trailing bytes in the same read resume as normal
    /// input (e.g. `C-g C-g abc` forwards a literal prefix then `abc`).
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
                    // A repeated prefix down while the key is still held (no release
                    // since its press) is a hold-repeat: ignored, keeping `ready`.
                    // A fresh press (a release landed in between) is a deliberate
                    // second press and sends one literal prefix byte.
                    if self.holding {
                        i += 1;
                        continue;
                    }
                    // Doubled prefix → one literal prefix byte; rest is normal input.
                    self.armed = false;
                    self.holding = false;
                    fwd.push(self.prefix);
                    i += 1;
                    continue;
                }
                // Any key while ready clears ready only; holding persists until the
                // release (the state machine: a key while ready → -ready).
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
                // Tab, or an ESC sequence (an arrow or another CSI/SS3 run) → leave the
                // terminal. Focus is switching away, so the remainder of this read belongs
                // to the new focus and is delivered on the next read; flush what was
                // forwarded and stop here.
                if b0 == b'\t' || b0 == 0x1b {
                    // Consume the WHOLE command key, including a multi-byte arrow
                    // (ESC [ A/B/C/D), so its tail isn't replayed as stray nav input.
                    let arrow = b0 == 0x1b
                        && bytes[i..].len() >= 3
                        && bytes[i + 1] == b'['
                        && matches!(bytes[i + 2], b'A' | b'B' | b'C' | b'D');
                    let cmd_len = if arrow { 3 } else { 1 };
                    // A bare Esc is NOT a prefix command: it has no branch of its own, so
                    // it falls through to the unrecognized-key arm below, which ends the
                    // chord (the prefix releases) and swallows the key.
                    let bare_esc = b0 == 0x1b && !arrow;
                    if !bare_esc {
                        // An arrow points AT the view it focuses: the terminal is right of the
                        // nav in Side and below it in Top, so prefix → (C) and prefix ↓ (B) both
                        // name the view that already has focus here. Swallow them and stay; the
                        // rest of the read resumes as mux input. prefix ← (D) and prefix ↑ (A)
                        // name the nav and fall through to the focus switch below.
                        if arrow && matches!(bytes[i + 2], b'C' | b'B') {
                            i += cmd_len;
                            continue;
                        }
                        if !fwd.is_empty() {
                            out.push(Action::Forward(std::mem::take(&mut fwd)));
                        }
                        // Hand any bytes AFTER the command to the nav (focus switching).
                        out.push(Action::FocusNav(bytes[i + cmd_len..].to_vec()));
                        break;
                    }
                }
                if b0 == b'q' {
                    if !fwd.is_empty() {
                        out.push(Action::Forward(std::mem::take(&mut fwd)));
                    }
                    out.push(Action::Quit);
                    break;
                }
                // Unrecognized single-byte follow-up: command mode swallows just this
                // key; the rest of the read resumes as normal input. A non-action key
                // also ends a held chord: on a terminal that reports no release the
                // hold would otherwise stay latched and keep the status bar up forever.
                self.holding = false;
                i += 1;
                continue;
            }

            let b = bytes[i];
            self.track_paste(b);
            if !self.in_paste && b == self.prefix {
                if !fwd.is_empty() {
                    out.push(Action::Forward(std::mem::take(&mut fwd)));
                }
                self.armed = true;
                self.holding = true;
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
    /// A release clears `holding` (only a kitty terminal reports releases); a held
    /// prefix's repeat keeps `ready`; a deliberate second press sends a literal
    /// prefix. Non-prefix events forward their legacy bytes (a release forwards
    /// nothing, because a legacy program has no key-up event).
    fn handle_kitty(&mut self, ev: &crate::display::decode::KittyEvent, fwd: &mut Vec<u8>) {
        let is_prefix = ev.code == self.prefix as u32;
        // A release may carry the base key instead of the control byte (Windows
        // Terminal reports the letter, e.g. C-g → 103 = 'g'), so the prefix's
        // release is either code. Presses and repeats keep the control byte.
        let is_prefix_release =
            is_prefix || ev.code == crate::display::decode::kitty_prefix_base(self.prefix);
        match ev.kind {
            3 => {
                // release: the key is up. The prefix's release ends the hold.
                if is_prefix_release {
                    self.holding = false;
                }
            }
            2 => {
                // repeat: the same key is still down. A held prefix stays armed (the
                // state machine: a prefix down keeps ready and holding); a non-prefix
                // repeat forwards its legacy bytes unless a command is mid-flight (a
                // repeat is not a command).
                if is_prefix {
                    self.armed = true;
                    self.holding = true;
                } else if !self.armed {
                    self.forward_legacy(ev, fwd);
                }
            }
            _ => {
                // press.
                if is_prefix {
                    if self.armed {
                        if !self.holding {
                            // a fresh press while ready: deliberate doubled prefix
                            fwd.push(self.prefix);
                            self.armed = false;
                            self.holding = false;
                        }
                        // else: the key was already held, this is the repeat
                    } else {
                        self.armed = true;
                        self.holding = true;
                    }
                } else if self.armed {
                    // a non-prefix key mid-command in CSI-u form is not one of the
                    // command keys; swallow it and end the command (ready only).
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
        // Learn the terminal reports releases: press, release, then a command.
        t.feed(&[0x07]); // press
        assert!(t.is_armed() && t.is_holding());
        t.feed(b"\x1b[7;5:3u"); // release clears the hold; ready survives
        assert!(!t.is_holding() && t.is_armed());
        t.feed(b"h"); // a command consumes ready
        assert!(!t.is_armed());
        // Now hold the prefix: a press arms, legacy autorepeat bytes are swallowed
        // (never forwarded as a literal), and the release clears the hold.
        t.feed(&[0x07]);
        assert!(t.is_armed() && t.is_holding());
        assert_eq!(
            fwd(&t.feed(&[0x07])),
            Vec::<u8>::new(),
            "a repeat forwards nothing"
        );
        assert!(t.is_armed(), "a hold-repeat must not disarm");
        assert_eq!(
            fwd(&t.feed(&[0x07])),
            Vec::<u8>::new(),
            "more repeats stay swallowed"
        );
        assert!(t.is_armed());
        assert_eq!(
            t.feed(b"\x1b[7;5:3u"),
            Vec::<Action>::new(),
            "release is swallowed"
        );
        assert!(!t.is_holding());
        assert!(t.is_armed(), "ready survives the release");
        // A deliberate doubled prefix (a fresh press after the release) sends a literal.
        assert_eq!(
            fwd(&t.feed(&[0x07])),
            vec![0x07],
            "a fresh second press sends a literal"
        );
        assert!(!t.is_armed());
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
    fn a_windows_terminal_release_clears_the_hold() {
        // WT's getKittyBaseKey strips Ctrl and reports the letter for the release
        // (C-g → CSI 103;5:3u, code 103 = 'g'), not the control byte. It must still
        // end the prefix's hold, or the status bar never hides.
        let mut t = m();
        t.feed(&[0x07]); // press
        assert!(t.is_armed() && t.is_holding());
        assert_eq!(
            t.feed(b"\x1b[103;5:3u"),
            Vec::<Action>::new(),
            "the release is swallowed"
        );
        assert!(!t.is_holding(), "the WT base-key release ends the hold");
        assert!(t.is_armed(), "ready survives the release");
    }

    #[test]
    fn kitty_hybrid_repeat_forwarded_as_legacy() {
        let mut t = m();
        // Up repeat (hybrid) forwards as the legacy CSI A.
        assert_eq!(fwd(&t.feed(b"\x1b[1;1:2A")), b"\x1b[A");
    }

    #[test]
    fn a_command_clears_ready_only_and_an_autorepeat_rearms_it() {
        // The state machine: a key while ready clears ready only (holding survives
        // until the release), and a prefix autorepeat re-arms ready. So a command key
        // (a resize arrow) can be pressed again and again while the prefix is held.
        let mut t = m();
        t.feed(&[0x07]); // prefix down: +ready +holding
        assert!(t.is_armed() && t.is_holding());
        assert_eq!(t.feed(b"h"), vec![Action::Width(-1)], "the arrow resizes");
        assert!(
            !t.is_armed() && t.is_holding(),
            "a key while ready clears ready only"
        );
        t.feed(&[0x07]); // the held key's autorepeat re-arms ready
        assert!(t.is_armed() && t.is_holding());
        assert_eq!(
            t.feed(b"h"),
            vec![Action::Width(-1)],
            "the next arrow resizes again"
        );
        t.feed(b"\x1b[7;5:3u"); // the release ends the hold
        assert!(!t.is_holding(), "the release clears holding");
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
    fn prefix_then_right_or_down_stays_in_terminal() {
        // prefix → and prefix ↓ both name the terminal view, which already has focus:
        // swallowed, no FocusNav, and any trailing bytes resume as forwarded input.
        for seq in [&b"\x1b[C"[..], &b"\x1b[B"[..]] {
            let mut t = m();
            t.feed(&[0x07]);
            assert!(
                t.feed(seq).is_empty(),
                "seq {seq:?} produces no action (stays in mux)"
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
    fn double_prefix_sends_one_literal() {
        // A fresh second press (the release landed between the two downs) sends a
        // literal; a second down with no release is a hold-repeat and is ignored.
        let mut t = m();
        t.feed(&[0x07]); // press
        t.feed(b"\x1b[7;5:3u"); // release ends the hold
        assert_eq!(fwd(&t.feed(&[0x07])), vec![0x07]);
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
    fn double_prefix_then_trailing_forwards_literal_and_rest() {
        // `C-g` (release) `C-g abc` in one read: a literal prefix byte then the
        // trailing input (no byte loss). A repeated down with no release is a
        // hold-repeat and never forwards a literal.
        let mut t = m();
        assert_eq!(
            fwd(&t.feed(b"\x07\x1b[7;5:3u\x07abc")),
            vec![0x07, b'a', b'b', b'c']
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
