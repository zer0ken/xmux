//! The switcher's modal surfaces: the single open-modal enum ([`Modal`]) and its
//! variants (the keys help and the inline input), plus the data types they carry. The switcher owns the modal *behavior* and the transient
//! popup geometry; this module owns the modal data model. Side-effect-free.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use crate::ui::palette;
use crate::ui::tree::RowRef;

/// An active border-drag of a modal popup: the grabbed screen cell and the
/// popup offset at grab time, so motion can compute the new offset.
#[derive(Clone, Copy)]
struct PopupDrag {
    grab: (u16, u16),
    origin: (i16, i16),
}

/// The transient geometry of the active modal popup, owned by the switcher: the
/// drag `offset` from the centered position, the `rect` it was last drawn at (for
/// border hit-testing), and the in-flight border `drag`. The drag behavior is
/// self-contained here so the switcher only forwards mouse events.
#[derive(Default)]
pub(crate) struct PopupGeometry {
    /// Drag offset (cells) applied to a modal popup's centered position. Reset
    /// to (0,0) when a popup opens; updated while its border is dragged.
    pub(crate) offset: (i16, i16),
    /// The drawn rect of the active modal popup (help/input/confirm), cached
    /// each render so a mouse press can hit-test its border. `Rect::default()`
    /// ⇒ no modal popup open.
    pub(crate) rect: Rect,
    /// Active border-drag of a modal popup. `None` ⇒ not dragging.
    drag: Option<PopupDrag>,
}

impl PopupGeometry {
    /// True while a modal popup is being border-dragged.
    pub(crate) fn drag_active(&self) -> bool {
        self.drag.is_some()
    }

    /// A left press on the active modal popup's border begins a move-drag. `open` is
    /// whether a modal popup is live: `rect` is only refreshed on render (frame-gated),
    /// so a popup closed by a keystroke can leave a stale rect - the caller gates on
    /// the live modal state so a press can't grab a popup that no longer exists.
    /// Returns true iff it grabbed (so the app consumes the event).
    pub(crate) fn begin_drag(&mut self, col: u16, row: u16, open: bool) -> bool {
        if !open {
            return false;
        }
        let r = self.rect;
        if r.width < 2 || r.height < 2 {
            return false; // no modal popup drawn yet
        }
        let inside = col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height;
        let on_border = inside
            && (col == r.x || col == r.x + r.width - 1 || row == r.y || row == r.y + r.height - 1);
        if !on_border {
            return false;
        }
        self.drag = Some(PopupDrag {
            grab: (col, row),
            origin: self.offset,
        });
        true
    }

    /// Updates `offset` from the pointer while a border-drag is active.
    pub(crate) fn drag(&mut self, col: u16, row: u16) {
        if let Some(d) = self.drag {
            let dx = col as i32 - d.grab.0 as i32;
            let dy = row as i32 - d.grab.1 as i32;
            self.offset = (
                (d.origin.0 as i32 + dx) as i16,
                (d.origin.1 as i32 + dy) as i16,
            );
        }
    }

    /// Ends a border-drag.
    pub(crate) fn end_drag(&mut self) {
        self.drag = None;
    }

