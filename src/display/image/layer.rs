//! The grid side: sixel strings and kitty graphics commands taken out of a child's
//! output, and the marker cells that place an image at the cursor.
//!
//! A marker cell holds a private-use character `U+F0000 + (row << 8 | col)` naming the
//! cell's place inside its image, a foreground naming the image, and a fixed background
//! that tells it apart from the same character drawn as text (Nerd Font icons live in
//! that plane).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use super::kitty_grid::KittyStore;
use super::sixel::{self, Bitmap};
use super::Caps;

/// The first private-use character a marker uses.
const MARKER_BASE: u32 = 0xF0000;
/// The background every marker cell carries.
pub const MARKER_BG: (u8, u8, u8) = (0x58, 0x4D, 0x58);
/// An image covers at most this many cells per side; the rest is clipped.
pub(super) const MAX_CELLS: usize = 255;
/// The longest sixel string or graphics command kept. A longer one is dropped whole.
pub(super) const MAX_DATA: usize = 32 << 20;

/// Image ids are unique across every grid, so a cell showing image 7 of one session
/// is never taken for image 7 of another.
static NEXT_ID: AtomicU32 = AtomicU32::new(1);

pub(super) fn next_id() -> u32 {
    // Ids live in a 24-bit colour; 0 is never used.
    loop {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed) & 0xFF_FFFF;
        if id != 0 {
            return id;
        }
    }
}

/// The image, row and column a marker cell names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Piece {
    pub id: u32,
    pub row: u8,
    pub col: u8,
}

/// The piece a vt100 cell holds, if it is a marker cell.
pub fn marker(cell: &vt100::Cell) -> Option<Piece> {
    let (r, g, b) = MARKER_BG;
    if cell.bgcolor() != vt100::Color::Rgb(r, g, b) {
        return None;
    }
    let vt100::Color::Rgb(i0, i1, i2) = cell.fgcolor() else {
        return None;
    };
    let mut chars = cell.contents().chars();
    let c = u32::from(chars.next()?);
    if chars.next().is_some() || !(MARKER_BASE..MARKER_BASE + 0x1_0000).contains(&c) {
        return None;
    }
    let off = c - MARKER_BASE;
    Some(Piece {
        id: u32::from(i0) << 16 | u32::from(i1) << 8 | u32::from(i2),
        row: (off >> 8) as u8,
        col: (off & 0xFF) as u8,
    })
}

/// Where the scanner is in the child's output.
#[derive(Default)]
enum Scan {
    #[default]
    Ground,
    /// An ESC ended the last read.
    Esc,
    /// Inside `ESC P`, collecting parameters until the final byte says whether this
    /// is a sixel string.
    Params(Vec<u8>),
    /// Inside a sixel string's data.
    Sixel {
        params: Vec<u8>,
        data: Vec<u8>,
        esc: bool,
    },
    /// Inside an APC string.
    Apc { data: Vec<u8>, esc: bool },
}

/// Where the grid's answers to the child go, in stream order with the parser's own.
pub trait Replies {
    fn reply(&mut self, bytes: &[u8]);
}

impl Replies for () {
    fn reply(&mut self, _: &[u8]) {}
}

/// The images of one grid: the scanner that takes sixel strings and graphics
/// commands out of the output, the decoded sixel images its marker cells name, and
/// the kitty images the child transmitted.
#[derive(Default)]
pub struct ImageLayer {
    scan: Scan,
    images: HashMap<u32, Arc<Bitmap>>,
    pub(super) kitty: KittyStore,
}

impl ImageLayer {
    /// The image a marker names, if the grid still holds it.
    pub fn image(&self, id: u32) -> Option<&Arc<Bitmap>> {
        self.images.get(&id)
    }

    /// The piece a cell shows: a marker naming an image this grid holds.
    pub fn piece(&self, cell: &vt100::Cell) -> Option<Piece> {
        marker(cell).filter(|p| self.images.contains_key(&p.id))
    }

    /// How the frame shows a cell of a kitty image; see [`KittyStore::cell`].
    pub fn kitty_cell(
        &self,
        cell: &vt100::Cell,
        left: Option<super::kitty_grid::KittyCell>,
    ) -> Option<Option<super::kitty_grid::KittyCell>> {
        self.kitty.cell(cell, left)
    }

