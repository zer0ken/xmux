//! The kitty graphics protocol pieces xmux needs: a command's keys and payload, an
//! image as the child sent it, the bytes that hand an image to the outer terminal, and
//! the Unicode placeholder cells that show it.
//!
//! xmux never decodes an image's pixels. The outer terminal does, and it crops,
//! scrolls and covers the image cell by cell because every image cell reaches it as a
//! placeholder cell in the frame. Only a PNG header is read, for the pixel size.

use std::collections::HashMap;

/// The character every Unicode placeholder cell starts with.
pub const PLACEHOLDER: char = '\u{10EEEE}';

/// The row and column diacritics of a Unicode placeholder cell, in index order, from
/// kitty's `rowcolumn-diacritics.txt`.
const DIACRITICS: [u32; 297] = [
    0x0305, 0x030D, 0x030E, 0x0310, 0x0312, 0x033D, 0x033E, 0x033F, 0x0346, 0x034A, 0x034B, 0x034C,
    0x0350, 0x0351, 0x0352, 0x0357, 0x035B, 0x0363, 0x0364, 0x0365, 0x0366, 0x0367, 0x0368, 0x0369,
    0x036A, 0x036B, 0x036C, 0x036D, 0x036E, 0x036F, 0x0483, 0x0484, 0x0485, 0x0486, 0x0487, 0x0592,
    0x0593, 0x0594, 0x0595, 0x0597, 0x0598, 0x0599, 0x059C, 0x059D, 0x059E, 0x059F, 0x05A0, 0x05A1,
    0x05A8, 0x05A9, 0x05AB, 0x05AC, 0x05AF, 0x05C4, 0x0610, 0x0611, 0x0612, 0x0613, 0x0614, 0x0615,
    0x0616, 0x0617, 0x0657, 0x0658, 0x0659, 0x065A, 0x065B, 0x065D, 0x065E, 0x06D6, 0x06D7, 0x06D8,
    0x06D9, 0x06DA, 0x06DB, 0x06DC, 0x06DF, 0x06E0, 0x06E1, 0x06E2, 0x06E4, 0x06E7, 0x06E8, 0x06EB,
    0x06EC, 0x0730, 0x0732, 0x0733, 0x0735, 0x0736, 0x073A, 0x073D, 0x073F, 0x0740, 0x0741, 0x0743,
    0x0745, 0x0747, 0x0749, 0x074A, 0x07EB, 0x07EC, 0x07ED, 0x07EE, 0x07EF, 0x07F0, 0x07F1, 0x07F3,
    0x0816, 0x0817, 0x0818, 0x0819, 0x081B, 0x081C, 0x081D, 0x081E, 0x081F, 0x0820, 0x0821, 0x0822,
    0x0823, 0x0825, 0x0826, 0x0827, 0x0829, 0x082A, 0x082B, 0x082C, 0x082D, 0x0951, 0x0953, 0x0954,
    0x0F82, 0x0F83, 0x0F86, 0x0F87, 0x135D, 0x135E, 0x135F, 0x17DD, 0x193A, 0x1A17, 0x1A75, 0x1A76,
    0x1A77, 0x1A78, 0x1A79, 0x1A7A, 0x1A7B, 0x1A7C, 0x1B6B, 0x1B6D, 0x1B6E, 0x1B6F, 0x1B70, 0x1B71,
    0x1B72, 0x1B73, 0x1CD0, 0x1CD1, 0x1CD2, 0x1CDA, 0x1CDB, 0x1CE0, 0x1DC0, 0x1DC1, 0x1DC3, 0x1DC4,
    0x1DC5, 0x1DC6, 0x1DC7, 0x1DC8, 0x1DC9, 0x1DCB, 0x1DCC, 0x1DD1, 0x1DD2, 0x1DD3, 0x1DD4, 0x1DD5,
    0x1DD6, 0x1DD7, 0x1DD8, 0x1DD9, 0x1DDA, 0x1DDB, 0x1DDC, 0x1DDD, 0x1DDE, 0x1DDF, 0x1DE0, 0x1DE1,
    0x1DE2, 0x1DE3, 0x1DE4, 0x1DE5, 0x1DE6, 0x1DFE, 0x20D0, 0x20D1, 0x20D4, 0x20D5, 0x20D6, 0x20D7,
    0x20DB, 0x20DC, 0x20E1, 0x20E7, 0x20E9, 0x20F0, 0x2CEF, 0x2CF0, 0x2CF1, 0x2DE0, 0x2DE1, 0x2DE2,
    0x2DE3, 0x2DE4, 0x2DE5, 0x2DE6, 0x2DE7, 0x2DE8, 0x2DE9, 0x2DEA, 0x2DEB, 0x2DEC, 0x2DED, 0x2DEE,
    0x2DEF, 0x2DF0, 0x2DF1, 0x2DF2, 0x2DF3, 0x2DF4, 0x2DF5, 0x2DF6, 0x2DF7, 0x2DF8, 0x2DF9, 0x2DFA,
    0x2DFB, 0x2DFC, 0x2DFD, 0x2DFE, 0x2DFF, 0xA66F, 0xA67C, 0xA67D, 0xA6F0, 0xA6F1, 0xA8E0, 0xA8E1,
    0xA8E2, 0xA8E3, 0xA8E4, 0xA8E5, 0xA8E6, 0xA8E7, 0xA8E8, 0xA8E9, 0xA8EA, 0xA8EB, 0xA8EC, 0xA8ED,
    0xA8EE, 0xA8EF, 0xA8F0, 0xA8F1, 0xAAB0, 0xAAB2, 0xAAB3, 0xAAB7, 0xAAB8, 0xAABE, 0xAABF, 0xAAC1,
    0xFE20, 0xFE21, 0xFE22, 0xFE23, 0xFE24, 0xFE25, 0xFE26, 0x10A0F, 0x10A38, 0x1D185, 0x1D186,
    0x1D187, 0x1D188, 0x1D189, 0x1D1AA, 0x1D1AB, 0x1D1AC, 0x1D1AD, 0x1D242, 0x1D243, 0x1D244,
];

