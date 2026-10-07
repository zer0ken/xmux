//! The kitty keyboard protocol between xmux's terminal and the session in the terminal
//! view. A client asks its terminal, which is xmux's PTY, for the protocol's flags; xmux
//! sets the same flags on its own terminal while that session holds the focus, so the
//! terminal encodes each key the way the client asked and xmux forwards it unchanged.
//! xmux itself reads these keys only to find the prefix and the keys after it.

use std::sync::atomic::{AtomicU8, Ordering};

/// Pushes a fresh entry for xmux on the terminal's flag stack.
pub const PUSH: &[u8] = b"\x1b[>0u";
/// Pops the entry [`PUSH`] made, restoring what the terminal had before xmux.
pub const POP: &[u8] = b"\x1b[<u";

/// The `CSI = flags ; 1 u` that sets the flags of xmux's entry on its terminal.
pub fn set_flags(flags: u8) -> Vec<u8> {
    format!("\x1b[={flags};1u").into_bytes()
}

const UNKNOWN: u8 = 0;
const ABSENT: u8 = 1;
const PRESENT: u8 = 2;

/// What xmux's terminal answered about the protocol. The terminal is one per process,
/// and both the input path and every attachment's pump read the answer.
#[cfg(not(test))]
static SUPPORT: AtomicU8 = AtomicU8::new(UNKNOWN);

// Each test thread stands for a process of its own, so one test's terminal answer
// never reaches another running beside it.
#[cfg(test)]
thread_local! {
    static SUPPORT: AtomicU8 = const { AtomicU8::new(UNKNOWN) };
}

fn support() -> u8 {
    #[cfg(not(test))]
    return SUPPORT.load(Ordering::Relaxed);
    #[cfg(test)]
    return SUPPORT.with(|s| s.load(Ordering::Relaxed));
}

/// Whether xmux's terminal answered that it has the protocol.
pub fn supported() -> bool {
    support() == PRESENT
}

/// Records the terminal's answer to the startup probe: the flags reply means it has the
/// protocol, and a device attributes reply that arrives first means it has not. Returns whether this
/// answer found the protocol, so the caller pushes xmux's entry exactly once.
pub fn record_support(present: bool) -> bool {
    let to = if present { PRESENT } else { ABSENT };
    let swap = |s: &AtomicU8| {
        s.compare_exchange(UNKNOWN, to, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    };
    #[cfg(not(test))]
    let recorded = swap(&SUPPORT);
    #[cfg(test)]
    let recorded = SUPPORT.with(swap);
    recorded && present
}

/// What a key event reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Press,
    Repeat,
    Release,
}

/// One key as the protocol encodes it: `CSI code[:alternates] [; mods[:event]] [; text]
/// u`, or a functional key's legacy form carrying an event (`CSI 1;5:3C`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Key {
    /// The key's number: a Unicode code point for `u`, the first parameter otherwise.
    pub code: u32,
    /// The code the key produces with Shift, when the terminal reports it.
    pub shifted: Option<u32>,
    /// Modifier bits: Shift 1, Alt 2, Ctrl 4, Super 8, and the rest the protocol names.
    pub mods: u8,
    pub event: Event,
    pub final_byte: u8,
    /// The bytes the key took.
    pub len: usize,
}

const SHIFT: u8 = 1;
const CTRL: u8 = 4;
/// Every modifier but Shift and the two locks.
const NON_SHIFT: u8 = 0b0011_1110;

impl Key {
    /// Whether this is the press or release of a modifier key itself, which only a
    /// client asking to have every key reported is sent.
    pub fn is_modifier(&self) -> bool {
        self.final_byte == b'u' && (57441..=57452).contains(&self.code)
    }

    /// The key's identity across its press and release.
    pub fn id(&self) -> (u32, u8) {
        (self.code, self.final_byte)
    }

    /// The legacy bytes this key stands for, for reading the prefix and the keys after
    /// it, or `None` for a key xmux binds nothing to in any encoding.
    pub fn legacy(&self) -> Option<Vec<u8>> {
        let mods = self.mods & !(64 | 128);
        if self.final_byte != b'u' {
            // A functional key in its legacy form, without the event.
            let m = mods + 1;
            return Some(if m == 1 && self.code == 1 && self.final_byte != b'~' {
                vec![0x1b, b'[', self.final_byte]
            } else if m == 1 {
                format!("\x1b[{}{}", self.code, self.final_byte as char).into_bytes()
            } else {
                format!("\x1b[{};{}{}", self.code, m, self.final_byte as char).into_bytes()
            });
        }
        let named = match self.code {
            27 => Some(0x1b),
            13 => Some(b'\r'),
            9 => Some(b'\t'),
            127 => Some(0x7f),
            _ => None,
        };
        if let Some(byte) = named {
            return (mods == 0).then(|| vec![byte]);
        }
        let c = char::from_u32(self.code)?;
        if mods == CTRL {
            return match c {
                'a'..='z' => Some(vec![c as u8 - b'a' + 1]),
                ' ' | '@' => Some(vec![0]),
                _ => None,
            };
        }
        if mods & NON_SHIFT != 0 {
            return None;
        }
        let c = if mods & SHIFT != 0 {
            match self.shifted.and_then(char::from_u32) {
                Some(shifted) => shifted,
                None if c.is_ascii_lowercase() => c.to_ascii_uppercase(),
                None => return None,
            }
        } else {
            c
        };
        Some(c.to_string().into_bytes())
    }
}

