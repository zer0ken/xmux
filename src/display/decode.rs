//! Minimal raw-byte → crossterm KeyEvent decoder for the switcher. It must cover
//! every key the UI ADVERTISES: a key the status line or the help modal names, but
//! that this decoder drops, is a feature the user cannot reach from the keyboard even
//! though the handler for it exists. A lone ESC that is not followed by `[<final>` or
//! `O<final>` is Esc.
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
                        // A kitty key event (`CSI ... u`, or Windows Terminal's
                        // letter-final hybrid with an event type) is parsed first so
                        // press/repeat/release reach the handler as kinds, and a
                        // release is never mis-read as a fresh press. A plain legacy
                        // CSI (an arrow, a mouse report) is not one and falls through.
                        if let Some((end, ev)) = parse_kitty_seq(&self.buf, i) {
                            if let Some(code) = kitty_keycode(&ev) {
                                out.push(KeyEvent::new_with_kind(
                                    code,
                                    kitty_modifiers(ev.modifiers),
                                    kitty_kind(&ev),
                                ));
                            }
                            i = end;
                            continue;
                        }
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
                                                 //
                                                 // Home/End/PageUp/PageDown/Delete come in two encodings and
                                                 // terminals disagree about which: a letter final (`ESC[H`,
                                                 // `ESC[F`) or a numbered tilde (`ESC[1~`, `ESC[4~`, `ESC[5~`,
                                                 // `ESC[6~`, `ESC[3~`). Decode both, since the nav advertises
                                                 // Home/End and PgUp/PgDn and the input row uses Home/End/Delete.
                        let params = &self.buf[seq_start..j];
                        let code = match final_byte {
                            b'A' => Some(KeyCode::Up),
                            b'B' => Some(KeyCode::Down),
                            b'C' => Some(KeyCode::Right),
                            b'D' => Some(KeyCode::Left),
                            b'H' => Some(KeyCode::Home),
                            b'F' => Some(KeyCode::End),
                            b'Z' => Some(KeyCode::BackTab),
                            b'~' => match csi_first_param(params) {
                                Some(1) | Some(7) => Some(KeyCode::Home),
                                Some(2) => Some(KeyCode::Insert),
                                Some(3) => Some(KeyCode::Delete),
                                Some(4) | Some(8) => Some(KeyCode::End),
                                Some(5) => Some(KeyCode::PageUp),
                                Some(6) => Some(KeyCode::PageDown),
                                _ => None,
                            },
                            _ => None,
                        };
                        if let Some(c) = code {
                            let mods = csi_modifiers(params);
                            out.push(KeyEvent::new(c, mods));
                            i += csi_len;
                            continue;
                        }
                        // Any other complete CSI — consume silently (no Esc spurion).
                        // Mouse reports and cursor-position replies land here.
                        i += csi_len;
                        continue;
                    }
                    // SS3 (`ESC O<final>`): what a terminal sends for the arrows and
                    // Home/End in application cursor mode. Without this the three bytes
                    // decode as Esc + 'O' + a letter, which cancels a modal and types a
                    // stray letter instead of moving the cursor.
                    if i + 2 < self.buf.len() && self.buf[i + 1] == b'O' {
                        let code = match self.buf[i + 2] {
                            b'A' => Some(KeyCode::Up),
                            b'B' => Some(KeyCode::Down),
                            b'C' => Some(KeyCode::Right),
                            b'D' => Some(KeyCode::Left),
                            b'H' => Some(KeyCode::Home),
                            b'F' => Some(KeyCode::End),
                            _ => None,
                        };
                        if let Some(c) = code {
                            out.push(KeyEvent::new(c, KeyModifiers::NONE));
                            i += 3;
                            continue;
                        }
                    }
                    // `ESC O` with no third byte yet: keep the tail buffered rather than
                    // emitting an Esc that a later byte would have completed.
                    if i + 1 < self.buf.len() && self.buf[i + 1] == b'O' && i + 2 >= self.buf.len()
                    {
                        break;
                    }
                    // Lone ESC (no following byte) or ESC followed by neither `[` nor `O`:
                    // emit Esc.
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
                    match std::str::from_utf8(&self.buf[i..i + len]) {
                        Ok(s) => {
                            if let Some(c) = s.chars().next() {
                                out.push(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
                            }
                            i += len;
                        }
                        // The estimated len was wrong (invalid lead byte): resync by a
                        // single byte so the next valid byte is not swallowed.
                        Err(_) => i += 1,
                    }
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

/// The first `;`-separated parameter of a CSI sequence, which is what selects the key
/// for a `~`-final sequence (`ESC[5~` = PageUp). Absent params read as `None`, so a
/// bare `ESC[~` decodes to nothing rather than to an arbitrary key.
fn csi_first_param(params: &[u8]) -> Option<u16> {
    std::str::from_utf8(params)
        .ok()?
        .split(';')
        .next()?
        .parse::<u16>()
        .ok()
}

/// A parsed kitty-protocol key event: the Unicode key-code, the kitty modifier
/// bitfield (1=shift, 2=alt, 4=ctrl, ...), the event type (1=press, 2=repeat,
/// 3=release; 0 when the sequence omits it), and the CSI final byte (`u` for the
/// codepoint form, a letter or `~` for Windows Terminal's hybrid form).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct KittyEvent {
    pub(crate) code: u32,
    pub(crate) modifiers: u8,
    pub(crate) kind: u8,
    pub(crate) final_byte: u8,
}

