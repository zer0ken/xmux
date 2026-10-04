//! Rendering, layout, and transient popup geometry for the switcher's modal surfaces.
//! [`State`](crate::state::State) owns the [`Modal`] and input data this module paints.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

pub(crate) use crate::state::{feed_reader, Input, InputMode, Modal};
#[cfg(test)]
use crate::state::{is_popup_open, modal_kind};
use crate::ui::palette;

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
    /// The drawn rect of the active modal popup (help/input/confirm), copied from the
    /// last frame's render plan when a press starts, so the press can hit-test its
    /// border. `Rect::default()` means no modal popup is open.
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

/// One row of the help: a section head, or a key (or glyph) cell with what it does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum HelpRow {
    Head(String),
    Key(String, String),
}

/// The section head the glyph legend stands under.
pub(crate) const GLYPH_SECTION: &str = "glyphs";

/// What each glyph on screen means, in the order a reader meets them: the card states,
/// the selection, the overflow cues, the border, and the toast levels. Every glyph is
/// read from the constant the surface that paints it uses.
fn glyph_legend() -> Vec<(String, String)> {
    use crate::state::notify::Level;
    use crate::ui::chrome::{BLOCK_MARK, LIST_FAILED_MARK, UNREACHABLE_MARK};
    vec![
        (BLOCK_MARK.into(), "a host that needs a login".into()),
        (
            UNREACHABLE_MARK.into(),
            "an unreachable host; on a toast, a warning that stays until dismissed".into(),
        ),
        (
            LIST_FAILED_MARK.into(),
            "a host whose session list could not be read; on a toast, a failure that stays".into(),
        ),
        (
            crate::ui::spinner_glyph(0).to_string(),
            "the spinner: a host still scanning, or a login step still running".into(),
        ),
        (
            crate::ui::switcher::SELECTED_MARK.into(),
            "the selected card".into(),
        ),
        (
            "‹ 5 · 7 ›".into(),
            "cards off screen to each side of a band, on its view border".into(),
        ),
        (
            "┃".into(),
            "cards off screen in a column: the view border is thick beside the cards shown".into(),
        ),
        (
            "║".into(),
            "the view border while auto-hide-nav is on".into(),
        ),
        (
            Level::Success.glyph().into(),
            "a toast reporting success; it leaves after five seconds".into(),
        ),
        (
            Level::Info.glyph().into(),
            "a toast reporting a fact; it leaves after five seconds".into(),
        ),
    ]
}

/// Every help row, built from the one key table plus the glyph legend. `prefix` is the
/// configured `[ui] prefix` binding; `nav_position` decides which arrow pair the focus
/// rows name.
pub(crate) fn help_rows(
    prefix: &str,
    nav_position: crate::ui::switcher::NavPosition,
) -> Vec<HelpRow> {
    use crate::model::keys::{Section, TABLE};
    let mut rows = Vec::new();
    for section in Section::ALL {
        rows.push(HelpRow::Head(section.title().to_string()));
        for entry in TABLE.iter().filter(|e| e.section == section) {
            rows.push(HelpRow::Key(
                entry.full_label(prefix, nav_position),
                entry.help.to_string(),
            ));
        }
    }
    rows.push(HelpRow::Head(GLYPH_SECTION.to_string()));
    rows.extend(
        glyph_legend()
            .into_iter()
            .map(|(glyph, meaning)| HelpRow::Key(glyph, meaning)),
    );
    rows
}

