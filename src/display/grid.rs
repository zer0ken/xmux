//! A one-pane vt100 grid the display layer tees child output into, used ONLY to repaint
//! the live pane after a transient modal. Not a multiplexer: one grid, no
//! layouts, no input routing. It also reads the input modes the child sets, which the
//! input path shapes forwarded input by. It also reads the input modes the child sets, which the
//! input path shapes forwarded input by.
use std::hash::{Hash, Hasher};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color as RColor, Modifier, Style};

use crate::display::callbacks::GridCallbacks;
use crate::display::image::layer::ImageLayer;
use crate::display::vt100;

/// What a child's output asked of the terminal around the screen: a bell, or a desktop
/// notification. The grid's parser consumes these, so the grid keeps each one until the
/// pump takes it and hands it to the loop, which re-emits it on xmux's own output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Alert {
    /// A BEL outside any escape sequence.
    Bell,
    /// An OSC 9 or OSC 777 `notify` desktop notification: its readable text and the
    /// whole sequence to re-emit, BEL-terminated.
    Notify { text: String, seq: Vec<u8> },
}

impl Alert {
    /// The bytes that ask the terminal above xmux for the same thing.
    pub fn bytes(&self) -> &[u8] {
        match self {
            Alert::Bell => b"\x07",
            Alert::Notify { seq, .. } => seq,
        }
    }
}

/// The most alerts one grid holds between two takes. A child that rings without pause
/// still reaches the terminal once per pump read, and nothing it sends grows the grid.
const ALERTS_MAX: usize = 16;

/// The longest notification payload re-emitted; a longer one is dropped whole.
const NOTIFY_MAX: usize = 4096;

/// The longest window title kept; a longer one is cut at a character boundary.
const TITLE_MAX: usize = 256;

/// Collects what the parser reports beside the cells while it processes a chunk: the
/// alerts, and the window title the child set.
#[derive(Default)]
pub(crate) struct ParserSink {
    pub(crate) alerts: Vec<Alert>,
    pub(crate) title: Option<String>,
}

impl ParserSink {
    fn push(&mut self, alert: Alert) {
        // One bell per take says everything a run of bells says.
        if alert == Alert::Bell && self.alerts.contains(&Alert::Bell) {
            return;
        }
        if self.alerts.len() < ALERTS_MAX {
            self.alerts.push(alert);
        }
    }
}

impl vt100::Callbacks for ParserSink {
    fn audible_bell(&mut self, _: &mut vt100::Screen) {
        self.push(Alert::Bell);
    }

    /// OSC 0 and OSC 2. An empty title takes the child's title back, and control
    /// characters are dropped so a title is only ever text when it is re-emitted.
    fn set_window_title(&mut self, _: &mut vt100::Screen, title: &[u8]) {
        let title: String = String::from_utf8_lossy(title)
            .chars()
            .filter(|c| !c.is_control())
            .collect();
        let end = title
            .char_indices()
            .map(|(i, c)| i + c.len_utf8())
            .take_while(|&end| end <= TITLE_MAX)
            .last()
            .unwrap_or(0);
        self.title = Some(title[..end].to_string()).filter(|t| !t.is_empty());
    }

    fn unhandled_osc(&mut self, _: &mut vt100::Screen, params: &[&[u8]]) {
        if let Some(alert) = notification(params) {
            self.push(alert);
        }
    }
}

/// The notification an OSC carries, when it is one: OSC 9 with free text (iTerm2), or
/// OSC 777 `notify` with a title and a body (rxvt, VTE). An OSC 9 whose first field is a
/// number is a ConEmu command such as the `9;4` progress report, not a notification.
fn notification(params: &[&[u8]]) -> Option<Alert> {
    let (ps, text) = match params {
        [b"9", first, ..] if !first.is_empty() && !first.iter().all(u8::is_ascii_digit) => {
            ("9", params[1..].join(&b';'))
        }
        [b"777", b"notify", title, body @ ..] => {
            let body = body.join(&b';');
            let text = match (title.is_empty(), body.is_empty()) {
                (true, _) => body,
                (false, true) => title.to_vec(),
                (false, false) => [*title, b": ", &body].concat(),
            };
            ("777", text)
        }
        _ => return None,
    };
    let payload = params[1..].join(&b';');
    if payload.len() > NOTIFY_MAX {
        return None;
    }
    let mut seq = format!("\x1b]{ps};").into_bytes();
    seq.extend_from_slice(&payload);
    seq.push(0x07);
    Some(Alert::Notify {
        text: String::from_utf8_lossy(&text).into_owned(),
        seq,
    })
}