/// The protocol key at the start of `bytes`, or `None` when they start with anything
/// else, a legacy key included.
pub fn parse(bytes: &[u8]) -> Option<Key> {
    let body = bytes.strip_prefix(b"\x1b[")?;
    let end = body
        .iter()
        .position(|b| !(b.is_ascii_digit() || *b == b';' || *b == b':'))?;
    let final_byte = body[end];
    let params = std::str::from_utf8(&body[..end]).ok()?;
    let mut fields = params.split(';');
    let key = fields.next().unwrap_or("");
    let modifier = fields.next().unwrap_or("");
    let is_u = final_byte == b'u';
    if !is_u
        && !(matches!(final_byte, b'A'..=b'D' | b'H' | b'F' | b'P'..=b'S' | b'~')
            && modifier.contains(':'))
    {
        return None;
    }
    let mut key_parts = key.split(':');
    let code = match key_parts.next() {
        Some("") | None => 1,
        Some(n) => n.parse().ok()?,
    };
    let shifted = key_parts.next().and_then(|n| n.parse().ok());
    let mut mod_parts = modifier.split(':');
    let mods = match mod_parts.next() {
        Some("") | None => 0,
        Some(n) => n.parse::<u16>().ok()?.saturating_sub(1).min(255) as u8,
    };
    let event = match mod_parts.next() {
        None | Some("") | Some("1") => Event::Press,
        Some("2") => Event::Repeat,
        Some("3") => Event::Release,
        Some(_) => return None,
    };
    Some(Key {
        code,
        shifted,
        mods,
        event,
        final_byte,
        len: 2 + end + 1,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn legacy(bytes: &[u8]) -> Option<Vec<u8>> {
        parse(bytes).and_then(|k| k.legacy())
    }

    #[test]
    fn protocol_keys_read_as_the_legacy_bytes_they_stand_for() {
        assert_eq!(legacy(b"\x1b[103;5u"), Some(vec![0x07]), "Ctrl+g");
        assert_eq!(legacy(b"\x1b[113u"), Some(b"q".to_vec()));
        assert_eq!(legacy(b"\x1b[113;1:1u"), Some(b"q".to_vec()));
        assert_eq!(legacy(b"\x1b[47:63;2u"), Some(b"?".to_vec()), "Shift+/");
        assert_eq!(legacy(b"\x1b[97;2u"), Some(b"A".to_vec()));
        assert_eq!(legacy(b"\x1b[9u"), Some(b"\t".to_vec()));
        assert_eq!(
            legacy(b"\x1b[13;2u"),
            None,
            "Shift+Enter has no legacy form"
        );
        assert_eq!(legacy(b"\x1b[1;5:1C"), Some(b"\x1b[1;5C".to_vec()));
        assert_eq!(legacy(b"\x1b[1;1:2D"), Some(b"\x1b[D".to_vec()));
        assert_eq!(legacy(b"\x1b[5;1:1~"), Some(b"\x1b[5~".to_vec()));
    }

    #[test]
    fn legacy_keys_are_not_protocol_keys() {
        assert_eq!(parse(b"\x1b[A"), None);
        assert_eq!(parse(b"\x1b[1;5C"), None);
        assert_eq!(parse(b"\x1b[5~"), None);
        assert_eq!(parse(b"q"), None);
        assert_eq!(parse(b"\x1b[<0;1;1M"), None, "a mouse report");
    }

    #[test]
    fn events_and_modifier_keys_are_told_apart() {
        let release = parse(b"\x1b[103;5:3uq").unwrap();
        assert_eq!(release.event, Event::Release);
        assert_eq!(release.len, 10);
        assert_eq!(parse(b"\x1b[103;5:2u").unwrap().event, Event::Repeat);
        assert!(parse(b"\x1b[57442;5u").unwrap().is_modifier(), "left Ctrl");
        assert!(!parse(b"\x1b[103;5u").unwrap().is_modifier());
    }
}
