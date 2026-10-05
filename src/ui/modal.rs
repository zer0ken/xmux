//! Rendering, layout, and transient popup geometry for the switcher's modal surfaces.
//! [`State`](crate::state::State) owns the [`Modal`] and input data this module paints.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use ratatui::Frame;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub(crate) use crate::state::{feed_reader, Input, InputMode, Modal};
#[cfg(test)]
use crate::state::{is_popup_open, modal_kind};
use crate::ui::palette;

/// An active drag of a modal popup: the grabbed screen cell, the popup offset at grab
/// time, so motion can compute the new offset, and whether the pointer has left the
/// grabbed cell. A press released on the cell it grabbed is a click, not a move.
#[derive(Clone, Copy)]
struct PopupDrag {
    grab: (u16, u16),
    origin: (i16, i16),
    moved: bool,
}

/// The transient geometry of the active modal popup, owned by the switcher: the
/// drag `offset` from its anchored position, the `rect` it was last drawn at (for
/// border hit-testing), and the in-flight border `drag`. The drag behavior is
/// self-contained here so the switcher only forwards mouse events.
#[derive(Default)]
pub(crate) struct PopupGeometry {
    /// Drag offset (cells) applied to the anchored position of the key list and of a
    /// modal popup. Kept while a prefix interaction goes from the key list to the popup
    /// its key opens, reset once neither is on screen; updated while one is dragged.
    pub(crate) offset: (i16, i16),
    /// The drawn rect of the key list or the active modal popup, copied from the last
    /// frame's render plan when a press starts, so the press can hit-test it.
    /// `Rect::default()` means neither is on screen.
    pub(crate) rect: Rect,
    /// Active drag of the key list or a modal popup. `None` ⇒ not dragging.
    drag: Option<PopupDrag>,
}

impl PopupGeometry {
    /// True while the key list or a modal popup is being dragged.
    pub(crate) fn drag_active(&self) -> bool {
        self.drag.is_some()
    }

    /// A left press anywhere on the key list or the active modal popup begins a
    /// move-drag, so the whole box is its handle. A press released without moving is a
    /// click instead (see [`Self::end_drag`]). `open` is whether one is live: `rect` is only refreshed on render (frame-gated),
    /// so a box closed by a keystroke can leave a stale rect - the caller gates on the
    /// live state so a press can't grab a box that no longer exists. Returns true iff
    /// it grabbed (so the app consumes the event).
    pub(crate) fn begin_drag(&mut self, col: u16, row: u16, open: bool) -> bool {
        if !open {
            return false;
        }
        let r = self.rect;
        if r.width < 2 || r.height < 2 {
            return false; // no modal popup drawn yet
        }
        let inside = col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height;
        if !inside {
            return false;
        }
        self.drag = Some(PopupDrag {
            grab: (col, row),
            origin: self.offset,
            moved: false,
        });
        true
    }

    /// Updates `offset` from the pointer while a drag is active.
    pub(crate) fn drag(&mut self, col: u16, row: u16) {
        if let Some(d) = &mut self.drag {
            d.moved |= (col, row) != d.grab;
            let dx = col as i32 - d.grab.0 as i32;
            let dy = row as i32 - d.grab.1 as i32;
            self.offset = (
                (d.origin.0 as i32 + dx) as i16,
                (d.origin.1 as i32 + dy) as i16,
            );
        }
    }

    /// Ends a drag. Returns the grabbed cell when the pointer never left it: that press
    /// and release are a click on the cell.
    pub(crate) fn end_drag(&mut self) -> Option<(u16, u16)> {
        self.drag.take().filter(|d| !d.moved).map(|d| d.grab)
    }

    /// Returns the key list and the popups to their anchored position.
    pub(crate) fn reset(&mut self) {
        self.offset = (0, 0);
        self.drag = None;
    }
}

/// Greedily word-wraps `text` to lines no wider than `width` display columns
/// (Unicode-aware), breaking on spaces; a word longer than `width` is hard-split so
/// nothing is ever clipped. Always returns at least one line. Every popup row wraps
/// through it, so a popup narrower than its text shows the text on more rows instead of
/// cutting it.
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

/// The rows above the help's body: the search field and the tab row.
const HELP_LEAD: usize = 2;

/// The tab row's offset from the help popup's inner top, under the search field.
pub(crate) const HELP_TAB_ROW: u16 = 1;

/// The help popup's natural inner width: its widest row unwrapped. The popup keeps this
/// width while a search narrows what it shows, so typing never moves its border.
pub(crate) fn help_width(prefix: &str, nav_position: crate::ui::switcher::NavPosition) -> u16 {
    let rows = help_rows(prefix, nav_position);
    let kw = key_column_width(&rows);
    rows.iter()
        .map(|r| match r {
            HelpRow::Head(h) => UnicodeWidthStr::width(h.as_str()) + 1,
            HelpRow::Key(k, d) => (1 + kw + 2 + UnicodeWidthStr::width(d.as_str()))
                .max(1 + UnicodeWidthStr::width(k.as_str())),
        })
        .max()
        .unwrap_or(0) as u16
}

/// The help popup's inner height at `inner` cells wide before any search: every row as it
/// wraps there, plus the search field and the tab row. Like the width, it holds while a
/// search narrows the rows.
pub(crate) fn help_height(
    prefix: &str,
    nav_position: crate::ui::switcher::NavPosition,
    inner: u16,
) -> u16 {
    let lines = help_layout(
        prefix,
        nav_position,
        "",
        inner,
        &palette::Palette::default(),
    )
    .lines
    .len();
    u16::try_from(lines + HELP_LEAD).unwrap_or(u16::MAX)
}

/// The furthest the help scrolls: the offset that shows the last page of `lines` display
/// rows in a popup with `visible` inner rows, two of which are the search field and the
/// tab row. The paint and the scroll keys both hold to it.
pub(crate) fn help_max_scroll(lines: usize, visible: u16) -> usize {
    lines.saturating_sub((visible as usize).saturating_sub(HELP_LEAD))
}

/// The widest a help key cell is. A key wider than the column takes a row of its own,
/// with its description on the next row under the description column.
const HELP_KEY_CAP: usize = 14;

/// The fewest description cells beside the key column. A help too narrow for the key
/// column and this many cells sets every key on a row of its own, its description under
/// it.
const HELP_DESC_MIN: usize = 10;

/// The help's key column: the widest key among the rows of the key sections, capped at
/// [`HELP_KEY_CAP`]. The mouse and glyph rows name gestures and glyphs rather than keys,
/// so they do not widen it.
fn key_column_width(rows: &[HelpRow]) -> usize {
    let mut section = "";
    let mut widest = 0;
    for r in rows {
        match r {
            HelpRow::Head(h) => section = h.as_str(),
            HelpRow::Key(k, _) if !matches!(section, "mouse" | GLYPH_SECTION) => {
                widest = widest.max(UnicodeWidthStr::width(k.as_str()));
            }
            HelpRow::Key(..) => {}
        }
    }
    widest.min(HELP_KEY_CAP)
}

/// The help body laid out at one width: its display rows, and each section's title with
/// the display row that title starts on.
struct HelpBody {
    lines: Vec<Line<'static>>,
    sections: Vec<(String, usize)>,
}

impl HelpBody {
    fn map(&self, visible: u16) -> crate::state::HelpMap {
        crate::state::HelpMap {
            heads: self.sections.iter().map(|(_, at)| *at).collect(),
            max_scroll: help_max_scroll(self.lines.len(), visible),
        }
    }

    fn titles(&self) -> Vec<String> {
        self.sections.iter().map(|(t, _)| t.clone()).collect()
    }
}