pub struct Grid {
    parser: vt100::Parser<GridCallbacks>,
    /// Set by a session switch: the next `feed` wipes the grid before applying the
    /// chunk, so the prior session's content stays on screen until the mux's fresh
    /// repaint arrives (no blank window between the switch and the repaint) and the
    /// grid still clears the instant the new content lands (no residue either).
    clear_on_feed: bool,
    /// The input modes the client has set, kept across a wipe of the cells.
    modes: crate::display::modes::ModeScanner,
    /// The sixel images the grid's marker cells name.
    images: ImageLayer,
}

impl Grid {
    pub fn new(rows: u16, cols: u16) -> Self {
        Self {
            parser: new_parser(rows, cols),
            clear_on_feed: false,
            modes: Default::default(),
            images: ImageLayer::default(),
        }
    }

    /// A fresh parser at `rows` x `cols` that keeps its callbacks: the alerts not taken
    /// yet and the answers still owed survive, and so do the cursor shape and modes the
    /// client set, since a wipe of the cells does not change the client.
    fn reset_parser(&mut self, rows: u16, cols: u16) {
        let sink = std::mem::take(self.parser.callbacks_mut());
        self.parser = vt100::Parser::new_with_callbacks(rows, cols, 0, sink);
    }

    /// The window title the child set with OSC 0 or OSC 2, if it set one.
    pub fn title(&self) -> Option<&str> {
        self.parser.callbacks().title()
    }

    /// The alerts the output fed since the last take asked for, oldest first.
    pub fn take_alerts(&mut self) -> Vec<Alert> {
        self.parser.callbacks_mut().take_alerts()
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.modes.feed(bytes);
        if self.clear_on_feed {
            self.clear_on_feed = false;
            self.clear();
        }
        // The modes the scanner read stand for the whole chunk, so a mode report
        // answered within it reads the chunk's end state.
        self.parser
            .callbacks_mut()
            .set_input_modes(self.modes.modes());
        // vt100 0.16.2 panics (screen.rs `Screen::text` unwrap on None) when a wide
        // (CJK) glyph lands on the last column in some cursor states - common after a
        // grid shrink. Catch it so the PTY pump thread survives; reset the parser so
        // the next mux repaint refills the grid cleanly instead of re-panicking on the
        // same stale cursor.
        let cell_px = crate::display::image::sixel_cell_px();
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.images.feed(bytes, cell_px, &mut self.parser);
        }));
        if res.is_err() {
            let (rows, cols) = self.parser.screen().size();
            self.reset_parser(rows, cols);
            self.images.clear();
        }
    }

    /// The bitmap of a sixel image this grid's cells show.
    pub fn image(&self, id: u32) -> Option<std::sync::Arc<crate::display::image::sixel::Bitmap>> {
        self.images.image(id).cloned()
    }

    /// The answers to the terminal queries the fed output held, in order, each once.
    /// The caller writes them to the child.
    pub fn take_replies(&mut self) -> Vec<u8> {
        self.parser.callbacks_mut().take_replies()
    }

    /// Wipes the grid to a blank slate (a fresh parser at the same size) at the start
    /// of the next feed. Used when the displayed session switches so the prior
    /// content stays on screen until the mux's full redraw arrives, then clears the
    /// moment the new content lands - stale cells from the previous session never
    /// linger behind the new repaint.
    pub fn clear_on_next_feed(&mut self) {
        self.clear_on_feed = true;
    }

    /// The primitive behind [`Grid::clear_on_next_feed`]: wipes to a fresh parser at
    /// the same size. Also used directly by tests.
    pub fn clear(&mut self) {
        let (rows, cols) = self.parser.screen().size();
        self.reset_parser(rows, cols);
        // The title belongs to the session the grid showed; the next one sets its own.
        self.parser.callbacks_mut().clear_title();
        self.images.clear();
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

    /// The input modes the child has set on its terminal.
    pub fn input_modes(&self) -> crate::display::modes::InputModes {
        self.modes.modes()
    }

    /// Whether the child has hidden its cursor.
    pub fn hide_cursor(&self) -> bool {
        self.parser.screen().hide_cursor()
    }

    /// Whether the grid has no visible content (all blank) - used to diagnose an
    /// attachment whose PTY child has not produced output yet.
    /// The last non-empty line the pane holds, for the log to name WHY a display
    /// terminal is gone.
    ///
    /// A pane that dies leaves its reason on its own screen: `ssh` says the connection
    /// closed, a mux says what it refused. Nothing else carries that sentence, so without
    /// reading it back the death is only ever a timestamp.
    pub fn last_line(&self) -> Option<String> {
        self.last_line_except(|_| false)
    }

    /// The last non-empty line that `ignored` does not reject.
    pub fn last_line_except(&self, ignored: impl Fn(&str) -> bool) -> Option<String> {
        let text = self.parser.screen().contents();
        // Trimmed at both ends: a pane keeps its cells padded to the full width, and
        // where a line STARTS on screen says nothing about what it says.
        text.lines()
            .map(str::trim)
            .rfind(|l| !l.is_empty() && !ignored(l))
            .map(str::to_string)
    }

    pub fn is_blank(&self) -> bool {
        self.parser.screen().contents().trim().is_empty()
    }

    /// Whether every visible line is blank or rejected by `ignored`.
    pub fn is_blank_except(&self, ignored: impl Fn(&str) -> bool) -> bool {
        self.parser
            .screen()
            .contents()
            .lines()
            .map(str::trim)
            .all(|l| l.is_empty() || ignored(l))
    }

    /// A cheap, stable hash of the visible cell contents. Changes if and only if the
    /// rendered text changes - used to detect whether a display transition actually
    /// produced a different screen, so a `display_show decision=switch` not followed
    /// by a `display_grid_changed` event indicates the mux switch had no visible effect.
    pub fn fingerprint(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.parser.screen().contents().hash(&mut h);
        h.finish()
    }

    /// Writes a top-left clip of the grid into `area` of `buf`, mapping each
    /// vt100 cell's symbol + colours + attrs to a ratatui cell. Cells past the
    /// grid size or `area` are skipped (the terminal view in Focus::Nav is narrower
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
                if let Some(piece) = self.images.piece(vcell) {
                    crate::display::image::paint::mark(cell, piece);
                    continue;
                }
                if vcell.is_wide() && c + 1 >= cols {
                    // A double-width char whose second half falls outside the
                    // clipped pane would overflow the right edge and wrap to col 0
                    // of the next line; blank it so the pane stays aligned.
                    cell.set_symbol(" ");
                } else if let Some(uri) = vcell
                    .has_contents()
                    .then(|| screen.hyperlink_uri(vcell.hyperlink_id()))
                    .flatten()
                    .filter(|uri| uri.len() <= LINK_MAX)
                {
                    // The cell carries its link inside its symbol, so the diff sees a
                    // link change as a change of the cell. The forced width tells the
                    // diff how many columns the cell covers, which the escape text
                    // would otherwise inflate.
                    cell.set_symbol(&linked_symbol(uri, vcell.contents()));
                    cell.set_diff_option(ratatui::buffer::CellDiffOption::ForcedWidth(
                        if vcell.is_wide() {
                            std::num::NonZeroU16::MIN.saturating_add(1)
                        } else {
                            std::num::NonZeroU16::MIN
                        },
                    ));
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
                    // diff to repaint it on transition - no full-screen clear, so no
                    // flash. While the wide glyph is stable the diff skips this cell
                    // via the leading cell's width, so it never redraws needlessly.
                    cell.set_diff_option(ratatui::buffer::CellDiffOption::AlwaysUpdate);
                }
            }
        }
    }
}

