//! The terminal emulator behind the grid: the cell model a session's client writes into,
//! and the sequences it sends that the cell model leaves to xmux.
//!
//! The emulator applies what draws (text, cursor motion, erases, attributes, links) and
//! drops what it does not model, such as notifications and most terminal queries, and
//! the replies it would make itself never leave it. A second parser in lockstep with it
//! hands each whole control sequence and bell to the grid's [`Hooks`] right after the
//! emulator has applied every byte before it, so a reply reads the cursor and modes as
//! the client left them at that point of the stream, and each query is seen once however
//! the reads split it.

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::{Dimensions, Grid};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Cell as TermCell, Flags};
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte;
use alacritty_terminal::vte::ansi::{Color as TermColor, NamedColor, Processor};

/// The fewest columns the emulator is sized to: narrower, its reflow has no column left
/// to move a wide character to.
const MIN_COLS: u16 = 2;
/// The fewest rows the emulator is sized to.
const MIN_ROWS: u16 = 1;

/// What the grid does with the sequences the emulator leaves to it. Each call comes after
/// the emulator has applied every byte before the sequence, and the sequence itself.
pub trait Hooks {
    /// A whole CSI sequence: its first and second intermediate (a private marker such as
    /// `?` or `>` counts as one), its parameters with their sub-parameters, and its final
    /// character.
    fn csi(
        &mut self,
        _screen: View<'_>,
        _i1: Option<u8>,
        _i2: Option<u8>,
        _params: &[&[u16]],
        _c: char,
    ) {
    }

