//! The frame side: image cells in the ratatui buffer, and the pieces drawn onto the
//! outer terminal after each frame.
//!
//! An image cell carries `CellDiffOption::Skip`, so ratatui never writes it and the
//! outer terminal keeps whatever image the painter drew there. Any widget painted over
//! the terminal view replaces the cell, so after the frame the cells still marked are
//! exactly the image cells the user sees. A cell that stops being an image cell is
//! written by ratatui in the next frame, and a sixel terminal erases the image from a
//! cell written over.

use std::collections::HashMap;
use std::sync::Arc;

use ratatui::buffer::{Buffer, Cell, CellDiffOption};
use ratatui::layout::Rect;
use ratatui::style::Color;

use super::layer::Piece;
use super::sixel::Bitmap;

/// The symbol of an image cell. ratatui never writes it, so no font has to draw it.
const SENTINEL: &str = "\u{10FFFD}";
/// The blue channel of an image cell's background, which carries the piece's row and
/// column in red and green.
const TAG: u8 = 0xA5;

/// Turns `cell` into an image cell showing `piece`.
pub fn mark(cell: &mut Cell, piece: Piece) {
    cell.set_symbol(SENTINEL);
    cell.fg = Color::Rgb(
        (piece.id >> 16) as u8,
        (piece.id >> 8) as u8,
        piece.id as u8,
    );
    cell.bg = Color::Rgb(piece.row, piece.col, TAG);
    cell.set_diff_option(CellDiffOption::Skip);
}

/// Turns `cell` into a kitty Unicode placeholder cell showing `shown`, or into a blank
/// cell for a placeholder cell that names nothing the outer terminal has.
pub fn placeholder(cell: &mut Cell, shown: Option<super::kitty_grid::KittyCell>) {
    cell.reset();
    if let Some(k) = shown {
        cell.set_symbol(&super::kitty::placeholder(k.row, k.col));
        cell.fg = Color::Rgb((k.id >> 16) as u8, (k.id >> 8) as u8, k.id as u8);
    }
}

/// The kitty images the outer terminal holds, each under the id xmux gave it, with the
/// cells of its virtual placement.
#[derive(Default)]
pub struct KittyOuter {
    sent: HashMap<u32, (u16, u16)>,
}

impl KittyOuter {
    /// The bytes that give the outer terminal every image in `need` it does not hold
    /// yet, and free every image it holds that `need` leaves out. Written before the
    /// frame whose placeholder cells name them.
    pub fn sync(&mut self, need: &[(u32, super::kitty_grid::Placed)]) -> Vec<u8> {
        let mut out = Vec::new();
        let wanted: HashMap<u32, (u16, u16)> =
            need.iter().map(|(id, p)| (*id, (p.cols, p.rows))).collect();
        self.sent.retain(|id, size| {
            let keep = wanted.get(id) == Some(size);
            if !keep {
                out.extend(super::kitty::delete(*id));
            }
            keep
        });
        for (id, p) in need {
            if !self.sent.contains_key(id) {
                out.extend(p.image.transmit(*id, p.cols, p.rows));
                self.sent.insert(*id, (p.cols, p.rows));
            }
        }
        out
    }
}

/// The piece an untouched image cell shows.
fn read(cell: &Cell) -> Option<Piece> {
    if cell.diff_option != CellDiffOption::Skip || cell.symbol() != SENTINEL {
        return None;
    }
    let (Color::Rgb(i0, i1, i2), Color::Rgb(row, col, TAG)) = (cell.fg, cell.bg) else {
        return None;
    };
    Some(Piece {
        id: u32::from(i0) << 16 | u32::from(i1) << 8 | u32::from(i2),
        row,
        col,
    })
}

/// What the outer terminal shows of the images, cell by cell, and the bytes that
/// bring it up to date with a frame.
#[derive(Default)]
pub struct Painter {
    shown: HashMap<(u16, u16), Piece>,
    area: Rect,
}

/// A rectangle of cells to draw from one image: screen position, size in cells, and
/// the image cell its top-left shows.
#[derive(Debug, PartialEq, Eq)]
struct Rectangle {
    x: u16,
    y: u16,
    w: u16,
    h: u16,
    row: u8,
    col: u8,
}

impl Painter {
    /// Forgets what the outer terminal shows, after the screen was cleared.
    pub fn forget(&mut self) {
        self.shown.clear();
    }

