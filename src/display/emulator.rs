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
//!
//! Scrolls inside left and right margins (DECLRMM, DECSLRM) and rectangle fills
//! (DECFRA) are applied here, since the emulator has neither: tmux uses them to scroll
//! and clear one pane of a split window when the terminal it draws to reports them.

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

/// The most UTF-8 bytes one cell's text joins, the limit tmux keeps a cell to
/// (`UTF8_SIZE`). A character that would pass it starts a cell of its own there, so it
/// does here.
const CELL_BYTES: usize = 21;

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
    /// The start of a UTF-8 character the last read cut off, held until the rest
    /// arrives so the character reaches the emulator, or is joined, whole.
    partial: Vec<u8>,
    /// Whether the last read ended on a zero-width joiner, so the next one's first
    /// character ends a piece of its own.
    zwj_tail: bool,
}

impl<H: Hooks> Parser<H> {
    pub fn new(rows: u16, cols: u16, hooks: H) -> Self {
        Self {
            screen: Screen::new(rows, cols),
            processor: Processor::new(),
            observer: vte::Parser::new(),
            seen: Seen::default(),
            hooks,
            partial: Vec::new(),
            zwj_tail: false,
        }
    }

    /// Applies `bytes`, calling the hooks at each sequence they hold.
    pub fn process(&mut self, bytes: &[u8]) {
        let mut joined = std::mem::take(&mut self.partial);
        joined.extend_from_slice(bytes);
        let cut = joined.len() - incomplete_utf8(&joined);
        self.partial = joined.split_off(cut);
        let mut points = join_points(&joined);
        if std::mem::take(&mut self.zwj_tail) {
            if let Some(&lead) = joined.first() {
                points.insert(0, utf8_len(lead).min(joined.len()));
            }
        }
        self.zwj_tail = joined.ends_with(ZWJ);
        let mut start = 0;
        for end in points {
            self.process_piece(&joined[start..end]);
            start = end;
        }
        self.process_piece(&joined[start..]);
    }

    /// Applies a piece of the stream. The observer prints a run of text in one go, so a
    /// character the grid places itself has to end the piece it is in.
    fn process_piece(&mut self, piece: &[u8]) {
        let mut rest = piece;
        while !rest.is_empty() {
            // While margins are set a line feed has to end its piece too: the observer
            // prints a run of text and the controls in it in one go.
            self.seen.margins = self.screen.margins().is_some();
            let end = if self.seen.margins {
                rest.iter()
                    .position(|b| (0x0a..=0x0c).contains(b))
                    .map_or(rest.len(), |i| i + 1)
            } else {
                rest.len()
            };
            let n = self
                .observer
                .advance_until_terminated(&mut self.seen, &rest[..end]);
            let mut buf = [0; 4];
            let (applied, held) = match self.seen.event {
                // The character is the last thing the observer read; the emulator gets
                // what came before it, and the character is placed here.
                Some(Event::Join(c)) if rest[..n].ends_with(c.encode_utf8(&mut buf).as_bytes()) => {
                    rest[..n].split_at(n - c.len_utf8())
                }
                Some(Event::Join(_)) => {
                    self.seen.event = None;
                    (&rest[..n], &[][..])
                }
                // The sequence is the last thing the observer read, from its ESC, or its
                // control byte; the emulator gets what came before it, and the sequence is
                // applied around the margins. A sequence an earlier read started is all of
                // what this read holds up to it.
                Some(Event::Scroll { control: true, .. }) => rest[..n].split_at(n - 1),
                Some(ref event) if self.holds(event) => {
                    let start = rest[..n].iter().rposition(|&b| b == 0x1b).unwrap_or(0);
                    rest[..n].split_at(start)
                }
                _ => (&rest[..n], &[][..]),
            };
            self.processor.advance(&mut self.screen.term, applied);
            rest = &rest[n..];
            if let Some(event) = self.seen.event.take() {
                self.dispatch(event, held);
            }
        }
    }

    /// Whether the sequence `event` ends is held back from the emulator, to be applied
    /// around the margins.
    fn holds(&self, event: &Event) -> bool {
        match event {
            Event::Scroll { .. } | Event::Reset => true,
            Event::Csi {
                i1: None,
                i2: None,
                c: 'S' | 'T',
                ..
            } => self.screen.margins().is_some(),
            Event::Csi {
                i1: None,
                i2: None,
                c: 's',
                ..
            } => self.screen.lrmm,
            _ => false,
        }
    }