    /// A whole OSC sequence, split at its semicolons.
    fn osc(&mut self, _screen: View<'_>, _params: &[&[u8]]) {}

    /// A BEL outside any escape sequence.
    fn bell(&mut self, _screen: View<'_>) {}
}

impl Hooks for () {}

/// Never holds a synchronized update: the emulator applies the client's bytes as they
/// arrive, and the grid holds the screen on view itself, so a write the grid makes
/// during an update (an image's cells) lands at once.
#[derive(Default)]
pub struct NoSync;

impl vte::ansi::Timeout for NoSync {
    fn set_timeout(&mut self, _: std::time::Duration) {}
    fn clear_timeout(&mut self) {}
    fn pending_timeout(&self) -> bool {
        false
    }
}

/// The emulator with the grid's hooks.
pub struct Parser<H> {
    screen: Screen,
    processor: Processor<NoSync>,
    observer: vte::Parser,
    seen: Seen,
    hooks: H,
}

impl<H: Hooks> Parser<H> {
    pub fn new(rows: u16, cols: u16, hooks: H) -> Self {
        Self {
            screen: Screen::new(rows, cols),
            processor: Processor::new(),
            observer: vte::Parser::new(),
            seen: Seen::default(),
            hooks,
        }
    }

    /// Applies `bytes`, calling the hooks at each sequence they hold.
    pub fn process(&mut self, bytes: &[u8]) {
        let mut rest = bytes;
        while !rest.is_empty() {
            let n = self.observer.advance_until_terminated(&mut self.seen, rest);
            self.processor.advance(&mut self.screen.term, &rest[..n]);
            rest = &rest[n..];
            if let Some(event) = self.seen.event.take() {
                self.dispatch(event);
            }
        }
    }

    fn dispatch(&mut self, event: Event) {
        let view = self.screen.view();
        match event {
            Event::Bell => self.hooks.bell(view),
            Event::Csi {
                i1,
                i2,
                ref params,
                c,
            } => {
                let params: Vec<&[u16]> = params.iter().map(Vec::as_slice).collect();
                self.hooks.csi(view, i1, i2, &params, c);
            }
            Event::Osc { ref params } => {
                let params: Vec<&[u8]> = params.iter().map(Vec::as_slice).collect();
                self.hooks.osc(view, &params);
            }
        }
    }

    pub fn screen(&self) -> &Screen {
        &self.screen
    }

    pub fn screen_mut(&mut self) -> &mut Screen {
        &mut self.screen
    }

    pub fn callbacks(&self) -> &H {
        &self.hooks
    }

    pub fn callbacks_mut(&mut self) -> &mut H {
        &mut self.hooks
    }
}

/// The sequence the observing parser stopped at.
enum Event {
    Bell,
    Csi {
        i1: Option<u8>,
        i2: Option<u8>,
        params: Vec<Vec<u16>>,
        c: char,
    },
    Osc {
        params: Vec<Vec<u8>>,
    },
}

/// The observing parser's performer: it records the first sequence it sees and stops
/// there, so the emulator can catch up before the hooks run.
#[derive(Default)]
struct Seen {
    event: Option<Event>,
}

impl vte::Perform for Seen {
    fn execute(&mut self, byte: u8) {
        if byte == 0x07 {
            self.event = Some(Event::Bell);
        }
    }

    fn csi_dispatch(&mut self, params: &vte::Params, intermediates: &[u8], ignore: bool, c: char) {
        if ignore {
            return;
        }
        self.event = Some(Event::Csi {
            i1: intermediates.first().copied(),
            i2: intermediates.get(1).copied(),
            params: params.iter().map(<[u16]>::to_vec).collect(),
            c,
        });
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        self.event = Some(Event::Osc {
            params: params.iter().map(|p| p.to_vec()).collect(),
        });
    }

    fn terminated(&self) -> bool {
        self.event.is_some()
    }
}

/// The live screen.
pub struct Screen {
    term: Term<VoidListener>,
}

impl Screen {
    fn new(rows: u16, cols: u16) -> Self {
        let config = Config {
            scrolling_history: 0,
            kitty_keyboard: false,
            osc52: alacritty_terminal::term::Osc52::Disabled,
            ..Config::default()
        };
        Self {
            term: Term::new(config, &size(rows, cols), VoidListener),
        }
    }

    pub fn view(&self) -> View<'_> {
        View {
            grid: self.term.grid(),
            mode: *self.term.mode(),
        }
    }

    /// Resizes to `rows` x `cols`, no smaller than the emulator can reflow.
    pub fn set_size(&mut self, rows: u16, cols: u16) {
        self.term.resize(size(rows, cols));
    }

    /// The SGR sequence that restores the attributes the client is writing with.
    pub fn attributes_formatted(&self) -> Vec<u8> {
        let template = &self.term.grid().cursor.template;
        let mut sgr = String::from("\x1b[0");
        for (flag, code) in [
            (Flags::BOLD, "1"),
            (Flags::DIM, "2"),
            (Flags::ITALIC, "3"),
            (Flags::INVERSE, "7"),
            (Flags::HIDDEN, "8"),
            (Flags::STRIKEOUT, "9"),
        ] {
            if template.flags.contains(flag) {
                sgr.push(';');
                sgr.push_str(code);
            }
        }
        if let Some(style) = underline_code(template.flags) {
            sgr.push_str(";4:");
            sgr.push(style);
        }
        push_colour(&mut sgr, "38", template.fg);
        push_colour(&mut sgr, "48", template.bg);
        if let Some(colour) = template.underline_color() {
            push_colour(&mut sgr, "58", colour);
        }
        sgr.push('m');
        sgr.into_bytes()
    }

    pub fn size(&self) -> (u16, u16) {
        self.view().size()
    }

    pub fn cursor_position(&self) -> (u16, u16) {
        self.view().cursor_position()
    }

    pub fn cell(&self, row: u16, col: u16) -> Option<Cell<'_>> {
        self.view().cell(row, col)
    }

    pub fn contents(&self) -> String {
        self.view().contents()
    }
}

fn size(rows: u16, cols: u16) -> TermSize {
    TermSize::new(
        usize::from(cols.max(MIN_COLS)),
        usize::from(rows.max(MIN_ROWS)),
    )
}

/// The `4:n` style of an underline, if the cell has one.
fn underline_code(flags: Flags) -> Option<char> {
    Some(match Underline::of(flags) {
        Underline::None => return None,
        Underline::Single => '1',
        Underline::Double => '2',
        Underline::Curly => '3',
        Underline::Dotted => '4',
        Underline::Dashed => '5',
    })
}

/// Appends the SGR fields that set a colour in the slot `base` names (38, 48, or 58).
fn push_colour(sgr: &mut String, base: &str, colour: TermColor) {
    use std::fmt::Write;
    let _ = match Color::from(colour) {
        Color::Default => return,
        Color::Idx(i) => write!(sgr, ";{base};5;{i}"),
        Color::Rgb(r, g, b) => write!(sgr, ";{base};2;{r};{g};{b}"),
    };
}

