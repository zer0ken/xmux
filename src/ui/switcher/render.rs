use super::*;

use ratatui::style::Modifier;
use ratatui::widgets::{Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState};

use crate::ui::palette;

/// Whether the hint bar floats over the whole window this frame instead of sitting in
/// the nav column.
///
/// Three states make it float, and they are the three where xmux must speak RIGHT
/// NOW: the prefix is armed (the cheatsheet is wanted, and says more than a nav column
/// fits), an input is open (the line being typed needs the room, and the user must see
/// it even when the nav is hidden), or a refusal flash is showing while the nav is
/// hidden (there is no nav row to put it in, and a refusal the user cannot see is worse
/// than a row borrowed for a moment).
///
/// Scan progress and the active filter deliberately do NOT float: they persist, and a
/// hidden nav means the user asked for the whole screen to be the mux.
fn hint_bar_floats(nav_width: u16, state: &crate::state::State) -> bool {
    state.is_inputting() || state.chrome.armed || (nav_width == 0 && !state.chrome.flash.is_empty())
}

/// Where the hint bar actually paints. At rest it is the nav-local rect
/// `compute_regions` derived (empty when the nav is hidden, so the mux keeps every row).
/// Floating, it spans the whole window width: on the nav's own rows when the nav is
/// visible, and on the window's bottom rows when the nav is hidden and the layout
/// reserved none. Only the paint moves; the layout is untouched, so nothing reflows.
fn hint_bar_rect(nav_local: Rect, area: Rect, hint_bar_h: u16, floating: bool) -> Rect {
    if !floating {
        return nav_local;
    }
    if nav_local.height == 0 {
        // Nav hidden: no row was reserved, so borrow the window's bottom rows.
        let h = hint_bar_h.min(area.height);
        return Rect {
            x: area.x,
            y: area.y + area.height - h,
            width: area.width,
            height: h,
        };
    }
    Rect {
        x: area.x,
        width: area.width,
        ..nav_local
    }
}

/// The glyph marking the SELECTED card, in its address column.
///
/// A SHAPE, never a solid block. The selected card is reverse video, which swaps that
/// cell's own pair, so a filled block inverts into a background-coloured half-cell and is
/// absorbed into the inverted row's left edge - the mark vanishes exactly where it is
/// needed. An outline keeps its silhouette either way round.
pub(super) const SELECTED_MARK: &str = "\u{276f}";

/// Splits a nav region into `(cards, scrollbar strip)`. `needed` false gives the whole
/// region to the cards and an empty strip, so a nav that fits spends nothing on furniture.
/// The strip is the bottom ROW when the cards scroll sideways (the portrait column flow)
/// and the right COLUMN when they scroll down (the side list). Reserving instead of
/// overlaying is what keeps the thumb out of the selected card's inverted rect.
fn reserve_bar(area: Rect, needed: bool, horizontal: bool) -> (Rect, Rect) {
    if !needed {
        return (area, Rect::default());
    }
    if horizontal {
        if area.height < 2 {
            return (area, Rect::default());
        }
        let cards = Rect {
            height: area.height - 1,
            ..area
        };
        let bar = Rect {
            y: area.y + area.height - 1,
            height: 1,
            ..area
        };
        (cards, bar)
    } else {
        if area.width < 2 {
            return (area, Rect::default());
        }
        let cards = Rect {
            width: area.width - 1,
            ..area
        };
        let bar = Rect {
            x: area.x + area.width - 1,
            width: 1,
            ..area
        };
        (cards, bar)
    }
}