    /// Every kitty image a cell of `screen` shows, by the id the outer terminal knows
    /// it under, with the cells its placement covers.
    pub fn kitty_in_use(&self, screen: &vt100::Screen) -> Vec<(u32, super::kitty_grid::Placed)> {
        let (rows, cols) = screen.size();
        let mut ids = std::collections::BTreeSet::new();
        for r in 0..rows {
            let mut left = None;
            for c in 0..cols {
                left = screen
                    .cell(r, c)
                    .and_then(|cell| self.kitty.cell(cell, left))
                    .flatten();
                if let Some(k) = left {
                    ids.insert(k.id);
                }
            }
        }
        ids.into_iter()
            .filter_map(|id| Some((id, self.kitty.placed(id)?.clone())))
            .collect()
    }

    /// Drops every image, for a grid that starts over.
    pub fn clear(&mut self) {
        self.images.clear();
        self.kitty = KittyStore::default();
        self.scan = Scan::Ground;
    }

    /// Feeds `bytes` to `parser`, taking out each sixel string and graphics command
    /// the outer terminal can show and placing its image at the cursor where it ended.
    /// Whatever the outer terminal cannot show passes through untouched, and the parser
    /// discards it as it does any DCS or APC string.
    pub fn feed<C: vt100::Callbacks + Replies>(
        &mut self,
        bytes: &[u8],
        caps: Caps,
        parser: &mut vt100::Parser<C>,
    ) {
        let sixel_px = caps.sixel_cell_px();
        if sixel_px.is_none() && !caps.kitty {
            parser.process(bytes);
            return;
        }
        let mut i = 0;
        let mut text_start = 0;
        while i < bytes.len() {
            let b = bytes[i];
            match &mut self.scan {
                Scan::Ground => {
                    if b == 0x1b {
                        parser.process(&bytes[text_start..i]);
                        text_start = i;
                        self.scan = Scan::Esc;
                    }
                    i += 1;
                }
                Scan::Esc => {
                    if b == b'P' && sixel_px.is_some() {
                        self.scan = Scan::Params(Vec::new());
                        i += 1;
                    } else if b == b'_' && caps.kitty {
                        self.scan = Scan::Apc {
                            data: Vec::new(),
                            esc: false,
                        };
                        i += 1;
                        text_start = i;
                    } else {
                        // Not a DCS: the ESC (perhaps held from the last read) goes to
                        // the parser with what follows it.
                        if text_start == i {
                            parser.process(b"\x1b");
                        }
                        self.scan = Scan::Ground;
                    }
                }
                Scan::Params(params) => {
                    if b.is_ascii_digit() || b == b';' {
                        params.push(b);
                        i += 1;
                    } else if b == b'q' {
                        let params = std::mem::take(params);
                        self.scan = Scan::Sixel {
                            params,
                            data: Vec::new(),
                            esc: false,
                        };
                        i += 1;
                        text_start = i;
                    } else {
                        // Another DCS: hand it to the parser from its ESC P on, and
                        // resume scanning at this byte.
                        let mut held = b"\x1bP".to_vec();
                        held.extend_from_slice(params);
                        parser.process(&held);
                        self.scan = Scan::Ground;
                        text_start = i;
                    }
                }
                Scan::Sixel { params, data, esc } => {
                    if *esc {
                        let params = std::mem::take(params);
                        let data = std::mem::take(data);
                        if let Some(cell_px) = sixel_px {
                            self.place(&params, &data, cell_px, parser);
                        }
                        self.scan = Scan::Ground;
                        if b == b'\\' {
                            i += 1;
                            text_start = i;
                        } else {
                            // Any ESC ends a DCS; this one starts the next sequence.
                            self.scan = Scan::Esc;
                            text_start = i;
                        }
                        continue;
                    }
                    match b {
                        0x1b => *esc = true,
                        // CAN and SUB abort the string.
                        0x18 | 0x1a => {
                            self.scan = Scan::Ground;
                            i += 1;
                            text_start = i;
                            continue;
                        }
                        _ if data.len() < MAX_DATA => data.push(b),
                        _ => {}
                    }
                    i += 1;
                    text_start = i;
                }
                Scan::Apc { data, esc } => {
                    if *esc {
                        let data = std::mem::take(data);
                        self.apc(&data, caps, parser);
                        self.scan = Scan::Ground;
                        if b == b'\\' {
                            i += 1;
                        } else {
                            self.scan = Scan::Esc;
                        }
                        text_start = i;
                        continue;
                    }
                    match b {
                        0x1b => *esc = true,
                        0x18 | 0x1a => {
                            self.scan = Scan::Ground;
                            i += 1;
                            text_start = i;
                            continue;
                        }
                        _ if data.len() < MAX_DATA => data.push(b),
                        _ => {}
                    }
                    i += 1;
                    text_start = i;
                }
            }
        }
        if matches!(self.scan, Scan::Ground) && text_start < bytes.len() {
            parser.process(&bytes[text_start..]);
        }
        // An ESC that ended the read stays held until the next one says what it is.
    }