/// A screen kept as it stood: the one a synchronized update holds on view.
pub struct Snapshot {
    grid: Grid<TermCell>,
    mode: TermMode,
}

impl Snapshot {
    pub fn view(&self) -> View<'_> {
        View {
            grid: &self.grid,
            mode: self.mode,
        }
    }
}

/// A read of a screen, live or kept.
#[derive(Clone, Copy)]
pub struct View<'a> {
    grid: &'a Grid<TermCell>,
    mode: TermMode,
}

impl<'a> View<'a> {
    /// `(rows, cols)`.
    pub fn size(&self) -> (u16, u16) {
        (
            u16::try_from(self.grid.screen_lines()).unwrap_or(u16::MAX),
            u16::try_from(self.grid.columns()).unwrap_or(u16::MAX),
        )
    }

    /// `(row, col)` of the cursor.
    pub fn cursor_position(&self) -> (u16, u16) {
        let point = self.grid.cursor.point;
        (
            u16::try_from(point.line.0.max(0)).unwrap_or(u16::MAX),
            u16::try_from(point.column.0).unwrap_or(u16::MAX),
        )
    }

    /// The screen as it stands, kept apart from the live one.
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            grid: self.grid.clone(),
            mode: self.mode,
        }
    }

    pub fn hide_cursor(&self) -> bool {
        !self.mode.contains(TermMode::SHOW_CURSOR)
    }

    pub fn application_cursor(&self) -> bool {
        self.mode.contains(TermMode::APP_CURSOR)
    }

    pub fn cell(&self, row: u16, col: u16) -> Option<Cell<'a>> {
        let (rows, cols) = self.size();
        (row < rows && col < cols)
            .then(|| Cell(&self.grid[Line(i32::from(row))][Column(usize::from(col))]))
    }

    /// The text on screen, one line per row with the trailing blanks of each row left
    /// out.
    pub fn contents(&self) -> String {
        let (rows, cols) = self.size();
        let mut text = String::new();
        for row in 0..rows {
            if row > 0 {
                text.push('\n');
            }
            let start = text.len();
            for col in 0..cols {
                match self.cell(row, col) {
                    Some(cell) if cell.is_wide_continuation() => {}
                    Some(cell) if cell.has_contents() => cell.push_contents(&mut text),
                    _ => text.push(' '),
                }
            }
            let end = start + text[start..].trim_end().len();
            text.truncate(end);
        }
        text
    }
}

/// A colour as the grid draws it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    /// The terminal's own foreground or background.
    Default,
    /// A palette index.
    Idx(u8),
    Rgb(u8, u8, u8),
}

impl From<TermColor> for Color {
    fn from(colour: TermColor) -> Self {
        match colour {
            TermColor::Spec(rgb) => Color::Rgb(rgb.r, rgb.g, rgb.b),
            TermColor::Indexed(i) => Color::Idx(i),
            TermColor::Named(named) => {
                let n = named as usize;
                let dim = NamedColor::DimBlack as usize;
                match n {
                    0..=15 => Color::Idx(n as u8),
                    _ if (dim..dim + 8).contains(&n) => Color::Idx((n - dim) as u8),
                    _ => Color::Default,
                }
            }
        }
    }
}

/// The style of a cell's underline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Underline {
    None,
    Single,
    Double,
    Curly,
    Dotted,
    Dashed,
}

impl Underline {
    fn of(flags: Flags) -> Self {
        if flags.contains(Flags::UNDERCURL) {
            Underline::Curly
        } else if flags.contains(Flags::DOUBLE_UNDERLINE) {
            Underline::Double
        } else if flags.contains(Flags::DOTTED_UNDERLINE) {
            Underline::Dotted
        } else if flags.contains(Flags::DASHED_UNDERLINE) {
            Underline::Dashed
        } else if flags.contains(Flags::UNDERLINE) {
            Underline::Single
        } else {
            Underline::None
        }
    }
}

/// One cell of a screen.
#[derive(Clone, Copy)]
pub struct Cell<'a>(&'a TermCell);