/// Lays `rows` out `inner` cells wide. Every row wraps rather than being cut: a section
/// title under itself, a key row's description under the description column, and a key
/// wider than the key column on rows of its own above its description. One blank row
/// parts two sections.
fn help_body(rows: &[HelpRow], kw: usize, inner: u16, palette: &palette::Palette) -> HelpBody {
    let key = crate::ui::keylist::key_cell_style(palette);
    let muted = Style::default().fg(palette.decoration);
    let head = crate::ui::keylist::title_style(palette).add_modifier(Modifier::BOLD);
    let inner = inner as usize;
    let kw = if inner > 1 + kw + 2 + HELP_DESC_MIN {
        kw
    } else {
        0
    };
    let lead = 1 + kw + 2;
    let words = inner.saturating_sub(lead + 1).max(1) as u16;
    let whole = inner.saturating_sub(2).max(1) as u16;
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut sections = Vec::new();
    for r in rows {
        match r {
            HelpRow::Head(h) => {
                if !sections.is_empty() {
                    lines.push(Line::default());
                }
                sections.push((h.clone(), lines.len()));
                lines.extend(
                    wrap_text(h, whole)
                        .into_iter()
                        .map(|c| Line::from(Span::styled(format!(" {c}"), head))),
                );
            }
            HelpRow::Key(k, d) => {
                let mut desc = wrap_text(d, words).into_iter();
                let k_w = UnicodeWidthStr::width(k.as_str());
                if k_w > kw {
                    lines.extend(
                        wrap_text(k, whole)
                            .into_iter()
                            .map(|c| Line::from(vec![Span::raw(" "), Span::styled(c, key)])),
                    );
                } else {
                    lines.push(Line::from(vec![
                        Span::raw(" "),
                        Span::styled(k.clone(), key),
                        Span::raw(" ".repeat(kw - k_w + 2)),
                        Span::styled(desc.next().unwrap_or_default(), muted),
                    ]));
                }
                lines.extend(desc.map(|c| {
                    Line::from(vec![Span::raw(" ".repeat(lead)), Span::styled(c, muted)])
                }));
            }
        }
    }
    HelpBody { lines, sections }
}

/// The help body for `query` at `inner` cells wide.
fn help_layout(
    prefix: &str,
    nav_position: crate::ui::switcher::NavPosition,
    query: &str,
    inner: u16,
    palette: &palette::Palette,
) -> HelpBody {
    let all = help_rows(prefix, nav_position);
    let kw = key_column_width(&all);
    help_body(&matching_help_rows(&all, query), kw, inner, palette)
}

/// Where the help's sections lie for `query` in a popup `inner` cells wide with `visible`
/// inner rows: the map the help's keys and its tab clicks are held to.
pub(crate) fn help_map(
    prefix: &str,
    nav_position: crate::ui::switcher::NavPosition,
    query: &str,
    inner: u16,
    visible: u16,
) -> crate::state::HelpMap {
    help_layout(
        prefix,
        nav_position,
        query,
        inner,
        &palette::Palette::default(),
    )
    .map(visible)
}

/// One tab on the help's tab row: the section it names, and the cells it covers counted
/// from the popup's inner left edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HelpTab {
    pub(crate) section: usize,
    pub(crate) x: u16,
    pub(crate) width: u16,
}

/// The tab row `inner` cells wide: the tabs it shows, and whether tabs are hidden before
/// and after them. The row is one line, so a short popup keeps its body rows. It shows
/// the tabs from the first one while the active tab fits, and otherwise starts late enough
/// to show the active tab; `‹` and `›` mark the hidden ones. An active title wider than
/// the whole row is shown cut, and the body's own title row carries it whole.
fn help_tabs(titles: &[String], active: usize, inner: u16) -> (Vec<HelpTab>, bool, bool) {
    let n = titles.len();
    if n == 0 {
        return (Vec::new(), false, false);
    }
    let active = active.min(n - 1);
    let room = (inner as usize).saturating_sub(1);
    for first in 0..=active {
        let mut x = 1 + if first > 0 { 2 } else { 0 };
        let mut tabs = Vec::new();
        for (i, title) in titles.iter().enumerate().skip(first) {
            let w = UnicodeWidthStr::width(title.as_str());
            let gap = if tabs.is_empty() { 0 } else { 2 };
            let mark = if i + 1 < n { 2 } else { 0 };
            if x + gap + w + mark > room {
                break;
            }
            x += gap;
            tabs.push(HelpTab {
                section: i,
                x: x as u16,
                width: w as u16,
            });
            x += w;
        }
        if tabs.iter().any(|t| t.section == active) {
            let after = tabs.last().is_some_and(|t| t.section + 1 < n);
            return (tabs, first > 0, after);
        }
    }
    let x = 1 + if active > 0 { 2 } else { 0 };
    let mark = if active + 1 < n { 2 } else { 0 };
    let width = room.saturating_sub(x + mark).max(1);
    (
        vec![HelpTab {
            section: active,
            x: x as u16,
            width: width as u16,
        }],
        active > 0,
        active + 1 < n,
    )
}

/// The tab row's line: the active tab in the popup title's accent bold, the others muted,
/// and the `hover` tab underlined as the soft selection.
fn help_tab_line(
    titles: &[String],
    active: usize,
    hover: Option<usize>,
    inner: u16,
    palette: &palette::Palette,
) -> Line<'static> {
    let muted = Style::default().fg(palette.decoration);
    let lit = Style::default()
        .fg(palette.accent)
        .add_modifier(Modifier::BOLD);
    let (tabs, before, after) = help_tabs(titles, active, inner);
    let mut spans = vec![Span::raw(" ")];
    let mut x = 1;
    if before {
        spans.push(Span::styled("‹ ", muted));
        x += 2;
    }
    for t in &tabs {
        spans.push(Span::raw(" ".repeat((t.x as usize).saturating_sub(x))));
        let style = if t.section == active { lit } else { muted };
        spans.push(Span::styled(
            middle_cut(&titles[t.section], t.width as usize),
            if hover == Some(t.section) {
                style.patch(palette::soft_selection_style())
            } else {
                style
            },
        ));
        x = (t.x + t.width) as usize;
    }
    if after {
        spans.push(Span::styled(" ›", muted));
    }
    Line::from(spans)
}

/// The section whose tab covers cell `x` of the help's tab row, counted from the popup's
/// inner left edge, as the row is painted for `query`, `scroll`, and `tab` in a popup
/// `inner` cells wide with `visible` inner rows. A cell between tabs names none.
#[allow(clippy::too_many_arguments)]
pub(crate) fn help_tab_at(
    prefix: &str,
    nav_position: crate::ui::switcher::NavPosition,
    query: &str,
    scroll: usize,
    tab: Option<usize>,
    inner: u16,
    visible: u16,
    x: u16,
) -> Option<usize> {
    let body = help_layout(
        prefix,
        nav_position,
        query,
        inner,
        &palette::Palette::default(),
    );
    let active = body.map(visible).active(tab, scroll);
    let (tabs, _, _) = help_tabs(&body.titles(), active, inner);
    tabs.iter()
        .find(|t| x >= t.x && x < t.x + t.width)
        .map(|t| t.section)
}

/// The help modal's `(meta, lines)` for a popup `inner` cells wide with `visible` inner
/// rows: the search field, the tab row, then the window of matching display rows that
/// starts `scroll` rows down, held so the last page stays full. The rows read like the key
/// list: a left-aligned bold key cell, whitespace, then the muted description. The active
/// tab is `tab` when a tab was chosen, and otherwise the section the scroll reached. A
/// `hover` tab, the soft selection, is underlined and the body shows its section instead,
/// while the active tab stays lit. The meta says which rows are on screen whenever they
/// are not all of them.
#[allow(clippy::too_many_arguments)]
pub(crate) fn help_lines(
    prefix: &str,
    nav_position: crate::ui::switcher::NavPosition,
    palette: &palette::Palette,
    query: &str,
    scroll: usize,
    tab: Option<usize>,
    hover: Option<usize>,
    visible: u16,
    inner: u16,
) -> (String, Vec<Line<'static>>) {
    let muted = Style::default().fg(palette.decoration);
    let body = help_layout(prefix, nav_position, query, inner, palette);
    // The search field is never scrolled or filtered away.
    let mut search = vec![Span::styled(" / ", muted)];
    search.extend(text_field_in(
        query,
        query.chars().count(),
        true,
        "type to search",
        field_room(inner, 3),
        palette,
    ));
    let map = body.map(visible);
    let total = body.lines.len();
    let window = (visible as usize).saturating_sub(HELP_LEAD);
    let hover = hover.filter(|&h| h < map.heads.len());
    let offset = hover
        .map(|h| map.scroll_to(h))
        .unwrap_or(scroll)
        .min(map.max_scroll);
    let mut lines = vec![
        Line::from(search),
        help_tab_line(
            &body.titles(),
            map.active(tab, scroll),
            hover,
            inner,
            palette,
        ),
    ];
    if total == 0 {
        let room = (inner as usize).saturating_sub(2).max(1) as u16;
        lines.extend(
            wrap_text("no key or glyph matches", room)
                .into_iter()
                .map(|c| Line::from(Span::styled(format!(" {c}"), muted))),
        );
    }
    lines.extend(body.lines.into_iter().skip(offset).take(window));
    let meta = if total > window && window > 0 {
        let last = (offset + window).min(total);
        format!("{}-{last} of {total}", offset + 1)
    } else {
        String::new()
    };
    (meta, lines)
}