/// Parses a kitty `CSI ... u` (or WT hybrid `CSI ...<letter>`) key event at
/// `bytes[i..]`. Returns `(end_index, event)`, `end_index` one past the final byte.
/// `None` when the sequence is incomplete or not a key event (a mouse report etc.).
pub(crate) fn parse_kitty_seq(bytes: &[u8], i: usize) -> Option<(usize, KittyEvent)> {
    if i + 1 >= bytes.len() || bytes[i] != 0x1b || bytes[i + 1] != b'[' {
        return None;
    }
    let start = i + 2;
    let mut j = start;
    while j < bytes.len() && !(0x40..=0x7e).contains(&bytes[j]) {
        j += 1;
    }
    if j >= bytes.len() {
        return None; // incomplete
    }
    let final_byte = bytes[j];
    if final_byte != b'u'
        && !matches!(
            final_byte,
            b'A' | b'B' | b'C' | b'D' | b'H' | b'F' | b'P' | b'Q' | b'R' | b'S' | b'~'
        )
    {
        return None; // not a key event
    }
    let params = std::str::from_utf8(&bytes[start..j]).ok()?;
    let mut fields = params.split(';');
    let code = fields.next()?.split(':').next()?.parse::<u32>().ok()?;
    let mut modifiers = 0u8;
    let mut kind = 0u8;
    if let Some(m) = fields.next() {
        let mut sub = m.split(':');
        if let Some(v) = sub.next() {
            // kitty modifier field is 1-based.
            modifiers = v.parse::<u32>().ok().unwrap_or(1).saturating_sub(1) as u8;
        }
        if let Some(v) = sub.next() {
            kind = v.parse::<u32>().ok().unwrap_or(0) as u8;
        }
    }
    // A kitty event is the `u`-final codepoint form, or a letter-final form that
    // carries an explicit event type (Windows Terminal's repeat/release hybrid). A
    // plain legacy sequence (`CSI A`, `CSI 1;5A`) is not one, so it is left to the
    // normal CSI handling and never sets the kitty-seen gate.
    if final_byte != b'u' && kind == 0 {
        return None;
    }
    Some((
        j + 1,
        KittyEvent {
            code,
            modifiers,
            kind,
            final_byte,
        },
    ))
}

/// kitty bitfield → crossterm modifiers (shift=1, alt=2, ctrl=4).
fn kitty_modifiers(bits: u8) -> KeyModifiers {
    let mut m = KeyModifiers::NONE;
    if bits & 1 != 0 {
        m |= KeyModifiers::SHIFT;
    }
    if bits & 2 != 0 {
        m |= KeyModifiers::ALT;
    }
    if bits & 4 != 0 {
        m |= KeyModifiers::CONTROL;
    }
    m
}

/// kitty key-code / hybrid final → crossterm KeyCode.
fn kitty_keycode(ev: &KittyEvent) -> Option<KeyCode> {
    if ev.final_byte != b'u' {
        return match ev.final_byte {
            b'A' => Some(KeyCode::Up),
            b'B' => Some(KeyCode::Down),
            b'C' => Some(KeyCode::Right),
            b'D' => Some(KeyCode::Left),
            b'H' => Some(KeyCode::Home),
            b'F' => Some(KeyCode::End),
            _ => None,
        };
    }
    match ev.code {
        27 => Some(KeyCode::Esc),
        13 => Some(KeyCode::Enter),
        9 => Some(KeyCode::Tab),
        127 => Some(KeyCode::Backspace),
        c @ 0..=31 => Some(KeyCode::Char(c as u8 as char)), // e.g. C-g = '\x07'
        c => char::from_u32(c).map(KeyCode::Char),
    }
}

