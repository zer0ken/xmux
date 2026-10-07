//! Underline SGR compatibility at the output pump, before unsupported colour payloads
//! can change the vt100 grid's text attributes.

use super::grid::Grid;

const MAX_CSI: usize = 1024;

#[derive(Default)]
enum State {
    #[default]
    Ground,
    Escape,
    Csi,
    String {
        osc: bool,
        escape: bool,
    },
    LongCsi,
}

#[derive(Default)]
pub(super) struct SgrNormalizer {
    state: State,
    pending: Vec<u8>,
    output: Vec<u8>,
}

impl SgrNormalizer {
    /// Feeds one pump read to the grid, retaining incomplete CSI sequences between reads.
    pub fn feed(&mut self, grid: &mut Grid, bytes: &[u8]) {
        self.output.clear();
        for &byte in bytes {
            match &mut self.state {
                State::Ground if byte == 0x1b => {
                    self.pending.push(byte);
                    self.state = State::Escape;
                }
                State::Ground => self.output.push(byte),
                State::Escape if byte == b'[' => {
                    self.pending.push(byte);
                    self.state = State::Csi;
                }
                State::Escape if byte == 0x1b => {
                    self.output.push(byte);
                }
                State::Escape if (byte < 0x20 || byte == 0x7f) && !matches!(byte, 0x18 | 0x1a) => {
                    self.output.push(byte);
                }
                State::Escape => {
                    self.output.append(&mut self.pending);
                    self.output.push(byte);
                    self.state = match byte {
                        b']' | b'P' | b'_' | b'^' | b'X' => State::String {
                            osc: byte == b']',
                            escape: false,
                        },
                        _ => State::Ground,
                    };
                }
                State::Csi if byte == 0x1b => {
                    self.output.append(&mut self.pending);
                    self.pending.push(byte);
                    self.state = State::Escape;
                }
                State::Csi if matches!(byte, 0x18 | 0x1a) => {
                    self.output.append(&mut self.pending);
                    self.output.push(byte);
                    self.state = State::Ground;
                }
                State::Csi if (0x40..=0x7e).contains(&byte) => {
                    self.pending.push(byte);
                    if byte == b'm' {
                        normalize_sgr(&self.pending, &mut self.output);
                    } else {
                        self.output.extend_from_slice(&self.pending);
                    }
                    self.pending.clear();
                    self.state = State::Ground;
                }
                State::Csi if byte < 0x20 || byte == 0x7f => self.output.push(byte),
                State::Csi => {
                    self.pending.push(byte);
                    if self.pending.len() >= MAX_CSI {
                        self.output.append(&mut self.pending);
                        self.state = State::LongCsi;
                    }
                }
                State::String { osc, escape } => {
                    self.output.push(byte);
                    if (*osc && byte == 0x07)
                        || (*escape && byte == b'\\')
                        || matches!(byte, 0x18 | 0x1a)
                    {
                        self.state = State::Ground;
                    } else {
                        *escape = byte == 0x1b;
                    }
                }
                State::LongCsi => {
                    self.output.push(byte);
                    if byte == 0x1b {
                        self.output.pop();
                        self.pending.push(byte);
                        self.state = State::Escape;
                    } else if (0x40..=0x7e).contains(&byte) || matches!(byte, 0x18 | 0x1a) {
                        self.state = State::Ground;
                    }
                }
            }
        }
        grid.feed(&self.output);
    }
}

fn number(bytes: &[u8]) -> Option<u16> {
    std::str::from_utf8(bytes).ok()?.parse().ok()
}

/// The number of semicolon fields in an extended colour, including its selector.
fn colour_fields(fields: &[&[u8]]) -> usize {
    match fields.get(1).and_then(|field| number(field)) {
        Some(2) => 5,
        Some(5) => 3,
        _ => 1,
    }
    .min(fields.len())
}