/// The help popup's title on its top border.
pub(crate) const HELP_TITLE: &str = "help";

/// One key a surface offers in its hint grammar: the key token and the words after it.
/// The words may be empty, for a key whose meaning the surface around it already says.
pub(crate) type Hint = (&'static str, &'static str);

fn hint_width(h: &Hint) -> usize {
    UnicodeWidthStr::width(h.0)
        + if h.1.is_empty() {
            0
        } else {
            1 + UnicodeWidthStr::width(h.1)
        }
}

/// The cells `hints` take on one line, ` · ` between them.
pub(crate) fn hints_width(hints: &[Hint]) -> usize {
    hints.iter().map(hint_width).sum::<usize>() + 3 * hints.len().saturating_sub(1)
}

/// The hints that fit `width` cells. A hint line never wraps: the hints before the last
/// one are given up from the end, and the last one (the way out) is always kept.
pub(crate) fn fit_hints(hints: &[Hint], width: usize) -> Vec<Hint> {
    let mut kept = hints.to_vec();
    while kept.len() > 1 && hints_width(&kept) > width {
        kept.remove(kept.len() - 2);
    }
    kept
}

/// `hints` as styled spans: the key token bold, its words and the separators muted.
pub(crate) fn hint_spans(hints: &[Hint], palette: &palette::Palette) -> Vec<Span<'static>> {
    let key = palette::interaction_key_style();
    let muted = Style::default().fg(palette.decoration);
    let mut spans = Vec::new();
    for (i, (k, words)) in hints.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", muted));
        }
        spans.push(Span::styled(k.to_string(), key));
        if !words.is_empty() {
            spans.push(Span::styled(format!(" {words}"), muted));
        }
    }
    spans
}

/// The cells of a text value around the caret, windowed so the caret stays in view when
/// the value is wider than `avail` cells: `(before, at, after)`, where `at` is the cell
/// the caret covers (a space past the end).
fn caret_window(value: &str, cursor: usize, avail: usize) -> (String, String, String) {
    let avail = avail.max(1);
    let chars: Vec<char> = value.chars().collect();
    let len = chars.len();
    let cur = cursor.min(len);
    let glyph_width = |c: char| UnicodeWidthChar::width(c).unwrap_or(0);
    let mut start = cur;
    let mut end = if cur == len { len } else { cur + 1 };
    let mut used = if cur == len {
        1
    } else {
        glyph_width(chars[cur])
    };
    while start > 0 && used + glyph_width(chars[start - 1]) <= avail {
        start -= 1;
        used += glyph_width(chars[start]);
    }
    while end < len && used + glyph_width(chars[end]) <= avail {
        used += glyph_width(chars[end]);
        end += 1;
    }
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
    (before, at, after)
}

/// The cells a search field has in a popup `inner` cells wide after a `lead`-cell prefix
/// and one cell of right padding.
fn field_room(inner: u16, lead: usize) -> usize {
    (inner as usize).saturating_sub(lead + 1).max(1)
}

/// A text field's value cells: the value in the default foreground with one reversed
/// caret cell at the edit position, windowed to `avail` cells. An empty value shows the
/// dim `placeholder` under the caret.
fn text_field_in(
    value: &str,
    cursor: usize,
    caret: bool,
    placeholder: &str,
    avail: usize,
    palette: &palette::Palette,
) -> Vec<Span<'static>> {
    let reversed = Style::default().add_modifier(Modifier::REVERSED);
    let dim = Style::default().fg(palette.disabled);
    if value.is_empty() {
        let mut chars = placeholder.chars();
        return match (caret, chars.next()) {
            (true, Some(first)) => vec![
                Span::styled(first.to_string(), dim.add_modifier(Modifier::REVERSED)),
                Span::styled(chars.collect::<String>(), dim),
            ],
            (true, None) => vec![Span::styled(" ", reversed)],
            (false, _) => vec![Span::styled(placeholder.to_string(), dim)],
        };
    }
    if !caret {
        return vec![Span::raw(value.to_string())];
    }
    let (before, at, after) = caret_window(value, cursor, avail);
    vec![
        Span::raw(before),
        Span::styled(at, reversed),
        Span::raw(after),
    ]
}

/// The cell offset of a text field's caret in `line`: the last reversed span, which is
/// the caret cell every field paints. The terminal's own cursor is placed there, because
/// an input method draws what it is composing at that cursor.
pub(crate) fn caret_offset(line: &Line) -> Option<u16> {
    let mut x = 0usize;
    let mut found = None;
    for span in &line.spans {
        let w = span.width();
        if w > 0 && span.style.add_modifier.contains(Modifier::REVERSED) {
            found = Some(x);
        }
        x += w;
    }
    found.and_then(|x| u16::try_from(x).ok())
}

/// An input's value as a text field `avail` cells wide.
pub(crate) fn input_field(
    input: &Input,
    avail: usize,
    placeholder: &str,
    palette: &palette::Palette,
) -> Vec<Span<'static>> {
    text_field_in(
        &input.buffer,
        input.cursor,
        true,
        placeholder,
        avail,
        palette,
    )
}

/// `text` cut in the middle with `…` to at most `width` cells.
pub(crate) fn middle_cut(text: &str, width: usize) -> String {
    if UnicodeWidthStr::width(text) <= width {
        return text.to_string();
    }
    if width <= 1 {
        return "…".repeat(width);
    }
    let chars: Vec<char> = text.chars().collect();
    let front_budget = (width - 1).div_ceil(2);
    let back_budget = width - 1 - front_budget;
    let take = |iter: &mut dyn Iterator<Item = &char>, budget: usize| {
        let mut out = Vec::new();
        let mut used = 0;
        for &ch in iter {
            let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
            if used + cw > budget {
                break;
            }
            out.push(ch);
            used += cw;
        }
        out
    };
    let front: String = take(&mut chars.iter(), front_budget).into_iter().collect();
    let back: String = take(&mut chars.iter().rev(), back_budget)
        .into_iter()
        .rev()
        .collect();
    format!("{front}…{back}")
}

/// The filter popup at `width` outer cells: `/` and the query, with the cards kept of the
/// cards listed as the meta.
pub(crate) fn filter_popup(
    input: &Input,
    matches: usize,
    total: usize,
    width: u16,
    palette: &palette::Palette,
) -> (PopupFrame, Vec<Line<'static>>) {
    let room = (width as usize).saturating_sub(2 + 3 + 1).max(1);
    let mut row = vec![Span::styled(" / ", Style::default().fg(palette.decoration))];
    row.extend(input_field(input, room, "", palette));
    (
        PopupFrame {
            title: "filter".into(),
            meta: format!("{matches} of {total}"),
            hints: FILTER_HINTS.to_vec(),
        },
        vec![Line::from(row)],
    )
}