impl Switcher {
    pub fn render(
        &mut self,
        frame: &mut Frame,
        grid: Option<&crate::display::grid::Grid>,
        terminal_focused: bool,
        nav: NavSize,
        state: &crate::state::State,
    ) {
        let area = frame.area();
        self.screen_area = area;
        let nav_width = nav.width;
        // Cache the stacking so key handling routes the arrows to match what is on screen.
        // Measured from the width the user SET, so hiding the nav leaves it alone.
        self.layout = view_layout(area, nav.natural);
        // Reset the buffer before painting. The widgets below do not all fill every cell
        // they own - the mux grid only paints its top-left clip (cells past the grid size
        // are skipped), the view border rule sets fg only, and the nav list leaves blank
        // rows - so when the tree width changes (drag / prefix h·l) cells that switched
        // panes would otherwise keep stale content (the residue seen while resizing).
        // Clearing first makes every unpainted cell default; ratatui still diffs against
        // the last frame, so static content writes nothing (no flicker).
        frame.render_widget(Clear, area);
        // nav_width == 0 is the "nav hidden" sentinel (terminal view focused + auto-hide):
        // the terminal view owns the whole area - no nav list, no view border, and no
        // status line of its own, since the user asked for the whole screen to be the mux.
        if nav_width == 0 {
            self.nav_inner = Rect::default();
            self.render_terminal_view(frame, area, grid);
            if let Some(g) = grid {
                if !g.hide_cursor() {
                    frame.set_cursor_position(terminal_cursor_pos(area, g.cursor()));
                }
            }
            // The bar still floats for the two states that must be seen even here: the
            // armed prefix and a refusal flash. Hiding the nav hides the status line, not
            // xmux's ability to answer a keypress.
            if hint_bar_floats(nav_width, state) {
                let h = state.chrome.hint_bar_lines(area.width, state).len().max(1) as u16;
                let rect = hint_bar_rect(Rect::default(), area, h, true);
                state
                    .chrome
                    .render_hint_bar(frame, rect, state, crate::ui::chrome::BarFill::Row);
            }
            // The modal stacks above the bar: a popup is a stronger claim on the screen.
            self.render_modal_popup(frame, area, state);
            return;
        }
        // One geometry source for the whole frame (compute_regions), shared with the PTY
        // sizing and mouse hit-testing so they never diverge: the nav list / terminal split
        // horizontally (Side) or vertically (Top, for a portrait screen), parted by the view
        // border, and the hint bar takes the nav's bottom rows. The hint bar is normally one
        // row; a long flash wraps, so size it to the wrapped line count (never clipped).
        // Measured at the width it will RENDER at: the nav column normally, the whole
        // window whenever the bar floats (see `hint_bar_floats` / `hint_bar_rect`).
        let floating = hint_bar_floats(nav_width, state);
        let bar_w = if floating { area.width } else { nav_width };
        let hint_bar_h = state.chrome.hint_bar_lines(bar_w, state).len().max(1) as u16;
        let r = compute_regions(area, nav, hint_bar_h);
        let hidden = self.render_nav(frame, r.tree, state);
        // The view border marks focus between the two views (vertical in Side, horizontal in Top).
        state
            .chrome
            .render_view_border(frame, r.view_border, terminal_focused);
        let term_area = r.terminal;
        // A selected host with no session to show has no live grid to mirror: its host
        // screen fills the region instead, so neither state is ever a blank view with no
        // next step. One call for both, because they are one screen in two states.
        if let Some(kind) = self.current_view_screen(state) {
            let headline = self.view_screen_headline(state, kind);
            state
                .chrome
                .render_view_screen(frame, term_area, state, &headline, kind);
        } else {
            self.render_terminal_view(frame, term_area, grid);
        }
        // The hint bar paints LAST of the two views, so a floating bar can cover the
        // terminal view. At rest it stays inside the nav (its own status line); floating,
        // it widens to the whole window - the layout never reflows, only the paint reaches
        // further, so arming the prefix cannot shift a single card.
        // The bar is fit to what it has to say in either nav layout: the side layout's
        // status line reads as a label on its row, and the portrait band's bar shares its
        // row with the flow's offscreen counts (the bar keeps its own background but takes
        // only the cells it needs, and the counts sit at the ends of what is left). An
        // ARMED or flashing bar takes the whole row back, because a cheatsheet has to be
        // readable over whatever it covers.
        let fill = if floating || !state.chrome.flash.is_empty() {
            crate::ui::chrome::BarFill::Row
        } else {
            crate::ui::chrome::BarFill::Content
        };
        if let (Some(counts), crate::ui::chrome::BarFill::Content) = (hidden, fill) {
            let chip = state
                .chrome
                .hint_bar_chip_width(r.hint_bar.width, state)
                .min(r.hint_bar.width);
            let track = Rect {
                x: r.hint_bar.x + chip,
                width: r.hint_bar.width - chip,
                height: 1,
                ..r.hint_bar
            };
            Self::render_hidden_counts(frame, track, counts);
        }
        state.chrome.render_hint_bar(
            frame,
            hint_bar_rect(r.hint_bar, area, hint_bar_h, floating),
            state,
            fill,
        );
        // In the terminal view, place the real cursor at the grid's cursor so typing in the
        // mux is visible and tracks. Skipped when the child hid its cursor.
        if terminal_focused {
            if let Some(g) = grid {
                if !g.hide_cursor() {
                    frame.set_cursor_position(terminal_cursor_pos(term_area, g.cursor()));
                }
            }
        }
        self.render_modal_popup(frame, area, state);
    }

