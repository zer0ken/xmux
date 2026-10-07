//! Sixel images a session's client draws, carried from the grid to the outer terminal.
//!
//! The grid keeps an image as marker cells, one per cell the image covers, written
//! through its own vt100 parser. The parser then scrolls, overwrites and clears them
//! exactly as it does text, which is what a sixel terminal does with an image's cells,
//! so the grid needs no image tracking of its own. The frame shows a marker cell as a
//! cell ratatui never writes, and after each frame the painter draws the image pieces
//! those cells show onto the outer terminal.
//!
//! None of this runs unless the outer terminal reported sixel support and its cell
//! size: without both, the grid discards sixel strings as before.

pub mod layer;
pub mod paint;
pub mod sixel;

/// The outer terminal's cell size as `(height, width)` in pixels, when it can show
/// sixel images.
///
/// Always `None` on Windows: the system ConPTY every attach child runs under answers
/// the child's device attributes query itself without sixel, and delivers a sixel
/// string twice, misterminated, and out of order with the text around it.
pub fn sixel_cell_px() -> Option<(u16, u16)> {
    // Tests run the unix path on every platform: CI runs the suite on Windows only.
    if cfg!(windows) && !cfg!(test) {
        return None;
    }
    let outer = crate::display::outer::outer();
    outer.sixel.then_some(outer.cell_px).flatten()
}

/// The size an attach PTY gets: cells, and pixels while sixel is on, which is where a
/// mux such as tmux reads the cell size it lays an image out with.
pub fn pty_size(rows: u16, cols: u16) -> portable_pty::PtySize {
    let (pixel_height, pixel_width) = sixel_cell_px()
        .map(|(h, w)| (rows.saturating_mul(h), cols.saturating_mul(w)))
        .unwrap_or((0, 0));
    portable_pty::PtySize {
        rows,
        cols,
        pixel_width,
        pixel_height,
    }
}