/// The rows a search keeps. A key row is kept when its keys or its description contain
/// the query, ignoring case; a section is kept whole when its head does, and otherwise
/// keeps its head over the rows of it that matched. An empty query keeps every row.
pub(crate) fn matching_help_rows(rows: &[HelpRow], query: &str) -> Vec<HelpRow> {
    let q = query.to_lowercase();
    if q.is_empty() {
        return rows.to_vec();
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i < rows.len() {
        let head = &rows[i];
        let end = rows[i + 1..]
            .iter()
            .position(|r| matches!(r, HelpRow::Head(_)))
            .map_or(rows.len(), |p| i + 1 + p);
        let body = &rows[i + 1..end];
        let head_hit = matches!(head, HelpRow::Head(h) if h.to_lowercase().contains(&q));
        let kept: Vec<HelpRow> = body
            .iter()
            .filter(|r| {
                head_hit
                    || matches!(r, HelpRow::Key(k, d)
                        if k.to_lowercase().contains(&q) || d.to_lowercase().contains(&q))
            })
            .cloned()
            .collect();
        if !kept.is_empty() {
            out.push(head.clone());
            out.extend(kept);
        }
        i = end;
    }
    out
}

/// The help popup's inner size before any search: the widest row, and the rows plus the
/// search line. The popup keeps this size while a search narrows what it shows, so typing
/// never moves its border.
pub(crate) fn help_size(
    prefix: &str,
    nav_position: crate::ui::switcher::NavPosition,
) -> (u16, u16) {
    let rows = help_rows(prefix, nav_position);
    let kw = key_column_width(&rows);
    let w = rows
        .iter()
        .map(|r| match r {
            HelpRow::Head(h) => UnicodeWidthStr::width(h.as_str()) + 1,
            HelpRow::Key(_, d) => kw + 4 + UnicodeWidthStr::width(d.as_str()),
        })
        .max()
        .unwrap_or(0);
    (w as u16, rows.len() as u16 + 1)
}

fn key_column_width(rows: &[HelpRow]) -> usize {
    rows.iter()
        .filter_map(|r| match r {
            HelpRow::Key(k, _) => Some(UnicodeWidthStr::width(k.as_str())),
            HelpRow::Head(_) => None,
        })
        .max()
        .unwrap_or(0)
}

/// The help modal's `(title, lines)` for a popup with `visible` inner rows: the search
/// line, then the window of matching rows that starts `scroll` rows down, held so the
/// last page stays full. tmux mode-tree style: a right-aligned, bold key column, a `│`
/// rule, then the description. The title says which rows are on screen whenever they
/// are not all of them.
pub(crate) fn help_lines(
    prefix: &str,
    nav_position: crate::ui::switcher::NavPosition,
    palette: &palette::Palette,
    query: &str,
    scroll: usize,
    visible: u16,
) -> (String, Vec<Line<'static>>) {
    let all = help_rows(prefix, nav_position);
    let kw = key_column_width(&all);
    let rows = matching_help_rows(&all, query);
    let bold = palette::interaction_key_style();
    let accent = Style::default().fg(palette.accent);
    let dim = Style::default().fg(palette.disabled);
    let rule = Span::styled("│ ", Style::default().fg(palette.decoration));
    let search = if query.is_empty() {
        Line::from(Span::styled(" type to search", dim))
    } else {
        Line::from(vec![
            Span::styled(" search ", dim),
            Span::styled(query.to_string(), bold),
            Span::styled(" ", Style::default().add_modifier(Modifier::REVERSED)),
        ])
    };
    let window = (visible as usize).saturating_sub(1);
    let offset = scroll.min(rows.len().saturating_sub(window));
    let mut lines = vec![search];
    if rows.is_empty() {
        lines.push(Line::from(Span::styled(" no key or glyph matches", dim)));
    }
    lines.extend(rows.iter().skip(offset).take(window).map(|r| match r {
        HelpRow::Head(h) => Line::from(Span::styled(
            format!(" {h}"),
            accent.add_modifier(Modifier::BOLD),
        )),
        HelpRow::Key(k, d) => {
            let pad = kw.saturating_sub(UnicodeWidthStr::width(k.as_str()));
            Line::from(vec![
                Span::styled(format!(" {}{k} ", " ".repeat(pad)), bold),
                rule.clone(),
                Span::raw(d.clone()),
            ])
        }
    }));
    let title = if rows.len() > window && window > 0 {
        let last = (offset + window).min(rows.len());
        format!("keys {}-{last} of {}", offset + 1, rows.len())
    } else {
        "keys".to_string()
    };
    (title, lines)
}

/// The hint-bar input line split into its parts: the feature head (the bracketed
/// name and the guide text, `[filter] filter sessions: `), the buffer before the
/// caret, the char under it (or a trailing space at end of line), and the buffer
/// after it. Optional guide segments give way before the buffer, which is WINDOWED
/// to the remaining `width`. The window always keeps the caret and the char under it
/// on screen, so the edit position never scrolls off as the buffer outgrows the bar.
fn input_segments(input: &Input, width: u16) -> (String, String, String, String, String) {
    let title = format!("[{}]", input_title(input.mode));
    let label = input.label.trim();
    let parts: Vec<&str> = label.split(" · ").collect();
    let mut labels = vec![label.to_string()];
    if parts.len() > 2 {
        labels.push(parts[..2].join(" · "));
    }
    if parts.len() > 1 {
        labels.push(parts[0].to_string());
    }
    labels.push(input_title(input.mode).to_string());
    labels.push(String::new());
    labels.dedup();
    let reserve = 2usize.min(width as usize);
    let guide = labels
        .into_iter()
        .map(|label| {
            if label.is_empty() {
                " ".to_string()
            } else {
                format!(" {label}: ")
            }
        })
        .find(|guide| title.chars().count() + guide.chars().count() + reserve <= width as usize)
        .unwrap_or_else(|| " ".to_string());
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
pub(crate) fn input_hint_line(
    input: &Input,
    width: u16,
    palette: &palette::Palette,
) -> Line<'static> {
    let (title, guide, before, at, after) = input_segments(input, width);
    let accent = Style::default()
        .fg(palette.bar_accent)
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
    palette: &palette::Palette,
) {
    frame.render_widget(Clear, rect);
    // Rounded corners + a muted border + an accent bold title: the popup reads as a
    // floating panel over the content rather than a boxed region of it. The reset
    // base style keeps the interior opaque (see the doc comment above).
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(palette.decoration))
        .title(Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(palette.accent)
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

    fn help() -> Option<Modal> {
        Some(Modal::Help {
            query: String::new(),
            scroll: 0,
        })
    }

    fn flat(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn help_focus_rows_name_the_arrow_pair_the_placement_makes_active() {
        // The focus rows read the pair the current placement makes active: at the
        // default (left) placement →/↓ name the terminal and ←/↑ the nav; pinned right
        // the whole pair flips.
        let palette = palette::Palette::default();
        let rows = |position| help_rows("C-g", position);
        let row = |rows: &[HelpRow], what: &str| {
            rows.iter()
                .find_map(|r| match r {
                    HelpRow::Key(k, d) if d.starts_with(what) => Some(k.clone()),
                    _ => None,
                })
                .unwrap()
        };
        let left = rows(crate::ui::switcher::NavPosition::Left);
        assert_eq!(row(&left, "focus the terminal ("), "C-g →/↓");
        assert_eq!(row(&left, "focus the nav"), "C-g ←/↑");
        let right = rows(crate::ui::switcher::NavPosition::Right);
        assert_eq!(row(&right, "focus the terminal ("), "C-g ←/↑");
        assert_eq!(row(&right, "focus the nav"), "C-g →/↓");
        let (t, _) = help_lines(
            "C-g",
            crate::ui::switcher::NavPosition::Left,
            &palette,
            "",
            0,
            200,
        );
        assert_eq!(t, "keys", "every row fits, so the title names no range");
    }

    #[test]
    fn help_is_built_from_every_key_table_entry_and_the_glyph_legend() {
        let rows = help_rows("C-b", crate::ui::switcher::NavPosition::Left);
        for entry in crate::model::keys::TABLE {
            let label = entry.full_label("C-b", crate::ui::switcher::NavPosition::Left);
            assert!(
                rows.contains(&HelpRow::Key(label.clone(), entry.help.to_string())),
                "the help lists {label:?}"
            );
        }
        assert!(
            rows.contains(&HelpRow::Key(
                "C-b C-b".into(),
                crate::model::keys::TABLE
                    .iter()
                    .find(|e| e.label.is_empty()
                        && !matches!(e.keys, crate::model::keys::Keys::PrefixArrows { .. }))
                    .unwrap()
                    .help
                    .into()
            )),
            "the literal prefix row writes the configured prefix twice"
        );
        let glyphs: Vec<&str> = rows
            .iter()
            .skip_while(|r| **r != HelpRow::Head(GLYPH_SECTION.into()))
            .filter_map(|r| match r {
                HelpRow::Key(k, _) => Some(k.as_str()),
                HelpRow::Head(_) => None,
            })
            .collect();
        for glyph in ["?", "▲", "✗", "⠋", "❯", "‹ 5 · 7 ›", "┃", "║", "✓", "·"]
        {
            assert!(
                glyphs.contains(&glyph),
                "the legend explains {glyph}: {glyphs:?}"
            );
        }
    }

    #[test]
    fn a_help_search_keeps_the_matching_rows_under_their_heads() {
        let rows = help_rows("C-g", crate::ui::switcher::NavPosition::Left);
        let hit = matching_help_rows(&rows, "QUIT");
        assert_eq!(
            hit,
            vec![
                HelpRow::Head("app".into()),
                HelpRow::Key("C-g q".into(), "quit xmux".into())
            ],
            "case is ignored and only the matching row stays, under its head"
        );
        let glyphs = matching_help_rows(&rows, "glyph");
        assert!(
            glyphs.len() > 5 && glyphs[0] == HelpRow::Head("glyphs".into()),
            "a matching head keeps its whole section: {glyphs:?}"
        );
        let toast = matching_help_rows(&rows, "toast");
        assert!(
            toast
                .iter()
                .any(|r| matches!(r, HelpRow::Key(k, _) if k == "✓"))
                && toast
                    .iter()
                    .any(|r| matches!(r, HelpRow::Key(k, _) if k == "click a toast")),
            "a search crosses sections: {toast:?}"
        );
        assert!(matching_help_rows(&rows, "zzzz").is_empty());
        let palette = palette::Palette::default();
        let (_, lines) = help_lines(
            "C-g",
            crate::ui::switcher::NavPosition::Left,
            &palette,
            "zzzz",
            0,
            20,
        );
        let text = flat(&lines);
        assert!(text.contains("search zzzz"), "{text}");
        assert!(text.contains("no key or glyph matches"), "{text}");
    }

    #[test]
    fn the_help_scrolls_and_holds_its_last_page_full() {
        let palette = palette::Palette::default();
        let pos = crate::ui::switcher::NavPosition::Left;
        let total = help_rows("C-g", pos).len();
        let (title, lines) = help_lines("C-g", pos, &palette, "", 0, 11);
        assert_eq!(lines.len(), 11, "the search line and ten rows");
        assert_eq!(title, format!("keys 1-10 of {total}"));
        assert!(flat(&lines).contains("move (nav focus)"));
        let (title, lines) = help_lines("C-g", pos, &palette, "", 5, 11);
        assert_eq!(title, format!("keys 6-15 of {total}"));
        assert!(!flat(&lines).contains("move (nav focus)"), "scrolled past");
        let (title, lines) = help_lines("C-g", pos, &palette, "", usize::MAX, 11);
        assert_eq!(
            title,
            format!("keys {}-{total} of {total}", total - 9),
            "a scroll past the end shows the last full page"
        );
        assert!(
            flat(&lines).contains("five seconds"),
            "the legend's last row"
        );
    }

    #[test]
    fn modal_kind_classifies_every_modal_as_a_popup() {
        use crate::app::focus::ModalKind;
        assert_eq!(modal_kind(&None), None);
        assert_eq!(modal_kind(&help()), Some(ModalKind::Popup));
        assert!(is_popup_open(&help()));
        assert!(!is_popup_open(&None));
    }

    #[test]
    fn help_feed_types_a_search_scrolls_and_closes_on_esc() {
        let mut m: Option<Modal> = None;
        assert!(!feed_reader(&mut m, b"q"), "closed → not consumed");

        m = help();
        assert!(feed_reader(&mut m, b"qu"), "open → consumed");
        assert!(
            matches!(&m, Some(Modal::Help { query, .. }) if query == "qu"),
            "printable keys type the search, q included"
        );
        assert!(feed_reader(&mut m, b"\x7f"));
        assert!(matches!(&m, Some(Modal::Help { query, .. }) if query == "q"));
        assert!(feed_reader(&mut m, b"\x1b[B\x1b[B\x1b[6~"));
        assert!(
            matches!(&m, Some(Modal::Help { scroll: 12, .. })),
            "↓ scrolls one row and PgDn ten"
        );
        assert!(feed_reader(&mut m, b"\x1b[A"));
        assert!(matches!(&m, Some(Modal::Help { scroll: 11, .. })));
        assert!(feed_reader(&mut m, b"x"));
        assert!(
            matches!(&m, Some(Modal::Help { scroll: 0, .. })),
            "a new search starts at the top of what it matches"
        );
        assert!(feed_reader(&mut m, b"\x15"));
        assert!(matches!(&m, Some(Modal::Help { query, .. }) if query.is_empty()));
        assert!(feed_reader(&mut m, b"\x1b"), "lone Esc → consumed");
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
                &palette::Palette::default(),
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