fn normalize_sgr(sequence: &[u8], output: &mut Vec<u8>) {
    let params = &sequence[2..sequence.len() - 1];
    if !params
        .iter()
        .all(|byte| byte.is_ascii_digit() || matches!(byte, b';' | b':'))
    {
        output.extend_from_slice(sequence);
        return;
    }
    let fields: Vec<_> = params.split(|&byte| byte == b';').collect();
    let mut kept: Vec<&[u8]> = Vec::new();
    let mut index = 0;
    while index < fields.len() {
        let field = fields[index];
        let mut parts = field.splitn(2, |&byte| byte == b':');
        let code = number(parts.next().unwrap_or_default());
        let subparams = parts.next();
        match (code, subparams) {
            (Some(58), None) => index += colour_fields(&fields[index..]),
            (Some(58 | 59), _) => index += 1,
            (Some(4), Some(style)) if matches!(number(style), Some(0..=5)) => {
                kept.push(if number(style) == Some(0) {
                    b"24"
                } else {
                    b"4"
                });
                index += 1;
            }
            // Foreground and background payload values are colours, not SGR commands.
            (Some(38 | 48), None) => {
                let end = index + colour_fields(&fields[index..]);
                kept.extend_from_slice(&fields[index..end]);
                index = end;
            }
            _ => {
                kept.push(field);
                index += 1;
            }
        }
    }
    // A colour-only underline command is a no-op, not an empty SGR (a full reset).
    if !kept.is_empty() {
        output.extend_from_slice(b"\x1b[");
        for (index, field) in kept.iter().enumerate() {
            if index != 0 {
                output.push(b';');
            }
            output.extend_from_slice(field);
        }
        output.push(b'm');
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::{Color, Modifier};

    fn normalized(bytes: &[u8], chunk_size: usize) -> Vec<u8> {
        let mut grid = Grid::new(2, 16);
        let mut sgr = SgrNormalizer::default();
        let mut output = Vec::new();
        for chunk in bytes.chunks(chunk_size) {
            sgr.feed(&mut grid, chunk);
            output.extend_from_slice(&sgr.output);
        }
        output
    }

    #[test]
    fn consumes_whole_underline_colours_and_keeps_other_colour_payloads() {
        let cases: &[(&[u8], &[u8])] = &[
            (
                b"\x1b[1;38;5;58;48;2;58;59;4;58;2;0;3;4;59;4:3m",
                b"\x1b[1;38;5;58;48;2;58;59;4;4m",
            ),
            (b"\x1b[58:2::0:128:255;59m", b""),
            (b"\x1b[58:5:0;58;5;59m", b""),
            (b"\x1b[4:0;1;31;44m", b"\x1b[24;1;31;44m"),
            (b"\x1b[58;5;0;0m", b"\x1b[0m"),
            (b"\x1b[58;2;0;1;2;m", b"\x1b[m"),
            (b"\x1b[59m", b""),
            (b"\x1b[058;5;0;004:03m", b"\x1b[4m"),
        ];
        for &(input, expected) in cases {
            for chunk_size in 1..=input.len() {
                assert_eq!(normalized(input, chunk_size), expected, "{input:?}");
            }
        }
    }

    #[test]
    fn every_read_boundary_preserves_bold_colours_and_underline_fallback() {
        let bytes = b"\x1b[1;38;2;11;22;33;48;5;77m\x1b[4:3;58;2;0;9;8;59mX\x1b[4:0;58:5:0mY";
        for split in 0..=bytes.len() {
            let mut grid = Grid::new(1, 16);
            let mut sgr = SgrNormalizer::default();
            sgr.feed(&mut grid, &bytes[..split]);
            sgr.feed(&mut grid, &bytes[split..]);
            let area = Rect::new(0, 0, 16, 1);
            let mut cells = Buffer::empty(area);
            grid.render_into(&mut cells, area);
            for x in 0..2 {
                let cell = &cells[(x, 0)];
                assert_eq!(cell.symbol(), if x == 0 { "X" } else { "Y" });
                assert!(cell.modifier.contains(Modifier::BOLD), "split={split}");
                assert!(!cell.modifier.contains(Modifier::DIM), "split={split}");
                assert_eq!(cell.modifier.contains(Modifier::UNDERLINED), x == 0);
                assert_eq!(cell.fg, Color::Rgb(11, 22, 33));
                assert_eq!(cell.bg, Color::Indexed(77));
            }
        }
    }

    #[test]
    fn leaves_other_control_sequences_and_string_payloads_untouched() {
        let bytes = b"hi\x1b[58;5;0H\x1b[38:2::1:2:3;4:6;48:5:59m\x1b]0;\x1b[59m\x07\x1bPq\x1b[58;5;0m\x1b\\\x1b_Gx;\x1b[4:0m\x1b\\\x1b[58;2;0\x18bye\x1b[1\x1b[31mX\x1bc";
        for chunk_size in 1..=bytes.len() {
            assert_eq!(normalized(bytes, chunk_size), bytes, "chunk={chunk_size}");
        }
    }

    #[test]
    fn csi_buffer_is_bounded_and_an_overlong_sequence_does_not_hide_following_text() {
        let bytes = [
            b"\x1b[".as_slice(),
            &vec![b'9'; MAX_CSI * 2],
            b"mtext\x1b[4:2mX",
        ]
        .concat();
        let expected = [
            b"\x1b[".as_slice(),
            &vec![b'9'; MAX_CSI * 2],
            b"mtext\x1b[4mX",
        ]
        .concat();
        assert_eq!(normalized(&bytes, 1), expected);
    }

    #[test]
    fn controls_inside_escape_sequences_keep_the_underline_colour_a_noop() {
        assert_eq!(normalized(b"\x1b\x07[58;2;0;1;2mX", 1), b"\x07X");
        assert_eq!(normalized(b"\x1b[58;5;\x7f0mX", 1), b"\x7fX");
    }
}