/// The most rows or columns a placeholder cell can name.
pub const MAX_CELLS: u16 = DIACRITICS.len() as u16;

/// The index of a row or column diacritic.
fn diacritic_index(c: char) -> Option<u16> {
    DIACRITICS
        .binary_search(&u32::from(c))
        .ok()
        .map(|i| i as u16)
}

/// The diacritic for row or column `n`.
fn diacritic(n: u16) -> char {
    char::from_u32(DIACRITICS[usize::from(n).min(DIACRITICS.len() - 1)]).unwrap_or(PLACEHOLDER)
}

/// The symbol of a placeholder cell naming `row` and `col` of its image.
pub fn placeholder(row: u16, col: u16) -> String {
    [PLACEHOLDER, diacritic(row), diacritic(col)]
        .iter()
        .collect()
}

/// What a placeholder cell's text names: the row, the column, and the image id's high
/// byte, each when present.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlaceholderText {
    pub row: Option<u16>,
    pub col: Option<u16>,
    pub id_high: Option<u16>,
}

/// Reads a cell's text as a placeholder cell, if it is one.
pub fn read_placeholder(text: &str) -> Option<PlaceholderText> {
    let mut chars = text.chars();
    if chars.next()? != PLACEHOLDER {
        return None;
    }
    let mut marks = chars.map(diacritic_index);
    let mut next = || marks.next().flatten();
    Some(PlaceholderText {
        row: next(),
        col: next(),
        id_high: next(),
    })
}

/// One graphics command: its keys and its payload.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Command {
    pub keys: HashMap<u8, String>,
    pub payload: Vec<u8>,
}

impl Command {
    /// Parses the body of `ESC _ G <body> ESC \`.
    pub fn parse(body: &[u8]) -> Self {
        let (control, payload) = match body.iter().position(|&b| b == b';') {
            Some(i) => (&body[..i], body[i + 1..].to_vec()),
            None => (body, Vec::new()),
        };
        let mut keys = HashMap::new();
        for pair in control.split(|&b| b == b',') {
            if let [k, b'=', v @ ..] = pair {
                keys.insert(*k, String::from_utf8_lossy(v).into_owned());
            }
        }
        Command { keys, payload }
    }

    /// A key's value as a character, or `default`.
    pub fn char(&self, key: u8, default: char) -> char {
        self.keys
            .get(&key)
            .and_then(|v| v.chars().next())
            .unwrap_or(default)
    }

    /// A key's value as a number, or 0.
    pub fn num(&self, key: u8) -> u32 {
        self.keys
            .get(&key)
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    }
}

/// An image as the child transmitted it: its format keys and its base64 payload.
#[derive(Debug, PartialEq, Eq)]
pub struct KittyImage {
    /// `f`: 24 (RGB), 32 (RGBA) or 100 (PNG).
    pub format: u32,
    /// The size in pixels.
    pub width: u32,
    pub height: u32,
    /// `o=z`: the payload is zlib-compressed.
    pub compressed: bool,
    /// The payload as received, still base64.
    pub data: Vec<u8>,
}

impl KittyImage {
    /// The image a completed transmission carries, if its size is known.
    pub fn from_command(cmd: &Command) -> Option<Self> {
        let format = match cmd.num(b'f') {
            0 => 32,
            f => f,
        };
        let (width, height) = if format == 100 {
            png_size(&cmd.payload)?
        } else {
            (cmd.num(b's'), cmd.num(b'v'))
        };
        (width > 0 && height > 0).then(|| KittyImage {
            format,
            width,
            height,
            compressed: cmd.char(b'o', ' ') == 'z',
            data: cmd.payload.clone(),
        })
    }

