//! A one-pane vt100 grid the proxy tees child output into, used ONLY to repaint
//! the live pane after a transient overlay. Not a multiplexer: one grid, no
//! layouts, no input routing.
use std::hash::{Hash, Hasher};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color as RColor, Modifier, Style};

pub struct Grid {
    parser: vt100::Parser,
}

impl Grid {
    pub fn new(rows: u16, cols: u16) -> Self {
        Self {
            parser: vt100::Parser::new(rows, cols, 0),
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        // vt100 0.16.2 panics (screen.rs `Screen::text` unwrap on None) when a wide
        // (CJK) glyph lands on the last column in some cursor states — common after a
        // grid shrink. Catch it so the PTY pump thread survives; reset the parser so
        // the next mux repaint refills the grid cleanly instead of re-panicking on the
        // same stale cursor.
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.parser.process(bytes);
        }));
        if res.is_err() {
            let (rows, cols) = self.parser.screen().size();
            self.parser = vt100::Parser::new(rows, cols, 0);
        }
    }

    /// Wipes the grid to a blank slate (a fresh parser at the same size). Used when
    /// the displayed session/window switches so stale cells from the previous
    /// content never linger behind the new repaint — the mux sends a full redraw on
    /// switch-client / select-window, so the cleared grid fills with the new content
    /// rather than leaving residue.
    pub fn clear(&mut self) {
        let (rows, cols) = self.parser.screen().size();
        self.parser = vt100::Parser::new(rows, cols, 0);
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.parser.screen_mut().set_size(rows, cols);
    }

    /// The vt100 cursor as ratatui `(x, y)` (col, row), clamped to the grid.
    pub fn cursor(&self) -> (u16, u16) {
        let screen = self.parser.screen();
        let (rows, cols) = screen.size();
        let (row, col) = screen.cursor_position();
        (
            col.min(cols.saturating_sub(1)),
            row.min(rows.saturating_sub(1)),
        )
    }

    /// Whether the child has hidden its cursor.
    pub fn hide_cursor(&self) -> bool {
        self.parser.screen().hide_cursor()
    }

    /// Whether the grid has no visible content (all blank) — used to diagnose an
    /// attachment whose PTY child has not produced output yet.
    pub fn is_blank(&self) -> bool {
        self.parser.screen().contents().trim().is_empty()
    }

    /// A cheap, stable hash of the visible cell contents. Changes if and only if the
    /// rendered text changes — used to detect whether a display transition actually
    /// produced a different screen, so a `display_show decision=switch` not followed
    /// by a `display_grid_changed` event indicates the mux switch had no visible effect.
    pub fn fingerprint(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.parser.screen().contents().hash(&mut h);
        h.finish()
    }

    /// Writes a top-left clip of the grid into `area` of `buf`, mapping each
    /// vt100 cell's symbol + colours + attrs to a ratatui cell. Cells past the
    /// grid size or `area` are skipped (the terminal view in Focus::Tree is narrower
    /// than the grid, so it shows a top-left clip).
    pub fn render_into(&self, buf: &mut Buffer, area: Rect) {
        let screen = self.parser.screen();
        let (grid_rows, grid_cols) = screen.size();
        let rows = area.height.min(grid_rows);
        let cols = area.width.min(grid_cols);
        for r in 0..rows {
            for c in 0..cols {
                let Some(vcell) = screen.cell(r, c) else {
                    continue;
                };
                let cell = &mut buf[(area.x + c, area.y + r)];
                if vcell.is_wide() && c + 1 >= cols {
                    // A double-width char whose second half falls outside the
                    // clipped pane would overflow the right edge and wrap to col 0
                    // of the next line; blank it so the pane stays aligned.
                    cell.set_symbol(" ");
                } else if vcell.has_contents() {
                    cell.set_symbol(vcell.contents());
                } else {
                    cell.set_symbol(" ");
                }
                cell.set_style(vt_cell_style(vcell));
                if vcell.is_wide_continuation() {
                    // ratatui's incremental diff skips the trailing cell of a
                    // standard wide (CJK) glyph, so a wide→narrow transition leaves
                    // the old glyph's right half as background residue on the
                    // terminal. Marking the trailing cell AlwaysUpdate makes it
                    // differ from any later narrow cell at this column, forcing the
                    // diff to repaint it on transition — no full-screen clear, so no
                    // flash. While the wide glyph is stable the diff skips this cell
                    // via the leading cell's width, so it never redraws needlessly.
                    cell.set_diff_option(ratatui::buffer::CellDiffOption::AlwaysUpdate);
                }
            }
        }
    }
}

