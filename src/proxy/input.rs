//! Terminal-focus input handling. When the terminal pane has focus every byte is
//! forwarded raw to the session's active pane (so a real program — vim, a pager —
//! sees exact input), EXCEPT a prefix (default `C-g`) followed by a command key,
//! which is intercepted to leave the terminal: `prefix Left|Right|Tab|Esc` returns
//! focus to the tree, `prefix q` quits, and a doubled prefix sends one literal
//! prefix byte. The prefix is a C0 control byte, so it cannot collide with a UTF-8
//! continuation byte or appear mid-CSI; bracketed paste is respected so a prefix
//! pasted as data is never intercepted.
use super::decode::KeyDecoder;
use ratatui::crossterm::event::KeyCode;

#[derive(Debug, PartialEq)]
pub enum TermAction {
    /// Raw bytes to forward to the focused session's active pane.
    Forward(Vec<u8>),
    /// `prefix` then Left/Right/Tab/Esc — move focus back to the tree.
    FocusTree,
    /// `prefix` then `q` — quit the cockpit.
    Quit,
}

pub struct TermInput {
    prefix: u8,
    /// Decodes the one command key that follows the prefix (so multi-byte arrows
    /// resolve correctly); reset after each resolution.
    decoder: KeyDecoder,
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
            decoder: KeyDecoder::new(),
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
    /// prefix sequence produces FocusTree/Quit (or a literal prefix byte).
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<TermAction> {
        let mut out = Vec::new();
        let mut fwd: Vec<u8> = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            if self.armed {
                // Resolve exactly ONE command key from the remaining bytes (fed as
                // a slice so a multi-byte arrow resolves whole, not as a stray Esc).
                // The command consumes the rest of this read (we break below).
                let keys = self.decoder.feed(&bytes[i..]);
                let Some(k) = keys.into_iter().next() else {
                    break; // incomplete — stay armed, resolve on the next read
                };
                self.armed = false;
                self.decoder = KeyDecoder::new();
                match k.code {
                    KeyCode::Left | KeyCode::Right | KeyCode::Esc => {
                        if !fwd.is_empty() {
                            out.push(TermAction::Forward(std::mem::take(&mut fwd)));
                        }
                        out.push(TermAction::FocusTree);
                    }
                    // Tab decodes as a literal HT char (the picker decoder has no
                    // dedicated Tab key).
                    KeyCode::Char('\t') => {
                        if !fwd.is_empty() {
                            out.push(TermAction::Forward(std::mem::take(&mut fwd)));
                        }
                        out.push(TermAction::FocusTree);
                    }
                    KeyCode::Char('q') => {
                        if !fwd.is_empty() {
                            out.push(TermAction::Forward(std::mem::take(&mut fwd)));
                        }
                        out.push(TermAction::Quit);
                    }
                    KeyCode::Char(c) if c as u32 == self.prefix as u32 => {
                        // Doubled prefix → one literal prefix byte to the pane.
                        fwd.push(self.prefix);
                    }
                    _ => {} // any other follow-up: command mode, swallowed
                }
                break;
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
        assert_eq!(t.feed(b"\t"), vec![TermAction::FocusTree]);
    }

    #[test]
    fn prefix_then_left_or_right_or_esc_focuses_tree() {
        for seq in [&b"\x1b[D"[..], &b"\x1b[C"[..], &b"\x1b"[..]] {
            let mut t = m();
            t.feed(&[0x07]);
            assert_eq!(t.feed(seq), vec![TermAction::FocusTree], "seq {seq:?} → tree");
        }
    }

    #[test]
    fn prefix_then_left_in_one_read_focuses_tree() {
        // The whole prefix + arrow arriving in a single read must still resolve as
        // one Left (not a stray Esc that drops the [D bytes to the pane).
        let mut t = m();
        let out = t.feed(b"\x07\x1b[D");
        assert_eq!(out, vec![TermAction::FocusTree]);
    }

    #[test]
    fn prefix_then_q_quits() {
        let mut t = m();
        t.feed(&[0x07]);
        assert_eq!(t.feed(b"q"), vec![TermAction::Quit]);
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
    fn bytes_before_prefix_forward_then_intercept() {
        let mut t = m();
        let out = t.feed(b"hi\x07\t");
        assert_eq!(out, vec![TermAction::Forward(b"hi".to_vec()), TermAction::FocusTree]);
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
        assert_eq!(t.feed(b"\t"), vec![TermAction::FocusTree]);
    }
}