    /// The bytes that transmit this image to the outer terminal as image `id` and
    /// give it a virtual placement of `cols` x `rows` cells, quietly, in chunks the
    /// protocol allows.
    pub fn transmit(&self, id: u32, cols: u16, rows: u16) -> Vec<u8> {
        const CHUNK: usize = 4096;
        let mut out = Vec::with_capacity(self.data.len() + 256);
        let chunks: Vec<&[u8]> = if self.data.is_empty() {
            vec![&[][..]]
        } else {
            self.data.chunks(CHUNK).collect()
        };
        for (n, chunk) in chunks.iter().enumerate() {
            let more = u8::from(n + 1 < chunks.len());
            if n == 0 {
                let o = if self.compressed { ",o=z" } else { "" };
                out.extend_from_slice(
                    format!(
                        "\x1b_Ga=t,q=2,i={id},f={},s={},v={}{o},m={more};",
                        self.format, self.width, self.height
                    )
                    .as_bytes(),
                );
            } else {
                out.extend_from_slice(format!("\x1b_Gq=2,m={more};").as_bytes());
            }
            out.extend_from_slice(chunk);
            out.extend_from_slice(b"\x1b\\");
        }
        out.extend_from_slice(
            format!("\x1b_Ga=p,U=1,q=2,i={id},c={cols},r={rows}\x1b\\").as_bytes(),
        );
        out
    }
}

/// The bytes that free image `id` on the outer terminal.
pub fn delete(id: u32) -> Vec<u8> {
    format!("\x1b_Ga=d,d=I,q=2,i={id}\x1b\\").into_bytes()
}

/// The width and height a base64 PNG's header declares.
fn png_size(b64: &[u8]) -> Option<(u32, u32)> {
    let head = base64_prefix(b64, 24)?;
    if &head[..8] != b"\x89PNG\r\n\x1a\n" || &head[12..16] != b"IHDR" {
        return None;
    }
    let be = |b: &[u8]| u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
    Some((be(&head[16..20]), be(&head[20..24])))
}

/// The first `n` bytes base64 `data` decodes to.
fn base64_prefix(data: &[u8], n: usize) -> Option<Vec<u8>> {
    let value = |c: u8| -> Option<u32> {
        Some(u32::from(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        }))
    };
    let mut out = Vec::with_capacity(n + 2);
    for quad in data.chunks(4) {
        if out.len() >= n {
            break;
        }
        let mut acc = 0u32;
        let mut bits = 0;
        for &c in quad.iter().take_while(|&&c| c != b'=') {
            acc = acc << 6 | value(c)?;
            bits += 6;
        }
        while bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    (out.len() >= n).then(|| {
        out.truncate(n);
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1x1 PNG, as base64.
    const PNG_1X1: &[u8] = b"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

    #[test]
    fn a_command_splits_into_keys_and_payload() {
        let c = Command::parse(b"a=T,f=100,i=7,q=2;AAAA");
        assert_eq!(c.char(b'a', 't'), 'T');
        assert_eq!(c.num(b'f'), 100);
        assert_eq!(c.num(b'i'), 7);
        assert_eq!(c.payload, b"AAAA");
        assert_eq!(Command::parse(b"a=d").char(b'd', 'a'), 'a');
    }

    #[test]
    fn a_png_header_gives_the_size() {
        assert_eq!(png_size(PNG_1X1), Some((1, 1)));
        let c = Command::parse(&[&b"f=100;"[..], PNG_1X1].concat());
        let img = KittyImage::from_command(&c).unwrap();
        assert_eq!((img.width, img.height), (1, 1));
        assert_eq!(png_size(b"AAAA"), None);
    }

    /// Placeholder text round trips through the diacritic table, and a cell with fewer
    /// diacritics leaves the rest to be inferred.
    #[test]
    fn placeholder_text_round_trips() {
        let s = placeholder(3, 200);
        assert_eq!(
            read_placeholder(&s),
            Some(PlaceholderText {
                row: Some(3),
                col: Some(200),
                id_high: None
            })
        );
        assert_eq!(
            read_placeholder("\u{10EEEE}\u{0305}"),
            Some(PlaceholderText {
                row: Some(0),
                col: None,
                id_high: None
            })
        );
        assert_eq!(read_placeholder("x"), None);
        assert!(
            DIACRITICS.windows(2).all(|w| w[0] < w[1]),
            "sorted for the search"
        );
    }

    /// A transmission larger than one chunk is split, each later chunk carries only
    /// `m`, and the virtual placement follows.
    #[test]
    fn transmit_chunks_and_places() {
        let img = KittyImage {
            format: 32,
            width: 50,
            height: 50,
            compressed: true,
            data: vec![b'A'; 5000],
        };
        let out = String::from_utf8(img.transmit(9, 5, 3)).unwrap();
        assert!(out.starts_with("\x1b_Ga=t,q=2,i=9,f=32,s=50,v=50,o=z,m=1;"));
        assert!(out.contains("\x1b\\\x1b_Gq=2,m=0;"));
        assert!(out.ends_with("\x1b_Ga=p,U=1,q=2,i=9,c=5,r=3\x1b\\"));
    }
}