/// The jump popup at `width` outer cells: `card 4▌` and, muted, the card the number names
/// now, with the numbers there are as the meta. `refused` is a number Enter found no card
/// for: the row then states that in the error colour.
pub(crate) fn jump_popup(
    input: &Input,
    target: Option<&str>,
    refused: bool,
    last: usize,
    width: u16,
    palette: &palette::Palette,
) -> (PopupFrame, Vec<Line<'static>>) {
    let muted = Style::default().fg(palette.decoration);
    let inner = (width as usize).saturating_sub(2);
    let rows = if refused {
        let error = Style::default().fg(palette.error);
        let text = format!("✗ no card {}", input.buffer.trim());
        wrap_text(&text, inner.saturating_sub(2).max(1) as u16)
            .into_iter()
            .map(|c| Line::from(Span::styled(format!(" {c}"), error)))
            .collect()
    } else {
        let mut spans = vec![Span::styled(" card ", muted)];
        spans.extend(input_field(input, 6, "", palette));
        let used: usize = spans.iter().map(|s| s.width()).sum();
        let mut rows = Vec::new();
        // The card's name wraps under its own column, so the field keeps its line.
        let mut name = target
            .map(|name| wrap_text(name, inner.saturating_sub(used + 2 + 1).max(1) as u16))
            .unwrap_or_default()
            .into_iter();
        if let Some(first) = name.next() {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(first, muted));
        }
        rows.push(Line::from(spans));
        rows.extend(name.map(|c| {
            Line::from(vec![
                Span::raw(" ".repeat(used + 2)),
                Span::styled(c, muted),
            ])
        }));
        rows
    };
    (
        PopupFrame {
            title: "jump".into(),
            meta: format!("1-{last}"),
            hints: JUMP_HINTS.to_vec(),
        },
        rows,
    )
}

const JUMP_HINTS: &[Hint] = &[("Enter", "select"), ("Esc", "cancel")];

/// The filter's keys on its bottom border.
pub(crate) const FILTER_HINTS: &[Hint] = &[("Enter", "apply"), ("Esc", "cancel")];

/// A popover's rows: the label column muted, two cells of whitespace, then the value. The
/// widest label sets the column. A row with an empty label and a value continues the value
/// above it under the value column; a row with neither is blank.
fn label_rows(
    rows: Vec<(&'static str, Vec<Span<'static>>)>,
    palette: &palette::Palette,
) -> Vec<Line<'static>> {
    let muted = Style::default().fg(palette.decoration);
    let lw = rows
        .iter()
        .map(|(l, _)| UnicodeWidthStr::width(*l))
        .max()
        .unwrap_or(0);
    rows.into_iter()
        .map(|(label, value)| {
            if label.is_empty() && value.is_empty() {
                return Line::from("");
            }
            let mut spans = vec![Span::styled(format!(" {label:<lw$}  "), muted)];
            spans.extend(value);
            Line::from(spans)
        })
        .collect()
}

/// The cells a row's label column takes, padding and gap included.
fn label_lead(labels: &[&str]) -> usize {
    1 + labels
        .iter()
        .map(|l| UnicodeWidthStr::width(*l))
        .max()
        .unwrap_or(0)
        + 2
}

/// `label` over `value` wrapped to `room` cells: the first row carries the label, and the
/// rows after it continue under the value column.
fn wrapped_row(
    label: &'static str,
    value: &str,
    room: usize,
) -> Vec<(&'static str, Vec<Span<'static>>)> {
    wrap_text(value, room as u16)
        .into_iter()
        .enumerate()
        .map(|(i, c)| (if i == 0 { label } else { "" }, vec![Span::raw(c)]))
        .collect()
}

/// The narrowest a prompt popover is drawn.
pub(crate) const POPOVER_MIN_WIDTH: u16 = 40;

/// The new-session popover at `width` outer cells: its frame and rows. `host` is the
/// `{host}/{mux}` the session lands on.
pub(crate) fn new_session_popover(
    host: &str,
    input: &Input,
    width: u16,
    palette: &palette::Palette,
) -> (PopupFrame, Vec<Line<'static>>) {
    let lead = label_lead(&["host", "name"]);
    let room = (width as usize).saturating_sub(2 + lead + 1).max(1);
    let mut rows = wrapped_row("host", host, room);
    rows.push(("name", input_field(input, room, "auto", palette)));
    let lines = label_rows(rows, palette);
    (
        PopupFrame {
            title: "new session".into(),
            meta: String::new(),
            hints: NEW_SESSION_HINTS.to_vec(),
        },
        lines,
    )
}

const NEW_SESSION_HINTS: &[Hint] = &[("Enter", "create"), ("Esc", "cancel")];
const LOGOUT_HINTS: &[Hint] = &[("Enter", "log out"), ("Esc", "cancel")];
const LOGOUT_KEYS_HINTS: &[Hint] = &[("Enter", "remove"), ("Esc", "keep it")];

/// The new-session popover's natural outer size for `host`: wide enough for the host on
/// one row.
pub(crate) fn new_session_size(host: &str) -> (u16, u16) {
    let lead = label_lead(&["host", "name"]);
    let w = (lead + UnicodeWidthStr::width(host) + 1 + 2) as u16;
    let hints = (hints_width(NEW_SESSION_HINTS) + 6) as u16;
    (w.max(hints).max(POPOVER_MIN_WIDTH), 4)
}

/// What tells the two logout confirms apart: the title, the word typed in the field, and
/// the keys on the bottom border. [`InputMode::Logout`] asks to log out at all;
/// [`InputMode::LogoutKeys`] asks whether a key line xmux did not add goes too.
fn logout_grammar(input: &Input) -> (&'static str, &'static str, &'static str, &'static [Hint]) {
    match input.mode {
        InputMode::LogoutKeys => ("remove key", "type remove", "remove", LOGOUT_KEYS_HINTS),
        _ => ("log out", "type logout", "logout", LOGOUT_HINTS),
    }
}

/// The machine whose connections a logout closes: the popover's meta.
fn logout_machine(input: &Input) -> String {
    input
        .source
        .as_deref()
        .map(|s| crate::session::machine_of(s).to_string())
        .unwrap_or_default()
}

/// The cells a logout confirm's label column takes.
fn logout_lead(input: &Input) -> usize {
    let mut labels: Vec<&str> = input.facts.iter().map(|(l, _)| *l).collect();
    labels.push(logout_grammar(input).1);
    label_lead(&labels)
}

/// The value column's cells in a logout confirm `width` outer cells wide.
fn logout_room(input: &Input, width: u16) -> usize {
    (width as usize)
        .saturating_sub(2 + logout_lead(input) + 1)
        .max(1)
}

/// A logout confirm popover's outer size, at most `max_w` wide, its facts wrapped there.
pub(crate) fn logout_size(input: &Input, max_w: u16) -> (u16, u16) {
    let widest = input
        .facts
        .iter()
        .map(|(_, v)| UnicodeWidthStr::width(v.as_str()))
        .max()
        .unwrap_or(0);
    let w = ((logout_lead(input) + widest + 1 + 2) as u16)
        .max((hints_width(logout_grammar(input).3) + 6) as u16)
        .max((logout_machine(input).len() + 16) as u16)
        .max(POPOVER_MIN_WIDTH)
        .min(max_w);
    let room = logout_room(input, w);
    let facts: usize = input
        .facts
        .iter()
        .map(|(_, v)| wrap_text(v, room as u16).len())
        .sum();
    (w, facts as u16 + 2 + 2)
}

/// A logout confirm popover at `width` outer cells: the facts as rows, a blank row, and
/// the field the confirming word is typed in.
pub(crate) fn logout_popover(
    input: &Input,
    width: u16,
    palette: &palette::Palette,
) -> (PopupFrame, Vec<Line<'static>>) {
    let (title, field, word, hints) = logout_grammar(input);
    let room = logout_room(input, width);
    let mut rows: Vec<(&'static str, Vec<Span<'static>>)> = input
        .facts
        .iter()
        .flat_map(|(l, v)| wrapped_row(l, v, room))
        .collect();
    rows.push(("", Vec::new()));
    rows.push((field, input_field(input, room, word, palette)));
    (
        PopupFrame {
            title: title.into(),
            meta: logout_machine(input),
            hints: hints.to_vec(),
        },
        label_rows(rows, palette),
    )
}

/// The description rows of one palette entry in a popup `inner` cells wide, under a key
/// column `key_w` cells wide.
fn palette_desc(desc: &str, key_w: usize, inner: u16) -> Vec<String> {
    wrap_text(
        desc,
        (inner as usize).saturating_sub(3 + key_w + 2 + 1).max(1) as u16,
    )
}

/// The display rows the palette's `entries` take in a popup `inner` cells wide, the count
/// its height is sized from.
pub(crate) fn palette_rows(entries: &[(String, String)], key_w: usize, inner: u16) -> usize {
    entries
        .iter()
        .map(|(_, d)| palette_desc(d, key_w, inner).len())
        .sum()
}