    /// The navigation cards. `Side` stacks them in one vertically-scrolling list; the
    /// portrait `Top` band flows them into columns (see [`columns`]), because a wide,
    /// short region shows three cards as a list and twenty as a grid.
    ///
    /// Either way a scrollbar, when one is needed, gets its own row or column of the
    /// region rather than sitting over the cards: the selected card is painted by
    /// inverting its whole rect, and a thumb inside that rect inverts with it into a
    /// hole in the bar.
    /// Returns how many cards are off screen either side of the portrait flow's window,
    /// as `(left, right)`: the caller writes those counts on the hint bar's row, at the
    /// ends the hidden columns are behind. `None` when the whole flow fits, and always in
    /// the side layout, whose own scrollbar is a column it reserves itself.
    fn render_nav(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        state: &crate::state::State,
    ) -> Option<(usize, usize)> {
        // No border box: the cards fill their region outright and a single rule
        // (render_view_border) separates it from the terminal view.
        self.nav_inner = area;
        self.nav_cells.clear();
        let spinner_glyph = crate::ui::spinner_glyph(state.chrome.spinner_frame);
        let num_w = self.number_width();
        match self.layout {
            ViewLayout::Side => {
                self.render_nav_list(frame, area, num_w, spinner_glyph);
                None
            }
            ViewLayout::Top => self.render_nav_columns(frame, area, num_w, spinner_glyph),
        }
    }

    /// The `Side` layout's nav: two BANDS of cards in one vertically-scrolling region,
    /// the section titles + session cards over the host-state cards, laid out by
    /// [`side::place`] - the one geometry the paint, the mouse hit-test and the
    /// scrollbar all read.
    /// Card heights are uniform - every navigation row, section title, session card, and
    /// host-state card alike, is one screen row - so a card's rect is recorded as it
    /// paints rather than derived a second time from a row pitch.
    /// `list_state` carries the settled scroll position for the next frame to resume
    /// from, and the selected card is painted in the terminal theme's own selected look
    /// by inverting its rect, so the card spans bake in no background of their own.
    fn render_nav_list(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        num_w: usize,
        spinner_glyph: char,
    ) {
        let heights = vec![1u16; self.rows.len()];
        // The placement decides whether the list scrolls, and the strip is a COLUMN, so
        // reserving it after the fact takes nothing away from what was just laid out.
        let flow = side::place(
            &heights,
            self.band_boundary(),
            area.height,
            self.list_state.offset(),
            self.selected,
        );
        let (cards, bar) = reserve_bar(area, flow.scrolls, false);
        *self.list_state.offset_mut() = flow.offset;
        for slot in &flow.slots {
            let rect = Rect {
                x: cards.x,
                y: cards.y + slot.y,
                width: cards.width,
                height: slot.h,
            };
            let lines = self.nav_row_lines(slot.idx, num_w, spinner_glyph, rect.width);
            frame.render_widget(Paragraph::new(lines), rect);
            if self.list_state.selected() == Some(slot.idx) {
                frame
                    .buffer_mut()
                    .set_style(rect, palette::selection_style());
            }
            self.nav_cells.push((slot.idx, rect));
        }
        if let Some(y) = flow.rule_y {
            Self::render_band_rule(
                frame,
                Rect {
                    x: cards.x,
                    y: cards.y + y,
                    width: cards.width,
                    height: 1,
                },
            );
        }
        self.render_nav_scrollbar(frame, bar, &flow);
    }

