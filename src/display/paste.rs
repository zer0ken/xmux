//! Bracketed paste on xmux's own terminal. xmux asks its terminal to wrap every paste
//! in `ESC[200~` and `ESC[201~`, so a paste is told apart from typing before the prefix
//! and the key bindings read it: pasted text is data for wherever it goes, never a
//! command. A session's client gets the markers back only when it enabled bracketed
//! paste itself.

pub const PASTE_START: &[u8] = b"\x1b[200~";
pub const PASTE_END: &[u8] = b"\x1b[201~";

/// One run of a stdin read: typed bytes, or the whole text of one paste.
#[derive(Debug, PartialEq, Eq)]
pub enum Segment {
    Keys(Vec<u8>),
    Paste(Vec<u8>),
}

/// Splits stdin reads into typed bytes and pastes. A paste is held until its end
/// marker arrives, so it is delivered as one piece however the terminal chunked it.
#[derive(Debug, Default)]
pub struct PasteSplitter {
    in_paste: bool,
    /// The paste text so far, or the start of a marker a read ended inside.
    held: Vec<u8>,
}

impl PasteSplitter {
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Segment> {
        let mut data = std::mem::take(&mut self.held);
        // A paste is searched for its end only past what was already searched.
        let mut from = if self.in_paste {
            data.len().saturating_sub(PASTE_END.len() - 1)
        } else {
            0
        };
        data.extend_from_slice(bytes);
        let mut out = Vec::new();
        loop {
            if self.in_paste {
                match find(&data[from..], PASTE_END) {
                    Some(at) => {
                        let end = from + at;
                        out.push(Segment::Paste(data[..end].to_vec()));
                        data.drain(..end + PASTE_END.len());
                        self.in_paste = false;
                        from = 0;
                    }
                    None => {
                        self.held = data;
                        return out;
                    }
                }
            } else {
                match find(&data, PASTE_START) {
                    Some(at) => {
                        if at > 0 {
                            out.push(Segment::Keys(data[..at].to_vec()));
                        }
                        data.drain(..at + PASTE_START.len());
                        self.in_paste = true;
                        from = 0;
                    }
                    None => {
                        // A read that ends inside a start marker keeps that tail for the
                        // next read. Only a tail already past `ESC [ 2` is kept: a shorter
                        // one is as likely a lone Esc or another key, which must not wait.
                        let keep = (3..PASTE_START.len())
                            .rev()
                            .find(|&n| data.ends_with(&PASTE_START[..n]))
                            .unwrap_or(0);
                        let keys = data.len() - keep;
                        if keys > 0 {
                            out.push(Segment::Keys(data[..keys].to_vec()));
                        }
                        self.held = data[keys..].to_vec();
                        return out;
                    }
                }
            }
        }
    }
}

/// The bytes a client reads for a paste of `text`: wrapped in the markers when it
/// enabled bracketed paste, the text alone otherwise.
pub fn for_client(text: &[u8], bracketed: bool) -> Vec<u8> {
    if bracketed {
        [PASTE_START, text, PASTE_END].concat()
    } else {
        text.to_vec()
    }
}

/// Pasted text as a text field takes it: line breaks, tabs, and other control bytes
/// are left out, since a field holds one line and a control byte there is a key.
pub fn field_text(text: &[u8]) -> Vec<u8> {
    text.iter()
        .copied()
        .filter(|&b| b >= 0x20 && b != 0x7f)
        .collect()
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(reads: &[&[u8]]) -> Vec<Segment> {
        let mut splitter = PasteSplitter::default();
        reads.iter().flat_map(|r| splitter.feed(r)).collect()
    }

    #[test]
    fn typing_passes_through_whole() {
        assert_eq!(split(&[b"ls\r"]), vec![Segment::Keys(b"ls\r".to_vec())]);
        assert_eq!(split(&[b"\x1b"]), vec![Segment::Keys(b"\x1b".to_vec())]);
        assert_eq!(split(&[b"\x1b["]), vec![Segment::Keys(b"\x1b[".to_vec())]);
    }

    #[test]
    fn a_paste_is_taken_out_of_the_keys_around_it() {
        assert_eq!(
            split(&[b"a\x1b[200~x\x07y\r\x1b[201~b"]),
            vec![
                Segment::Keys(b"a".to_vec()),
                Segment::Paste(b"x\x07y\r".to_vec()),
                Segment::Keys(b"b".to_vec()),
            ]
        );
    }

    #[test]
    fn a_paste_split_across_reads_arrives_as_one() {
        assert_eq!(
            split(&[b"\x1b[20", b"0~one\r", b"two\x1b[2", b"01~"]),
            vec![Segment::Paste(b"one\rtwo".to_vec())]
        );
    }

    #[test]
    fn an_empty_paste_is_still_a_paste() {
        assert_eq!(
            split(&[b"\x1b[200~\x1b[201~"]),
            vec![Segment::Paste(Vec::new())]
        );
    }

    #[test]
    fn a_client_gets_markers_only_when_it_asked() {
        assert_eq!(for_client(b"a\rb", false), b"a\rb");
        assert_eq!(for_client(b"a\rb", true), b"\x1b[200~a\rb\x1b[201~");
    }

    #[test]
    fn a_field_takes_the_text_without_controls() {
        assert_eq!(
            field_text("dé v\r\n\t\x07\x1b".as_bytes()),
            "dé v".as_bytes()
        );
    }
}