/// Maps a vt100 colour to a ratatui colour. `Default` → `Reset` (terminal
/// default), `Idx` → 256-colour index, `Rgb` → true colour.
pub fn vt_color_to_ratatui(c: vt100::Color) -> RColor {
    match c {
        vt100::Color::Default => RColor::Reset,
        vt100::Color::Idx(i) => RColor::Indexed(i),
        vt100::Color::Rgb(r, g, b) => RColor::Rgb(r, g, b),
    }
}

/// Maps a vt100 cell's colours and attributes to a ratatui `Style`.
fn vt_cell_style(cell: &vt100::Cell) -> Style {
    let mut style = Style::default()
        .fg(vt_color_to_ratatui(cell.fgcolor()))
        .bg(vt_color_to_ratatui(cell.bgcolor()));
    let mut m = Modifier::empty();
    if cell.bold() {
        m |= Modifier::BOLD;
    }
    if cell.italic() {
        m |= Modifier::ITALIC;
    }
    if cell.underline() {
        m |= Modifier::UNDERLINED;
    }
    if cell.inverse() {
        m |= Modifier::REVERSED;
    }
    style.add_modifier = m;
    style
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::Color as RColor;

    #[test]
    fn color_mapping_covers_default_idx_rgb() {
        assert_eq!(vt_color_to_ratatui(vt100::Color::Default), RColor::Reset);
        assert_eq!(
            vt_color_to_ratatui(vt100::Color::Idx(4)),
            RColor::Indexed(4)
        );
        assert_eq!(
            vt_color_to_ratatui(vt100::Color::Rgb(10, 20, 30)),
            RColor::Rgb(10, 20, 30)
        );
    }

    #[test]
    fn clear_blanks_the_grid() {
        // On a session/window switch the grid is wiped so no stale cells linger
        // behind the mux's fresh repaint.
        let mut g = Grid::new(24, 80);
        g.feed(b"residue content that must vanish");
        assert!(!g.is_blank(), "precondition: grid has content");
        g.clear();
        assert!(g.is_blank(), "clear wipes all visible content");
    }

    // NOTE: this test deliberately triggers the vt100 panic that Grid::feed catches, so
    // `cargo test` prints one "thread panicked at vt100 ... screen.rs" line to stderr —
    // expected, not a failure. (The hook is not silenced here because it is process-
    // global and tests run in parallel.)
    #[test]
    fn feed_survives_wide_char_at_last_column() {
        // Regression: vt100 0.16.2 panics (drawing_cell_mut(col+1).unwrap() on None) when
        // a wide CJK glyph prints on the last column — observed crashing the PTY pump
        // thread. Grid::feed must catch+recover so the pump survives and the grid stays
        // usable (a subsequent repaint lands).
        let mut g = Grid::new(1, 4);
        g.feed(b"\x1b[1;3H"); // cursor to 0-based col 2
        g.feed("한".as_bytes()); // wide glyph occupies cols 2-3 (the right edge)
        g.resize(1, 3); // shrink → the wide glyph's second half (col 3) is truncated
        g.feed(b"\x1b[1;3HX"); // overwrite the now-edge wide glyph → vt100 panics here
        g.feed(b"\x1b[H\x1b[2JOK"); // recovered grid still repaints
        let mut buf = Buffer::empty(Rect::new(0, 0, 3, 1));
        g.render_into(&mut buf, Rect::new(0, 0, 3, 1));
        assert_eq!(
            buf[(0, 0)].symbol(),
            "O",
            "grid usable after the wide-char edge case"
        );
    }

    #[test]
    fn render_into_writes_cell_symbols_into_buffer() {
        let mut g = Grid::new(24, 80);
        g.feed(b"AB");
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 24));
        g.render_into(&mut buf, Rect::new(0, 0, 80, 24));
        assert_eq!(buf[(0, 0)].symbol(), "A");
        assert_eq!(buf[(1, 0)].symbol(), "B");
    }

    #[test]
    fn render_into_clips_to_area_top_left() {
        // A grid wider than the area renders only the top-left clip; nothing is
        // written past area.width/height.
        let mut g = Grid::new(24, 80);
        g.feed(b"HELLO");
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 24));
        // Narrow 3-wide area: only H E L land.
        g.render_into(&mut buf, Rect::new(0, 0, 3, 1));
        assert_eq!(buf[(0, 0)].symbol(), "H");
        assert_eq!(buf[(2, 0)].symbol(), "L");
        // Column 3 was outside the area and must be untouched (default space).
        assert_eq!(buf[(3, 0)].symbol(), " ");
    }

    #[test]
    fn render_into_blanks_wide_char_straddling_right_edge() {
        // A grid wider than the area can place a double-width char at the last
        // visible column, whose second half falls outside the area. Drawing it
        // would overflow the real terminal's right edge and wrap to col 0 of the
        // next line (the Hangul "overlap at col 0" bug). render_into must blank it.
        let mut g = Grid::new(1, 10);
        g.feed("한국어".as_bytes()); // 한=cols0-1, 국=2-3, 어=4-5 (each double-width)
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 1));
        // 5-wide area: 한(0-1) and 국(2-3) fit; 어 needs cols 4-5 but col 5 is
        // outside the area → it must be blanked, not drawn at col 4.
        g.render_into(&mut buf, Rect::new(0, 0, 5, 1));
        assert_eq!(buf[(0, 0)].symbol(), "한");
        assert_eq!(buf[(2, 0)].symbol(), "국");
        assert_eq!(
            buf[(4, 0)].symbol(),
            " ",
            "straddling wide char blanked, no overflow"
        );
    }

    #[test]
    fn render_into_keeps_wide_char_fully_inside_area() {
        // A double-width char with room for both halves inside the area is drawn.
        let mut g = Grid::new(1, 10);
        g.feed("한국".as_bytes()); // 한=0-1, 국=2-3
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 1));
        g.render_into(&mut buf, Rect::new(0, 0, 4, 1));
        assert_eq!(buf[(0, 0)].symbol(), "한");
        assert_eq!(buf[(2, 0)].symbol(), "국", "fully-inside wide char is kept");
    }

    #[test]
    fn render_into_repaints_wide_char_trailing_cell_on_transition() {
        // ratatui 0.30.1's incremental diff skips the trailing cell of a standard
        // wide (CJK) glyph, assuming the terminal clears it when the wide glyph is
        // printed. On a wide→narrow transition the terminal keeps the old glyph's
        // right half as background residue. render_into must make the diff repaint
        // that trailing cell so no residue survives.
        let area = Rect::new(0, 0, 4, 1);

        // Frame 1: a wide glyph at col 0 (occupies cols 0-1; col 1 is its trailing).
        let mut g_prev = Grid::new(1, 4);
        g_prev.feed("가".as_bytes());
        let mut prev = Buffer::empty(area);
        g_prev.render_into(&mut prev, area);

        // Frame 2: col 0 is now a narrow char; col 1 falls back to a blank space
        // whose symbol matches the old trailing cell — the residue-producing case.
        let mut g_next = Grid::new(1, 4);
        g_next.feed(b"a");
        let mut next = Buffer::empty(area);
        g_next.render_into(&mut next, area);

        // The diff ratatui flushes must include the trailing cell (1,0) so the old
        // glyph's right half is overwritten on the real terminal.
        let diff = prev.diff(&next);
        assert!(
            diff.iter().any(|&(x, y, _)| x == 1 && y == 0),
            "wide-char trailing cell must be repainted on transition, got diff {diff:?}"
        );
    }

    #[test]
    fn render_into_does_not_redraw_stable_wide_char() {
        // The trailing-cell repaint must fire only on a transition, never while the
        // wide glyph is unchanged — otherwise every frame would redraw and flash.
        // Two identical wide-char frames must produce an empty diff.
        let area = Rect::new(0, 0, 4, 1);

        let mut g1 = Grid::new(1, 4);
        g1.feed("가".as_bytes());
        let mut a = Buffer::empty(area);
        g1.render_into(&mut a, area);

        let mut g2 = Grid::new(1, 4);
        g2.feed("가".as_bytes());
        let mut b = Buffer::empty(area);
        g2.render_into(&mut b, area);

        assert!(
            a.diff(&b).is_empty(),
            "an unchanged wide-char frame must not redraw, got diff {:?}",
            a.diff(&b)
        );
    }

    #[test]
    fn cursor_reports_position_in_xy_order() {
        let mut g = Grid::new(24, 80);
        g.feed(b"abc"); // cursor advances to col 3, row 0
        assert_eq!(g.cursor(), (3, 0), "cursor is (col, row)");
    }

    #[test]
    fn fingerprint_same_contents_same_hash() {
        // Two grids fed the same bytes must produce the same fingerprint — the hash
        // is a function of visible content only, not parser identity or call count.
        let mut a = Grid::new(24, 80);
        let mut b = Grid::new(24, 80);
        a.feed(b"hello world");
        b.feed(b"hello world");
        assert_eq!(
            a.fingerprint(),
            b.fingerprint(),
            "identical content yields identical fingerprint"
        );
    }

    #[test]
    fn fingerprint_different_contents_different_hash() {
        // A grid whose visible content changed must produce a different fingerprint so
        // display_grid_changed fires only when the screen actually changed.
        let mut g = Grid::new(24, 80);
        g.feed(b"session-a output");
        let fp_a = g.fingerprint();
        g.clear();
        g.feed(b"session-b output");
        let fp_b = g.fingerprint();
        assert_ne!(fp_a, fp_b, "different content yields different fingerprint");
    }
}
