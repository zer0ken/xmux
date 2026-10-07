//! What the terminal xmux runs in says about itself: its colours, its colour scheme,
//! its cell size in pixels, and whether it has the kitty keyboard protocol.
//!
//! A session's client asks these of its terminal, and its terminal is a grid xmux
//! renders, so the grid answers with what the real terminal answered xmux. xmux asks
//! once, when it takes the terminal over, and its stdin reader removes the replies
//! before they reach the key decoder or a session. A fact the terminal never answered
//! stays unknown, and the grid then leaves the matching query unanswered, as that
//! terminal would.

use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// The queries xmux sends its terminal: the foreground and background colours, the 16
/// ANSI palette slots, the colour scheme, the cell size, and the kitty keyboard
/// protocol's flags, which only a terminal with the protocol answers. The primary device
/// attributes query comes last because every terminal answers it and answers in order,
/// so its reply marks the end of the other replies.
pub const PROBE: &[u8] = b"\x1b]10;?\x1b\\\x1b]11;?\x1b\\\
\x1b]4;0;?;1;?;2;?;3;?;4;?;5;?;6;?;7;?;8;?;9;?;10;?;11;?;12;?;13;?;14;?;15;?\x1b\\\
\x1b[?996n\x1b[16t\x1b[?u\x1b[c";

/// How long the stdin reader keeps an incomplete reply waiting for the rest of it. The
/// probe ends earlier, at the reply to the primary device attributes query.
const PROBE_WINDOW: Duration = Duration::from_secs(2);

/// The facts the terminal answered. Colours keep the terminal's own spec string (for
/// example `rgb:1e1e/1e1e/2e2e`), so a client reads exactly what the terminal said.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OuterTerminal {
    pub foreground: Option<String>,
    pub background: Option<String>,
    pub palette: [Option<String>; 16],
    /// The `CSI ? 997 ; Ps n` value: 1 dark, 2 light.
    pub scheme: Option<u8>,
    /// One cell as `(height, width)` in pixels.
    pub cell_px: Option<(u16, u16)>,
    /// Whether the primary device attributes list sixel graphics (attribute 4).
    pub sixel: bool,
}

impl OuterTerminal {
    /// The colour scheme the terminal reported, or else the one its background colour
    /// implies.
    pub fn colour_scheme(&self) -> Option<u8> {
        self.scheme.or_else(|| {
            let (r, g, b) = parse_rgb(self.background.as_deref()?)?;
            let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
            Some(if luma < 0.5 { 1 } else { 2 })
        })
    }
}

static OUTER: Mutex<OuterTerminal> = Mutex::new(OuterTerminal {
    foreground: None,
    background: None,
    palette: [const { None }; 16],
    scheme: None,
    cell_px: None,
    sixel: false,
});

fn outer_mut() -> MutexGuard<'static, OuterTerminal> {
    OUTER.lock().unwrap_or_else(|e| e.into_inner())
}

/// A copy of what the terminal has answered so far.
pub fn outer() -> OuterTerminal {
    outer_mut().clone()
}

/// An `rgb:R/G/B` spec (1 to 4 hex digits per channel) as channel fractions.
fn parse_rgb(spec: &str) -> Option<(f64, f64, f64)> {
    let mut parts = spec.strip_prefix("rgb:")?.split('/');
    let mut channel = || {
        let hex = parts.next()?;
        if hex.is_empty() || hex.len() > 4 {
            return None;
        }
        let v = u32::from_str_radix(hex, 16).ok()?;
        Some(f64::from(v) / f64::from((1u32 << (4 * hex.len())) - 1))
    };
    let rgb = (channel()?, channel()?, channel()?);
    parts.next().is_none().then_some(rgb)
}

/// One complete reply found at the start of a byte slice.
enum Reply {
    /// The reply is complete and `len` bytes long.
    Whole(usize),
    /// The bytes so far are the start of a reply.
    Partial,
    /// The bytes are not a reply.
    No,
}

/// Removes the terminal's replies to [`PROBE`] from stdin reads and records what they
/// say. Bytes that are not a reply pass through in order.
pub struct ReplyFilter {
    /// Until the primary device attributes reply arrives (or the window closes), an
    /// incomplete reply at the end of a read waits for the next read.
    probing_until: Option<Instant>,
    held: Vec<u8>,
}

impl ReplyFilter {
    /// A filter for a probe sent now.
    pub fn new() -> Self {
        Self {
            probing_until: Some(Instant::now() + PROBE_WINDOW),
            held: Vec::new(),
        }
    }

    /// The bytes of `read` that are not replies to the probe.
    pub fn filter(&mut self, read: &[u8]) -> Vec<u8> {
        let probing = self.probing_until.is_some_and(|t| Instant::now() < t);
        if !probing {
            self.probing_until = None;
        }
        let mut data = std::mem::take(&mut self.held);
        data.extend_from_slice(read);
        let mut out = Vec::with_capacity(data.len());
        let mut i = 0;
        while i < data.len() {
            if data[i] != 0x1b {
                out.push(data[i]);
                i += 1;
                continue;
            }
            match self.reply_at(&data[i..], probing) {
                Reply::Whole(len) => i += len,
                Reply::Partial if probing => {
                    self.held = data[i..].to_vec();
                    break;
                }
                Reply::Partial | Reply::No => {
                    out.push(data[i]);
                    i += 1;
                }
            }
        }
        out
    }