    /// Resets a modal popup to its centered position (called when one opens).
    pub(crate) fn reset(&mut self) {
        self.offset = (0, 0);
        self.drag = None;
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum InputMode {
    Filter,
    New,
    /// Jump to a session by its number (the user-facing name: a `card` is the visual
    /// row, the session is what it stands for). Unlike the other modes this one acts
    /// WHILE it is open: every edit moves the selection while the number names a card,
    /// so the number is a live cursor rather than a value submitted at the end. Enter
    /// closes the popup when the number names a card and flashes the valid range while
    /// leaving it open otherwise; Esc restores where the jump started.
    Jump,
}

pub(crate) struct Input {
    pub(crate) mode: InputMode,
    pub(crate) label: String,
    pub(crate) buffer: String,
    /// Caret position as a char index into `buffer` (`0..=buffer char count`). Every
    /// edit and movement keeps it in range; the entry line renders a block caret at
    /// this column, so editing is no longer append-only.
    pub(crate) cursor: usize,
    /// The create source captured when the input opened, so the action lands on the
    /// host the user was on, not wherever streaming results moved the selection by
    /// the time they pressed Enter.
    pub(crate) source: Option<String>,
    /// [`InputMode::Jump`] only: the card the selection was on when the popup opened,
    /// held by IDENTITY (not row index) so a rebuild during the jump cannot restore
    /// onto the wrong card. Esc returns here; Enter leaves the selection where the
    /// live jump already put it.
    pub(crate) restore: Option<RowRef>,
    /// [`InputMode::Filter`] only: the filter the input opened from, restored on Esc.
    /// The filter applies live while the input is open, so cancelling must undo every
    /// edit back to this value.
    pub(crate) restore_filter: Option<String>,
}

impl Input {
    /// Builds an input with the caret at the END of `buffer`, so a prefilled name
    /// (rename / filter) is ready to edit from its tail. The one constructor keeps
    /// the caret-init rule in a single place.
    pub(crate) fn new(
        mode: InputMode,
        label: String,
        buffer: String,
        source: Option<String>,
    ) -> Self {
        let cursor = buffer.chars().count();
        Input {
            mode,
            label,
            buffer,
            cursor,
            source,
            restore: None,
            restore_filter: None,
        }
    }

    /// Inserts `c` at the caret and advances past it. Char-indexed so multi-byte
    /// (CJK) text stays correct.
    pub(crate) fn insert(&mut self, c: char) {
        let mut v: Vec<char> = self.buffer.chars().collect();
        let i = self.cursor.min(v.len());
        v.insert(i, c);
        self.cursor = i + 1;
        self.buffer = v.into_iter().collect();
    }

    /// Deletes the char before the caret (Backspace).
    pub(crate) fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let mut v: Vec<char> = self.buffer.chars().collect();
        let i = self.cursor.min(v.len());
        if i == 0 {
            return;
        }
        v.remove(i - 1);
        self.cursor = i - 1;
        self.buffer = v.into_iter().collect();
    }

    /// Deletes the char at the caret (Delete); a no-op at end of line.
    pub(crate) fn delete(&mut self) {
        let mut v: Vec<char> = self.buffer.chars().collect();
        if self.cursor >= v.len() {
            return;
        }
        v.remove(self.cursor);
        self.buffer = v.into_iter().collect();
    }

    /// Deletes the word (and any run of spaces) before the caret (Ctrl-W).
    pub(crate) fn delete_word_before(&mut self) {
        let v: Vec<char> = self.buffer.chars().collect();
        let end = self.cursor.min(v.len());
        let mut i = end;
        while i > 0 && v[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !v[i - 1].is_whitespace() {
            i -= 1;
        }
        let mut v = v;
        v.drain(i..end);
        self.cursor = i;
        self.buffer = v.into_iter().collect();
    }

    /// Clears the whole line (Ctrl-U).
    pub(crate) fn clear_line(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
    }

    pub(crate) fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub(crate) fn right(&mut self) {
        if self.cursor < self.buffer.chars().count() {
            self.cursor += 1;
        }
    }

    pub(crate) fn home(&mut self) {
        self.cursor = 0;
    }

    pub(crate) fn end(&mut self) {
        self.cursor = self.buffer.chars().count();
    }
}

/// The single open modal, if any - at most one of the keys help and the inline
/// input. Modeling it as one `Option` (not two independent fields) makes the
/// modals' mutual exclusion structural: opening one drops whatever was open, and
/// the compiler guarantees two can never coexist, so the hand-maintained "clear
/// the others" invariant cannot drift. Lives on [`crate::state::State`]; the
/// switcher owns only the behavior and the transient popup geometry (drag offset
/// / drawn rect).
///
/// The input carries several owned strings (label, buffer, a create source, a
/// jump restore reference, a filter restore value), so it is boxed to keep the
/// enum small; callers pattern-match through the box and never see the pointer.
pub(crate) enum Modal {
    Help,
    Input(Box<Input>),
}

/// True while a centered modal popup is open. Every modal is one today, so this
/// is `is_some()`; it stays a named predicate because callers ask the QUESTION
/// ("is a draggable popup on screen?"), not the representation.
pub(crate) fn is_popup_open(modal: &Option<Modal>) -> bool {
    modal.is_some()
}

/// True while an inline input (filter / new session) is open.
pub(crate) fn is_inputting(modal: &Option<Modal>) -> bool {
    matches!(modal, Some(Modal::Input(_)))
}

/// Which kind of modal is open - the focus machine derives its modal dimension from
/// this each loop-top, so focus can never mirror-and-desync from the open popup.
pub(crate) fn modal_kind(modal: &Option<Modal>) -> Option<crate::app::focus::ModalKind> {
    use crate::app::focus::ModalKind;
    modal.as_ref().map(|_| ModalKind::Popup)
}

/// Feeds a raw key read to the help modal, tmux view-mode style. While help is open
/// every key is consumed (returns true - nothing reaches the nav or the terminal
/// view); `q` or a lone Esc closes it, every other key is swallowed. Returns false
/// when help is closed, so the read falls through to normal routing.
pub(crate) fn feed_help(modal: &mut Option<Modal>, bytes: &[u8]) -> bool {
    if !matches!(modal, Some(Modal::Help)) {
        return false;
    }
    // `q`, or a real Esc (a lone ESC, not the ESC `[` that starts an arrow/CSI).
    let esc = bytes.contains(&0x1b) && !bytes.windows(2).any(|w| w == [0x1b, b'[']);
    if bytes.contains(&b'q') || esc {
        *modal = None;
    }
    true
}

/// Greedily word-wraps `text` to lines no wider than `width` display columns
/// (Unicode-aware), breaking on spaces; a word longer than `width` is hard-split so
/// nothing is ever clipped. Always returns at least one line. Used so the input
/// prompt's description wraps across a narrow nav column instead of being truncated.
pub(crate) fn wrap_text(text: &str, width: u16) -> Vec<String> {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
    let width = (width as usize).max(1);
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut cur_w = 0usize;
    for word in text.split(' ') {
        let ww = UnicodeWidthStr::width(word);
        let sep = usize::from(!cur.is_empty());
        if !cur.is_empty() && cur_w + sep + ww > width {
            lines.push(std::mem::take(&mut cur));
            cur_w = 0;
        }
        if ww > width {
            // Longer than a whole line: hard-split across as many lines as needed.
            if !cur.is_empty() {
                lines.push(std::mem::take(&mut cur));
                cur_w = 0;
            }
            for ch in word.chars() {
                let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
                if cur_w + cw > width && !cur.is_empty() {
                    lines.push(std::mem::take(&mut cur));
                    cur_w = 0;
                }
                cur.push(ch);
                cur_w += cw;
            }
        } else {
            if !cur.is_empty() {
                cur.push(' ');
                cur_w += 1;
            }
            cur.push_str(word);
            cur_w += ww;
        }
    }
    lines.push(cur);
    lines
}

/// The help modal's `(title, lines)`, built once and rendered through the
/// shared modal-popup path. `prefix` is the configured `[ui] prefix` binding.
pub(crate) fn help_lines(prefix: &str) -> (String, Vec<Line<'static>>) {
    // tmux mode-tree style: a right-aligned, bold key column, a `│` rule, then
    // the description. `Head` breaks the flat list into navigation/focus/terminal sections;
    // `Note` is a description-only row (the mux state has no keys of its own).
    //
    // The navigation and terminal sections have no configurable keys so they are static.
    // The focus section uses `prefix` so the help modal matches the
    // active binding from config.
    enum HelpRow {
        Head(String),
        Key(String, String),
        Note(&'static str),
        Gap,
    }

    let p = prefix;

    // Tree section - the mutating keys carry the prefix (bare presses are inert);
    // navigation and the `/` filter stay bare.
    let rows: Vec<HelpRow> = vec![
        HelpRow::Head("navigation".into()),
        HelpRow::Key("↑/↓ · j/k".into(), "move one card".into()),
        HelpRow::Key(
            "←/→".into(),
            "previous / next host/mux (host cards as one)".into(),
        ),
        HelpRow::Key("PgUp/PgDn".into(), "jump by 10".into()),
        HelpRow::Key("Home/End".into(), "first / last card".into()),
        HelpRow::Key(
            format!("{p} 1-9"),
            "jump to a session by its number (keep typing for 10+)".into(),
        ),
        HelpRow::Key(format!("{p} n"), "new session on the selected host".into()),
        HelpRow::Key("/".into(), "fuzzy filter <source>/<name>".into()),
        HelpRow::Key(format!("{p} r"), "re-scan every host".into()),
        HelpRow::Gap,
        // Focus section - prefix rows built from `prefix`.
        HelpRow::Head(format!("focus ({p} = prefix)")),
        HelpRow::Key(format!("Enter · {p} →/↓"), "focus the terminal".into()),
        HelpRow::Key(
            format!("{p} Tab"),
            "toggle focus between nav and terminal".into(),
        ),
        HelpRow::Key(format!("{p} ←/↑ · {p} Esc"), "focus the nav".into()),
        HelpRow::Key(
            format!("{p} C-←/→"),
            "resize nav width (side); h/l too. repeats briefly".into(),
        ),
        HelpRow::Key(
            format!("{p} C-↑/↓"),
            "resize nav height (portrait); repeats briefly".into(),
        ),
        HelpRow::Key(
            format!("{p} t"),
            "toggle auto-hide-nav (║ view border = on)".into(),
        ),
        HelpRow::Key(format!("{p} ?"), "show this help (q / Esc closes)".into()),
        HelpRow::Key("click a view".into(), "focus that view".into()),
        HelpRow::Key("drag the view border".into(), "resize the nav".into()),
        HelpRow::Key(format!("{p} q"), "quit".into()),
        HelpRow::Key(format!("{p} {p}"), format!("send a literal {p} to the mux")),
        HelpRow::Gap,
        // Terminal section - no configurable keys; keep as literals.
        HelpRow::Head("terminal (focused)".into()),
        HelpRow::Note("keys, scroll & clicks go to the pane"),
        HelpRow::Note("(the mux needs its own mouse mode on)"),
    ];

    let kw = rows
        .iter()
        .filter_map(|r| match r {
            HelpRow::Key(k, _) => Some(k.chars().count()),
            _ => None,
        })
        .max()
        .unwrap_or(0);
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let accent = Style::default().fg(palette::get().accent);
    let rule = Span::styled("│ ", Style::default().fg(palette::get().decoration));
    let lines: Vec<Line> = rows
        .into_iter()
        .map(|r| match r {
            HelpRow::Gap => Line::from(""),
            HelpRow::Head(h) => Line::from(Span::styled(
                format!(" {h}"),
                accent.add_modifier(Modifier::BOLD),
            )),
            HelpRow::Key(k, d) => Line::from(vec![
                Span::styled(format!(" {k:>kw$} "), bold),
                rule.clone(),
                Span::raw(d),
            ]),
            HelpRow::Note(n) => Line::from(vec![
                Span::raw(format!(" {:>kw$} ", "")),
                rule.clone(),
                Span::raw(n),
            ]),
        })
        .collect();
    ("keys".to_string(), lines)
}

/// The hint-bar input line split into its parts: the feature head (the bracketed
/// name and the guide text, `[filter] filter sessions: `), the buffer before the
/// caret, the char under it (or a trailing space at end of line), and the buffer
/// after it. The buffer is WINDOWED to `width`: only as many cells as the bar can
/// spare after the head show, and the window always keeps the caret (and the char
/// under it) on screen, so the edit position never scrolls off as the buffer
/// outgrows the bar.
fn input_segments(input: &Input, width: u16) -> (String, String, String, String, String) {
    let title = format!("[{}]", input_title(input.mode));
    let guide = format!(" {}: ", input.label.trim());
    let head_w = title.chars().count() + guide.chars().count();
    // Cells the buffer area can use; never 0, so the caret stays on screen however
    // narrow the bar gets. A block caret at END of buffer needs its own cell past the
    // last char, so the window holds one fewer buffer char then.
    let avail = (width as i32 - head_w as i32).max(1) as usize;
    let chars: Vec<char> = input.buffer.chars().collect();
    let len = chars.len();
    let cur = input.cursor.min(len);
    let cell_budget = if cur == len {
        avail.saturating_sub(1)
    } else {
        avail
    };
    let overflow = len > cell_budget;
    // The window start. No overflow: the head. Overflow: slide so the caret rides the
    // window - at end of buffer the window ends at the caret (the tail shows, the caret
    // owns the last cell); mid-buffer it includes the char under the caret.
    let start = if !overflow {
        0
    } else if cur == len {
        cur - cell_budget
    } else {
        (cur + 1).saturating_sub(avail)
    };
    let end = (start + cell_budget).min(len);
    let visible = &chars[start..end];
    let caret_at = if cur < len { cur - start } else { end - start };
    let before: String = visible[..caret_at].iter().collect();
    let (at, after): (String, String) = if caret_at < visible.len() {
        (
            visible[caret_at].to_string(),
            visible[caret_at + 1..].iter().collect(),
        )
    } else {
        (" ".to_string(), String::new())
    };
    (title, guide, before, at, after)
}

/// The active input as the plain hint-bar text (no caret styling): the feature head
/// followed by the windowed buffer. Lets the hint bar size itself and tests read the
/// exact line without a backend.
pub(crate) fn input_hint_text(input: &Input, width: u16) -> String {
    let (title, guide, before, at, after) = input_segments(input, width);
    format!("{title}{guide}{before}{at}{after}")
}

/// The active input rendered as one hint-bar line: the feature name in the bar's
/// key accent, the guide text plain, and the buffer with a reversed-block caret at
/// the edit position. The buffer is windowed (see [`input_segments`]) so the caret
/// stays visible however long it grows.
pub(crate) fn input_hint_line(input: &Input, width: u16) -> Line<'static> {
    let (title, guide, before, at, after) = input_segments(input, width);
    let accent = Style::default()
        .fg(palette::get().bar_accent)
        .add_modifier(Modifier::BOLD);
    let caret = Style::default().add_modifier(Modifier::REVERSED);
    Line::from(vec![
        Span::styled(title, accent),
        Span::raw(guide),
        Span::raw(before),
        Span::styled(at, caret),
        Span::raw(after),
    ])
}

/// Renders an opaque bordered popup at `rect` (titled, content `lines`), in tmux's
/// edge style. Two things make it tmux-consistent:
///
/// 1. **Opaque, no margin.** The box is filled with the reset (default) style so the
///    mux grid's background colours behind it cannot bleed through, and ONLY `rect`
///    itself is cleared - there is no blanket one-cell margin around the box, so
///    half-width neighbours sit flush against the border.
/// 2. **Wide-glyph edge handling.** A double-width (CJK) glyph whose right half the
///    LEFT border now covers would otherwise leave its orphaned left half rendering
///    as a broken glyph just outside the box. That single cell is blanked - and only
///    that cell, only when it is actually a wide glyph. The right edge needs no fixup:
///    ratatui stores a wide char as `[glyph][space]`, so a glyph whose lead the box
///    covers leaves only its already-blank continuation outside.
pub(crate) fn render_popup(
    frame: &mut Frame,
    area: Rect,
    rect: Rect,
    title: &str,
    lines: Vec<Line>,
) {
    frame.render_widget(Clear, rect);
    // Rounded corners + a muted border + an accent bold title: the popup reads as a
    // floating panel over the content rather than a boxed region of it. The reset
    // base style keeps the interior opaque (see the doc comment above).
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(palette::get().decoration))
        .title(Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(palette::get().accent)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::reset());
    frame.render_widget(Paragraph::new(Text::from(lines)).block(block), rect);
    if rect.x > area.x {
        let x = rect.x - 1;
        let y_end = (rect.y + rect.height).min(area.y + area.height);
        let buf = frame.buffer_mut();
        for y in rect.y..y_end {
            if buf[(x, y)].symbol().width() > 1 {
                buf[(x, y)].set_symbol(" ");
            }
        }
    }
}

fn centered_rect(w: u16, h: u16, area: Rect) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect {
        x,
        y,
        width: w,
        height: h,
    }
}