impl Cell<'_> {
    /// The cell's text: its character and the zero-width characters written after it,
    /// or nothing for a cell no character was written to.
    pub fn contents(&self) -> String {
        let mut text = String::new();
        self.push_contents(&mut text);
        text
    }

    fn push_contents(&self, text: &mut String) {
        if self.has_contents() {
            text.push(self.0.c);
            text.extend(self.0.zerowidth().into_iter().flatten());
        }
    }

    pub fn has_contents(&self) -> bool {
        self.0.c != ' ' || self.0.zerowidth().is_some()
    }

    pub fn fgcolor(&self) -> Color {
        self.0.fg.into()
    }

    pub fn bgcolor(&self) -> Color {
        self.0.bg.into()
    }

    pub fn bold(&self) -> bool {
        self.0.flags.contains(Flags::BOLD)
    }

    pub fn italic(&self) -> bool {
        self.0.flags.contains(Flags::ITALIC)
    }

    pub fn inverse(&self) -> bool {
        self.0.flags.contains(Flags::INVERSE)
    }

    pub fn underline(&self) -> Underline {
        Underline::of(self.0.flags)
    }

    /// The underline's own colour, when the client set one apart from the text's.
    pub fn underline_color(&self) -> Option<Color> {
        self.0.underline_color().map(Color::from)
    }

    /// The URI of the OSC 8 link the cell was written under.
    pub fn hyperlink(&self) -> Option<String> {
        self.0.hyperlink().map(|link| link.uri().to_string())
    }

    /// Whether the cell holds a character two columns wide.
    pub fn is_wide(&self) -> bool {
        self.0.flags.contains(Flags::WIDE_CHAR)
    }

    /// Whether the cell is the right half of a wide character.
    pub fn is_wide_continuation(&self) -> bool {
        self.0.flags.contains(Flags::WIDE_CHAR_SPACER)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A CSI as the hooks saw it: its intermediates, parameters, final character, and
    /// the cursor at that point.
    type SeenCsi = (Option<u8>, Option<u8>, Vec<Vec<u16>>, char, (u16, u16));

    #[derive(Default)]
    struct Log {
        csi: Vec<SeenCsi>,
        osc: Vec<Vec<Vec<u8>>>,
        bells: usize,
    }

    impl Hooks for Log {
        fn csi(
            &mut self,
            screen: View<'_>,
            i1: Option<u8>,
            i2: Option<u8>,
            params: &[&[u16]],
            c: char,
        ) {
            let params = params.iter().map(|p| p.to_vec()).collect();
            self.csi.push((i1, i2, params, c, screen.cursor_position()));
        }

        fn osc(&mut self, _: View<'_>, params: &[&[u8]]) {
            self.osc.push(params.iter().map(|p| p.to_vec()).collect());
        }

        fn bell(&mut self, _: View<'_>) {
            self.bells += 1;
        }
    }

    fn row(p: &Parser<Log>, r: u16) -> String {
        let (_, cols) = p.screen().size();
        (0..cols)
            .filter_map(|c| p.screen().cell(r, c))
            .filter(|c| !c.is_wide_continuation())
            .map(|c| {
                let s = c.contents();
                if s.is_empty() {
                    " ".to_string()
                } else {
                    s
                }
            })
            .collect()
    }

    #[test]
    fn a_query_sees_the_cursor_the_text_before_it_left() {
        let mut p = Parser::new(4, 10, Log::default());
        p.process(b"ab\x1b[6nc");
        assert_eq!(p.callbacks().csi.len(), 1);
        let (i1, i2, params, c, cursor) = &p.callbacks().csi[0];
        assert_eq!(
            (*i1, *i2, params.clone(), *c),
            (None, None, vec![vec![6]], 'n')
        );
        assert_eq!(*cursor, (0, 2));
        assert_eq!(row(&p, 0).trim_end(), "abc");
    }

    #[test]
    fn a_split_sequence_is_seen_once_whole() {
        let mut p = Parser::new(4, 10, Log::default());
        for b in b"x\x1b[?2026h\x1b]9;hi\x07\x07" {
            p.process(&[*b]);
        }
        let log = p.callbacks();
        assert_eq!(log.csi.len(), 1);
        assert_eq!(log.csi[0].0, Some(b'?'));
        assert_eq!(log.csi[0].2, vec![vec![2026]]);
        assert_eq!(log.osc, vec![vec![b"9".to_vec(), b"hi".to_vec()]]);
        assert_eq!(log.bells, 1, "a BEL ending an OSC is not a bell");
    }

    #[test]
    fn a_synchronized_update_applies_at_once() {
        let mut p = Parser::new(2, 10, Log::default());
        p.process(b"\x1b[?2026hnew");
        assert_eq!(row(&p, 0).trim_end(), "new");
    }

    #[test]
    fn shrinking_through_a_wide_char_keeps_the_screen_usable() {
        let mut p = Parser::new(4, 6, Log::default());
        for width in [6u16, 5, 4, 3, 2, 1, 0, 3, 7, 5] {
            p.process("가나다라마바\r\n한글".as_bytes());
            p.screen_mut().set_size(4, width);
            p.process("\x1b[2K\x1b[1;1H가\x1b[K".as_bytes());
            let (_, cols) = p.screen().size();
            assert!(cols >= MIN_COLS);
        }
        p.screen_mut().set_size(2, 8);
        p.process("\x1b[2J\x1b[H가나다".as_bytes());
        assert_eq!(row(&p, 0).trim_end(), "가나다");
    }

    #[test]
    fn cells_keep_links_and_underline_styles() {
        let mut p = Parser::new(2, 10, Log::default());
        p.process(b"\x1b]8;;https://e.x\x07L\x1b]8;;\x07\x1b[4:3;58;2;1;2;3mU\x1b[0mP");
        let s = p.screen();
        assert_eq!(
            s.cell(0, 0).unwrap().hyperlink().as_deref(),
            Some("https://e.x")
        );
        assert_eq!(s.cell(0, 1).unwrap().hyperlink(), None);
        assert_eq!(s.cell(0, 1).unwrap().underline(), Underline::Curly);
        assert_eq!(
            s.cell(0, 1).unwrap().underline_color(),
            Some(Color::Rgb(1, 2, 3))
        );
        assert_eq!(s.cell(0, 2).unwrap().underline(), Underline::None);
    }

    #[test]
    fn colours_map_to_palette_indexes_and_the_default() {
        let mut p = Parser::new(2, 10, Log::default());
        p.process(b"\x1b[31;42ma\x1b[38;5;200mb\x1b[39;49mc\x1b[38;2;1;2;3md");
        let s = p.screen();
        let fg = |c| s.cell(0, c).unwrap().fgcolor();
        assert_eq!(fg(0), Color::Idx(1));
        assert_eq!(s.cell(0, 0).unwrap().bgcolor(), Color::Idx(2));
        assert_eq!(fg(1), Color::Idx(200));
        assert_eq!(fg(2), Color::Default);
        assert_eq!(fg(3), Color::Rgb(1, 2, 3));
    }

    #[test]
    fn attributes_formatted_restores_the_template() {
        let mut p = Parser::new(2, 10, Log::default());
        p.process(b"\x1b[1;4:3;38;5;9;48;2;1;2;3m");
        let sgr = p.screen().attributes_formatted();
        let mut q = Parser::new(2, 10, Log::default());
        q.process(&sgr);
        q.process(b"x");
        let c = q.screen().cell(0, 0).unwrap();
        assert!(c.bold());
        assert_eq!(c.underline(), Underline::Curly);
        assert_eq!(c.fgcolor(), Color::Idx(9));
        assert_eq!(c.bgcolor(), Color::Rgb(1, 2, 3));
    }

    #[test]
    fn a_snapshot_keeps_the_screen_it_was_taken_of() {
        let mut p = Parser::new(2, 10, Log::default());
        p.process(b"old");
        let snap = p.screen().view().snapshot();
        p.process(b"\x1b[2J\x1b[Hnew");
        assert_eq!(snap.view().contents().lines().next(), Some("old"));
        assert_eq!(p.screen().contents().lines().next(), Some("new"));
    }

    #[test]
    fn combining_marks_join_their_cell() {
        let mut p = Parser::new(2, 10, Log::default());
        p.process("e\u{301}x".as_bytes());
        assert_eq!(p.screen().cell(0, 0).unwrap().contents(), "e\u{301}");
        assert_eq!(p.screen().cell(0, 1).unwrap().contents(), "x");
    }
}