fn new_parser(rows: u16, cols: u16) -> vt100::Parser<GridCallbacks> {
    vt100::Parser::new_with_callbacks(rows, cols, 0, GridCallbacks::default())
}

/// The longest link URI written; a cell with a longer one is drawn without its link.
const LINK_MAX: usize = 2048;

/// The OSC 8 open sequence's lead, which [`visible_symbol`] looks for.
const LINK_OPEN: &str = "\x1b]8;";
/// The OSC 8 sequence that ends a link.
const LINK_CLOSE: &str = "\x1b]8;;\x1b\\";

/// `text` inside an OSC 8 hyperlink to `uri`. Every linked cell opens and closes its own
/// link, so a cell drawn alone by the diff is still linked; the `id` derived from the
/// URI lets the terminal treat the cells of one link as one. Control characters are
/// left out of the URI so the sequence cannot end early.
fn linked_symbol(uri: &str, text: &str) -> String {
    use std::hash::{Hash, Hasher};
    let uri: String = uri.chars().filter(|c| !c.is_control()).collect();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    uri.hash(&mut h);
    format!(
        "{LINK_OPEN}id=xmux-{:x};{uri}\x1b\\{text}{LINK_CLOSE}",
        h.finish()
    )
}

/// The text a buffer cell shows: its symbol without the OSC 8 link around it, for
/// readers of the buffer that want what is on screen rather than what is written.
pub fn visible_symbol(symbol: &str) -> &str {
    symbol
        .strip_prefix(LINK_OPEN)
        .and_then(|rest| rest.split_once("\x1b\\"))
        .and_then(|(_, rest)| rest.strip_suffix(LINK_CLOSE))
        .unwrap_or(symbol)
}

