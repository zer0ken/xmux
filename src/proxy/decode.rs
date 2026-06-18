//! Minimal raw-byte → crossterm KeyEvent decoder for Picker mode. Covers only the
//! keys the switcher uses. A lone ESC that is not followed by `[<final>` is Esc.
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub struct KeyDecoder {
    buf: Vec<u8>,
}

impl KeyDecoder {
    pub fn new() -> Self { Self { buf: Vec::new() } }

    pub fn feed(&mut self, bytes: &[u8]) -> Vec<KeyEvent> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        let mut i = 0;
        while i < self.buf.len() {
            let b = self.buf[i];
            match b {
                0x1b => {
                    // CSI arrow: ESC [ A/B/C/D
                    if i + 2 < self.buf.len() && self.buf[i + 1] == b'[' {
                        let code = match self.buf[i + 2] {
                            b'A' => Some(KeyCode::Up),
                            b'B' => Some(KeyCode::Down),
                            b'C' => Some(KeyCode::Right),
                            b'D' => Some(KeyCode::Left),
                            _ => None,
                        };
                        if let Some(c) = code {
                            out.push(KeyEvent::new(c, KeyModifiers::NONE));
                            i += 3;
                            continue;
                        }
                    }
                    // ESC `[` with no final byte yet — wait for more data.
                    if i + 1 < self.buf.len() && self.buf[i + 1] == b'[' && i + 2 >= self.buf.len() {
                        break; // keep the tail buffered
                    }
                    // Lone ESC (no following byte) or ESC followed by non-`[`: emit Esc.
                    out.push(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
                    i += 1;
                }
                b'\r' | b'\n' => { out.push(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)); i += 1; }
                0x7f | 0x08 => { out.push(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)); i += 1; }
                _ if b < 0x80 => { out.push(KeyEvent::new(KeyCode::Char(b as char), KeyModifiers::NONE)); i += 1; }
                _ => {
                    // UTF-8 multibyte: find the char length, decode if complete.
                    let len = utf8_len(b);
                    if i + len > self.buf.len() { break; } // incomplete, buffer it
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
    if lead < 0x80 { 1 } else if lead < 0xe0 { 2 } else if lead < 0xf0 { 3 } else { 4 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};

    fn codes(bytes: &[u8]) -> Vec<KeyCode> {
        KeyDecoder::new().feed(bytes).into_iter().map(|k| k.code).collect()
    }

    #[test]
    fn printable_ascii() {
        assert_eq!(codes(b"dev"), vec![KeyCode::Char('d'), KeyCode::Char('e'), KeyCode::Char('v')]);
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
    fn utf8_multibyte_char() {
        // 2-byte char é = C3 A9
        assert_eq!(codes(&[0xc3, 0xa9]), vec![KeyCode::Char('é')]);
    }

    #[test]
    fn lone_esc_then_char_is_esc_and_char() {
        // a bare ESC not starting a CSI is Esc; the next byte is its own key
        assert_eq!(codes(b"\x1bx"), vec![KeyCode::Esc, KeyCode::Char('x')]);
    }
}
