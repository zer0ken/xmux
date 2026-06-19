//! Pure control-mode (`-CC`) wire functions: `%output` octal decode, line
//! framing/classification, the notification parse table, the `send-keys -H`
//! batched-hex builder, and the `refresh-client -C WxH` formatter. No I/O — every
//! wire detail is unit-testable headlessly against tmux 3.3.x.

#[inline]
fn is_octal_digit(b: u8) -> bool {
    (b'0'..=b'7').contains(&b)
}

/// Decodes a `%output` value into `out` (cleared first, capacity retained). Every
/// `\ooo` 3-digit octal escape becomes one byte; all other bytes pass through.
pub fn decode_output_into(out: &mut Vec<u8>, data: &[u8]) {
    out.clear();
    let n = data.len();
    let mut i = 0;
    while i < n {
        let b = data[i];
        if b == b'\\'
            && i + 3 < n
            && is_octal_digit(data[i + 1])
            && is_octal_digit(data[i + 2])
            && is_octal_digit(data[i + 3])
        {
            let v = (data[i + 1] - b'0') * 64 + (data[i + 2] - b'0') * 8 + (data[i + 3] - b'0');
            out.push(v);
            i += 4;
        } else {
            out.push(b);
            i += 1;
        }
    }
}

/// For `%extended-output`, returns the data part after the single `:` separator
/// (`[research §3, §8]`); the bytes up to and including the first `:` are the age
/// and future-args field, ignored. No `:` ⇒ the input is returned unchanged.
pub fn strip_extended_prefix(data: &[u8]) -> &[u8] {
    match data.iter().position(|&b| b == b':') {
        Some(i) => {
            let rest = &data[i + 1..];
            // tmux emits "<age> ... : <data>" with a space after the colon.
            rest.strip_prefix(b" ").unwrap_or(rest)
        }
        None => data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_handles_octal_and_literals() {
        let mut out = Vec::new();
        decode_output_into(&mut out, b"\\134");        // backslash
        assert_eq!(out, b"\\");
        decode_output_into(&mut out, b"\\012");        // LF
        assert_eq!(out, b"\n");
        decode_output_into(&mut out, b"\\015");        // CR
        assert_eq!(out, b"\r");
        decode_output_into(&mut out, b"\\000");        // NUL
        assert_eq!(out, b"\x00");
        decode_output_into(&mut out, b"\\033[31mhi");  // ESC + literal tail
        assert_eq!(out, b"\x1b[31mhi");
        decode_output_into(&mut out, &[0xc3, 0xa9]);   // UTF-8 é passes through raw
        assert_eq!(out, &[0xc3, 0xa9]);
        decode_output_into(&mut out, b"\x7f");          // DEL (0x7f) is NOT escaped
        assert_eq!(out, b"\x7f");
    }

    #[test]
    fn decode_reuses_buffer_no_growth_per_call() {
        let mut out = Vec::with_capacity(64);
        decode_output_into(&mut out, b"first");
        let cap = out.capacity();
        decode_output_into(&mut out, b"second");
        assert_eq!(out, b"second", "each call refills from scratch");
        assert!(out.capacity() >= cap, "capacity is retained for reuse");
    }

    #[test]
    fn strip_extended_prefix_drops_age_and_future_args() {
        // %extended-output %0 <age> ...future... : <data>  — the reader has already
        // split off "%extended-output %0 "; this fn gets "<age> ... : <data>".
        assert_eq!(strip_extended_prefix(b"512 : \\033[2J"), b"\\033[2J");
        assert_eq!(strip_extended_prefix(b"7 future stuff : payload"), b"payload");
        assert_eq!(strip_extended_prefix(b"no colon here"), b"no colon here");
    }
}