    /// Classifies the bytes at the start of `s` (which starts with ESC), recording a
    /// whole reply. A primary device attributes reply counts only while probing,
    /// since only the probe asked for one.
    fn reply_at(&mut self, s: &[u8], probing: bool) -> Reply {
        if s.len() < 2 {
            return Reply::Partial;
        }
        match s[1] {
            b']' => osc_reply(s),
            b'[' => match csi_reply(s) {
                Some((len, body, final_byte)) => {
                    if record_csi(body, final_byte) {
                        Reply::Whole(len)
                    } else if probing && final_byte == b'u' && keyboard_flags_reply(body) {
                        crate::display::keyboard::record_support(true);
                        Reply::Whole(len)
                    } else if probing && final_byte == b'c' && body.first() == Some(&b'?') {
                        // Every reply comes before this one, so a terminal that has not
                        // answered the keyboard flags by now has no such protocol.
                        crate::display::keyboard::record_support(false);
                        self.probing_until = None;
                        outer_mut().sixel = body[1..].split(|&b| b == b';').any(|a| a == b"4");
                        Reply::Whole(len)
                    } else {
                        Reply::No
                    }
                }
                None if s[2..]
                    .iter()
                    .all(|b| b.is_ascii_digit() || b";?".contains(b)) =>
                {
                    Reply::Partial
                }
                None => Reply::No,
            },
            _ => Reply::No,
        }
    }
}

impl Default for ReplyFilter {
    fn default() -> Self {
        Self::new()
    }
}

/// An `ESC ] 10|11|4 ; ...` colour reply, terminated by BEL or ST.
fn osc_reply(s: &[u8]) -> Reply {
    let body_start = 2;
    let mut end = None;
    for (j, &b) in s.iter().enumerate().skip(body_start) {
        if b == 0x07 {
            end = Some((j, j + 1));
            break;
        }
        if b == 0x1b {
            if s.get(j + 1) == Some(&b'\\') {
                end = Some((j, j + 2));
            } else if j + 1 == s.len() {
                break;
            } else {
                return Reply::No;
            }
            break;
        }
        if !(0x20..0x7f).contains(&b) {
            return Reply::No;
        }
    }
    let Some((body_end, len)) = end else {
        let body = &s[body_start..];
        let starts = |p: &[u8]| body.starts_with(p) || p.starts_with(body);
        return if starts(b"10;rgb:") || starts(b"11;rgb:") || starts(b"4;") {
            Reply::Partial
        } else {
            Reply::No
        };
    };
    let Ok(body) = std::str::from_utf8(&s[body_start..body_end]) else {
        return Reply::No;
    };
    let mut fields = body.split(';');
    let mut o = outer_mut();
    match fields.next() {
        Some("10") | Some("11") => {
            let which = &body[..2];
            let Some(spec) = fields.next().filter(|v| v.starts_with("rgb:")) else {
                return Reply::No;
            };
            let slot = if which == "10" {
                &mut o.foreground
            } else {
                &mut o.background
            };
            *slot = Some(spec.to_string());
        }
        Some("4") => {
            let rest: Vec<&str> = fields.collect();
            if rest.is_empty() || !rest.len().is_multiple_of(2) {
                return Reply::No;
            }
            for pair in rest.chunks(2) {
                let (Ok(n), spec) = (pair[0].parse::<usize>(), pair[1]) else {
                    return Reply::No;
                };
                if !spec.starts_with("rgb:") {
                    return Reply::No;
                }
                if let Some(slot) = o.palette.get_mut(n) {
                    *slot = Some(spec.to_string());
                }
            }
        }
        _ => return Reply::No,
    }
    Reply::Whole(len)
}

/// Whether a `u` reply's parameters are the kitty keyboard flags reply `? flags`.
fn keyboard_flags_reply(body: &[u8]) -> bool {
    body.strip_prefix(b"?")
        .is_some_and(|flags| !flags.is_empty() && flags.iter().all(u8::is_ascii_digit))
}

/// A complete CSI at the start of `s` as `(length, parameter bytes, final byte)`.
fn csi_reply(s: &[u8]) -> Option<(usize, &[u8], u8)> {
    let j = s.iter().skip(2).position(|b| (0x40..=0x7e).contains(b))? + 2;
    Some((j + 1, &s[2..j], s[j]))
}

/// Records a colour scheme or cell size reply, returning whether `body` and
/// `final_byte` were one.
fn record_csi(body: &[u8], final_byte: u8) -> bool {
    let Ok(body) = std::str::from_utf8(body) else {
        return false;
    };
    let nums = |s: &str| -> Option<Vec<u16>> { s.split(';').map(|p| p.parse().ok()).collect() };
    match final_byte {
        b'n' => match body.strip_prefix('?').and_then(nums).as_deref() {
            Some([997, scheme @ (1 | 2)]) => {
                outer_mut().scheme = Some(*scheme as u8);
                true
            }
            _ => false,
        },
        b't' => match nums(body).as_deref() {
            Some([6, h, w]) => {
                if *h > 0 && *w > 0 {
                    outer_mut().cell_px = Some((*h, *w));
                }
                true
            }
            _ => false,
        },
        _ => false,
    }
}

