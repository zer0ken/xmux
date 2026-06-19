//! Pure control-mode (`-CC`) wire functions: `%output` octal decode, line
//! framing/classification, the notification parse table, the `send-keys -H`
//! batched-hex builder, and the `refresh-client -C WxH` formatter. No I/O — every
//! wire detail is unit-testable headlessly against tmux 3.3.x.

/// A single stdout line from tmux control mode, classified by shape.
///
/// `Body` is any non-`%`-prefixed line that appears inside a `%begin…%end` block.
/// The caller's IDLE/IN\_BLOCK state machine decides whether to treat an
/// unrecognised `%`-line as a `Notification` or a block body; `classify` only
/// determines the line shape.
#[derive(Debug, PartialEq)]
pub enum Line<'a> {
    Begin { num: u64 },
    End { num: u64 },
    Error { num: u64 },
    Output { pane: &'a str, data: &'a str },
    ExtendedOutput { pane: &'a str, rest: &'a str },
    Notification(Notif<'a>),
    Body(&'a str),
}

/// A parsed tmux control-mode notification (any `%`-prefixed line that is not a
/// recognised `%begin`/`%end`/`%error`/`%output`/`%extended-output` frame).
/// Unknown or malformed notifications are represented as `Other`.
#[derive(Debug, PartialEq)]
pub enum Notif<'a> {
    SessionChanged { id: &'a str, name: &'a str },
    SessionsChanged,
    WindowAdd { window: &'a str },
    WindowClose { window: &'a str },
    WindowRenamed { window: &'a str, name: &'a str },
    WindowPaneChanged { window: &'a str, pane: &'a str },
    SessionWindowChanged { session: &'a str, window: &'a str },
    LayoutChange { window: &'a str },
    Pause { pane: &'a str },
    Continue { pane: &'a str },
    Exit { reason: Option<&'a str> },
    ClientDetached,
    Other,
}

/// Classifies one control-mode stdout line (trailing `\n` already stripped).
///
/// Frame format: `%<verb> <unix-time> <num> <flags>` — `<num>` (the 3rd
/// whitespace field) is the correlator; `<flags>` is ignored per `[research §2]`.
/// A `%`-prefixed line whose verb is not a recognised frame/output verb is
/// classified as `Notification(parse_notif(line))`. A line not starting with
/// `%` is `Body`.
pub fn classify(line: &str) -> Line<'_> {
    if let Some(rest) = line.strip_prefix("%begin ") {
        return match frame_num_after(rest) {
            Some(num) => Line::Begin { num },
            None => Line::Notification(Notif::Other),
        };
    }
    if let Some(rest) = line.strip_prefix("%end ") {
        return match frame_num_after(rest) {
            Some(num) => Line::End { num },
            None => Line::Notification(Notif::Other),
        };
    }
    if let Some(rest) = line.strip_prefix("%error ") {
        return match frame_num_after(rest) {
            Some(num) => Line::Error { num },
            None => Line::Notification(Notif::Other),
        };
    }
    if let Some(rest) = line.strip_prefix("%output ") {
        // "%output %<pane> <data>" — pane up to first space, data is the remainder.
        if let Some((pane, data)) = rest.split_once(' ') {
            return Line::Output { pane, data };
        }
    }
    if let Some(rest) = line.strip_prefix("%extended-output ") {
        // "%extended-output %<pane> <rest>" — pane up to first space.
        if let Some((pane, rest)) = rest.split_once(' ') {
            return Line::ExtendedOutput { pane, rest };
        }
    }
    if line.starts_with('%') {
        return Line::Notification(parse_notif(line));
    }
    Line::Body(line)
}

/// `rest` is the line with `%<verb> ` stripped: `"<unix-time> <num> <flags>"`.
/// `<num>` is the second whitespace field; `<flags>` is ignored.
fn frame_num_after(rest: &str) -> Option<u64> {
    rest.split_whitespace().nth(1)?.parse().ok()
}

/// Parses a `%`-prefixed notification line into a `Notif` variant.
/// Unknown or malformed notifications return `Notif::Other`.
pub fn parse_notif(line: &str) -> Notif<'_> {
    let mut it = line.splitn(4, ' ');
    let verb = it.next().unwrap_or("");
    match verb {
        "%session-changed" => match (it.next(), it.next()) {
            (Some(id), Some(name)) => Notif::SessionChanged { id, name },
            _ => Notif::Other,
        },
        "%sessions-changed" => Notif::SessionsChanged,
        "%window-add" => it.next().map_or(Notif::Other, |w| Notif::WindowAdd { window: w }),
        "%window-close" => it.next().map_or(Notif::Other, |w| Notif::WindowClose { window: w }),
        "%window-renamed" => match (it.next(), it.next()) {
            (Some(w), Some(name)) => Notif::WindowRenamed { window: w, name },
            _ => Notif::Other,
        },
        "%window-pane-changed" => match (it.next(), it.next()) {
            (Some(w), Some(p)) => Notif::WindowPaneChanged { window: w, pane: p },
            _ => Notif::Other,
        },
        "%session-window-changed" => match (it.next(), it.next()) {
            (Some(s), Some(w)) => Notif::SessionWindowChanged { session: s, window: w },
            _ => Notif::Other,
        },
        "%layout-change" => it.next().map_or(Notif::Other, |w| Notif::LayoutChange { window: w }),
        "%pause" => it.next().map_or(Notif::Other, |p| Notif::Pause { pane: p }),
        "%continue" => it.next().map_or(Notif::Other, |p| Notif::Continue { pane: p }),
        "%client-detached" => Notif::ClientDetached,
        "%exit" => {
            let reason = line.strip_prefix("%exit").map(str::trim).filter(|s| !s.is_empty());
            Notif::Exit { reason }
        }
        _ => Notif::Other,
    }
}

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
    fn classify_frames_and_output() {
        assert!(matches!(classify("%begin 1363006971 2 1"), Line::Begin { num: 2 }));
        assert!(matches!(classify("%end 1363006971 2 1"), Line::End { num: 2 }));
        assert!(matches!(classify("%error 1363006971 5 0"), Line::Error { num: 5 }));
        match classify("%output %0 hi\\012") {
            Line::Output { pane, data } => { assert_eq!(pane, "%0"); assert_eq!(data, "hi\\012"); }
            _ => panic!("expected Output"),
        }
        match classify("%extended-output %3 512 : \\033[2J") {
            Line::ExtendedOutput { pane, rest } => { assert_eq!(pane, "%3"); assert_eq!(rest, "512 : \\033[2J"); }
            _ => panic!("expected ExtendedOutput"),
        }
        assert!(matches!(classify("0: ksh* (1 panes)"), Line::Body("0: ksh* (1 panes)")));
    }

    #[test]
    fn classify_begin_num_is_the_correlator() {
        // %begin <time> <num> <flags> — num is field 2, flags (field 3) ignored.
        assert!(matches!(classify("%begin 999 17 0"), Line::Begin { num: 17 }));
        // A malformed begin (non-numeric num) classifies as an Other notification,
        // never panics.
        assert!(matches!(classify("%begin x y z"), Line::Notification(_)));
    }

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

    #[test]
    fn parse_notif_full_table() {
        assert!(matches!(parse_notif("%session-changed $1 work"),
            Notif::SessionChanged { id: "$1", name: "work" }));
        assert!(matches!(parse_notif("%sessions-changed"), Notif::SessionsChanged));
        assert!(matches!(parse_notif("%window-add @4"), Notif::WindowAdd { window: "@4" }));
        assert!(matches!(parse_notif("%window-close @4"), Notif::WindowClose { window: "@4" }));
        assert!(matches!(parse_notif("%window-renamed @4 logs"),
            Notif::WindowRenamed { window: "@4", name: "logs" }));
        assert!(matches!(parse_notif("%window-pane-changed @4 %9"),
            Notif::WindowPaneChanged { window: "@4", pane: "%9" }));
        assert!(matches!(parse_notif("%session-window-changed $1 @4"),
            Notif::SessionWindowChanged { session: "$1", window: "@4" }));
        assert!(matches!(parse_notif("%pause %9"), Notif::Pause { pane: "%9" }));
        assert!(matches!(parse_notif("%continue %9"), Notif::Continue { pane: "%9" }));
        assert!(matches!(parse_notif("%client-detached client0"), Notif::ClientDetached));
        assert!(matches!(parse_notif("%unlinked-window-add @9"), Notif::Other));
    }

    #[test]
    fn parse_notif_exit_reason_optional() {
        assert!(matches!(parse_notif("%exit"), Notif::Exit { reason: None }));
        assert!(matches!(parse_notif("%exit too far behind"),
            Notif::Exit { reason: Some("too far behind") }));
    }
}