/// How many columns a buffer cell covers: its forced width when it carries a link, the
/// width of its symbol otherwise.
pub fn cell_width(cell: &ratatui::buffer::Cell) -> usize {
    match cell.diff_option {
        ratatui::buffer::CellDiffOption::ForcedWidth(width) => usize::from(width.get()),
        _ => unicode_width::UnicodeWidthStr::width(cell.symbol()),
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

    /// A pane that dies leaves its reason on its own screen, and the log reads it from
    /// there. The LAST written line is the one that matters: what the child said just
    /// before it stopped, not the banner it opened with.
    #[test]
    fn last_line_reads_what_the_child_said_last() {
        let mut g = Grid::new(6, 40);
        assert_eq!(g.last_line(), None, "a blank pane has nothing to report");

        g.feed(
            b"Welcome
Connection to host closed.
",
        );
        assert_eq!(
            g.last_line().as_deref(),
            Some("Connection to host closed."),
            "the trailing blank rows are skipped for the last written line"
        );
    }

    /// Holds the terminal facts at their defaults (no sixel) for a test that reads them.
    fn plain_outer_terminal() -> std::sync::MutexGuard<'static, ()> {
        use crate::display::outer::{set_outer_for_test, OuterTerminal, TEST_LOCK};
        let lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_outer_for_test(OuterTerminal::default());
        lock
    }

    /// Holds the terminal facts of a sixel terminal with 10x20 px cells.
    fn sixel_outer_terminal() -> std::sync::MutexGuard<'static, ()> {
        use crate::display::outer::{set_outer_for_test, OuterTerminal, TEST_LOCK};
        let lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_outer_for_test(OuterTerminal {
            cell_px: Some((20, 10)),
            sixel: true,
            ..OuterTerminal::default()
        });
        lock
    }

    /// A 30x40 px red sixel.
    const SIXEL: &[u8] = b"\x1bPq\"1;1;30;40#1;2;100;0;0#1!30~-!30~-!30~-!30~-!30~-!30~-!30N\x1b\\";

    /// A sixel image the child draws shows in the frame as cells ratatui leaves to the
    /// image, at the image's place in the view, and the painter draws it there.
    #[test]
    fn a_sixel_from_the_child_reaches_the_frame_as_image_cells() {
        let _lock = sixel_outer_terminal();
        let mut g = Grid::new(10, 20);
        g.feed(b"ab\x1b[2;3H");
        g.feed(SIXEL);
        let mut buf = Buffer::empty(Rect::new(0, 0, 30, 12));
        g.render_into(&mut buf, Rect::new(5, 1, 20, 10));
        let skipped: Vec<(u16, u16)> = (0..12)
            .flat_map(|y| (0..30).map(move |x| (x, y)))
            .filter(|&(x, y)| buf[(x, y)].diff_option == ratatui::buffer::CellDiffOption::Skip)
            .collect();
        // 30x40 px is 3x2 cells at grid (2, 1), drawn at view offset (5, 1).
        assert_eq!(
            skipped,
            vec![(7, 2), (8, 2), (9, 2), (7, 3), (8, 3), (9, 3)]
        );
        assert_eq!(buf[(5, 1)].symbol(), "a");
        let mut painter = crate::display::image::paint::Painter::default();
        let out = painter.paint(&buf, |id| g.image(id), (20, 10));
        let out = String::from_utf8_lossy(&out);
        assert!(out.contains("\x1b[3;8H\x1bP0;0;0q\"1;1;30;40"), "{out}");
    }

    /// The grid claims sixel to the child only while the outer terminal can show it.
    #[test]
    fn device_attributes_claim_sixel_only_for_a_sixel_terminal() {
        {
            let _lock = sixel_outer_terminal();
            let mut g = Grid::new(24, 80);
            assert_eq!(replies(&mut g, b"\x1b[c"), b"\x1b[?62;4;22c");
            let size = crate::display::image::pty_size(24, 80);
            assert_eq!((size.pixel_width, size.pixel_height), (800, 480));
        }
        let _lock = plain_outer_terminal();
        let mut g = Grid::new(24, 80);
        assert_eq!(replies(&mut g, b"\x1b[c"), b"\x1b[?62;22c");
        g.feed(b"a");
        g.feed(SIXEL);
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 24));
        let area = buf.area;
        g.render_into(&mut buf, area);
        assert!(buf
            .content
            .iter()
            .all(|c| c.diff_option != ratatui::buffer::CellDiffOption::Skip));
        let size = crate::display::image::pty_size(24, 80);
        assert_eq!((size.pixel_width, size.pixel_height), (0, 0));
    }

    fn replies(g: &mut Grid, bytes: &[u8]) -> Vec<u8> {
        g.feed(bytes);
        g.take_replies()
    }

    /// The cursor report names the cursor where the query sits in the stream, and the
    /// device attributes claim only what the grid models.
    #[test]
    fn the_grid_answers_status_and_attribute_queries() {
        let _lock = plain_outer_terminal();
        let mut g = Grid::new(24, 80);
        assert_eq!(replies(&mut g, b"\x1b[3;5H\x1b[6nmore"), b"\x1b[3;5R");
        assert_eq!(replies(&mut g, b"\x1b[H\x1b[?6n"), b"\x1b[?1;1;1R");
        assert_eq!(replies(&mut g, b"\x1b[5n"), b"\x1b[0n");
        assert_eq!(
            replies(&mut g, b"\x1b[c\x1b[0c"),
            b"\x1b[?62;22c\x1b[?62;22c"
        );
        let version = env!("CARGO_PKG_VERSION");
        assert_eq!(
            replies(&mut g, b"\x1b[>c"),
            format!("\x1b[>0;{};0c", super::super::callbacks::version_number()).as_bytes()
        );
        assert_eq!(
            replies(&mut g, b"\x1b[>q"),
            format!("\x1bP>|xmux({version})\x1b\\").as_bytes()
        );
        assert!(replies(&mut g, b"plain output\r\n").is_empty());
    }

    /// A query split across two reads is answered once, when it completes, and a
    /// query that ends a read is not answered again by the next one.
    #[test]
    fn the_keyboard_flags_query_is_answered_before_the_attributes_over_the_protocol() {
        let mut g = Grid::new(4, 10);
        assert_eq!(
            replies(&mut g, b"\x1b[>1u\x1b[?u"),
            b"",
            "no protocol outside"
        );
        crate::display::keyboard::record_support(true);
        assert_eq!(
            replies(&mut g, b"\x1b[?u\x1b[c"),
            b"\x1b[?1u\x1b[?62;22c",
            "the pushed flags, then the attributes"
        );
    }

    #[test]
    fn a_split_query_is_answered_exactly_once() {
        let _lock = plain_outer_terminal();
        let mut g = Grid::new(24, 80);
        assert!(replies(&mut g, b"prompt\x1b[").is_empty());
        assert_eq!(replies(&mut g, b"6n more\x1b[c"), b"\x1b[1;7R\x1b[?62;22c");
        assert!(replies(&mut g, b" tail").is_empty());
    }

    /// DECRQM reports the modes the grid keeps as set or reset and every other mode as
    /// not recognized.
    #[test]
    fn the_grid_reports_the_modes_it_keeps() {
        let mut g = Grid::new(24, 80);
        assert_eq!(replies(&mut g, b"\x1b[?2004$p"), b"\x1b[?2004;2$y");
        assert_eq!(
            replies(&mut g, b"\x1b[?2004h\x1b[?2004$p\x1b[?1049h\x1b[?1049$p"),
            b"\x1b[?2004;1$y\x1b[?1049;1$y"
        );
        assert_eq!(
            replies(&mut g, b"\x1b[?1006h\x1b[?1006$p"),
            b"\x1b[?1006;1$y"
        );
        assert_eq!(replies(&mut g, b"\x1b[?1004$p"), b"\x1b[?1004;2$y");
        assert_eq!(
            replies(&mut g, b"\x1b[?1004h\x1b[?1004$p"),
            b"\x1b[?1004;1$y"
        );
        assert_eq!(replies(&mut g, b"\x1b[?25l\x1b[?25$p"), b"\x1b[?25;2$y");
        assert_eq!(replies(&mut g, b"\x1b[?7727$p"), b"\x1b[?7727;0$y");
        assert_eq!(replies(&mut g, b"\x1b[4$p"), b"\x1b[4;0$y");
    }

    /// Colours, the colour scheme, and pixel sizes come from what the real terminal
    /// answered xmux; what it never answered stays unanswered.
    #[test]
    fn the_grid_answers_with_the_outer_terminals_facts() {
        use crate::display::outer::{set_outer_for_test, OuterTerminal, TEST_LOCK};
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut g = Grid::new(24, 80);

        set_outer_for_test(OuterTerminal::default());
        assert!(replies(&mut g, b"\x1b]11;?\x07\x1b]10;?\x1b\\x1b]4;1;?\x07").is_empty());
        assert!(replies(&mut g, b"\x1b[?996n\x1b[16t\x1b[14t").is_empty());
        assert_eq!(replies(&mut g, b"\x1b[18t"), b"\x1b[8;24;80t");

        let mut palette: [Option<String>; 16] = Default::default();
        palette[1] = Some("rgb:cdcd/0000/0000".into());
        set_outer_for_test(OuterTerminal {
            foreground: Some("rgb:cccc/cccc/cccc".into()),
            background: Some("rgb:1e1e/1e1e/2e2e".into()),
            palette,
            scheme: None,
            cell_px: Some((18, 9)),
            sixel: false,
        });
        assert_eq!(
            replies(&mut g, b"\x1b]11;?\x07\x1b]10;?\x1b\\"),
            b"\x1b]11;rgb:1e1e/1e1e/2e2e\x1b\\\x1b]10;rgb:cccc/cccc/cccc\x1b\\"
        );
        assert_eq!(
            replies(&mut g, b"\x1b]4;1;?;2;?\x07"),
            b"\x1b]4;1;rgb:cdcd/0000/0000\x1b\\",
            "a palette slot the terminal never reported stays unanswered"
        );
        assert_eq!(replies(&mut g, b"\x1b[?996n"), b"\x1b[?997;1n");
        assert_eq!(replies(&mut g, b"\x1b[16t"), b"\x1b[6;18;9t");
        assert_eq!(replies(&mut g, b"\x1b[14t"), b"\x1b[4;432;720t");
        set_outer_for_test(OuterTerminal::default());
    }

    /// A parser reset after a vt100 panic keeps the answers owed for the queries it
    /// parsed before the panic.
    #[test]
    fn replies_survive_a_parser_reset() {
        let mut g = Grid::new(1, 4);
        g.feed(b"\x1b[6n\x1b[1;4H");
        g.feed("한".as_bytes());
        g.feed(b"\x1b[K");
        assert_eq!(g.take_replies(), b"\x1b[1;1R");
    }

    #[test]
    fn a_bell_and_notifications_are_kept_for_the_loop() {
        let mut g = Grid::new(4, 20);
        g.feed(b"a\x07b\x07\x1b]9;build done\x07\x1b]777;notify;Claude;needs input\x1b\\");
        assert_eq!(
            g.take_alerts(),
            vec![
                Alert::Bell,
                Alert::Notify {
                    text: "build done".into(),
                    seq: b"\x1b]9;build done\x07".to_vec(),
                },
                Alert::Notify {
                    text: "Claude: needs input".into(),
                    seq: b"\x1b]777;notify;Claude;needs input\x07".to_vec(),
                },
            ],
            "a run of bells is one bell, and each notification is re-emitted whole"
        );
        assert!(
            g.take_alerts().is_empty(),
            "a take empties the grid's alerts"
        );
        assert_eq!(
            g.last_line().as_deref(),
            Some("ab"),
            "no alert reaches the cells"
        );
    }

    #[test]
    fn a_bell_terminating_an_osc_and_conemu_commands_are_not_alerts() {
        let mut g = Grid::new(4, 20);
        g.feed(b"\x1b]0;title\x07\x1b]9;4;1;50\x07\x1b]52;c;aGk=\x07");
        assert!(g.take_alerts().is_empty());
    }

    #[test]
    fn the_title_the_child_set_is_kept_until_it_is_taken_back_or_the_grid_clears() {
        let mut g = Grid::new(4, 20);
        assert_eq!(g.title(), None);
        g.feed(b"\x1b]0;first\x07");
        assert_eq!(g.title(), Some("first"));
        g.feed(b"\x1b]2;vim \x1b\\");
        assert_eq!(g.title(), Some("vim "), "OSC 2 with an ST terminator");
        g.feed(b"\x1b]1;icon only\x07");
        assert_eq!(
            g.title(),
            Some("vim "),
            "OSC 1 names the icon, not the window"
        );
        g.feed(b"\x1b]2;\x07");
        assert_eq!(g.title(), None, "an empty title takes it back");
        g.feed(b"\x1b]2;a\xc2\x9bb\x07");
        assert_eq!(g.title(), Some("ab"), "control characters are dropped");
        g.feed(format!("\x1b]2;{}\x07", "\u{d55c}".repeat(200)).as_bytes());
        assert_eq!(g.title().unwrap().len(), 255, "cut at a character boundary");
        g.clear();
        assert_eq!(g.title(), None, "the next session sets its own");
    }

    #[test]
    fn a_notification_text_keeps_its_semicolons() {
        let mut g = Grid::new(4, 20);
        g.feed(b"\x1b]9;a;b\x07");
        assert_eq!(
            g.take_alerts(),
            vec![Alert::Notify {
                text: "a;b".into(),
                seq: b"\x1b]9;a;b\x07".to_vec(),
            }]
        );
    }

    #[test]
    fn alerts_survive_a_clear_and_stay_bounded() {
        let mut g = Grid::new(4, 20);
        g.feed(b"\x07");
        g.clear();
        for i in 0..40 {
            g.feed(format!("\x1b]9;n{i}\x07").as_bytes());
        }
        let alerts = g.take_alerts();
        assert_eq!(alerts[0], Alert::Bell, "a clear keeps what was not taken");
        assert_eq!(alerts.len(), ALERTS_MAX);
    }

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
    fn input_modes_outlive_a_wipe_of_the_cells() {
        // A session switch wipes the cells, but the client keeps the modes it set.
        let mut g = Grid::new(4, 10);
        g.feed(b"\x1b[?2004h");
        g.clear_on_next_feed();
        g.feed(b"next session");
        assert!(g.input_modes().bracketed_paste);
    }

    #[test]
    fn clear_blanks_the_grid() {
        // On a session switch the grid is wiped so no stale cells linger
        // behind the mux's fresh repaint.
        let mut g = Grid::new(24, 80);
        g.feed(b"residue content that must vanish");
        assert!(!g.is_blank(), "precondition: grid has content");
        g.clear();
        assert!(g.is_blank(), "clear wipes all visible content");
    }

    // NOTE: this test deliberately triggers the vt100 panic that Grid::feed catches, so
    // `cargo test` prints one "thread panicked at vt100 ... screen.rs" line to stderr -
    // expected, not a failure. (The hook is not silenced here because it is process-
    // global and tests run in parallel.)
    #[test]
    fn streamed_full_width_lines_keep_their_first_char() {
        // A mux draws a full-width row and lets auto-wrap carry into the next row
        // (no CR between rows when the cursor is at the right margin). In a 4-wide
        // terminal "ABCDEFGH" lands as "ABCD" / "EFGH". The parser must wrap at the
        // viewport width, never a column later: a padded grid would absorb the first
        // char of the wrapped row into the invisible padding column.
        let mut g = Grid::new(2, 4);
        g.feed(b"ABCDEFGH");
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 2));
        g.render_into(&mut buf, Rect::new(0, 0, 4, 2));
        assert_eq!(buf[(0, 0)].symbol(), "A");
        assert_eq!(buf[(3, 0)].symbol(), "D");
        assert_eq!(
            buf[(0, 1)].symbol(),
            "E",
            "first char of the wrapped row must stay at the line start"
        );
        assert_eq!(buf[(3, 1)].symbol(), "H");
    }

    #[test]
    fn feed_survives_wide_char_at_last_column() {
        // Regression: vt100 0.16.2 panics (drawing_cell_mut(col+1).unwrap() on None) when
        // a wide CJK glyph prints on the last column - observed crashing the PTY pump
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
        // whose symbol matches the old trailing cell - the residue-producing case.
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
        // wide glyph is unchanged - otherwise every frame would redraw and flash.
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
        // Two grids fed the same bytes must produce the same fingerprint - the hash
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

    // NOTE: this test deliberately triggers the vt100 panic that Grid::feed catches, so
    // `cargo test` prints one "thread panicked at vt100 ..." line to stderr - expected,
    // not a failure. (The hook is not silenced here because it is process-global and
    // tests run in parallel.)
    #[test]
    fn feed_survives_clear_wide_panic_at_last_column() {
        // Regression: vt100 0.16.2's `Row::clear_wide` (row.rs:89/91) panics when an
        // erase/remove lands on the boundary of a wide (CJK) glyph - most often a
        // double-width char whose first half sits at the last column (col+1 OOB, the
        // row.rs:89 the panic.log shows as "len is 130 but the index is 130") or whose
        // continuation wraps to column 0 (col-1 underflow). Both are the same code path
        // and are caught by Grid::feed's catch_unwind; this pins the survival so a
        // change to the catch does not silently re-expose the PTY pump to it.
        let mut g = Grid::new(1, 4);
        g.feed(b"\x1b[1;4H"); // cursor to 0-based col 3 (the right edge)
        g.feed("한".as_bytes()); // wide glyph straddles/overflows the last column
        g.feed(b"\x1b[K"); // erase-in-line on the dangling wide boundary → vt100 panics
                           // Recovered grid must still repaint: a later clear+redraw lands cleanly.
        g.clear();
        g.feed(b"OK");
        let mut buf = Buffer::empty(Rect::new(0, 0, 3, 1));
        g.render_into(&mut buf, Rect::new(0, 0, 3, 1));
        assert_eq!(
            buf[(0, 0)].symbol(),
            "O",
            "grid usable after the clear_wide edge case"
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

    /// What the terminal receives when ratatui flushes `next` over `prev`: the same diff
    /// and backend calls a frame's flush makes.
    fn flushed(prev: &Buffer, next: &Buffer) -> String {
        use ratatui::backend::Backend;
        let mut out = Vec::new();
        ratatui::backend::CrosstermBackend::new(&mut out)
            .draw(prev.diff(next).into_iter())
            .unwrap();
        String::from_utf8(out).unwrap()
    }

    fn rendered(bytes: &[u8], area: Rect) -> Buffer {
        let mut g = Grid::new(area.height, area.width);
        g.feed(bytes);
        let mut buf = Buffer::empty(area);
        g.render_into(&mut buf, area);
        buf
    }

    #[test]
    fn a_linked_cell_carries_its_link_and_its_width() {
        let area = Rect::new(0, 0, 8, 1);
        let buf = rendered(
            b"a\x1b]8;;https://example.com\x07L\xed\x95\x9c\x1b]8;;\x07b",
            area,
        );
        let open = linked_symbol("https://example.com", "");
        let open = open.strip_suffix(LINK_CLOSE).unwrap();
        assert_eq!(
            buf[(0, 0)].symbol(),
            "a",
            "a cell outside the link is plain"
        );
        assert_eq!(buf[(1, 0)].symbol(), format!("{open}L{LINK_CLOSE}"));
        assert_eq!(buf[(2, 0)].symbol(), format!("{open}\u{d55c}{LINK_CLOSE}"));
        assert_eq!(
            buf[(4, 0)].symbol(),
            "b",
            "the link ends where the child ended it"
        );
        assert_eq!(cell_width(&buf[(1, 0)]), 1);
        assert_eq!(
            cell_width(&buf[(2, 0)]),
            2,
            "a wide glyph keeps both columns"
        );
        assert_eq!(visible_symbol(buf[(2, 0)].symbol()), "\u{d55c}");
        assert_eq!(visible_symbol("x"), "x");
    }

    #[test]
    fn the_diffing_renderer_writes_links_and_their_changes_only() {
        let area = Rect::new(0, 0, 8, 1);
        let blank = Buffer::empty(area);
        let linked = rendered(b"\x1b]8;;https://a.example\x07AB\x1b]8;;\x07C", area);
        let out = flushed(&blank, &linked);
        let open = "\x1b]8;id=xmux-";
        assert_eq!(
            out.matches(open).count(),
            2,
            "each linked cell opens its link: {out:?}"
        );
        assert!(
            out.contains(";https://a.example\x1b\\A\x1b]8;;\x1b\\"),
            "{out:?}"
        );
        assert!(
            out.contains(";https://a.example\x1b\\B\x1b]8;;\x1b\\C"),
            "{out:?}"
        );

        let out = flushed(&linked, &linked.clone());
        assert!(
            !out.contains("\x1b]8;") && !out.contains('A'),
            "an unchanged linked frame writes no cell: {out:?}"
        );

        let relinked = rendered(b"\x1b]8;;https://b.example\x07AB\x1b]8;;\x07C", area);
        let out = flushed(&linked, &relinked);
        assert_eq!(out.matches("https://b.example").count(), 2, "{out:?}");
        assert!(
            !out.contains('C'),
            "a cell whose link did not change is not written"
        );

        let unlinked = rendered(b"ABC", area);
        let out = flushed(&linked, &unlinked);
        assert!(
            !out.contains("\x1b]8;"),
            "a cell that lost its link is written plain: {out:?}"
        );
        assert!(out.contains('A') && out.contains('B'), "{out:?}");
    }

    #[test]
    fn a_linked_wide_glyph_moves_the_cursor_past_both_columns() {
        let area = Rect::new(0, 0, 6, 1);
        let blank = Buffer::empty(area);
        let next = rendered(
            b"\x1b]8;;https://a.example\x07\xed\x95\x9c\x1b]8;;\x07z",
            area,
        );
        let out = flushed(&blank, &next);
        let glyph = out.find('\u{d55c}').unwrap();
        let z = out.find('z').unwrap();
        assert!(
            out[glyph..z].contains("\x1b[1;3H"),
            "z is placed after the two columns the linked glyph covers: {out:?}"
        );
    }

    /// The link each cell was written under, by column, from the parser itself.
    fn links(g: &Grid, row: u16, cols: u16) -> Vec<Option<String>> {
        let screen = g.parser.screen();
        (0..cols)
            .map(|c| {
                let cell = screen.cell(row, c)?;
                screen
                    .hyperlink_uri(cell.hyperlink_id())
                    .map(str::to_string)
            })
            .collect()
    }

    #[test]
    fn a_link_outlives_an_sgr_reset_and_rides_a_scroll() {
        let mut g = Grid::new(3, 6);
        g.feed(b"\x1b]8;id=x;https://a.example/p;q\x07A\x1b[1mB\x1b[0mC\x1b]8;;\x07D");
        let a = Some("https://a.example/p;q".to_string());
        assert_eq!(
            links(&g, 0, 4),
            vec![a.clone(), a.clone(), a.clone(), None],
            "an SGR reset leaves the link open, and a `;` in the URI is kept"
        );
        g.feed(b"\x1b[3;1H\x1b]8;;https://b.example\x07Z\x1b]8;;\x07\r\n");
        assert_eq!(
            links(&g, 1, 1),
            vec![Some("https://b.example".to_string())],
            "a linked cell keeps its link as the screen scrolls"
        );
        g.feed(b"\x1b[H\x1b]8;;https://a.example/p;q\x07E\x1b]8;;\x07");
        assert_eq!(links(&g, 0, 1), vec![a], "a URI seen before is reused");
    }
}