/// The command palette's rows for a popup `inner` cells wide with `visible` rows under the
/// query field: the query field, then one entry per command in the key list's grammar,
/// the key cell bold in a column as wide as the widest key, then the description, wrapped
/// under the description column rather than cut. The selected entry is reversed across the
/// whole width with `❯` in its first column, and the window starts late enough to show it
/// whole. The `hover` entry, the soft selection, is underlined across its rows.
/// `entries` pairs each key cell with its description; an empty key is a login the
/// palette offers, marked with the login-needed glyph. Each line comes with the entry it
/// belongs to, so a click is hit-tested against the rows the paint shows.
#[allow(clippy::too_many_arguments)]
pub(crate) fn palette_lines(
    query: &str,
    entries: &[(String, String)],
    key_w: usize,
    selected: usize,
    hover: Option<usize>,
    visible: usize,
    inner: u16,
    palette: &palette::Palette,
) -> ItemLines {
    let muted = Style::default().fg(palette.decoration);
    let key = crate::ui::keylist::key_cell_style(palette);
    let mut q = vec![Span::styled(" : ", muted)];
    q.extend(text_field_in(
        query,
        query.chars().count(),
        true,
        "",
        field_room(inner, 3),
        palette,
    ));
    let mut lines = vec![(None, Line::from(q))];
    if entries.is_empty() {
        let room = (inner as usize).saturating_sub(2).max(1) as u16;
        lines.extend(
            wrap_text("no matching commands", room)
                .into_iter()
                .map(|c| (None, Line::from(Span::styled(format!(" {c}"), muted)))),
        );
        return lines;
    }
    let descs: Vec<Vec<String>> = entries
        .iter()
        .map(|(_, d)| palette_desc(d, key_w, inner))
        .collect();
    let selected = selected.min(entries.len() - 1);
    // The first entry shown: the earliest one from which the selected entry still ends
    // inside the window.
    let mut start = selected;
    let mut used = descs[selected].len();
    while start > 0 && used + descs[start - 1].len() <= visible {
        start -= 1;
        used += descs[start].len();
    }
    let mut body = Vec::new();
    for (i, ((k, _), desc)) in entries.iter().zip(&descs).enumerate().skip(start) {
        if body.len() >= visible {
            break;
        }
        let chosen = i == selected;
        // The selected entry is reversed as one surface, so its key cell keeps its weight
        // and no colour the reversal would turn into a second background.
        let (cell, cell_style) = if k.is_empty() {
            (
                crate::ui::chrome::BLOCK_MARK.to_string(),
                if chosen {
                    Style::default()
                } else {
                    Style::default().fg(palette.warning)
                },
            )
        } else if chosen {
            (k.clone(), palette::interaction_key_style())
        } else {
            (k.clone(), key)
        };
        let pad = key_w.saturating_sub(UnicodeWidthStr::width(cell.as_str()));
        for (n, chunk) in desc.iter().enumerate() {
            let mut spans = if n == 0 {
                vec![
                    Span::raw(if chosen {
                        format!(" {} ", crate::ui::switcher::SELECTED_MARK)
                    } else {
                        "   ".to_string()
                    }),
                    Span::styled(cell.clone(), cell_style),
                    Span::raw(" ".repeat(pad + 2)),
                    Span::raw(chunk.clone()),
                ]
            } else {
                vec![
                    Span::raw(" ".repeat(3 + key_w + 2)),
                    Span::raw(chunk.clone()),
                ]
            };
            let used: usize = spans.iter().map(|s| s.width()).sum();
            spans.push(Span::raw(" ".repeat((inner as usize).saturating_sub(used))));
            let mut style = Style::default();
            if chosen {
                style = style.patch(palette::selection_style(palette));
            }
            if hover == Some(i) {
                style = style.patch(palette::soft_selection_style());
            }
            body.push((Some(i), Line::from(spans).style(style)));
        }
    }
    body.truncate(visible);
    lines.extend(body);
    lines
}

/// A list popup's body lines, each with the item it belongs to (none for a row that is
/// no item), so the paint and the pointer's hit-test read one layout.
pub(crate) type ItemLines = Vec<(Option<usize>, Line<'static>)>;

/// The palette's keys on its bottom border.
pub(crate) const PALETTE_HINTS: &[Hint] = &[("↑↓", "select"), ("Enter", "run"), ("Esc", "close")];
/// The help's keys on its bottom border.
pub(crate) const HELP_HINTS: &[Hint] = &[("←→", "section"), ("↑↓", "scroll"), ("Esc", "close")];
/// The hosts-to-check table's keys on its bottom border.
pub(crate) const CHECK_HINTS: &[Hint] = &[("↑↓", "select"), ("Enter", "open"), ("Esc", "close")];
/// The history's keys on its bottom border.
pub(crate) const HISTORY_HINTS: &[Hint] = &[("Esc", "close")];

/// What a popup's frame says: the accent title at the left of its top border, the muted
/// meta at the right of it, and the keys at the right of its bottom border.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct PopupFrame {
    pub(crate) title: String,
    pub(crate) meta: String,
    pub(crate) hints: Vec<Hint>,
}