    /// One complete APC string: a graphics command goes to the kitty store, anything
    /// else to the parser, which discards it.
    fn apc<C: vt100::Callbacks + Replies>(
        &mut self,
        data: &[u8],
        caps: Caps,
        parser: &mut vt100::Parser<C>,
    ) {
        match data.split_first() {
            Some((b'G', body)) if data.len() < MAX_DATA => {
                self.kitty.command(body, caps, parser);
            }
            _ => {
                let mut whole = b"\x1b_".to_vec();
                whole.extend_from_slice(data);
                whole.extend_from_slice(b"\x1b\\");
                parser.process(&whole);
            }
        }
    }

    /// Writes the marker cells for one decoded sixel string at the parser's cursor,
    /// and moves the cursor as Windows Terminal does: the screen scrolls until the
    /// image's last sixel band fits, and the cursor ends on that band's top row, in
    /// the column it started in.
    fn place<C: vt100::Callbacks>(
        &mut self,
        params: &[u8],
        data: &[u8],
        (cell_h, cell_w): (u16, u16),
        parser: &mut vt100::Parser<C>,
    ) {
        if data.len() >= MAX_DATA {
            return;
        }
        let bmp = sixel::decode(params, data);
        if bmp.width == 0 || bmp.height == 0 {
            return;
        }
        let (cell_h, cell_w) = (usize::from(cell_h.max(1)), usize::from(cell_w.max(1)));
        let screen = parser.screen();
        let (rows, cols) = screen.size();
        let (rows, cols) = (usize::from(rows), usize::from(cols));
        if rows == 0 || cols == 0 {
            return;
        }
        let (cur_row, cur_col) = screen.cursor_position();
        let (cur_row, cur_col) = (usize::from(cur_row), usize::from(cur_col).min(cols - 1));
        let restore = screen.attributes_formatted();

        let banded = bmp.height.div_ceil(6) * 6;
        let rows_needed = banded.div_ceil(cell_h);
        let scroll = (cur_row + rows_needed).saturating_sub(rows).min(cur_row);
        let top = cur_row - scroll;
        let img_rows = bmp.height.div_ceil(cell_h).min(MAX_CELLS);
        let img_cols = bmp
            .width
            .div_ceil(cell_w)
            .min(MAX_CELLS)
            .min(cols - cur_col);
        let id = next_id();
        let end_row = (top + (banded - 6) / cell_h).min(rows - 1);
        let out = marker_bytes(
            id,
            Area {
                scroll,
                top,
                left: cur_col,
                rows: img_rows,
                cols: img_cols,
                screen_rows: rows,
            },
            (end_row, cur_col),
            &restore,
        );
        parser.process(&out);

        self.images.insert(id, Arc::new(bmp));
        self.retain_shown(parser.screen());
    }

    /// Drops the images no cell of `screen` names any more.
    fn retain_shown(&mut self, screen: &vt100::Screen) {
        let shown = marker_ids(screen);
        self.images.retain(|id, _| shown.contains(id));
    }
}

/// The ids every marker cell of `screen` names.
pub(super) fn marker_ids(screen: &vt100::Screen) -> std::collections::HashSet<u32> {
    let (rows, cols) = screen.size();
    let mut shown = std::collections::HashSet::new();
    for r in 0..rows {
        for c in 0..cols {
            if let Some(p) = screen.cell(r, c).and_then(marker) {
                shown.insert(p.id);
            }
        }
    }
    shown
}

/// Where marker cells go: the lines to scroll first, then the cells from `top`,
/// `left`, clipped to the screen's rows.
pub(super) struct Area {
    pub scroll: usize,
    pub top: usize,
    pub left: usize,
    pub rows: usize,
    pub cols: usize,
    pub screen_rows: usize,
}

