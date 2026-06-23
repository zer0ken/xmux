//! Terminal-focus input handling. When the terminal pane has focus every byte is
//! forwarded raw to the session's active pane (so a real program — vim, a pager —
//! sees exact input), EXCEPT a prefix (default `C-g`) followed by a command key,
//! which is intercepted: `prefix Left|Tab|Esc` returns focus to the tree, `prefix Right`
//! keeps focus on the (already-focused) mux pane, `prefix q` quits, `prefix ?` toggles
//! the keys help, `prefix h`/`l` and `prefix Ctrl+←/→` resize the tree, and a doubled
//! prefix sends one literal prefix byte. The same command set works in tree focus, so a
//! command behaves identically regardless of which pane holds focus. The prefix is a C0
//! control byte, so it cannot collide with a UTF-8 continuation byte or appear mid-CSI;
//! bracketed paste is respected so a prefix pasted as data is never intercepted.
#[derive(Debug, PartialEq)]
pub enum TermAction {
    /// Raw bytes to forward to the focused session's active pane.
    Forward(Vec<u8>),
    /// `prefix` then Left/Tab/Esc — move focus back to the tree. Carries any
    /// bytes that followed the switch command in the same read: focus has changed,
    /// so the caller must hand them to the tree, not the pane.
    FocusTree(Vec<u8>),
    /// `prefix` then `q` — quit the cockpit.
    Quit,
    /// `prefix ?` — toggle the keys help overlay. Focus stays on the mux pane.
    ShowHelp,
    /// `prefix h`/`l` or `prefix Ctrl+←/→` — adjust the tree width by this signed
    /// delta. Focus stays on the mux pane (the resize is a layout change).
    Width(i32),
}

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
    /// prefix sequence produces FocusTree/Quit (or a literal prefix byte). The
    /// command key after a prefix is resolved at the byte level and consumes ONLY
    /// its own byte(s), so any trailing bytes in the same read resume as normal
    /// input (e.g. `C-g C-g abc` forwards a literal prefix then `abc`).
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<TermAction> {
        let mut out = Vec::new();
        let mut fwd: Vec<u8> = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            if self.armed {
                self.armed = false;
                let b0 = bytes[i];
                if b0 == self.prefix {
                    // Doubled prefix → one literal prefix byte; rest is normal input.
                    fwd.push(self.prefix);
                    i += 1;
                    continue;
                }
                // prefix ? / h / l keep mux focus (help toggle, tree resize), so the
                // rest of the read still forwards to the pane — flush, emit, continue.
                if b0 == b'?' {
                    if !fwd.is_empty() {
                        out.push(TermAction::Forward(std::mem::take(&mut fwd)));
                    }
                    out.push(TermAction::ShowHelp);
                    i += 1;
                    continue;
                }
                if b0 == b'h' || b0 == b'l' {
                    if !fwd.is_empty() {
                        out.push(TermAction::Forward(std::mem::take(&mut fwd)));
                    }
                    out.push(TermAction::Width(if b0 == b'l' { 1 } else { -1 }));
                    i += 1;
                    continue;
                }
                // prefix Ctrl+←/→ (ESC [ 1 ; 5 D/C) → resize. Matched before the plain
                // ESC/arrow focus handling below so the Ctrl-arrow is not read as Esc.
                if b0 == 0x1b
                    && bytes[i..].len() >= 6
                    && bytes[i + 1] == b'['
                    && &bytes[i + 2..i + 5] == b"1;5"
                    && matches!(bytes[i + 5], b'C' | b'D')
                {
                    if !fwd.is_empty() {
                        out.push(TermAction::Forward(std::mem::take(&mut fwd)));
                    }
                    out.push(TermAction::Width(if bytes[i + 5] == b'C' { 1 } else { -1 }));
                    i += 6;
                    continue;
                }
                // Tab, or any ESC sequence (Esc / Left / Right / other arrows) →
                // leave the terminal. Focus is switching away, so the remainder of
                // this read belongs to the new focus and is delivered on the next
                // read; flush what was forwarded and stop here.
                if b0 == b'\t' || b0 == 0x1b {
                    // Consume the WHOLE command key, including a multi-byte arrow
                    // (ESC [ A/B/C/D), so its tail isn't replayed as stray tree input.
                    let cmd_len = if b0 == 0x1b
                        && bytes[i..].len() >= 3
                        && bytes[i + 1] == b'['
                        && matches!(bytes[i + 2], b'A' | b'B' | b'C' | b'D')
                    {
                        3
                    } else {
                        1
                    };
                    // prefix → (Right): focus the right (mux) pane — already focused here,
                    // so swallow it and stay; the rest of the read resumes as mux input.
                    if cmd_len == 3 && bytes[i + 2] == b'C' {
                        i += cmd_len;
                        continue;
                    }
                    if !fwd.is_empty() {
                        out.push(TermAction::Forward(std::mem::take(&mut fwd)));
                    }
                    // Hand any bytes AFTER the command to the tree (focus switching).
                    out.push(TermAction::FocusTree(bytes[i + cmd_len..].to_vec()));
                    break;
                }
                if b0 == b'q' {
                    if !fwd.is_empty() {
                        out.push(TermAction::Forward(std::mem::take(&mut fwd)));
                    }
                    out.push(TermAction::Quit);
                    break;
                }
                // Unrecognized single-byte follow-up: command mode swallows just this
                // key; the rest of the read resumes as normal input.
                i += 1;
                continue;
            }

            let b = bytes[i];
            self.track_paste(b);
            if !self.in_paste && b == self.prefix {
                if !fwd.is_empty() {
                    out.push(TermAction::Forward(std::mem::take(&mut fwd)));
                }
                self.armed = true;
            } else {
                fwd.push(b);
            }
            i += 1;
        }
        if !fwd.is_empty() {
            out.push(TermAction::Forward(fwd));
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
    fn fwd(a: &[TermAction]) -> Vec<u8> {
        a.iter()
            .flat_map(|x| match x {
                TermAction::Forward(b) => b.clone(),
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
    fn prefix_then_tab_focuses_tree() {
        let mut t = m();
        assert!(t.feed(&[0x07]).is_empty(), "prefix alone is held");
        assert_eq!(t.feed(b"\t"), vec![TermAction::FocusTree(vec![])]);
    }

    #[test]
    fn prefix_then_left_or_esc_focuses_tree() {
        // Each command key is consumed whole, so the replay tail is empty (no stray
        // `[D` leaking to the tree).
        for seq in [&b"\x1b[D"[..], &b"\x1b"[..]] {
            let mut t = m();
            t.feed(&[0x07]);
            assert_eq!(t.feed(seq), vec![TermAction::FocusTree(vec![])], "seq {seq:?} → tree");
        }
    }

    #[test]
    fn prefix_then_right_stays_in_terminal() {
        // prefix → focuses the (already-focused) mux pane: swallowed, no FocusTree,
        // and any trailing bytes resume as forwarded input.
        let mut t = m();
        t.feed(&[0x07]);
        assert!(t.feed(b"\x1b[C").is_empty(), "prefix → produces no action (stays in mux)");
        let mut t2 = m();
        t2.feed(&[0x07]);
        assert_eq!(fwd(&t2.feed(b"\x1b[Cabc")), b"abc", "trailing input after prefix → forwards");
    }

    #[test]
    fn prefix_then_arrow_in_one_read_consumes_the_whole_arrow() {
        // `C-g Left` in one read leaves to tree with NO replay tail (the `[D` of the
        // arrow must not leak as stray tree input).
        let mut t = m();
        assert_eq!(t.feed(b"\x07\x1b[D"), vec![TermAction::FocusTree(vec![])]);
        // With trailing input after the arrow, only that trailing input is replayed.
        let mut t2 = m();
        assert_eq!(t2.feed(b"\x07\x1b[Dabc"), vec![TermAction::FocusTree(b"abc".to_vec())]);
    }

    #[test]
    fn prefix_then_tab_then_trailing_goes_to_tree() {
        // `C-g Tab abc` in one read: focus leaves to the tree carrying `abc` (no
        // byte loss — the trailing input belongs to the new focus).
        let mut t = m();
        assert_eq!(t.feed(b"\x07\tabc"), vec![TermAction::FocusTree(b"abc".to_vec())]);
    }

    #[test]
    fn prefix_then_q_quits() {
        let mut t = m();
        t.feed(&[0x07]);
        assert_eq!(t.feed(b"q"), vec![TermAction::Quit]);
    }

    #[test]
    fn prefix_then_question_toggles_help() {
        let mut t = m();
        t.feed(&[0x07]);
        assert_eq!(t.feed(b"?"), vec![TermAction::ShowHelp]);
    }

    #[test]
    fn prefix_then_h_or_l_resizes() {
        let mut t = m();
        t.feed(&[0x07]);
        assert_eq!(t.feed(b"h"), vec![TermAction::Width(-1)], "h narrows");
        let mut t2 = m();
        t2.feed(&[0x07]);
        assert_eq!(t2.feed(b"l"), vec![TermAction::Width(1)], "l widens");
    }

    #[test]
    fn prefix_then_ctrl_arrow_resizes() {
        let mut t = m();
        t.feed(&[0x07]);
        assert_eq!(t.feed(b"\x1b[1;5C"), vec![TermAction::Width(1)], "Ctrl-Right widens");
        let mut t2 = m();
        t2.feed(&[0x07]);
        assert_eq!(t2.feed(b"\x1b[1;5D"), vec![TermAction::Width(-1)], "Ctrl-Left narrows");
    }

    #[test]
    fn prefix_command_keeps_focus_and_forwards_rest() {
        // help/resize keep mux focus, so trailing bytes in the same read still forward.
        let mut t = m();
        assert_eq!(
            t.feed(b"\x07?abc"),
            vec![TermAction::ShowHelp, TermAction::Forward(b"abc".to_vec())]
        );
        // Bytes before the prefix flush first, preserving order around the command.
        let mut t2 = m();
        assert_eq!(
            t2.feed(b"ab\x07lcd"),
            vec![
                TermAction::Forward(b"ab".to_vec()),
                TermAction::Width(1),
                TermAction::Forward(b"cd".to_vec()),
            ]
        );
    }

    #[test]
    fn double_prefix_sends_one_literal() {
        let mut t = m();
        t.feed(&[0x07]);
        assert_eq!(fwd(&t.feed(&[0x07])), vec![0x07]);
    }

    #[test]
    fn prefix_then_other_key_is_swallowed() {
        let mut t = m();
        t.feed(&[0x07]);
        let out = t.feed(b"x");
        assert!(out.is_empty(), "unrecognised follow-up is swallowed: {out:?}");
    }

    #[test]
    fn double_prefix_then_trailing_forwards_literal_and_rest() {
        // `C-g C-g abc` in one read: a literal prefix byte then the trailing input
        // (no byte loss).
        let mut t = m();
        assert_eq!(fwd(&t.feed(b"\x07\x07abc")), vec![0x07, b'a', b'b', b'c']);
    }

    #[test]
    fn prefix_then_unknown_then_trailing_forwards_rest() {
        // `C-g x abc`: x is swallowed as command mode; abc still forwards.
        let mut t = m();
        assert_eq!(fwd(&t.feed(b"\x07xabc")), b"abc");
    }

    #[test]
    fn bytes_before_prefix_forward_then_intercept() {
        let mut t = m();
        let out = t.feed(b"hi\x07\t");
        assert_eq!(out, vec![TermAction::Forward(b"hi".to_vec()), TermAction::FocusTree(vec![])]);
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
        assert_eq!(t.feed(b"\t"), vec![TermAction::FocusTree(vec![])]);
    }
}
