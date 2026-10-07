//! Images a session's client draws, sixel or kitty graphics, carried from the grid to
//! the outer terminal.
//!
//! The grid keeps an image as marker cells, one per cell the image covers, written
//! through its own vt100 parser. The parser then scrolls, overwrites and clears them
//! exactly as it does text, which is what a sixel terminal does with an image's cells,
//! so the grid needs no image tracking of its own. A sixel image's marker cell shows in
//! the frame as a cell ratatui never writes, and after each frame the painter draws
//! the sixel pieces those cells show onto the outer terminal. A kitty image's cells,
//! markers or the child's own placeholder cells, show in the frame as Unicode
//! placeholder cells, and before each frame the outer terminal receives each image
//! those cells name.
//!
//! Each protocol runs only when the outer terminal can show it; otherwise the grid
//! discards its strings as before.

pub mod kitty;
pub mod kitty_grid;
pub mod layer;
pub mod paint;
pub mod sixel;

/// What the outer terminal can show of a child's images.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Caps {
    /// The cell size as `(height, width)` in pixels.
    pub cell_px: Option<(u16, u16)>,
    /// It draws sixel images (and its cell size is known).
    pub sixel: bool,
    /// It draws kitty graphics through Unicode placeholder cells.
    pub kitty: bool,
}

impl Caps {
    /// What the outer terminal reported at startup.
    ///
    /// Nothing on Windows: the system ConPTY every attach child runs under answers the
    /// child's device attributes query itself without sixel, and delivers a sixel or
    /// APC string twice, misterminated, and out of order with the text around it.
    pub fn current() -> Self {
        // Tests run the unix path on every platform: CI runs the suite on Windows only.
        if cfg!(windows) && !cfg!(test) {
            return Caps::default();
        }
        let outer = crate::display::outer::outer();
        Caps {
            cell_px: outer.cell_px,
            sixel: outer.sixel && outer.cell_px.is_some(),
            kitty: outer.kitty_graphics && draws_placeholders(outer.name.as_deref()),
        }
    }

    /// The cell size, when the outer terminal draws sixel images.
    pub fn sixel_cell_px(&self) -> Option<(u16, u16)> {
        self.sixel.then_some(self.cell_px).flatten()
    }
}

/// Whether the terminal named by its XTVERSION reply draws Unicode placeholder cells.
/// The graphics protocol has no query for it, and a terminal that answers the graphics
/// query without drawing placeholders would show nothing.
fn draws_placeholders(name: Option<&str>) -> bool {
    name.is_some_and(|n| {
        let n = n.to_ascii_lowercase();
        n.starts_with("kitty") || n.starts_with("ghostty")
    })
}

/// The outer terminal's cell size as `(height, width)` in pixels, when it can show
/// sixel images.
pub fn sixel_cell_px() -> Option<(u16, u16)> {
    Caps::current().sixel_cell_px()
}

/// The size an attach PTY gets: cells, and pixels while either image protocol is on,
/// which is where a mux such as tmux reads the cell size it lays an image out with.
pub fn pty_size(rows: u16, cols: u16) -> portable_pty::PtySize {
    let caps = Caps::current();
    let (pixel_height, pixel_width) = caps
        .cell_px
        .filter(|_| caps.sixel || caps.kitty)
        .map(|(h, w)| (rows.saturating_mul(h), cols.saturating_mul(w)))
        .unwrap_or((0, 0));
    portable_pty::PtySize {
        rows,
        cols,
        pixel_width,
        pixel_height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_are_drawn_by_kitty_and_ghostty_only() {
        assert!(draws_placeholders(Some("kitty(0.41.1)")));
        assert!(draws_placeholders(Some("ghostty 1.1.3")));
        assert!(!draws_placeholders(Some("WezTerm 20240203")));
        assert!(!draws_placeholders(None));
    }
}
