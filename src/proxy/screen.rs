//! A one-pane vt100 grid the proxy tees child output into, used ONLY to repaint
//! the live pane after a transient overlay. Not a multiplexer: one grid, no
//! layouts, no input routing.
pub struct Grid {
    parser: vt100::Parser,
}

impl Grid {
    pub fn new(rows: u16, cols: u16) -> Self {
        Self { parser: vt100::Parser::new(rows, cols, 0) }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.parser.screen_mut().set_size(rows, cols);
    }

    /// Full repaint of the visible grid + re-emit of the private modes the child
    /// set, so its input handling is not silently broken after an overlay close.
    pub fn restore_bytes(&self) -> Vec<u8> {
        let screen = self.parser.screen();
        let mut out = screen.contents_formatted();
        if screen.bracketed_paste() { out.extend_from_slice(b"\x1b[?2004h"); }
        if screen.application_cursor() { out.extend_from_slice(b"\x1b[?1h"); }
        if screen.application_keypad() { out.extend_from_slice(b"\x1b="); }
        if screen.hide_cursor() { out.extend_from_slice(b"\x1b[?25l"); }
        out
    }

    #[cfg(test)]
    fn restore_includes_alt_marker(&self) -> bool {
        self.parser.screen().contents().contains("alt-text")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconstructs_plain_content() {
        let mut g = Grid::new(24, 80);
        g.feed(b"hello-VT100-grid");
        let bytes = g.restore_bytes();
        assert!(!bytes.is_empty());
        assert!(String::from_utf8_lossy(&bytes).contains("hello-VT100-grid"));
    }

    #[test]
    fn tracks_alternate_screen() {
        let mut g = Grid::new(10, 40);
        g.feed(b"\x1b[?1049h\x1b[Halt-text");
        assert!(g.restore_includes_alt_marker());
    }

    #[test]
    fn restore_reemits_bracketed_paste_mode() {
        // child enabled bracketed paste; restore must re-assert it
        let mut g = Grid::new(10, 40);
        g.feed(b"\x1b[?2004h");
        let bytes = g.restore_bytes();
        assert!(bytes.windows(8).any(|w| w == b"\x1b[?2004h"),
            "restore must re-emit bracketed-paste enable");
    }
}
