/// A parsed SGR mouse event: button code, 1-based (col,row), press/release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MouseEvent {
    pub cb: u16,
    pub col: u16,
    pub row: u16,
    pub pressed: bool,
}

/// Parses one SGR mouse sequence `ESC [ < cb ; col ; row (M|m)`. Returns the event
/// and the total byte length consumed, or None if `data` is not a complete SGR
/// mouse sequence (too short, malformed, or not a mouse seq at all).
///
/// A mouse sequence split across two reads is treated as incomplete and returns None;
/// its bytes fall through to the non-mouse stream (v1 acceptable: rare in practice).
pub fn parse_sgr_mouse(data: &[u8]) -> Option<(MouseEvent, usize)> {
    // Require ESC [ < prefix (3 bytes minimum)
    if data.len() < 3 || data[0] != 0x1b || data[1] != b'[' || data[2] != b'<' {
        return None;
    }
    let mut i = 3;
    // Parse cb
    let (cb, adv) = parse_decimal(&data[i..])?;
    i += adv;
    if i >= data.len() || data[i] != b';' {
        return None;
    }
    i += 1;
    // Parse col
    let (col, adv) = parse_decimal(&data[i..])?;
    i += adv;
    if i >= data.len() || data[i] != b';' {
        return None;
    }
    i += 1;
    // Parse row
    let (row, adv) = parse_decimal(&data[i..])?;
    i += adv;
    if i >= data.len() {
        return None;
    }
    let pressed = match data[i] {
        b'M' => true,
        b'm' => false,
        _ => return None,
    };
    i += 1;
    Some((
        MouseEvent {
            cb,
            col,
            row,
            pressed,
        },
        i,
    ))
}

/// Re-encodes an SGR mouse event at new 1-based (col,row).
pub fn encode_sgr_mouse(ev: &MouseEvent, col: u16, row: u16) -> Vec<u8> {
    let term = if ev.pressed { 'M' } else { 'm' };
    format!("\x1b[<{};{};{}{}", ev.cb, col, row, term).into_bytes()
}

/// Parses a decimal integer from the start of `data`. Returns the value and the
/// number of bytes consumed, or None if `data` is empty or has no leading digit.
fn parse_decimal(data: &[u8]) -> Option<(u16, usize)> {
    if data.is_empty() || !data[0].is_ascii_digit() {
        return None;
    }
    let mut n: u32 = 0;
    let mut i = 0;
    while i < data.len() && data[i].is_ascii_digit() {
        n = n * 10 + (data[i] - b'0') as u32;
        if n > u16::MAX as u32 {
            return None;
        }
        i += 1;
    }
    Some((n as u16, i))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_press_roundtrip() {
        let input = b"\x1b[<0;10;5M";
        let (ev, len) = parse_sgr_mouse(input).expect("must parse");
        assert_eq!(
            ev,
            MouseEvent {
                cb: 0,
                col: 10,
                row: 5,
                pressed: true
            }
        );
        assert_eq!(len, input.len());
    }

    #[test]
    fn parse_release_form() {
        let input = b"\x1b[<0;10;5m";
        let (ev, len) = parse_sgr_mouse(input).expect("must parse release");
        assert!(!ev.pressed);
        assert_eq!(len, input.len());
    }

    #[test]
    fn encode_at_new_coords() {
        let ev = MouseEvent {
            cb: 0,
            col: 10,
            row: 5,
            pressed: true,
        };
        let encoded = encode_sgr_mouse(&ev, 3, 2);
        assert_eq!(encoded, b"\x1b[<0;3;2M");
    }

    #[test]
    fn parse_partial_returns_none() {
        // Incomplete: missing row + terminator
        assert!(parse_sgr_mouse(b"\x1b[<0;10").is_none());
    }

    #[test]
    fn parse_non_mouse_returns_none() {
        // ESC [ A  (cursor up) — not a mouse sequence
        assert!(parse_sgr_mouse(b"\x1b[A").is_none());
    }

    #[test]
    fn parse_multi_digit_coords() {
        let input = b"\x1b[<32;120;48M";
        let (ev, len) = parse_sgr_mouse(input).expect("multi-digit coords");
        assert_eq!(
            ev,
            MouseEvent {
                cb: 32,
                col: 120,
                row: 48,
                pressed: true
            }
        );
        assert_eq!(len, input.len());
    }

    #[test]
    fn parse_overflow_coord_returns_none() {
        assert!(
            parse_sgr_mouse(b"\x1b[<0;65536;5M").is_none(),
            "coordinate >= 65536 must parse as None"
        );
    }

    #[test]
    fn encode_release() {
        let ev = MouseEvent {
            cb: 0,
            col: 5,
            row: 3,
            pressed: false,
        };
        let encoded = encode_sgr_mouse(&ev, 5, 3);
        assert_eq!(encoded, b"\x1b[<0;5;3m");
    }
}