    /// The rule parting the side list's two bands once they scroll as one run. A single
    /// light horizontal line across the nav: it says the cards below it are a different
    /// kind of thing, which is all the blank gap says while both bands fit on screen.
    fn render_band_rule(frame: &mut Frame, rect: Rect) {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                BAND_RULE.repeat(rect.width as usize),
                Style::default().fg(palette::get().overlay),
            ))),
            rect,
        );
    }

    /// The portrait `Top` band's nav: the same rows flowed into columns that fill
    /// downward and continue to the right, each column holding whole sections (a
    /// `{host}/{mux}` title over its session cards) or a host-state card.
    ///
    /// The band spends none of its rows on the scrollbar: the thumb goes on the hint bar's
    /// row, beside the bar's own label, so every row of the band stays a card row and no
    /// card rect can contain the thumb (a selected card inverts its whole rect, and a thumb
    /// inside one inverts with it into a hole in the bar).
    fn render_nav_columns(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        num_w: usize,
        spinner_glyph: char,
    ) -> Option<(usize, usize)> {
        let cards: Vec<columns::Card> = (0..self.rows.len())
            .map(|i| self.flow_card(i, num_w, spinner_glyph))
            .collect();
        let band = area;
        let boundary = self.band_boundary().unwrap_or(cards.len());
        let placed = columns::place(&cards, band.height, boundary);
        let widths = columns::widths(&cards, &placed, band.width);
        let bcol = columns::boundary_col(&placed, boundary);
        let parting = columns::parting(&widths, bcol, band.width, COL_GUTTER);
        // Keep the selected card's column on screen, scrolling the least it takes. In the
        // gap parting there is nothing off screen to scroll to, so the offset resets.
        let sel_col = self
            .list_state
            .selected()
            .and_then(|i| placed.get(i))
            .map_or(0, |p| p.col);
        match parting {
            Some(columns::Parting::Gap) => self.nav_col_offset = 0,
            Some(columns::Parting::Rule) => {
                let dw = columns::display_widths(&widths, bcol, columns::Parting::Rule);
                let sel = columns::display_col(sel_col, bcol, parting);
                self.nav_col_offset =
                    columns::scroll_to(&dw, band.width, COL_GUTTER, self.nav_col_offset, sel);
            }
            None => {
                self.nav_col_offset = columns::scroll_to(
                    &widths,
                    band.width,
                    COL_GUTTER,
                    self.nav_col_offset,
                    sel_col,
                );
            }
        }
        let (cells, rule) = columns::cells(
            &placed,
            &widths,
            bcol,
            parting,
            band,
            self.nav_col_offset,
            COL_GUTTER,
        );
        for cell in &cells {
            // A section that split across a column break re-states its title at the top
            // of the continuation: that header row is drawn (never clickable) above the
            // card it continues.
            if let Some(h) = cell.header {
                let header_rect = Rect {
                    y: cell.rect.y - 1,
                    ..cell.rect
                };
                let lines = self.nav_row_lines(h, num_w, spinner_glyph, header_rect.width);
                frame.render_widget(Paragraph::new(lines), header_rect);
            }
            let lines = self.nav_row_lines(cell.idx, num_w, spinner_glyph, cell.rect.width);
            frame.render_widget(Paragraph::new(lines), cell.rect);
            if self.list_state.selected() == Some(cell.idx) {
                // The card's OWN rect, not the band's width: in a grid the selection marks
                // one cell, and a full-width bar would claim the columns beside it.
                frame
                    .buffer_mut()
                    .set_style(cell.rect, palette::selection_style());
            }
            self.nav_cells.push((cell.idx, cell.rect));
        }
        if let Some(rule_rect) = rule {
            Self::render_column_rule(frame, rule_rect);
        }
        // What the caller needs for the offscreen cue: the cards behind the columns the
        // window does not reach, counted on each side.
        let (shown, n) = match parting {
            Some(columns::Parting::Gap) => (widths.len(), widths.len()),
            Some(columns::Parting::Rule) => {
                let dw = columns::display_widths(&widths, bcol, columns::Parting::Rule);
                (
                    columns::visible_cols(&dw, band.width, self.nav_col_offset, COL_GUTTER),
                    dw.len(),
                )
            }
            None => (
                columns::visible_cols(&widths, band.width, self.nav_col_offset, COL_GUTTER),
                widths.len(),
            ),
        };
        if shown >= n {
            return None;
        }
        Some(columns::hidden_counts(
            &placed,
            bcol,
            parting,
            self.nav_col_offset,
            shown,
        ))
    }

    /// The vertical rule parting the two bands in the portrait flow once they cannot
    /// stay apart by a gap. A single light vertical line across the band, the same
    /// statement the side list's horizontal rule makes.
    fn render_column_rule(frame: &mut Frame, rect: Rect) {
        let style = Style::default().fg(palette::get().overlay);
        let buf = frame.buffer_mut();
        for y in rect.y..rect.y + rect.height {
            let cell = &mut buf[(rect.x, y)];
            cell.set_symbol("│");
            cell.set_style(style);
        }
    }

    /// Writes the offscreen-card counts in `track` - the hint bar's row minus the cells the
    /// bar's own label takes - at the ends the hidden columns are behind: `<< 5 more` on the
    /// left, `7 more >>` on the right. The arrows point the way the cards went, and the
    /// count says how many, which a scrollbar thumb cannot. Dropped, not clipped, when the
    /// row is too narrow to hold them.
    fn render_hidden_counts(frame: &mut Frame, track: Rect, (left, right): (usize, usize)) {
        // Neither count sits flush against what it is beside: the left one clears the
        // status label, the right one the window's edge, so each reads as a note in the
        // margin rather than text jammed into a corner.
        const PAD: u16 = 2;
        let track = Rect {
            x: track.x + PAD.min(track.width),
            width: track.width.saturating_sub(PAD * 2),
            ..track
        };
        // The overflow cue's OWN role, with the count BOLD so the number - the thing a
        // user reaches for - stands off the `<< … more >>` furniture around it.
        let more_style = Style::default().fg(palette::get().more);
        let bold = more_style.add_modifier(Modifier::BOLD);
        // `<< n more` / `n more >>`, the count the one bold cell in the run.
        let make_label = |n: usize, left_arrow: bool| -> (Vec<Span<'static>>, u16) {
            let n = n.to_string();
            let (pre, post) = if left_arrow {
                ("<< ", " more")
            } else {
                ("", " more >>")
            };
            let w = (pre.chars().count() + n.chars().count() + post.chars().count()) as u16;
            (
                vec![
                    Span::styled(pre, more_style),
                    Span::styled(n, bold),
                    Span::styled(post, more_style),
                ],
                w,
            )
        };
        let mut used = 0u16;
        if left > 0 {
            let (spans, w) = make_label(left, true);
            if w <= track.width {
                frame.render_widget(
                    Paragraph::new(Line::from(spans)),
                    Rect {
                        width: w,
                        height: 1,
                        ..track
                    },
                );
                used = w + 1;
            }
        }
        if right > 0 {
            let (spans, w) = make_label(right, false);
            if w + used <= track.width {
                frame.render_widget(
                    Paragraph::new(Line::from(spans)),
                    Rect {
                        x: track.x + track.width - w,
                        width: w,
                        height: 1,
                        ..track
                    },
                );
            }
        }
    }

    /// A minimal scrollbar in the strip `reserve_bar` set aside beside the nav list,
    /// drawn only when the cards overflow the region - the offscreen-content cue the flat
    /// list otherwise lacks. Thumb only (no track / arrows) so it reads as a position
    /// marker, not furniture. Counted in cards (not screen rows) over the variable card
    /// heights, from the placement the cards were painted with.
    fn render_nav_scrollbar(&mut self, frame: &mut Frame, bar: Rect, flow: &side::Flow) {
        let total = self.rows.len();
        if bar.width == 0 || bar.height == 0 {
            return;
        }
        let mut sb = ScrollbarState::new(total.saturating_sub(flow.visible))
            .position(flow.offset)
            .viewport_content_length(flow.visible);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .track_symbol(None)
                .thumb_symbol("▐")
                .thumb_style(Style::default().fg(palette::get().overlay)),
            bar,
            &mut sb,
        );
    }

    /// How many columns the card numbers need: the digit count of the highest card
    /// number. One width for the whole frame, so the names stay aligned with each
    /// other instead of stepping right as the numbers gain a digit, and the numbers
    /// themselves line up by units place. Section titles carry no number, so the width
    /// counts the SELECTABLE cards only.
    fn number_width(&self) -> usize {
        self.selectable_count()
            .saturating_sub(1)
            .to_string()
            .len()
            .max(1)
    }

    /// One row measured for the column flow: whether it opens a unit, how wide its
    /// content paints, and how many rows it takes. A section title's measured width is
    /// its `{host}/{mux}` alone - its trailing rule fills whatever column width is
    /// left, so it never widens a column.
    fn flow_card(&self, i: usize, num_w: usize, spinner_glyph: char) -> columns::Card {
        let lines = self.nav_row_lines(i, num_w, spinner_glyph, 0);
        let w = |n: usize| lines.get(n).map_or(0, |l: &Line| l.width() as u16);
        columns::Card {
            starts_run: self.starts_run(i),
            width: w(0),
            lines: 1,
        }
    }

    /// Builds one navigation row's lines. A session card is the address column + the
    /// session name on a single detail line; a section title is the `{host}/{mux}`
    /// header (dim, with a rule filling the row's width) and carries no address column;
    /// a host-state card is the host/mux name on its row, with the unreachable mark
    /// (`⚠`) riding after the host name and the mux taking the accent, or a spinner in
    /// the level a scanning host has not resolved. A host-state card claims a mux only
    /// when the mux is CONFIRMED - a bare-id host that is unreachable or still scanning
    /// names none, so the card reads the host alone or spins in the mux position.
    ///
    /// The ADDRESS column carries the card's dim number - the thing `prefix <digit>`
    /// types - on the same row as the session it names. On the SELECTED card that
    /// column holds the mark instead of a number, because "you are here" answers the
    /// same question the number answers, and one column pays for both. Every card's
    /// name therefore starts at the same screen column whatever the selection is doing.
    /// A name that shifts as the cursor passes is what makes a list twitch. Focus
    /// changes nothing else about a card: it does not grow a context line, and the
    /// session keeps the same style selected or not (the selected look is the inverted
    /// rect the paint applies, not a per-span style here).
    ///
    /// The surface background comes from the paint's `selection_style`, so no per-span
    /// background is baked in here.
    fn nav_row_lines(
        &self,
        i: usize,
        num_w: usize,
        spinner_glyph: char,
        width: u16,
    ) -> Vec<Line<'static>> {
        let row = &self.rows[i];
        let selected = self.list_state.selected() == Some(i);
        let accent = Style::default().fg(palette::get().accent);
        let number = Style::default().fg(color_number());
        let separator = Style::default().fg(color_separator());
        // The address column every card writes on - the only line, now that a card has
        // none other. A section title never calls it: it carries no number and is never
        // the selection.
        let address = move || -> Vec<Span<'static>> {
            if selected {
                vec![Span::styled(format!("{SELECTED_MARK:>num_w$} "), accent)]
            } else {
                let n = self.card_number(i);
                vec![Span::styled(format!("{n:>num_w$} "), number)]
            }
        };

        // Section title: `{host}/{mux}` in the quiet header role, followed by a rule
        // filling the row. Not a card - no number, not selectable, and the selection
        // can never land on it.
        if let RowRef::Section { .. } = &row.reference {
            let (host, mux, _) = context_of(row);
            let header = Style::default().fg(palette::get().overlay);
            let title = if mux.is_empty() {
                host.to_string()
            } else {
                format!("{host}/{mux}")
            };
            let title_w = UnicodeWidthStr::width(title.as_str()) as u16;
            let rule_w = width.saturating_sub(title_w.saturating_add(1));
            let mut spans = vec![Span::styled(title, header)];
            if rule_w > 0 {
                spans.push(Span::styled(
                    format!(" {}", BAND_RULE.repeat(rule_w as usize)),
                    header,
                ));
            }
            spans.push(Span::raw(" "));
            return vec![Line::from(spans)];
        }

        // Host-state card: a settled host (reachable empty or unreachable) and a
        // scanning host read the same way, one row: the host name, the state mark that
        // rides it (`⚠` unreachable), the confirmed mux, and - while the host is still
        // scanning - ONE spinner trailing the line. The spinner always stands in that
        // one trailing place whether or not the mux is already known, so every scanning
        // card reads as the same thing loading. The mux is accent whenever it is shown:
        // flatten emits it only once confirmed, so a card never shows a mux it is not
        // sure of, and the mux it shows is a settled fact even while its sessions stream.
        if let RowRef::Host {
            unreachable,
            scanning,
            ..
        } = &row.reference
        {
            let (host, mux, _) = context_of(row);
            let pending = Style::default().fg(palette::get().pending);
            // A host-state card's number sits on the host/mux line: the row is a word
            // about the host, not the thing the number names.
            let mut line = address();
            line.push(Span::styled(
                host.to_string(),
                Style::default().fg(color_text()),
            ));
            if *unreachable {
                // The mark rides the host row flush after the host name.
                // Danger keeps its colour: an unreachable host is still a failure, the
                // card just says so with a mark instead of a second row of text.
                line.push(Span::styled(
                    "⚠",
                    Style::default().fg(palette::get().danger),
                ));
            }
            if !mux.is_empty() {
                line.push(Span::styled("/", separator));
                // The mux is confirmed whenever it is shown, so it takes the accent
                // even while the host still scans for its sessions.
                line.push(Span::styled(mux.to_string(), accent));
            }
            if *scanning {
                line.push(Span::styled(format!(" {spinner_glyph}"), pending));
            }
            line.push(Span::raw(" "));
            return vec![Line::from(line)];
        }

        // Session card: the address column + the session name on a single detail line.
        // The `{host}/{mux}` it used to restate now lives on the section title above it,
        // and there is no connector - the title draws the group. The session name is
        // the lowest level the card displays, so it takes the accent and stays bold.
        let (_, _, sess) = context_of(row);
        let mut detail = address();
        detail.push(Span::styled(
            sess.to_string(),
            accent.add_modifier(Modifier::BOLD),
        ));
        detail.push(Span::raw(" "));
        vec![Line::from(detail)]
    }

    fn render_terminal_view(
        &self,
        frame: &mut Frame,
        area: Rect,
        grid: Option<&crate::display::grid::Grid>,
    ) {
        // No border box: the live grid fills the area; render_view_border draws the
        // separating rule.
        match grid {
            Some(g) => {
                let buf = frame.buffer_mut();
                g.render_into(buf, area);
            }
            None => {
                // No confirmed grid yet (only at first launch). Blank, never a
                // placeholder: a session switch keeps the prior grid until the new
                // one is ready (stale-while-revalidate), so nothing transitional is
                // ever shown here.
                frame.render_widget(Clear, area);
            }
        }
    }

    /// Draws the active centered modal popup, shifted by `popup_offset`, through the
    /// shared opaque `render_popup`, and caches its rect for drag hit-testing. Only the
    /// keys help is a popup now: an input renders in the hint bar instead, so its
    /// presence never draws a centered box here.
    fn render_modal_popup(&mut self, frame: &mut Frame, area: Rect, state: &crate::state::State) {
        let Some(Modal::Help) = &state.modal else {
            self.popup_geo.rect = Rect::default();
            return;
        };
        let (title, lines) = modal::help_lines(&state.chrome.ui_prefix);
        let inner_w = lines.iter().map(Line::width).max().unwrap_or(0) as u16;
        // borders + a cell of right padding, at least 24 wide, never past the screen.
        // `.max(24).min(width)` (not `clamp`) so a sub-24-col terminal cannot panic.
        let w = (inner_w + 3).max(24).min(area.width.max(1));
        let h = (lines.len() as u16 + 2).min(area.height.max(1));
        let rect = modal::offset_centered(w, h, area, self.popup_geo.offset);
        self.popup_geo.rect = rect;
        modal::render_popup(frame, area, rect, &title, lines);
    }
}
