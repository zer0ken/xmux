//! Minimal raw-byte → crossterm KeyEvent decoder for Picker mode. Covers only the
//! keys the switcher uses. A lone ESC that is not followed by `[<final>` is Esc.
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Default)]
pub struct KeyDecoder {
    buf: Vec<u8>,
}

impl KeyDecoder {
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Vec<KeyEvent> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        let mut i = 0;
        while i < self.buf.len() {
            let b = self.buf[i];
            match b {
                0x1b => {
                    // Need at least ESC + `[` to start a CSI.
                    if i + 1 < self.buf.len() && self.buf[i + 1] == b'[' {
                        // Scan for the CSI final byte (0x40..=0x7e) after the params/intermediates.
                        let seq_start = i + 2; // first byte after ESC [
                        let mut j = seq_start;
                        while j < self.buf.len() && !(0x40..=0x7eu8).contains(&self.buf[j]) {
                            j += 1;
                        }
                        if j >= self.buf.len() {
                            // No final byte yet — keep the whole tail buffered.
                            break;
                        }
                        // j now points at the final byte.
                        let final_byte = self.buf[j];
                        let csi_len = j + 1 - i; // total bytes: ESC [ params... final
                                                 // Arrows, bare (`ESC[A`) or with a modifier (`ESC[1;5A` =
                                                 // Ctrl-Up): the params between `[` and the final byte carry the
                                                 // modifier code in their 2nd `;`-field.
                        let code = match final_byte {
                            b'A' => Some(KeyCode::Up),
                            b'B' => Some(KeyCode::Down),
                            b'C' => Some(KeyCode::Right),
                            b'D' => Some(KeyCode::Left),
                            _ => None,
                        };
                        if let Some(c) = code {
                            let mods = csi_modifiers(&self.buf[seq_start..j]);
                            out.push(KeyEvent::new(c, mods));
                            i += csi_len;
                            continue;
                        }
                        // Any other complete CSI — consume silently (no Esc spurion).
                        i += csi_len;
                        continue;
                    }
                    // Lone ESC (no following byte) or ESC followed by non-`[`: emit Esc.
                    out.push(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
                    i += 1;
                }
                b'\r' | b'\n' => {
                    out.push(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
                    i += 1;
                }
                0x7f | 0x08 => {
                    out.push(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
                    i += 1;
                }
                _ if b < 0x80 => {
                    out.push(KeyEvent::new(KeyCode::Char(b as char), KeyModifiers::NONE));
                    i += 1;
                }
                _ => {
                    // UTF-8 multibyte: find the char length, decode if complete.
                    let len = utf8_len(b);
                    if i + len > self.buf.len() {
                        break;
                    } // incomplete, buffer it
                    if let Ok(s) = std::str::from_utf8(&self.buf[i..i + len]) {
                        if let Some(c) = s.chars().next() {
                            out.push(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
                        }
                    }
                    i += len;
                }
            }
        }
        self.buf.drain(0..i);
        out
    }
}

fn utf8_len(lead: u8) -> usize {
    if lead < 0x80 {
        1
    } else if lead < 0xe0 {
        2
    } else if lead < 0xf0 {
        3
    } else {
        4
    }
}

/// Decodes the modifier from a CSI arrow's params (`1;<m>` → bitfield in `m-1`:
/// Shift=1, Alt=2, Ctrl=4). Empty/absent params (a bare arrow) → no modifiers.
fn csi_modifiers(params: &[u8]) -> KeyModifiers {
    let m = std::str::from_utf8(params)
        .ok()
        .and_then(|s| s.split(';').nth(1))
        .and_then(|n| n.parse::<u8>().ok());
    match m {
        Some(m) if m >= 1 => {
            let bits = m - 1;
            let mut mods = KeyModifiers::NONE;
            if bits & 1 != 0 {
                mods |= KeyModifiers::SHIFT;
            }
            if bits & 2 != 0 {
                mods |= KeyModifiers::ALT;
            }
            if bits & 4 != 0 {
                mods |= KeyModifiers::CONTROL;
            }
            mods
        }
        _ => KeyModifiers::NONE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::KeyCode;

    fn codes(bytes: &[u8]) -> Vec<KeyCode> {
        KeyDecoder::new()
            .feed(bytes)
            .into_iter()
            .map(|k| k.code)
            .collect()
    }

    #[test]
    fn printable_ascii() {
        assert_eq!(
            codes(b"dev"),
            vec![KeyCode::Char('d'), KeyCode::Char('e'), KeyCode::Char('v')]
        );
    }

    #[test]
    fn enter_esc_backspace() {
        assert_eq!(codes(b"\r"), vec![KeyCode::Enter]);
        assert_eq!(codes(b"\x1b"), vec![KeyCode::Esc]);
        assert_eq!(codes(b"\x7f"), vec![KeyCode::Backspace]);
    }

    #[test]
    fn csi_arrows() {
        assert_eq!(codes(b"\x1b[A"), vec![KeyCode::Up]);
        assert_eq!(codes(b"\x1b[B"), vec![KeyCode::Down]);
        assert_eq!(codes(b"\x1b[C"), vec![KeyCode::Right]);
        assert_eq!(codes(b"\x1b[D"), vec![KeyCode::Left]);
    }

    #[test]
    fn csi_arrows_with_ctrl_modifier() {
        // `ESC[1;5A` = Ctrl+Up; the modifier param `5` = 1 + Ctrl(4). Bare arrows
        // stay NONE. Used for the level-aware Ctrl+↑/↓ sibling navigation.
        let ev = KeyDecoder::new().feed(b"\x1b[1;5A");
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].code, KeyCode::Up);
        assert!(ev[0].modifiers.contains(KeyModifiers::CONTROL));
        let down = KeyDecoder::new().feed(b"\x1b[1;5B");
        assert_eq!(down[0].code, KeyCode::Down);
        assert!(down[0].modifiers.contains(KeyModifiers::CONTROL));
        // A bare arrow carries no modifiers.
        assert!(KeyDecoder::new().feed(b"\x1b[A")[0].modifiers.is_empty());
    }

    #[test]
    fn utf8_multibyte_char() {
        // 2-byte char é = C3 A9
        assert_eq!(codes(&[0xc3, 0xa9]), vec![KeyCode::Char('é')]);
    }

    #[test]
    fn lone_esc_then_char_is_esc_and_char() {
        // a bare ESC not starting a CSI is Esc; the next byte is its own key
        assert_eq!(codes(b"\x1bx"), vec![KeyCode::Esc, KeyCode::Char('x')]);
    }

    #[test]
    fn unrecognized_csi_consumed_silently() {
        // Delete (ESC[3~), PgDn (ESC[6~), and Home (ESC[H) must produce no events —
        // never a spurious Esc that would cancel the picker.
        assert_eq!(
            codes(b"\x1b[3~"),
            Vec::<KeyCode>::new(),
            "Delete should be silent"
        );
        assert_eq!(
            codes(b"\x1b[6~"),
            Vec::<KeyCode>::new(),
            "PgDn should be silent"
        );
        assert_eq!(
            codes(b"\x1b[H"),
            Vec::<KeyCode>::new(),
            "Home (ESC[H) should be silent"
        );
    }
}