fn kitty_kind(ev: &KittyEvent) -> ratatui::crossterm::event::KeyEventKind {
    use ratatui::crossterm::event::KeyEventKind;
    match ev.kind {
        2 => KeyEventKind::Repeat,
        3 => KeyEventKind::Release,
        _ => KeyEventKind::Press,
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
    fn every_advertised_navigation_key_decodes() {
        // The nav's status line and help modal name Home/End and PgUp/PgDn, and the
        // input row uses Home/End/Delete. A key the UI advertises but this decoder
        // drops is unreachable from the keyboard however complete its handler is.
        assert_eq!(codes(b"\x1b[H"), vec![KeyCode::Home], "Home, letter final");
        assert_eq!(codes(b"\x1b[1~"), vec![KeyCode::Home], "Home, tilde form");
        assert_eq!(codes(b"\x1b[F"), vec![KeyCode::End], "End, letter final");
        assert_eq!(codes(b"\x1b[4~"), vec![KeyCode::End], "End, tilde form");
        assert_eq!(codes(b"\x1b[5~"), vec![KeyCode::PageUp], "PageUp");
        assert_eq!(codes(b"\x1b[6~"), vec![KeyCode::PageDown], "PageDown");
        assert_eq!(codes(b"\x1b[3~"), vec![KeyCode::Delete], "Delete");
    }

    #[test]
    fn ss3_arrows_and_home_end_decode() {
        // Application cursor mode sends SS3, not CSI. Decoding it as Esc + letters would
        // cancel a modal and type a stray character instead of moving the cursor.
        assert_eq!(codes(b"\x1bOA"), vec![KeyCode::Up]);
        assert_eq!(codes(b"\x1bOD"), vec![KeyCode::Left]);
        assert_eq!(codes(b"\x1bOH"), vec![KeyCode::Home]);
        assert_eq!(codes(b"\x1bOF"), vec![KeyCode::End]);
        // A split read must not turn the pending `ESC O` into an Esc.
        let mut d = KeyDecoder::new();
        assert!(d.feed(b"\x1bO").is_empty(), "incomplete SS3 stays buffered");
        assert_eq!(
            d.feed(b"B").into_iter().map(|e| e.code).collect::<Vec<_>>(),
            vec![KeyCode::Down]
        );
    }

    #[test]
    fn unrecognized_csi_consumed_silently() {
        // A CSI the switcher has no key for (a mouse report here) must produce no
        // events, and never a spurious Esc that would cancel a modal.
        assert_eq!(
            codes(b"\x1b[<0;10;5M"),
            Vec::<KeyCode>::new(),
            "an SGR mouse report should be silent"
        );
        assert_eq!(
            codes(b"\x1b[~"),
            Vec::<KeyCode>::new(),
            "a tilde sequence with no parameter names no key"
        );
    }

    #[test]
    fn kitty_parses_press_repeat_release_and_hybrid() {
        // C-g press / repeat / release (ctrl modifier field 5 = 1 + ctrl(4)).
        let (end, ev) = parse_kitty_seq(b"\x1b[7;5u", 0).unwrap();
        assert_eq!(end, 6);
        assert_eq!((ev.code, ev.modifiers, ev.kind, ev.final_byte), (7, 4, 0, b'u'));
        let (_, ev) = parse_kitty_seq(b"\x1b[7;5:2u", 0).unwrap();
        assert_eq!((ev.code, ev.kind), (7, 2));
        let (_, ev) = parse_kitty_seq(b"\x1b[7;5:3u", 0).unwrap();
        assert_eq!((ev.code, ev.kind), (7, 3));
        // A plain text key release.
        let (_, ev) = parse_kitty_seq(b"\x1b[97;1:3u", 0).unwrap();
        assert_eq!((ev.code, ev.modifiers, ev.kind), (97, 0, 3));
        // Windows Terminal hybrid: Up repeat/release, letter final.
        let (_, ev) = parse_kitty_seq(b"\x1b[1;1:2A", 0).unwrap();
        assert_eq!((ev.code, ev.modifiers, ev.kind, ev.final_byte), (1, 0, 2, b'A'));
        // A normal arrow (no event type) is NOT a kitty event: it stays legacy.
        assert!(parse_kitty_seq(b"\x1b[A", 0).is_none());
        assert!(parse_kitty_seq(b"\x1b[1;5A", 0).is_none());
        // An SGR mouse report is not a key event.
        assert!(parse_kitty_seq(b"\x1b[<0;10;5M", 0).is_none());
    }

    #[test]
    fn kitty_sequences_decode_with_kinds() {
        use ratatui::crossterm::event::KeyEventKind;
        // C-g release arrives as CSI 7;5:3u: a Release event (the nav must clear holding).
        let evs = KeyDecoder::new().feed(b"\x1b[7;5:3u");
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].code, KeyCode::Char('\x07'));
        assert_eq!(evs[0].kind, KeyEventKind::Release);
        // WT hybrid: an Up repeat decodes as a Repeat Up, a release as Release Up.
        let evs = KeyDecoder::new().feed(b"\x1b[1;1:2A");
        assert_eq!(evs[0].code, KeyCode::Up);
        assert_eq!(evs[0].kind, KeyEventKind::Repeat);
        let evs = KeyDecoder::new().feed(b"\x1b[1;1:3A");
        assert_eq!(evs[0].code, KeyCode::Up);
        assert_eq!(evs[0].kind, KeyEventKind::Release);
        // A normal arrow press stays a Press.
        assert_eq!(KeyDecoder::new().feed(b"\x1b[A")[0].kind, KeyEventKind::Press);
    }

    #[test]
    fn invalid_utf8_lead_resyncs_without_swallowing_next() {
        // A stray invalid lead byte (0x80, estimated len 2) must be dropped ONE byte
        // at a time so the following valid byte is not swallowed by the guess.
        assert_eq!(codes(&[0x80, b'x']), vec![KeyCode::Char('x')]);
        // A len-4 invalid lead (0xff) resyncs the same way and still yields the ASCII.
        assert_eq!(
            codes(&[0xff, b'a', b'b', b'c']),
            vec![KeyCode::Char('a'), KeyCode::Char('b'), KeyCode::Char('c')]
        );
    }
}