/// `centered_rect` shifted by `offset` (cells) and clamped fully inside `area`.
pub(crate) fn offset_centered(w: u16, h: u16, area: Rect, offset: (i16, i16)) -> Rect {
    let base = centered_rect(w, h, area);
    let max_x = area.x + area.width.saturating_sub(base.width);
    let max_y = area.y + area.height.saturating_sub(base.height);
    let x = (base.x as i32 + offset.0 as i32).clamp(area.x as i32, max_x as i32) as u16;
    let y = (base.y as i32 + offset.1 as i32).clamp(area.y as i32, max_y as i32) as u16;
    Rect {
        x,
        y,
        width: base.width,
        height: base.height,
    }
}

/// A short popup title for an input mode (shown on the box's top border).
fn input_title(mode: InputMode) -> &'static str {
    match mode {
        InputMode::Filter => "filter",
        InputMode::New => "new session",
        InputMode::Jump => "jump",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;
    use ratatui::Terminal;

    fn edit_input(buffer: &str) -> Input {
        Input::new(InputMode::New, "t".into(), buffer.to_string(), None)
    }

    #[test]
    fn input_edits_and_moves_at_the_caret() {
        // new() drops the caret at the end of a prefilled buffer.
        let mut i = edit_input("abc");
        assert_eq!(i.cursor, 3);
        i.left();
        i.left();
        i.insert('X'); // mid-string insert, not append
        assert_eq!((i.buffer.as_str(), i.cursor), ("aXbc", 2));
        i.backspace(); // deletes the char BEFORE the caret
        assert_eq!((i.buffer.as_str(), i.cursor), ("abc", 1));
        i.delete(); // deletes the char AT the caret
        assert_eq!((i.buffer.as_str(), i.cursor), ("ac", 1));
        i.home();
        i.backspace(); // no-op at start
        assert_eq!((i.buffer.as_str(), i.cursor), ("ac", 0));
        i.end();
        i.right(); // no-op at end
        assert_eq!(i.cursor, 2);
    }

    #[test]
    fn input_hint_windows_the_buffer_so_the_caret_stays_visible() {
        // The head `[filter] filter sessions: ` is 26 cells; at width 60 the buffer
        // area is 34 cells. A short buffer fits whole; a long one shows its tail with
        // the caret at the right edge; a mid-buffer caret keeps the char under it in
        // view. The line never exceeds `width`.
        let mk = |buffer: &str, cursor: usize| {
            let mut i = Input::new(
                InputMode::Filter,
                " filter sessions".into(),
                buffer.into(),
                None,
            );
            i.cursor = cursor;
            i
        };
        // A short buffer fits whole, caret as the trailing cell.
        assert_eq!(
            input_hint_text(&mk("ab", 2), 60),
            "[filter] filter sessions: ab "
        );
        // A buffer exactly the window shows its head, caret over the last shown char.
        assert_eq!(
            input_hint_text(&mk("0123456789", 5), 60),
            "[filter] filter sessions: 0123456789"
        );
        // A long buffer at the end: the head stays for context, the buffer's own head
        // scrolls off, its tail shows, and the caret (a trailing cell) rides the right
        // edge; the line is exactly `width`.
        let long = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ"; // 52 chars
        let t = input_hint_text(&mk(long, 52), 60);
        assert_eq!(
            t.chars().count(),
            60,
            "line fills but never exceeds the bar: {t:?}"
        );
        assert!(
            t.starts_with("[filter] filter sessions: "),
            "the feature head stays for context: {t:?}"
        );
        assert!(t.ends_with("XYZ "), "the tail survives: {t:?}");
        // The BUFFER's own head scrolls off, not the guide.
        let window: String = t
            .chars()
            .skip("[filter] filter sessions: ".chars().count())
            .collect();
        assert!(
            window.starts_with("tuvwxyz"),
            "the buffer window starts at its tail: {window:?}"
        );
        // A mid-buffer caret keeps the char it points at visible.
        let alpha: String = ('a'..='z').chain('A'..='Z').collect(); // 52 chars, like `long`
        let t2 = input_hint_text(&mk(&alpha, 45), 60);
        let buf: Vec<char> = alpha.chars().collect();
        assert!(
            t2.ends_with(&format!("{}{}", buf[44], buf[45])),
            "the char under a mid-buffer caret stays in view: {t2:?}"
        );
        assert_eq!(t2.chars().count(), 60, "still fits: {t2:?}");
        // A width narrower than the head still shows the caret (never zero cells).
        let narrow = input_hint_text(&mk("abc", 1), 8);
        assert!(
            !narrow.is_empty() && narrow.ends_with('b'),
            "caret survives: {narrow:?}"
        );
    }

    #[test]
    fn input_ctrl_w_ctrl_u_and_cjk_are_char_indexed() {
        let mut i = edit_input("one two three");
        i.delete_word_before();
        assert_eq!(i.buffer, "one two ");
        i.delete_word_before();
        assert_eq!(i.buffer, "one ");
        i.clear_line();
        assert_eq!((i.buffer.as_str(), i.cursor), ("", 0));
        // Multi-byte text: the caret is a char index, so an edit never splits a syllable.
        let mut k = edit_input("가나");
        assert_eq!(k.cursor, 2);
        k.left();
        k.insert('X');
        assert_eq!((k.buffer.as_str(), k.cursor), ("가X나", 2));
        k.backspace();
        assert_eq!(k.buffer, "가나");
    }

    #[test]
    fn modal_help_variant_constructs() {
        let m = Modal::Help;
        assert!(matches!(m, Modal::Help));
    }

    #[test]
    fn modal_kind_classifies_every_modal_as_a_popup() {
        use crate::app::focus::ModalKind;
        assert_eq!(modal_kind(&None), None);
        assert_eq!(modal_kind(&Some(Modal::Help)), Some(ModalKind::Popup));
        assert!(is_popup_open(&Some(Modal::Help)));
        assert!(!is_popup_open(&None));
    }

    #[test]
    fn help_feed_consumes_and_closes_on_q_or_esc() {
        // tmux view-mode style: while open, every key is consumed; q/Esc closes, the
        // rest are swallowed; while closed, nothing is consumed (falls through).
        let mut m: Option<Modal> = None;
        assert!(!feed_help(&mut m, b"q"), "closed → not consumed");

        m = Some(Modal::Help);
        assert!(feed_help(&mut m, b"j"), "open → consumed");
        assert!(
            matches!(m, Some(Modal::Help)),
            "a non-close key is swallowed but keeps help open"
        );
        assert!(
            feed_help(&mut m, b"\x1b[A"),
            "an arrow (ESC [) is swallowed, not a close"
        );
        assert!(matches!(m, Some(Modal::Help)), "arrow keeps help open");
        assert!(feed_help(&mut m, b"q"), "q → consumed");
        assert!(m.is_none(), "q closes help");

        m = Some(Modal::Help);
        assert!(feed_help(&mut m, b"\x1b"), "lone Esc → consumed");
        assert!(m.is_none(), "Esc closes help");
    }

    #[test]
    fn wrap_text_wraps_on_words_and_hard_splits_long_words() {
        use unicode_width::UnicodeWidthStr;
        let s = "filter sessions · Esc to cancel";
        let lines = wrap_text(s, 19);
        assert!(
            lines.len() >= 2,
            "wraps when narrower than the text: {lines:?}"
        );
        assert!(
            lines.iter().all(|l| l.as_str().width() <= 19),
            "no line exceeds width: {lines:?}"
        );
        assert!(
            lines.join(" ").contains("cancel"),
            "tail survives (not clipped): {lines:?}"
        );
        // A single word longer than the width is hard-split, each piece within width.
        let long = wrap_text("supercalifragilistic", 5);
        assert!(
            long.len() >= 4 && long.iter().all(|l| l.as_str().width() <= 5),
            "{long:?}"
        );
        // A wide enough width keeps it on one line.
        assert_eq!(wrap_text(s, 100).len(), 1);
    }

    #[test]
    fn popup_blanks_only_a_wide_glyph_bisected_by_the_left_border() {
        // tmux edge behaviour: no blanket margin. A double-width glyph whose right half
        // the left border covers is blanked (its orphaned half would render broken); a
        // half-width char at the same edge column stays flush; the box covers opaquely.
        let backend = TestBackend::new(40, 10);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| {
            let area = f.area();
            f.buffer_mut()[(9u16, 3u16)].set_symbol("한"); // wide; right half under the border at x=10
            f.buffer_mut()[(9u16, 4u16)].set_symbol("Y"); // half-width at the same edge column
            f.buffer_mut()[(15u16, 4u16)].set_style(Style::default().bg(Color::Red)); // behind the popup
            let rect = Rect::new(10, 2, 12, 5);
            render_popup(
                f,
                area,
                rect,
                "t",
                vec![Line::from("focus"), Line::from("kill"), Line::from("x")],
            );
        })
        .unwrap();
        let buf = term.backend().buffer();
        assert_eq!(
            buf[(9u16, 3u16)].symbol(),
            " ",
            "wide glyph bisected by the left border is blanked"
        );
        assert_eq!(
            buf[(9u16, 4u16)].symbol(),
            "Y",
            "a half-width char at the edge stays flush - no margin"
        );
        assert_eq!(
            buf[(15u16, 4u16)].bg,
            Color::Reset,
            "the popup covers the background colour opaquely"
        );
    }
}