/// The rounded, opaque box every popup and the key list share: muted rule, the accent bold
/// title in the top border's left end, and the muted `meta` at its right end where both fit
/// with a corner's worth of rule between them.
pub(crate) fn popup_block(
    title: &str,
    meta: &str,
    width: u16,
    palette: &palette::Palette,
) -> Block<'static> {
    let mut block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(palette.decoration))
        .title(Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(palette.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::reset());
    let used = UnicodeWidthStr::width(title) + UnicodeWidthStr::width(meta) + 4 + 4;
    if !meta.is_empty() && used <= width as usize {
        block = block.title_top(
            Line::from(Span::styled(
                format!(" {meta} "),
                Style::default().fg(palette.decoration),
            ))
            .right_aligned(),
        );
    }
    block
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
    chrome: &PopupFrame,
    lines: Vec<Line>,
    palette: &palette::Palette,
) {
    frame.render_widget(Clear, rect);
    // Rounded corners + a muted border + an accent bold title: the popup reads as a
    // floating panel over the content rather than a boxed region of it. The reset
    // base style keeps the interior opaque (see the doc comment above). The keys sit on
    // the bottom border's right end and never wrap: what does not fit is given up.
    let mut block = popup_block(&chrome.title, &chrome.meta, rect.width, palette);
    let hints = fit_hints(&chrome.hints, (rect.width as usize).saturating_sub(6));
    if !hints.is_empty() {
        let mut spans = vec![Span::raw(" ")];
        spans.extend(hint_spans(&hints, palette));
        spans.push(Span::raw(" "));
        block = block.title_bottom(Line::from(spans).right_aligned());
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;
    use ratatui::Terminal;

    fn edit_input(buffer: &str) -> Input {
        Input::new(InputMode::New, buffer.to_string(), None)
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
    fn the_caret_window_keeps_the_caret_in_view() {
        // A short value fits whole with the caret as the trailing cell; a long one shows
        // its tail with the caret at the right edge; a mid-value caret keeps the char under
        // it in view. The window never exceeds its cells.
        let join = |(a, b, c): (String, String, String)| format!("{a}{b}{c}");
        assert_eq!(join(caret_window("ab", 2, 40)), "ab ");
        let long = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
        let t = join(caret_window(long, 52, 20));
        assert_eq!(t.chars().count(), 20, "{t:?}");
        assert!(t.ends_with("XYZ "), "the tail survives: {t:?}");
        let (before, at, _) = caret_window(long, 45, 20);
        assert_eq!(at, "T", "the char under a mid-value caret stays in view");
        assert!(before.ends_with('S'));
        let (_, at, _) = caret_window("abc", 1, 1);
        assert_eq!(at, "b", "a one-cell window still shows the caret");
    }

    #[test]
    fn the_caret_window_measures_terminal_cells_for_wide_text() {
        let (before, at, after) = caret_window(&"한".repeat(20), 20, 11);
        let w = UnicodeWidthStr::width(format!("{before}{at}{after}").as_str());
        assert!(w <= 11, "{w}");
        assert!(before.contains('한'));
    }

    #[test]
    fn a_hint_line_gives_up_hints_from_the_end_and_keeps_the_way_out() {
        let hints: &[Hint] = &[("↑↓", "select"), ("Enter", "run"), ("Esc", "close")];
        assert_eq!(hints_width(hints), 33);
        assert_eq!(fit_hints(hints, 100), hints.to_vec());
        assert_eq!(
            fit_hints(hints, 21),
            vec![("↑↓", "select"), ("Esc", "close")]
        );
        assert_eq!(fit_hints(hints, 3), vec![("Esc", "close")]);
    }

    #[test]
    fn an_empty_field_shows_its_placeholder_under_the_caret() {
        let p = palette::Palette::default();
        let input = Input::new(InputMode::New, String::new(), None);
        let spans = input_field(&input, 20, "auto", &p);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "auto");
        assert!(spans[0].style.add_modifier.contains(Modifier::REVERSED));
        assert!(!spans[1].style.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn the_logout_popover_states_its_facts_as_rows() {
        let p = palette::Palette::default();
        let mut input = Input::new(InputMode::Logout, "logo".into(), Some("gpu-01".into()));
        input.facts = vec![
            ("session", "gpu-01/train".into()),
            ("SSH login", "not observed".into()),
            ("password", "held password is cleared".into()),
            (
                "key",
                "removed from gpu-01; asks first if xmux did not add it".into(),
            ),
            ("connections", "closes gpu-01 connections".into()),
        ];
        let (w, h) = logout_size(&input, 140);
        let (chrome, lines) = logout_popover(&input, w, &p);
        assert_eq!(chrome.title, "log out");
        assert_eq!(chrome.meta, "gpu-01");
        assert_eq!(lines.len() as u16 + 2, h);
        let text = flat(&lines);
        for row in [
            "session      gpu-01/train",
            "SSH login    not observed",
            "password     held password is cleared",
            "key          removed from gpu-01; asks first if xmux did not add it",
            "connections  closes gpu-01 connections",
            "type logout  logo",
        ] {
            assert!(text.contains(row), "{row:?} in {text}");
        }
    }

    #[test]
    fn the_logout_key_popover_keeps_the_logout_grammar_with_its_own_word() {
        let p = palette::Palette::default();
        let mut input = Input::new(InputMode::LogoutKeys, String::new(), Some("gpu-01".into()));
        input.facts = vec![
            ("key", "1 line of this PC's key not added by xmux".into()),
            ("remove", "ssh outside xmux loses this key too".into()),
        ];
        let (w, h) = logout_size(&input, 140);
        let (chrome, lines) = logout_popover(&input, w, &p);
        assert_eq!(chrome.title, "remove key");
        assert_eq!(chrome.meta, "gpu-01");
        assert_eq!(chrome.hints, vec![("Enter", "remove"), ("Esc", "keep it")]);
        assert_eq!(lines.len() as u16 + 2, h);
        let text = flat(&lines);
        for row in [
            "key          1 line of this PC's key not added by xmux",
            "remove       ssh outside xmux loses this key too",
            "type remove  remove",
        ] {
            assert!(text.contains(row), "{row:?} in {text}");
        }
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
            tab: None,
            hover: None,
            decoder: crate::display::decode::KeyDecoder::new(),
        })
    }

    /// Feeds `bytes` to the help with a layout too long to hold any scroll back.
    fn feed(m: &mut Option<Modal>, bytes: &[u8]) -> bool {
        feed_reader(m, bytes, &|_| crate::state::HelpMap {
            heads: vec![0],
            max_scroll: usize::MAX,
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
            None,
            None,
            200,
            u16::MAX,
        );
        assert_eq!(t, "", "every row fits, so the meta names no range");
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
            None,
            None,
            20,
            u16::MAX,
        );
        let text = flat(&lines);
        assert!(text.contains(" / zzzz"), "{text}");
        assert!(text.contains("no key or glyph matches"), "{text}");
    }

    #[test]
    fn a_key_wider_than_the_help_key_column_takes_its_own_row() {
        let palette = palette::Palette::default();
        let pos = crate::ui::switcher::NavPosition::Left;
        let (_, lines) = help_lines(
            "C-g",
            pos,
            &palette,
            "click a card",
            0,
            None,
            None,
            50,
            u16::MAX,
        );
        let text: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
        let at = text
            .iter()
            .position(|l| l.trim_end() == " click a card")
            .unwrap_or_else(|| panic!("the key alone on its row: {text:?}"));
        let kw = key_column_width(&help_rows("C-g", pos));
        assert!(kw <= HELP_KEY_CAP && kw < "click a card".len(), "{kw}");
        assert!(
            text[at + 1].starts_with(&" ".repeat(1 + kw + 2))
                && text[at + 1].trim_start().starts_with("open it"),
            "the description under the description column: {:?}",
            text[at + 1]
        );
    }

    #[test]
    fn the_help_scrolls_and_holds_its_last_page_full() {
        let palette = palette::Palette::default();
        let pos = crate::ui::switcher::NavPosition::Left;
        let (_, all) = help_lines("C-g", pos, &palette, "", 0, None, None, u16::MAX, u16::MAX);
        let total = all.len() - HELP_LEAD;
        let (title, lines) = help_lines("C-g", pos, &palette, "", 0, None, None, 12, u16::MAX);
        assert_eq!(
            lines.len(),
            12,
            "the search field, the tab row, and ten rows"
        );
        assert_eq!(title, format!("1-10 of {total}"));
        assert!(flat(&lines).contains("move (nav focus)"));
        let (title, lines) = help_lines("C-g", pos, &palette, "", 5, None, None, 12, u16::MAX);
        assert_eq!(title, format!("6-15 of {total}"));
        assert!(
            !flat(&lines[HELP_LEAD..]).contains("move (nav focus)"),
            "scrolled past"
        );
        let (title, lines) = help_lines(
            "C-g",
            pos,
            &palette,
            "",
            usize::MAX,
            None,
            None,
            12,
            u16::MAX,
        );
        assert_eq!(
            title,
            format!("{}-{total} of {total}", total - 9),
            "a scroll past the end shows the last full page"
        );
        assert!(
            flat(&lines).contains("five seconds"),
            "the legend's last row"
        );
    }

    #[test]
    fn the_search_field_stays_on_the_first_row_however_the_rows_are_filtered() {
        let palette = palette::Palette::default();
        let pos = crate::ui::switcher::NavPosition::Left;
        for (query, scroll) in [("", 0), ("quit", 0), ("zzzz", 0), ("", usize::MAX)] {
            let (_, lines) =
                help_lines("C-g", pos, &palette, query, scroll, None, None, 6, u16::MAX);
            let first = flat(&lines[..1]);
            assert!(first.starts_with(" / "), "{query:?}: {first}");
        }
        assert_eq!(HELP_HINTS.last().map(|h| h.0), Some("Esc"));
    }

    #[test]
    fn an_arrow_split_across_two_reads_scrolls_and_types_nothing() {
        let mut m = help();
        assert!(feed(&mut m, b"\x1b["));
        assert!(feed(&mut m, b"B"));
        assert!(
            matches!(&m, Some(Modal::Help { query, scroll: 1, .. }) if query.is_empty()),
            "the arrow's last byte is not typed into the search"
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
        assert!(!feed(&mut m, b"q"), "closed → not consumed");

        m = help();
        assert!(feed(&mut m, b"qu"), "open → consumed");
        assert!(
            matches!(&m, Some(Modal::Help { query, .. }) if query == "qu"),
            "printable keys type the search, q included"
        );
        assert!(feed(&mut m, b"\x7f"));
        assert!(matches!(&m, Some(Modal::Help { query, .. }) if query == "q"));
        assert!(feed(&mut m, b"\x1b[B\x1b[B\x1b[6~"));
        assert!(
            matches!(&m, Some(Modal::Help { scroll: 12, .. })),
            "↓ scrolls one row and PgDn ten"
        );
        assert!(feed(&mut m, b"\x1b[A"));
        assert!(matches!(&m, Some(Modal::Help { scroll: 11, .. })));
        assert!(feed(&mut m, b"x"));
        assert!(
            matches!(&m, Some(Modal::Help { scroll: 0, .. })),
            "a new search starts at the top of what it matches"
        );
        assert!(feed(&mut m, b"\x15"));
        assert!(matches!(&m, Some(Modal::Help { query, .. }) if query.is_empty()));
        assert!(feed(&mut m, b"\x1b"), "lone Esc → consumed");
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
                &PopupFrame {
                    title: "t".into(),
                    ..Default::default()
                },
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

    #[test]
    fn a_long_search_keeps_its_caret_inside_the_popup() {
        let palette = palette::Palette::default();
        let query = "a".repeat(40);
        let (_, help) = help_lines(
            "C-g",
            crate::ui::switcher::NavPosition::Left,
            &palette,
            &query,
            0,
            None,
            None,
            10,
            29,
        );
        let commands: Vec<Line> = palette_lines(&query, &[], 1, 0, None, 0, 29, &palette)
            .into_iter()
            .map(|(_, line)| line)
            .collect();
        for line in [&help[0], &commands[0]] {
            assert!(line.width() <= 29, "{}", line.width());
            assert!(caret_offset(line).is_some_and(|x| x < 29));
        }
    }

    #[test]
    fn a_caret_past_the_last_cell_a_terminal_has_is_no_caret() {
        let line = Line::from(vec![
            Span::raw("a".repeat(70_000)),
            Span::styled(" ", Style::default().add_modifier(Modifier::REVERSED)),
        ]);
        assert_eq!(caret_offset(&line), None);
    }

    /// Every cell of `lines` with the whitespace taken out, so text wrapped or hard-split
    /// across rows still reads as one run.
    fn squeezed(lines: &[Line<'static>]) -> String {
        flat(lines).split_whitespace().collect()
    }

    fn squeeze(text: &str) -> String {
        text.split_whitespace().collect()
    }

    const POS: crate::ui::switcher::NavPosition = crate::ui::switcher::NavPosition::Left;

    #[test]
    fn the_help_parts_its_sections_with_one_blank_row() {
        let palette = palette::Palette::default();
        let (_, lines) = help_lines("C-g", POS, &palette, "", 0, None, None, u16::MAX, 200);
        let body: Vec<String> = lines[HELP_LEAD..].iter().map(|l| l.to_string()).collect();
        let titles: Vec<String> = crate::model::keys::Section::ALL
            .iter()
            .map(|s| s.title().to_string())
            .chain([GLYPH_SECTION.to_string()])
            .collect();
        assert_eq!(
            body[0],
            format!(" {}", titles[0]),
            "no blank row before the first"
        );
        for title in &titles[1..] {
            let at = body
                .iter()
                .position(|l| l == &format!(" {title}"))
                .unwrap_or_else(|| panic!("{title} in {body:?}"));
            assert!(body[at - 1].is_empty(), "one blank row before {title}");
            assert!(
                !body[at - 2].is_empty(),
                "only one blank row before {title}"
            );
        }
        let blanks = body.iter().filter(|l| l.is_empty()).count();
        assert_eq!(
            blanks,
            titles.len() - 1,
            "a blank row only between sections"
        );
        let rows = help_rows("C-g", POS);
        let kw = key_column_width(&rows);
        let keys = rows
            .iter()
            .filter(|r| matches!(r, HelpRow::Key(..)))
            .count();
        let wide = rows
            .iter()
            .filter(|r| matches!(r, HelpRow::Key(k, _) if UnicodeWidthStr::width(k.as_str()) > kw))
            .count();
        assert_eq!(
            body.len(),
            titles.len() + keys + blanks + wide,
            "the same rows, a key wider than its column on a row of its own"
        );
    }

    #[test]
    fn every_help_row_wraps_inside_a_narrow_popup_and_keeps_its_words() {
        let palette = palette::Palette::default();
        for inner in [20u16, 30, 44] {
            let (_, lines) = help_lines("C-g", POS, &palette, "", 0, None, None, u16::MAX, inner);
            for l in &lines[HELP_LEAD..] {
                assert!(l.width() <= inner as usize, "{inner}: {l:?}");
            }
            let all = squeezed(&lines[HELP_LEAD..]);
            for row in help_rows("C-g", POS) {
                let (HelpRow::Head(t) | HelpRow::Key(t, _)) = &row;
                assert!(all.contains(&squeeze(t)), "{inner}: {t:?}");
                if let HelpRow::Key(_, d) = &row {
                    assert!(all.contains(&squeeze(d)), "{inner}: {d:?}");
                }
            }
        }
    }

    #[test]
    fn a_wrapped_help_description_hangs_under_the_description_column() {
        let palette = palette::Palette::default();
        let kw = key_column_width(&help_rows("C-g", POS));
        let (_, lines) = help_lines(
            "C-g",
            POS,
            &palette,
            "current source",
            0,
            None,
            None,
            40,
            40,
        );
        let text: Vec<String> = lines[HELP_LEAD..].iter().map(|l| l.to_string()).collect();
        let at = text
            .iter()
            .position(|l| l.starts_with(" C-g i "))
            .unwrap_or_else(|| panic!("{text:?}"));
        assert!(
            text[at + 1].starts_with(&" ".repeat(1 + kw + 2)) && !text[at + 1].trim().is_empty(),
            "the description continues under its column, not under the key: {text:?}"
        );
    }

    #[test]
    fn the_tab_row_names_each_section_and_lights_the_active_one() {
        let palette = palette::Palette::default();
        let (_, lines) = help_lines("C-g", POS, &palette, "", 0, None, None, 30, 120);
        let row = &lines[HELP_TAB_ROW as usize];
        let shown: Vec<&str> = row
            .spans
            .iter()
            .map(|s| s.content.trim())
            .filter(|s| !s.is_empty())
            .collect();
        assert_eq!(
            shown,
            [
                "move (nav focus)",
                "navigate",
                "sessions",
                "view",
                "app",
                "mouse",
                "glyphs"
            ]
        );
        let lit: Vec<&str> = row
            .spans
            .iter()
            .filter(|s| s.style.add_modifier.contains(Modifier::BOLD))
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(lit, ["move (nav focus)"], "the active tab alone is lit");
        assert!(row
            .spans
            .iter()
            .all(|s| !s.style.add_modifier.contains(Modifier::REVERSED)));
    }

    /// The map the help's keys use in a popup `inner` wide and `visible` tall.
    fn feed_sized(m: &mut Option<Modal>, bytes: &[u8], inner: u16, visible: u16) {
        feed_reader(m, bytes, &|q| help_map("C-g", POS, q, inner, visible));
    }

    fn help_at(m: &Option<Modal>) -> (usize, Option<usize>) {
        match m {
            Some(Modal::Help { scroll, tab, .. }) => (*scroll, *tab),
            _ => panic!("help closed"),
        }
    }

    #[test]
    fn a_tab_key_scrolls_its_section_title_to_the_top_of_the_body() {
        let palette = palette::Palette::default();
        let (inner, visible) = (60, 14);
        let map = help_map("C-g", POS, "", inner, visible);
        let mut m = help();
        feed_sized(&mut m, b"\x1b[C", inner, visible);
        assert_eq!(help_at(&m), (map.heads[1], Some(1)));
        let (_, lines) = help_lines(
            "C-g",
            POS,
            &palette,
            "",
            map.heads[1],
            Some(1),
            None,
            visible,
            inner,
        );
        assert_eq!(
            lines[HELP_LEAD].to_string(),
            " navigate",
            "the title is the top body row"
        );
        feed_sized(&mut m, b"\x1b[C\x1b[C", inner, visible);
        assert_eq!(help_at(&m), (map.heads[3], Some(3)));
        feed_sized(&mut m, b"\x1b[D", inner, visible);
        assert_eq!(help_at(&m), (map.heads[2], Some(2)));
        // The last section is shorter than a page: its tab is chosen and lit while the
        // scroll stops at the end with its title in view.
        let last = map.heads.len() - 1;
        feed_sized(&mut m, &b"\x1b[C".repeat(10), inner, visible);
        assert_eq!(
            help_at(&m),
            (map.max_scroll.min(map.heads[last]), Some(last))
        );
        let at = help_at(&m).0;
        let (_, lines) = help_lines(
            "C-g",
            POS,
            &palette,
            "",
            at,
            Some(last),
            None,
            visible,
            inner,
        );
        let lit: Vec<String> = lines[HELP_TAB_ROW as usize]
            .spans
            .iter()
            .filter(|s| s.style.add_modifier.contains(Modifier::BOLD))
            .map(|s| s.content.to_string())
            .collect();
        assert_eq!(lit, [GLYPH_SECTION]);
        assert!(flat(&lines[HELP_LEAD..]).contains(&format!(" {GLYPH_SECTION}")));
        feed_sized(&mut m, b"\x1b[D", inner, visible);
        assert_eq!(
            help_at(&m).1,
            Some(last - 1),
            "← steps back from the chosen tab"
        );
    }

    #[test]
    fn scrolling_hands_the_active_tab_to_the_section_at_the_top() {
        let (inner, visible) = (60, 14);
        let map = help_map("C-g", POS, "", inner, visible);
        assert_eq!(map.active(None, 0), 0);
        assert_eq!(
            map.active(None, map.heads[2]),
            2,
            "its title on the top row"
        );
        assert_eq!(
            map.active(None, map.heads[2] - 1),
            1,
            "the blank row ending the section before it"
        );
        assert_eq!(
            map.active(None, map.heads[2] + 1),
            2,
            "its title above the top"
        );
        let mut m = help();
        feed_sized(&mut m, b"\x1b[C\x1b[C", inner, visible);
        feed_sized(&mut m, b"\x1b[B", inner, visible);
        assert_eq!(
            help_at(&m),
            (map.heads[2] + 1, None),
            "a scroll key frees the tab"
        );
        assert_eq!(map.active(None, help_at(&m).0), 2);
        feed_sized(&mut m, b"\x1b[A\x1b[A", inner, visible);
        assert_eq!(
            map.active(None, help_at(&m).0),
            1,
            "scrolled back into navigate"
        );
    }

    #[test]
    fn a_search_leaves_the_tabs_of_the_sections_it_matched() {
        let palette = palette::Palette::default();
        let (_, lines) = help_lines("C-g", POS, &palette, "quit", 0, None, None, 20, 80);
        assert_eq!(lines[HELP_TAB_ROW as usize].to_string().trim(), "app");
        let (_, lines) = help_lines("C-g", POS, &palette, "toast", 0, None, None, 20, 80);
        let tabs = lines[HELP_TAB_ROW as usize].to_string();
        assert!(
            tabs.contains("mouse") && tabs.contains(GLYPH_SECTION) && !tabs.contains("navigate"),
            "{tabs}"
        );
        let (_, lines) = help_lines("C-g", POS, &palette, "zzzz", 0, None, None, 20, 80);
        assert!(lines[HELP_TAB_ROW as usize].to_string().trim().is_empty());
        let mut m = help();
        feed_sized(&mut m, b"zzzz\x1b[C", 80, 20);
        assert_eq!(help_at(&m), (0, None), "no section, no tab to move to");
    }

    #[test]
    fn a_tab_row_too_narrow_for_every_tab_keeps_the_active_one_in_view() {
        let titles: Vec<String> = ["move (nav focus)", "navigate", "sessions", "view", "app"]
            .map(String::from)
            .to_vec();
        let (tabs, before, after) = help_tabs(&titles, 0, 30);
        assert_eq!(
            (
                tabs.iter().map(|t| t.section).collect::<Vec<_>>(),
                before,
                after
            ),
            (vec![0, 1], false, true)
        );
        let (tabs, before, after) = help_tabs(&titles, 4, 30);
        assert!(
            tabs.iter().any(|t| t.section == 4) && before && !after,
            "{tabs:?}"
        );
        assert!(tabs.iter().all(|t| (t.x + t.width) as usize <= 29));
        let line = help_tab_line(&titles, 4, None, 30, &palette::Palette::default());
        assert!(line.to_string().starts_with(" ‹ "), "{line}");
        assert!(line.width() <= 30);
        let (tabs, _, _) = help_tabs(&titles, 0, 10);
        assert_eq!(
            tabs.len(),
            1,
            "an active title wider than the row is shown cut"
        );
        assert!((tabs[0].x + tabs[0].width) as usize <= 9);
    }

    #[test]
    fn the_palette_wraps_a_description_under_its_column_and_keeps_the_selection_whole() {
        let p = palette::Palette::default();
        let entries: Vec<(String, String)> = (0..6)
            .map(|i| {
                (
                    format!("C-g {i}"),
                    format!("command {i} with a description far wider than the popup"),
                )
            })
            .collect();
        let rows = palette_desc(&entries[0].1, 5, 30).len();
        assert!(rows > 1);
        let lines: Vec<Line> = palette_lines("", &entries, 5, 5, None, 2 * rows, 30, &p)
            .into_iter()
            .map(|(_, line)| line)
            .collect();
        assert!(lines.iter().all(|l| l.width() <= 30));
        let body: Vec<String> = lines[1..].iter().map(|l| l.to_string()).collect();
        assert_eq!(body.len(), 2 * rows, "two entries fill the window");
        assert!(
            body[1].starts_with(&" ".repeat(3 + 5 + 2)),
            "the description hangs under its column: {body:?}"
        );
        let all = squeezed(&lines[1..]);
        assert!(
            all.contains(&squeeze(&entries[5].1)),
            "the selected entry is whole: {all}"
        );
        assert!(lines.last().unwrap().style == palette::selection_style(&p));
        assert_eq!(palette_rows(&entries, 5, 30), 6 * rows);
    }

    #[test]
    fn a_narrow_logout_new_session_and_jump_wrap_their_values() {
        let p = palette::Palette::default();
        let mut input = Input::new(InputMode::Logout, "lo".into(), Some("gpu-01".into()));
        input.facts = vec![
            ("session", "gpu-01/a-session-with-a-long-name".into()),
            (
                "key",
                "removed from gpu-01; asks first if xmux did not add it".into(),
            ),
        ];
        let (w, h) = logout_size(&input, 40);
        let (_, lines) = logout_popover(&input, w, &p);
        assert_eq!(
            lines.len() as u16 + 2,
            h,
            "the size counts the wrapped rows"
        );
        assert!(lines.iter().all(|l| l.width() <= (w - 2) as usize));
        let all = squeezed(&lines);
        for (_, v) in &input.facts {
            assert!(all.contains(&squeeze(v)), "{v} in {all}");
        }
        let text: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
        assert!(text[1].starts_with(&" ".repeat(1 + 11 + 2)), "{text:?}");

        let host = "a-very-long-host-name.example.com/tmux";
        let input = Input::new(InputMode::New, String::new(), Some("x".into()));
        let (_, lines) = new_session_popover(host, &input, 30, &p);
        assert!(lines.iter().all(|l| l.width() <= 28));
        assert!(squeezed(&lines).contains(host));
        assert!(
            caret_offset(lines.last().unwrap()).is_some(),
            "the field stays one row"
        );

        let input = Input::new(InputMode::Jump, "4".into(), None);
        let name = "gpu-02/a-session-with-a-name-longer-than-the-popup";
        let (_, lines) = jump_popup(&input, Some(name), false, 9, 30, &p);
        assert!(lines.len() > 1 && lines.iter().all(|l| l.width() <= 28));
        assert!(squeezed(&lines).contains(name));
        assert!(caret_offset(&lines[0]).is_some());
    }

    #[test]
    fn the_hovered_palette_entry_is_underlined_apart_from_the_selected_one() {
        let p = palette::Palette::default();
        let entries: Vec<(String, String)> = ["one", "two", "three"]
            .map(|d| ("k".to_string(), d.to_string()))
            .to_vec();
        let lines = palette_lines("", &entries, 1, 0, Some(1), 10, 40, &p);
        let items: Vec<Option<usize>> = lines.iter().map(|(i, _)| *i).collect();
        assert_eq!(
            items,
            [None, Some(0), Some(1), Some(2)],
            "each row names its entry"
        );
        let style = |i: usize| lines[i].1.style;
        assert_eq!(style(1), palette::selection_style(&p), "the hard selection");
        assert_eq!(
            style(2),
            palette::soft_selection_style(),
            "the soft selection"
        );
        assert_eq!(style(3), Style::default());
        let both = palette_lines("", &entries, 1, 1, Some(1), 10, 40, &p);
        assert_eq!(
            both[2].1.style,
            palette::selection_style(&p).patch(palette::soft_selection_style()),
            "one entry under both shows both"
        );
    }
}