    fn dispatch(&mut self, event: Event, held: &[u8]) {
        let processor = &mut self.processor;
        match event {
            Event::Join(c) => {
                if !self.screen.join(c) {
                    processor.advance(&mut self.screen.term, held);
                }
            }
            Event::Widen => self.screen.widen(),
            Event::Bell => self.hooks.bell(self.screen.view()),
            Event::Scroll { next_line, .. } => {
                self.screen
                    .inside_margins(|term| processor.advance(term, held));
                if next_line {
                    if let Some((left, _)) = self.screen.margins() {
                        self.screen.term.grid_mut().cursor.point.column = Column(left);
                    }
                }
            }
            Event::Reset => {
                processor.advance(&mut self.screen.term, held);
                self.screen.lrmm = false;
                self.screen.lr_margins = None;
            }
            Event::Csi {
                i1,
                i2,
                ref params,
                c,
            } => {
                let params: Vec<&[u16]> = params.iter().map(Vec::as_slice).collect();
                let modes = || params.iter().filter_map(|p| p.first().copied());
                match (i1, i2, c) {
                    (None, None, 'S' | 'T') => self
                        .screen
                        .inside_margins(|term| processor.advance(term, held)),
                    (None, None, 's') if self.screen.lrmm => {
                        // CSI s sets the margins while DECLRMM is on, and saves the
                        // cursor only while it is off; the emulator knows the latter.
                        let saved = self.screen.term.grid().saved_cursor.clone();
                        processor.advance(&mut self.screen.term, held);
                        self.screen.term.grid_mut().saved_cursor = saved;
                        self.screen.set_margins(&params);
                    }
                    (Some(b'?'), None, 'h' | 'l') if modes().any(|m| m == 69) => {
                        self.screen.lrmm = c == 'h';
                        self.screen.lr_margins = None;
                    }
                    (Some(b'!'), None, 'p') => self.screen.lr_margins = None,
                    (Some(b'$'), None, 'x') => self.screen.fill(&params),
                    _ => processor.advance(&mut self.screen.term, held),
                }
                if matches!((i1, i2, c), (None, None, 'X' | 'K' | 'J')) {
                    self.screen.clear_orphaned_spacers();
                }
                self.hooks.csi(self.screen.view(), i1, i2, &params, c);
            }
            Event::Osc { ref params } => {
                let params: Vec<&[u8]> = params.iter().map(Vec::as_slice).collect();
                self.hooks.osc(self.screen.view(), &params);
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

/// What the observing parser stopped at.
enum Event {
    Bell,
    /// A character tmux joins into the cell before it: a skin tone modifier, or a
    /// character after a zero-width joiner.
    Join(char),
    /// A VS16, which tmux takes as asking for the character before it two columns wide.
    Widen,
    /// A line feed, index, next line, or reverse index while margins are set. `control`
    /// tells a control byte from an ESC sequence.
    Scroll {
        next_line: bool,
        control: bool,
    },
    /// A full reset, which clears the margins too.
    Reset,
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
    /// Whether the last character printed was a zero-width joiner.
    after_zwj: bool,
    /// Whether left and right margins are set, so a line feed has to stop the observer.
    margins: bool,
}

impl vte::Perform for Seen {
    fn print(&mut self, c: char) {
        use unicode_width::UnicodeWidthChar;
        let wide = c.width().is_some_and(|w| w > 0);
        if ('\u{1f3fb}'..='\u{1f3ff}').contains(&c) || (self.after_zwj && wide) {
            self.event = Some(Event::Join(c));
        } else if c == '\u{fe0f}' {
            self.event = Some(Event::Widen);
        }
        self.after_zwj = c == '\u{200d}';
    }

    fn execute(&mut self, byte: u8) {
        self.after_zwj = false;
        if byte == 0x07 {
            self.event = Some(Event::Bell);
        } else if self.margins && (0x0a..=0x0c).contains(&byte) {
            self.event = Some(Event::Scroll {
                next_line: false,
                control: true,
            });
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
        self.after_zwj = false;
        if ignore || !intermediates.is_empty() {
            return;
        }
        match byte {
            b'D' | b'M' | b'E' if self.margins => {
                self.event = Some(Event::Scroll {
                    next_line: byte == b'E',
                    control: false,
                });
            }
            b'c' => self.event = Some(Event::Reset),
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &vte::Params, intermediates: &[u8], ignore: bool, c: char) {
        self.after_zwj = false;
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
        self.after_zwj = false;
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
    /// Whether DECLRMM lets CSI s set left and right margins.
    lrmm: bool,
    /// The left and right margin columns DECSLRM set, `None` for the whole width.
    lr_margins: Option<(usize, usize)>,
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
            lrmm: false,
            lr_margins: None,
        }
    }

    /// The left and right margin columns while they narrow the screen.
    fn margins(&self) -> Option<(usize, usize)> {
        let (left, right) = self.lr_margins.filter(|_| self.lrmm)?;
        let last = self.term.grid().columns() - 1;
        let right = right.min(last);
        (left < right && (left > 0 || right < last)).then_some((left, right))
    }

    /// DECSLRM: sets the margins from 1-based left and right columns and homes the
    /// cursor. A left column not before the right one leaves the margins as they are.
    fn set_margins(&mut self, params: &[&[u16]]) {
        let columns = self.term.grid().columns();
        let left = param(params, 0, 1);
        let right = param(params, 1, columns).min(columns);
        if left >= right {
            return;
        }
        self.lr_margins = Some((left - 1, right - 1));
        let cursor = &mut self.term.grid_mut().cursor;
        cursor.point = alacritty_terminal::index::Point::new(Line(0), Column(0));
        cursor.input_needs_wrap = false;
    }

    /// Applies a scroll the emulator makes across whole rows inside the margins only:
    /// the columns outside them keep the cells they had.
    fn inside_margins(&mut self, apply: impl FnOnce(&mut Term<VoidListener>)) {
        let Some((left, right)) = self.margins() else {
            apply(&mut self.term);
            return;
        };
        let grid = self.term.grid();
        let columns = grid.columns();
        let outside = || (0..left).chain(right + 1..columns);
        let kept: Vec<Vec<TermCell>> = (0..grid.screen_lines())
            .map(|y| {
                let row = &grid[Line(y as i32)];
                outside().map(|x| row[Column(x)].clone()).collect()
            })
            .collect();
        apply(&mut self.term);
        let grid = self.term.grid_mut();
        for (y, cells) in kept.into_iter().enumerate() {
            let row = &mut grid[Line(y as i32)];
            for (x, cell) in outside().zip(cells) {
                row[Column(x)] = cell;
            }
        }
    }

    /// A partial erase can leave a spacer without its wide glyph. Keeping its flag
    /// lets the emulator's next wide write clear the new glyph while placing its
    /// right half. Only the spacer loses its flag; surviving wide glyphs stay intact.
    fn clear_orphaned_spacers(&mut self) {
        let grid = self.term.grid_mut();
        let columns = grid.columns();
        let line = grid.cursor.point.line;
        let row = &mut grid[line];
        for col in 1..columns {
            if row[Column(col)].flags.contains(Flags::WIDE_CHAR_SPACER)
                && !row[Column(col - 1)].flags.contains(Flags::WIDE_CHAR)
            {
                row[Column(col)].flags.remove(Flags::WIDE_CHAR_SPACER);
            }
        }
    }

    /// DECFRA: fills the rectangle between 1-based top, left, bottom, and right with
    /// the character `Pc`, written with the client's current attributes.
    fn fill(&mut self, params: &[&[u16]]) {
        let Some(c) = params
            .first()
            .and_then(|p| p.first())
            .and_then(|&code| char::from_u32(u32::from(code)))
            .filter(|c| matches!(*c as u32, 32..=126 | 160..=255))
        else {
            return;
        };
        let grid = self.term.grid_mut();
        let (rows, columns) = (grid.screen_lines(), grid.columns());
        let top = param(params, 1, 1);
        let left = param(params, 2, 1);
        let bottom = param(params, 3, rows).min(rows);
        let right = param(params, 4, columns).min(columns);
        if top > bottom || left > right {
            return;
        }
        let mut cell = grid.cursor.template.clone();
        cell.c = c;
        cell.flags.remove(
            Flags::WIDE_CHAR
                | Flags::WIDE_CHAR_SPACER
                | Flags::LEADING_WIDE_CHAR_SPACER
                | Flags::WRAPLINE,
        );
        for y in top - 1..bottom {
            let row = &mut grid[Line(y as i32)];
            for x in left - 1..right {
                row[Column(x)] = cell.clone();
            }
        }
    }

    /// The column of the cell the last character went into, on the cursor's line.
    fn last_written(&self) -> Option<Column> {
        let grid = self.term.grid();
        let cursor = &grid.cursor;
        let mut col = cursor.point.column;
        if !cursor.input_needs_wrap {
            col = Column(col.0.checked_sub(1)?);
        }
        if grid[cursor.point.line][col]
            .flags
            .contains(Flags::WIDE_CHAR_SPACER)
        {
            col = Column(col.0.checked_sub(1)?);
        }
        Some(col)
    }

    /// Joins `c` into the cell the last character went into, while that cell's text
    /// stays within [`CELL_BYTES`]. False when there is no such cell or no room.
    fn join(&mut self, c: char) -> bool {
        let Some(col) = self.last_written() else {
            return false;
        };
        let line = self.term.grid().cursor.point.line;
        let cell = &mut self.term.grid_mut()[line][col];
        let bytes = cell.c.len_utf8()
            + cell
                .zerowidth()
                .into_iter()
                .flatten()
                .map(|z| z.len_utf8())
                .sum::<usize>();
        if cell.c == ' ' || bytes + c.len_utf8() > CELL_BYTES {
            return false;
        }
        cell.push_zerowidth(c);
        true
    }

    /// Makes the one-column character the cursor just passed two columns wide, the
    /// cursor moving past both, when the row has a column left for it.
    fn widen(&mut self) {
        let grid = self.term.grid_mut();
        let cursor = &grid.cursor;
        if cursor.input_needs_wrap || cursor.point.column.0 == 0 {
            return;
        }
        let line = cursor.point.line;
        let col = cursor.point.column.0 - 1;
        let columns = grid.columns();
        if col + 1 >= columns {
            return;
        }
        let row = &mut grid[line];
        if row[Column(col)]
            .flags
            .intersects(Flags::WIDE_CHAR | Flags::WIDE_CHAR_SPACER)
        {
            return;
        }
        row[Column(col)].flags.insert(Flags::WIDE_CHAR);
        let mut spacer = TermCell {
            bg: row[Column(col)].bg,
            ..TermCell::default()
        };
        spacer.flags = Flags::WIDE_CHAR_SPACER;
        row[Column(col + 1)] = spacer;
        let cursor = &mut grid.cursor;
        if col + 2 < columns {
            cursor.point.column = Column(col + 2);
        } else {
            cursor.point.column = Column(col + 1);
            cursor.input_needs_wrap = true;
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
        self.lr_margins = None;
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

const ZWJ: &[u8] = "\u{200d}".as_bytes();
const VS16: &[u8] = "\u{fe0f}".as_bytes();

/// The length of the UTF-8 character `lead` starts, 1 for a byte that starts none.
fn utf8_len(lead: u8) -> usize {
    match lead {
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    }
}

/// Where the characters the grid places itself end: a skin tone modifier, a VS16, and
/// the character after a zero-width joiner. Wherever they are, inside a control string
/// too, a piece ending there changes nothing but where the stream is split.
fn join_points(bytes: &[u8]) -> Vec<usize> {
    let mut points = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let rest = &bytes[i..];
        if rest.starts_with(VS16) || matches!(rest, [0xf0, 0x9f, 0x8f, 0xbb..=0xbf, ..]) {
            i += utf8_len(rest[0]);
            points.push(i);
        } else if rest.starts_with(ZWJ) && i + ZWJ.len() < bytes.len() {
            i += ZWJ.len();
            i = (i + utf8_len(bytes[i])).min(bytes.len());
            points.push(i);
        } else {
            i += 1;
        }
    }
    points
}

/// How many bytes at the end of `bytes` start a UTF-8 character that is not complete.
fn incomplete_utf8(bytes: &[u8]) -> usize {
    for back in 1..=bytes.len().min(3) {
        let b = bytes[bytes.len() - back];
        let len = match b {
            0x80..=0xbf => continue,
            0xc0..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf7 => 4,
            _ => return 0,
        };
        return if back < len { back } else { 0 };
    }
    0
}

/// The 1-based parameter at `index`, `default` when it is missing or zero.
fn param(params: &[&[u16]], index: usize, default: usize) -> usize {
    params
        .get(index)
        .and_then(|p| p.first())
        .filter(|&&v| v != 0)
        .map_or(default, |&v| usize::from(v))
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

    /// The cell at `(row, col)`. A wide character and its right half are read as such
    /// only while both are there: an erase or a shift that took one half leaves the other
    /// as a blank cell, which is what a terminal and the client's mux hold there.
    pub fn cell(&self, row: u16, col: u16) -> Option<Cell<'a>> {
        let (rows, cols) = self.size();
        if row >= rows || col >= cols {
            return None;
        }
        let line = &self.grid[Line(i32::from(row))];
        let col = usize::from(col);
        let flags = |c: usize| line[Column(c)].flags;
        let wide = flags(col).contains(Flags::WIDE_CHAR)
            && col + 1 < usize::from(cols)
            && flags(col + 1).contains(Flags::WIDE_CHAR_SPACER);
        let continuation = flags(col).contains(Flags::WIDE_CHAR_SPACER)
            && col > 0
            && flags(col - 1).contains(Flags::WIDE_CHAR);
        let orphan = flags(col).contains(Flags::WIDE_CHAR) && !wide;
        Some(Cell {
            cell: &line[Column(col)],
            wide,
            continuation,
            orphan,
        })
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
                    Some(cell) if cell.has_contents() => text.extend(cell.chars()),
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
pub struct Cell<'a> {
    cell: &'a TermCell,
    /// A wide character whose right half is there.
    wide: bool,
    /// The right half of a wide character that is there.
    continuation: bool,
    /// A wide character whose right half is gone, read as blank.
    orphan: bool,
}

impl Cell<'_> {
    /// The cell's text: its character and the zero-width characters written after it,
    /// or nothing for a cell no character was written to.
    pub fn contents(&self) -> String {
        self.chars().collect()
    }

    /// The characters the cell shows, none for a blank cell.
    pub fn chars(&self) -> impl Iterator<Item = char> + '_ {
        self.has_contents()
            .then(|| {
                std::iter::once(self.cell.c)
                    .chain(self.cell.zerowidth().unwrap_or_default().iter().copied())
            })
            .into_iter()
            .flatten()
    }

    pub fn has_contents(&self) -> bool {
        !self.orphan && (self.cell.c != ' ' || self.cell.zerowidth().is_some())
    }

    pub fn fgcolor(&self) -> Color {
        self.cell.fg.into()
    }

    pub fn bgcolor(&self) -> Color {
        self.cell.bg.into()
    }

    pub fn bold(&self) -> bool {
        self.cell.flags.contains(Flags::BOLD)
    }

    pub fn italic(&self) -> bool {
        self.cell.flags.contains(Flags::ITALIC)
    }

    pub fn inverse(&self) -> bool {
        self.cell.flags.contains(Flags::INVERSE)
    }

    pub fn underline(&self) -> Underline {
        Underline::of(self.cell.flags)
    }

    /// The underline's own colour, when the client set one apart from the text's.
    pub fn underline_color(&self) -> Option<Color> {
        self.cell.underline_color().map(Color::from)
    }

    /// The URI of the OSC 8 link the cell was written under.
    pub fn hyperlink(&self) -> Option<String> {
        self.cell.hyperlink().map(|link| link.uri().to_string())
    }

    /// Whether the cell holds a character two columns wide.
    pub fn is_wide(&self) -> bool {
        self.wide
    }

    /// Whether the cell is the right half of a wide character.
    pub fn is_wide_continuation(&self) -> bool {
        self.continuation
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

    /// The columns tmux 3.5a advances for each string, which the grid follows so the
    /// text tmux writes after it lands where tmux drew it.
    const TMUX_WIDTHS: &[(&str, u16)] = &[
        ("a", 1),
        ("한", 2),
        ("👍", 2),
        ("👍🏽", 2),
        ("🇰🇷", 2),
        ("👨‍👩‍👧‍👦", 4),
        ("❤️", 2),
        ("❤", 1),
        ("✅", 2),
        ("é", 1),
        ("ｱ", 1),
    ];

    #[test]
    fn emoji_clusters_take_the_columns_tmux_gives_them() {
        for &(s, width) in TMUX_WIDTHS {
            let mut whole = Parser::new(2, 20, ());
            whole.process(format!("{s}x").as_bytes());
            assert_eq!(whole.screen().cursor_position(), (0, width + 1), "{s:?}");
            assert_eq!(
                whole.screen().cell(0, width).unwrap().contents(),
                "x",
                "{s:?}"
            );
            let mut split = Parser::new(2, 20, ());
            for b in format!("{s}x").as_bytes() {
                split.process(&[*b]);
            }
            assert_eq!(
                split.screen().cursor_position(),
                (0, width + 1),
                "{s:?} split"
            );
            assert_eq!(
                split.screen().contents(),
                whole.screen().contents(),
                "{s:?}"
            );
        }
    }

    #[test]
    fn a_joined_cluster_is_one_cell() {
        let mut p = Parser::new(2, 20, ());
        p.process("👍🏽❤️👨‍👩‍👧‍👦".as_bytes());
        let s = p.screen();
        assert_eq!(s.cell(0, 0).unwrap().contents(), "👍🏽");
        assert!(s.cell(0, 0).unwrap().is_wide());
        assert_eq!(s.cell(0, 2).unwrap().contents(), "❤️");
        assert!(s.cell(0, 2).unwrap().is_wide());
        assert!(s.cell(0, 3).unwrap().is_wide_continuation());
        assert_eq!(
            s.cell(0, 4).unwrap().contents(),
            "👨\u{200d}👩\u{200d}👧\u{200d}",
            "a cell holds 21 bytes, as tmux keeps it"
        );
        assert_eq!(s.cell(0, 6).unwrap().contents(), "👦");
    }

    #[test]
    fn a_vs16_at_the_last_column_stays_one_column() {
        let mut p = Parser::new(2, 4, ());
        p.process("abc❤️".as_bytes());
        assert!(!p.screen().cell(0, 3).unwrap().is_wide());
        assert_eq!(p.screen().cell(0, 3).unwrap().contents(), "❤️");
    }

    #[test]
    fn clusters_inside_a_control_string_reach_the_hooks_whole() {
        let mut p = Parser::new(2, 20, Log::default());
        p.process("\x1b]2;👍🏽❤️👨‍👩\x07ok".as_bytes());
        assert_eq!(
            p.callbacks().osc,
            vec![vec![b"2".to_vec(), "👍🏽❤️👨‍👩".as_bytes().to_vec()]]
        );
        assert_eq!(p.screen().contents().lines().next(), Some("ok"));
    }

    #[test]
    fn a_half_erased_wide_char_reads_as_blank() {
        for erase in ["\x1b[1;2H\x1b[X", "\x1b[1;2H\x1b[K"] {
            let mut p = Parser::new(1, 6, ());
            p.process(format!("가b{erase}").as_bytes());
            let s = p.screen();
            assert!(!s.cell(0, 0).unwrap().is_wide(), "{erase:?}");
            assert!(!s.cell(0, 0).unwrap().has_contents(), "{erase:?}");
            assert!(!s.cell(0, 1).unwrap().is_wide_continuation(), "{erase:?}");
        }
        let mut p = Parser::new(1, 6, ());
        p.process("가\x1b[1;1H\x1b[X".as_bytes());
        assert!(
            !p.screen().cell(0, 1).unwrap().is_wide_continuation(),
            "a right half whose wide char is gone is a blank cell"
        );
    }

    #[test]
    fn a_wide_glyph_survives_repainting_its_erased_leading_cell() {
        for erase in ["\x1b[X", "\x1b[0X", "\x1b[1X", "\x1b[1K", "\x1b[1J"] {
            let input = format!("가b다\x1b[1G{erase}나");
            for chunk in [1, input.len()] {
                let mut p = Parser::new(1, 6, ());
                for bytes in input.as_bytes().chunks(chunk) {
                    p.process(bytes);
                }
                assert_eq!(p.screen().cell(0, 0).unwrap().contents(), "나", "{erase:?}");
                assert!(p.screen().cell(0, 0).unwrap().is_wide());
                assert!(p.screen().cell(0, 1).unwrap().is_wide_continuation());
                assert_eq!(p.screen().cell(0, 2).unwrap().contents(), "b");
                assert_eq!(p.screen().cell(0, 3).unwrap().contents(), "다");
                assert!(p.screen().cell(0, 4).unwrap().is_wide_continuation());
            }
        }
    }

    /// A 3x6 screen of rows `abcdef`, `ghijkl`, and `mnopqr`, with left and right
    /// margins on columns 2 to 4.
    fn margined() -> Parser<Log> {
        let mut p = Parser::new(3, 6, Log::default());
        p.process(b"abcdef\r\nghijkl\r\nmnopqr\x1b[?69h\x1b[2;4s");
        p
    }

    fn rows(p: &Parser<Log>) -> Vec<String> {
        (0..p.screen().size().0).map(|r| row(p, r)).collect()
    }

    #[test]
    fn scrolls_stay_inside_the_margins() {
        for (input, want) in [
            (&b"\x1b[3;3H\n"[..], ["ahijef", "gnopkl", "m   qr"]),
            (b"\x1b[3;3H\x1bD", ["ahijef", "gnopkl", "m   qr"]),
            (b"\x1b[2S", ["anopef", "g   kl", "m   qr"]),
            (b"\x1b[1;3H\x1bM", ["a   ef", "gbcdkl", "mhijqr"]),
            (b"\x1b[T", ["a   ef", "gbcdkl", "mhijqr"]),
            (b"\x1b[1;3H\n", ["abcdef", "ghijkl", "mnopqr"]),
            (b"\x1b[3;3H\nZ", ["ahijef", "gnopkl", "m Z qr"]),
        ] {
            let mut p = margined();
            p.process(input);
            assert_eq!(rows(&p), want, "{input:?}");
            let mut split = margined();
            for b in input {
                split.process(&[*b]);
            }
            assert_eq!(rows(&split), want, "{input:?} split");
        }
    }

    #[test]
    fn a_next_line_inside_the_margins_returns_to_the_left_margin() {
        let mut p = margined();
        p.process(b"\x1b[3;4H\x1bE");
        assert_eq!(p.screen().cursor_position(), (2, 1));
        assert_eq!(rows(&p), ["ahijef", "gnopkl", "m   qr"]);
    }

    #[test]
    fn margins_set_only_while_declrmm_is_on() {
        let mut p = Parser::new(3, 6, Log::default());
        p.process(b"abcdef\r\nghijkl\r\nmnopqr\x1b[2;4s\x1b[3;3H\n");
        assert_eq!(rows(&p), ["ghijkl", "mnopqr", "      "]);
        p.process(b"\x1b[u");
        assert_eq!(
            p.screen().cursor_position(),
            (2, 5),
            "CSI s saves the cursor without DECLRMM"
        );
        let mut p = margined();
        p.process(b"\x1b[?69l\x1b[3;3H\n");
        assert_eq!(rows(&p), ["ghijkl", "mnopqr", "      "]);
        let mut p = margined();
        p.process(b"\x1bc\x1b[3;1Habcdef\r\n");
        assert_eq!(rows(&p)[1], "abcdef", "a full reset clears the margins");
    }

    #[test]
    fn setting_margins_keeps_the_saved_cursor() {
        let mut p = Parser::new(3, 6, Log::default());
        p.process(b"\x1b[2;2H\x1b7\x1b[?69h\x1b[2;4s");
        assert_eq!(
            p.screen().cursor_position(),
            (0, 0),
            "DECSLRM homes the cursor"
        );
        p.process(b"\x1b8");
        assert_eq!(p.screen().cursor_position(), (1, 1));
    }

    #[test]
    fn a_rectangle_fill_writes_its_character_with_the_current_attributes() {
        let mut p = margined();
        p.process(b"\x1b[44m\x1b[32;1;2;2;3$x\x1b[0m");
        assert_eq!(rows(&p), ["a  def", "g  jkl", "mnopqr"]);
        let cell = p.screen().cell(1, 2).unwrap();
        assert_eq!(cell.bgcolor(), Color::Idx(4));
        assert_eq!(p.screen().cell(1, 3).unwrap().bgcolor(), Color::Default);
    }

    #[test]
    fn combining_marks_join_their_cell() {
        let mut p = Parser::new(2, 10, Log::default());
        p.process("e\u{301}x".as_bytes());
        assert_eq!(p.screen().cell(0, 0).unwrap().contents(), "e\u{301}");
        assert_eq!(p.screen().cell(0, 1).unwrap().contents(), "x");
    }
}