    /// The bytes that draw each image cell of the completed frame `buf` the outer
    /// terminal does not show yet. `image` resolves an id to its bitmap and
    /// `(cell_h, cell_w)` is the outer terminal's cell size. Empty when nothing changed.
    pub fn paint(
        &mut self,
        buf: &Buffer,
        image: impl Fn(u32) -> Option<Arc<Bitmap>>,
        (cell_h, cell_w): (u16, u16),
    ) -> Vec<u8> {
        if buf.area != self.area {
            // A resize clears the screen.
            self.area = buf.area;
            self.shown.clear();
        }
        let mut now = HashMap::new();
        for y in buf.area.top()..buf.area.bottom() {
            for x in buf.area.left()..buf.area.right() {
                if let Some(p) = read(&buf[(x, y)]) {
                    now.insert((x, y), p);
                }
            }
        }
        // Cells to draw, grouped by image and by where its top-left lands, so one
        // rectangle never mixes two placements of an image.
        let mut todo: HashMap<(u32, i32, i32), Vec<(u16, u16)>> = HashMap::new();
        for (&(x, y), &p) in &now {
            if self.shown.get(&(x, y)) != Some(&p) {
                let origin = (
                    i32::from(x) - i32::from(p.col),
                    i32::from(y) - i32::from(p.row),
                );
                todo.entry((p.id, origin.0, origin.1))
                    .or_default()
                    .push((x, y));
            }
        }
        self.shown = now;
        if todo.is_empty() {
            return Vec::new();
        }
        let (ch, cw) = (usize::from(cell_h.max(1)), usize::from(cell_w.max(1)));
        let bottom = buf.area.bottom();
        let mut out = b"\x1b7".to_vec();
        let mut keys: Vec<_> = todo.keys().copied().collect();
        keys.sort_unstable();
        for key in keys {
            let (id, ox, oy) = key;
            let Some(bmp) = image(id) else { continue };
            for r in rectangles(&todo[&key], ox, oy) {
                let px = usize::from(r.col) * cw;
                let py = usize::from(r.row) * ch;
                let pw = usize::from(r.w) * cw;
                let mut ph = (usize::from(r.h) * ch).min(bmp.height.saturating_sub(py));
                // The last sixel band must end on screen, or the terminal scrolls.
                let room = usize::from(bottom - r.y) * ch;
                if ph.div_ceil(6) * 6 > room {
                    ph = room / 6 * 6;
                }
                if pw == 0 || ph == 0 || px >= bmp.width {
                    continue;
                }
                for row in 0..r.h {
                    out.extend_from_slice(
                        format!("\x1b[{};{}H\x1b[0m\x1b[{}X", r.y + row + 1, r.x + 1, r.w)
                            .as_bytes(),
                    );
                }
                out.extend_from_slice(format!("\x1b[{};{}H", r.y + 1, r.x + 1).as_bytes());
                out.extend(bmp.encode(px, py, pw, ph));
            }
        }
        out.extend_from_slice(b"\x1b8");
        out
    }
}