/// The bytes that scroll, write image `id`'s marker cells over `area`, then put the
/// cursor at `cursor` and the text attributes back to `restore`.
pub(super) fn marker_bytes(id: u32, area: Area, cursor: (usize, usize), restore: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    if area.scroll > 0 {
        out.extend_from_slice(format!("\x1b[{}S", area.scroll).as_bytes());
    }
    let (r, g, b) = MARKER_BG;
    let sgr = format!(
        "\x1b[0;38;2;{};{};{};48;2;{r};{g};{b}m",
        id >> 16,
        (id >> 8) & 0xFF,
        id & 0xFF
    );
    for row in 0..area.rows.min(MAX_CELLS) {
        if area.top + row >= area.screen_rows {
            break;
        }
        out.extend_from_slice(format!("\x1b[{};{}H", area.top + row + 1, area.left + 1).as_bytes());
        out.extend_from_slice(sgr.as_bytes());
        for col in 0..area.cols.min(MAX_CELLS) {
            let c = char::from_u32(MARKER_BASE + ((row as u32) << 8 | col as u32)).unwrap_or(' ');
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        }
    }
    out.extend_from_slice(format!("\x1b[{};{}H", cursor.0 + 1, cursor.1 + 1).as_bytes());
    out.extend_from_slice(restore);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const CELL: Caps = Caps {
        cell_px: Some((20, 10)),
        sixel: true,
        kitty: false,
    };

    /// A red sixel of `w` x `h` pixels.
    fn sixel(w: usize, h: usize) -> Vec<u8> {
        let mut s = format!("\x1bP0;1;0q\"1;1;{w};{h}#1;2;100;0;0#1").into_bytes();
        let bands = h.div_ceil(6);
        for band in 0..bands {
            let left = h - band * 6;
            let bits = (1u8 << left.min(6)) - 1;
            s.extend_from_slice(format!("!{w}").as_bytes());
            s.push(bits + 0x3f);
            if band + 1 < bands {
                s.push(b'-');
            }
        }
        s.extend_from_slice(b"\x1b\\");
        s
    }

    fn pieces(layer: &ImageLayer, parser: &vt100::Parser) -> Vec<(u16, u16, u8, u8)> {
        let (rows, cols) = parser.screen().size();
        let mut v = Vec::new();
        for r in 0..rows {
            for c in 0..cols {
                if let Some(p) = parser.screen().cell(r, c).and_then(|c| layer.piece(c)) {
                    v.push((r, c, p.row, p.col));
                }
            }
        }
        v
    }

    /// An image lands at the cursor and covers the cells its pixels need; the text
    /// around it stays, and the cursor ends on the last band's top row.
    #[test]
    fn a_sixel_becomes_marker_cells_at_the_cursor() {
        let mut p = vt100::Parser::new(10, 40, 0);
        let mut l = ImageLayer::default();
        let mut bytes = b"\x1b[31mhello\x1b[3;5H".to_vec();
        bytes.extend(sixel(25, 30));
        bytes.extend_from_slice(b"X");
        l.feed(&bytes, CELL, &mut p);
        let got = pieces(&l, &p);
        // 25x30 px over 10x20 px cells: 3 columns, 2 rows.
        // The `X` written at the cursor afterwards takes the second row's first cell.
        let want: Vec<_> = (0..2u16)
            .flat_map(|r| (0..3u16).map(move |c| (2 + r, 4 + c, r as u8, c as u8)))
            .filter(|&(r, c, _, _)| (r, c) != (3, 4))
            .collect();
        assert_eq!(got, want);
        assert!(p.screen().contents().starts_with("hello"));
        // Bands end at 30 px: the last band starts at 24 px, in the image's 2nd row.
        // The text attributes in force before the image still apply.
        let cell = p.screen().cell(3, 4).unwrap();
        assert_eq!(cell.contents(), "X");
        assert_eq!(cell.fgcolor(), vt100::Color::Idx(1));
    }

    /// Text written over part of an image removes those cells only; a clear removes
    /// the image, and the next placement drops it from the store.
    #[test]
    fn writes_and_clears_remove_image_cells() {
        let mut p = vt100::Parser::new(10, 40, 0);
        let mut l = ImageLayer::default();
        let mut bytes = b"\x1b[1;1H".to_vec();
        bytes.extend(sixel(30, 40));
        l.feed(&bytes, CELL, &mut p);
        assert_eq!(pieces(&l, &p).len(), 6);
        l.feed(b"\x1b[1;2Hab", CELL, &mut p);
        assert_eq!(pieces(&l, &p).len(), 4);
        l.feed(b"\x1b[2J", CELL, &mut p);
        assert!(pieces(&l, &p).is_empty());
        let mut again = b"\x1b[5;5H".to_vec();
        again.extend(sixel(10, 20));
        l.feed(&again, CELL, &mut p);
        assert_eq!(l.images.len(), 1, "the cleared image left the store");
    }

    /// The marker cells scroll with the text.
    #[test]
    fn image_cells_scroll_with_the_text() {
        let mut p = vt100::Parser::new(10, 40, 0);
        let mut l = ImageLayer::default();
        let mut bytes = b"\x1b[5;1H".to_vec();
        bytes.extend(sixel(10, 20));
        l.feed(&bytes, CELL, &mut p);
        l.feed(b"\x1b[2S", CELL, &mut p);
        assert_eq!(pieces(&l, &p), vec![(2, 0, 0, 0)]);
    }

    /// An image that does not fit below the cursor scrolls the screen until its last
    /// band fits, as Windows Terminal does.
    #[test]
    fn an_image_at_the_bottom_scrolls_the_screen() {
        let mut p = vt100::Parser::new(10, 40, 0);
        let mut l = ImageLayer::default();
        let mut bytes = b"top\x1b[9;1H".to_vec();
        bytes.extend(sixel(10, 60));
        l.feed(&bytes, CELL, &mut p);
        // 60 px is 3 rows from row 8 of 10: one row of scroll.
        assert_eq!(
            pieces(&l, &p).iter().map(|x| x.0).collect::<Vec<_>>(),
            vec![7, 8, 9]
        );
        assert!(!p.screen().contents().starts_with("top"));
        assert_eq!(p.screen().cursor_position(), (9, 0));
    }

    /// A sixel string split across reads, at every byte, places the same image as one
    /// read; other DCS strings and escapes pass through to the parser.
    #[test]
    fn a_split_sixel_places_the_same_image() {
        let mut bytes = b"a\x1bP$qm\x1b\\b\x1b[2;2H".to_vec();
        bytes.extend(sixel(20, 20));
        bytes.extend_from_slice(b"\x1b[31mc");
        let mut whole = vt100::Parser::new(5, 20, 0);
        let mut lw = ImageLayer::default();
        lw.feed(&bytes, CELL, &mut whole);
        for cut in 1..bytes.len() {
            let mut p = vt100::Parser::new(5, 20, 0);
            let mut l = ImageLayer::default();
            l.feed(&bytes[..cut], CELL, &mut p);
            l.feed(&bytes[cut..], CELL, &mut p);
            assert_eq!(pieces(&l, &p), pieces(&lw, &whole), "cut at {cut}");
            assert_eq!(
                p.screen().contents(),
                whole.screen().contents(),
                "cut at {cut}"
            );
        }
        // The `c` written at the cursor afterwards takes the image's first cell.
        assert_eq!(pieces(&lw, &whole).len(), 1);
        assert_eq!(whole.screen().cell(1, 1).map(|c| c.contents()), Some("c"));
    }

    /// Without a cell size the layer does nothing: no markers, the parser sees the
    /// sixel string as any DCS.
    #[test]
    fn without_a_cell_size_sixel_is_discarded() {
        let mut p = vt100::Parser::new(5, 20, 0);
        let mut l = ImageLayer::default();
        let mut bytes = b"a".to_vec();
        bytes.extend(sixel(20, 20));
        l.feed(&bytes, Caps::default(), &mut p);
        assert!(pieces(&l, &p).is_empty());
        assert_eq!(p.screen().contents().trim(), "a");
    }

    /// A private-use character drawn as text is not a marker.
    #[test]
    fn a_nerd_font_icon_is_not_a_marker() {
        let mut p = vt100::Parser::new(2, 10, 0);
        p.process("\x1b[38;2;0;0;1m\u{F0001}".as_bytes());
        assert_eq!(marker(p.screen().cell(0, 0).unwrap()), None);
    }
}