#[cfg(test)]
pub(crate) fn set_outer_for_test(facts: OuterTerminal) {
    *outer_mut() = facts;
}

/// Serializes the tests that read or write the process-wide terminal facts.
#[cfg(test)]
pub(crate) static TEST_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    fn lock() -> MutexGuard<'static, ()> {
        TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A terminal answers the whole probe in one read, with keys the user typed around
    /// it: the replies are removed and recorded, and the keys pass through in order.
    #[test]
    fn the_filter_removes_and_records_the_probe_replies() {
        let _g = lock();
        set_outer_for_test(OuterTerminal::default());
        let mut f = ReplyFilter::new();
        let read = b"a\x1b]10;rgb:cccc/cccc/cccc\x1b\\\x1b]11;rgb:1e1e/1e1e/2e2e\x07\
\x1b]4;1;rgb:cd/00/00\x1b\\\x1b[?997;1n\x1b[6;18;9t\x1b[?62;22cb";
        assert_eq!(f.filter(read), b"ab");
        let o = outer();
        assert_eq!(o.foreground.as_deref(), Some("rgb:cccc/cccc/cccc"));
        assert_eq!(o.background.as_deref(), Some("rgb:1e1e/1e1e/2e2e"));
        assert_eq!(o.palette[1].as_deref(), Some("rgb:cd/00/00"));
        assert_eq!(o.scheme, Some(1));
        assert_eq!(o.cell_px, Some((18, 9)));
        assert!(!o.sixel, "the device attributes list no sixel");
        // The probe ended at the device attributes reply: a later one is a key sequence
        // xmux did not ask for, and passes through.
        assert_eq!(f.filter(b"\x1b[?62c"), b"\x1b[?62c");
    }

    /// A device attributes reply listing attribute 4 records sixel support.
    #[test]
    fn the_filter_records_sixel_from_the_device_attributes() {
        let _g = lock();
        set_outer_for_test(OuterTerminal::default());
        let mut f = ReplyFilter::new();
        let read = b"\x1b[6;20;10t\x1b[?61;4;6;7;14;21;22;23;24;28;32;42;52c";
        assert_eq!(f.filter(read), b"");
        assert!(outer().sixel);
        assert_eq!(outer().cell_px, Some((20, 10)));
    }

    /// A terminal with the kitty keyboard protocol answers its flags query before the
    /// device attributes, and one without answers only the latter.
    #[test]
    fn the_filter_reads_whether_the_terminal_has_the_keyboard_protocol() {
        let mut f = ReplyFilter::new();
        assert_eq!(f.filter(b"\x1b[?0u\x1b[?62;22c"), b"");
        assert!(crate::display::keyboard::supported());
        let without = std::thread::spawn(|| {
            let mut f = ReplyFilter::new();
            let keys = f.filter(b"\x1b[?62;22cq");
            (keys, crate::display::keyboard::supported())
        });
        assert_eq!(without.join().unwrap(), (b"q".to_vec(), false));
    }

    /// Sixteen palette replies outgrow one stdin read, so a reply arrives split: the
    /// first part waits for the rest instead of reaching a session as typed text.
    #[test]
    fn a_split_reply_waits_for_its_rest_while_probing() {
        let _g = lock();
        set_outer_for_test(OuterTerminal::default());
        let mut f = ReplyFilter::new();
        assert_eq!(f.filter(b"x\x1b]4;2;rgb:00/cd"), b"x");
        assert_eq!(f.filter(b"/00\x1b\\y"), b"y");
        assert_eq!(outer().palette[2].as_deref(), Some("rgb:00/cd/00"));
    }

    /// Keys are never mistaken for replies: arrows, Alt+] and Esc pass through.
    #[test]
    fn keys_pass_through_the_filter() {
        let _g = lock();
        let mut f = ReplyFilter::new();
        assert_eq!(f.filter(b"\x1b[A\x1b]x\x1bq"), b"\x1b[A\x1b]x\x1bq");
        f.probing_until = None;
        assert_eq!(f.filter(b"\x1b"), b"\x1b");
        assert_eq!(f.filter(b"\x1b]4;"), b"\x1b]4;");
    }

    #[test]
    fn the_scheme_follows_the_background_when_not_reported() {
        let mut o = OuterTerminal {
            background: Some("rgb:ffff/ffff/ffff".into()),
            ..Default::default()
        };
        assert_eq!(o.colour_scheme(), Some(2));
        o.background = Some("rgb:00/00/00".into());
        assert_eq!(o.colour_scheme(), Some(1));
        o.scheme = Some(2);
        assert_eq!(o.colour_scheme(), Some(2), "a reported scheme wins");
        assert_eq!(OuterTerminal::default().colour_scheme(), None);
    }
}