/// Splits one placement's cells into rectangles: runs along each row, then runs with
/// the same columns on consecutive rows merged.
fn rectangles(cells: &[(u16, u16)], ox: i32, oy: i32) -> Vec<Rectangle> {
    let mut cells = cells.to_vec();
    cells.sort_unstable_by_key(|&(x, y)| (y, x));
    let mut runs: Vec<(u16, u16, u16)> = Vec::new(); // (y, x0, x1 exclusive)
    for (x, y) in cells {
        match runs.last_mut() {
            Some((ry, _, x1)) if *ry == y && *x1 == x => *x1 += 1,
            _ => runs.push((y, x, x + 1)),
        }
    }
    let mut rects: Vec<Rectangle> = Vec::new();
    for (y, x0, x1) in runs {
        let merged = rects
            .iter_mut()
            .find(|r| r.x == x0 && r.w == x1 - x0 && r.y + r.h == y);
        match merged {
            Some(r) => r.h += 1,
            None => rects.push(Rectangle {
                x: x0,
                y,
                w: x1 - x0,
                h: 1,
                row: (i32::from(y) - oy) as u8,
                col: (i32::from(x0) - ox) as u8,
            }),
        }
    }
    rects
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::image::sixel;

    /// A 40x60 px bitmap: 4x3 cells of 10x20 px.
    fn bitmap() -> Arc<Bitmap> {
        let mut data = b"\"1;1;40;60#1;2;100;0;0#1".to_vec();
        data.extend(std::iter::repeat_n(&b"!40~-"[..], 10).flatten());
        Arc::new(sixel::decode(b"0;1", &data))
    }

    fn frame(w: u16, h: u16, at: (u16, u16), cells: (u16, u16), id: u32) -> Buffer {
        let mut buf = Buffer::empty(Rect::new(0, 0, w, h));
        for r in 0..cells.1 {
            for c in 0..cells.0 {
                mark(
                    &mut buf[(at.0 + c, at.1 + r)],
                    Piece {
                        id,
                        row: r as u8,
                        col: c as u8,
                    },
                );
            }
        }
        buf
    }

    fn draws(bytes: &[u8]) -> usize {
        bytes.windows(3).filter(|w| *w == b"\x1bP0").count()
    }

    /// A new image is drawn once, at its cells, and an unchanged frame draws nothing.
    #[test]
    fn an_image_is_drawn_once() {
        let bmp = bitmap();
        let mut p = Painter::default();
        let buf = frame(20, 10, (2, 1), (4, 3), 9);
        let out = p.paint(&buf, |_| Some(bmp.clone()), (20, 10));
        assert_eq!(draws(&out), 1);
        assert!(out.starts_with(b"\x1b7"));
        assert!(out.ends_with(b"\x1b8"));
        let s = String::from_utf8_lossy(&out);
        assert!(s.contains("\x1b[2;3H\x1b[0m\x1b[4X"), "{s}");
        assert!(s.contains("\x1b[2;3H\x1bP0;1;0q\"1;1;40;60"), "{s}");
        assert!(p.paint(&buf, |_| Some(bmp.clone()), (20, 10)).is_empty());
    }

    /// A popup over part of the image leaves its cells to ratatui; the image is drawn
    /// in the rectangles around it, cropped.
    #[test]
    fn an_overlay_splits_the_image_into_pieces() {
        let bmp = bitmap();
        let mut p = Painter::default();
        let mut buf = frame(20, 10, (0, 0), (4, 3), 9);
        buf[(1, 1)] = Cell::new("x");
        let out = p.paint(&buf, |_| Some(bmp.clone()), (20, 10));
        let s = String::from_utf8_lossy(&out);
        // Row 0 whole, row 1 split around the popup cell, row 2 whole.
        assert_eq!(draws(&out), 4, "{s}");
        assert!(s.contains("\"1;1;40;20"), "{s}");
        assert!(s.contains("\"1;1;10;20"), "{s}");
        assert!(s.contains("\"1;1;20;20"), "{s}");
    }

    /// When the popup closes, only the uncovered cell is drawn.
    #[test]
    fn a_closed_overlay_redraws_only_what_it_covered() {
        let bmp = bitmap();
        let mut p = Painter::default();
        let mut covered = frame(20, 10, (0, 0), (4, 3), 9);
        covered[(1, 1)] = Cell::new("x");
        p.paint(&covered, |_| Some(bmp.clone()), (20, 10));
        let out = p.paint(
            &frame(20, 10, (0, 0), (4, 3), 9),
            |_| Some(bmp.clone()),
            (20, 10),
        );
        let s = String::from_utf8_lossy(&out);
        assert_eq!(draws(&out), 1, "{s}");
        assert!(s.contains("\x1b[2;2H\x1bP0;1;0q\"1;1;10;20"), "{s}");
    }

    /// An image on the bottom row is cut to whole sixel bands that fit, so drawing it
    /// never scrolls the terminal.
    #[test]
    fn a_piece_on_the_bottom_row_never_scrolls() {
        let bmp = bitmap();
        let mut p = Painter::default();
        // 17 px cells: three rows are 51 px, whose bands would end at 54 px.
        let out = p.paint(
            &frame(20, 3, (0, 0), (2, 3), 9),
            |_| Some(bmp.clone()),
            (17, 10),
        );
        assert!(String::from_utf8_lossy(&out).contains("\"1;1;20;48"));
    }

    /// A resize clears the screen, so every image cell is drawn again.
    #[test]
    fn a_resize_draws_everything_again() {
        let bmp = bitmap();
        let mut p = Painter::default();
        p.paint(
            &frame(20, 10, (0, 0), (4, 3), 9),
            |_| Some(bmp.clone()),
            (20, 10),
        );
        let out = p.paint(
            &frame(21, 10, (0, 0), (4, 3), 9),
            |_| Some(bmp.clone()),
            (20, 10),
        );
        assert_eq!(draws(&out), 1);
    }
}
