use super::*;

use ratatui::style::Modifier;
use ratatui::widgets::{Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState};

use crate::ui::palette;

/// Whether the hint bar floats over the whole window this frame instead of sitting in
/// the nav column.
///
/// Two states make it float, and they are the two where xmux must speak RIGHT NOW: the
/// prefix is armed (the cheatsheet is wanted, and says more than a nav column fits), or
/// a refusal flash is showing while the nav is hidden (there is no nav row to put it in,
/// and a refusal the user cannot see is worse than a row borrowed for a moment).
///
/// Scan progress and the active filter deliberately do NOT float: they persist, and a
/// hidden nav means the user asked for the whole screen to be the mux.
fn hint_bar_floats(nav_width: u16, state: &crate::state::State) -> bool {
    state.chrome.armed || (nav_width == 0 && !state.chrome.flash.is_empty())
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
    /// the session cards over the host-state cards, laid out by [`side::place`] - the one
    /// geometry the paint, the mouse hit-test and the scrollbar all read.
    ///
    /// Card heights vary (two rows expanded, one collapsed - see
    /// [`Switcher::card_collapsed`]), so a card's rect is recorded as it paints rather
    /// than derived a second time from a row pitch. `list_state` carries the settled
    /// scroll position for the next frame to resume from, and the selected card is painted
    /// in the terminal theme's own selected look by inverting its rect, so the card spans
    /// bake in no background of their own.
    fn render_nav_list(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        num_w: usize,
        spinner_glyph: char,
    ) {
        let heights: Vec<u16> = (0..self.rows.len()).map(|i| self.card_height(i)).collect();
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
            let lines = self.nav_row_lines(
                slot.idx,
                num_w,
                spinner_glyph,
                self.card_collapsed(slot.idx),
                self.card_collapsed(slot.idx + 1),
            );
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

    /// The portrait `Top` band's nav: the same cards flowed into columns that fill
    /// downward and continue to the right, each column holding whole host/mux runs.
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
        let placed = columns::place(&cards, band.height);
        let widths = columns::widths(&cards, &placed, band.width);
        // Keep the selected card's column on screen, scrolling the least it takes.
        let sel_col = self
            .list_state
            .selected()
            .and_then(|i| placed.get(i))
            .map_or(0, |p| p.col);
        self.nav_col_offset = columns::scroll_to(
            &widths,
            band.width,
            COL_GUTTER,
            self.nav_col_offset,
            sel_col,
        );
        let cells = columns::cells(&placed, &widths, band, self.nav_col_offset, COL_GUTTER);
        for cell in &cells {
            // The connector hangs a card under the one above it, so it only reads as a
            // sibling while that card is in the SAME column.
            let next_hangs = placed
                .get(cell.idx + 1)
                .is_some_and(|n| n.col == placed[cell.idx].col && n.collapsed);
            let lines =
                self.nav_row_lines(cell.idx, num_w, spinner_glyph, cell.collapsed, next_hangs);
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
        // What the caller needs for the offscreen cue: the cards behind the columns the
        // window does not reach, counted on each side.
        let shown = columns::visible_cols(&widths, band.width, self.nav_col_offset, COL_GUTTER);
        if shown >= widths.len() {
            return None;
        }
        Some(columns::hidden_counts(&placed, self.nav_col_offset, shown))
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

    /// One card measured for the column flow: where a run starts, and how wide each of
    /// its two lines paints. A host-state card reports one width for both lines - it
    /// never drops a line, so a column must reserve room for the wider one.
    fn flow_card(&self, i: usize, num_w: usize, spinner_glyph: char) -> columns::Card {
        let lines = self.nav_row_lines(i, num_w, spinner_glyph, false, false);
        let w = |n: usize| lines.get(n).map_or(0, |l: &Line| l.width() as u16);
        let host_card = matches!(
            self.rows.get(i).map(|r| &r.reference),
            Some(RowRef::Host { .. })
        );
        let (ctx_w, detail_w) = if host_card {
            let m = w(0).max(w(1));
            (m, m)
        } else {
            (w(0), w(1))
        };
        columns::Card {
            starts_run: !self.hangs_under_prev(i),
            ctx_w,
            detail_w,
            lines: if self.is_one_line_host(i) { 1 } else { 2 },
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

    /// How many columns the card numbers need: the digit count of the highest number
    /// in the list. One width for the whole frame, so the names stay aligned with each
    /// other instead of stepping right as the numbers gain a digit, and the numbers
    /// themselves line up by units place.
    fn number_width(&self) -> usize {
        self.rows.len().saturating_sub(1).to_string().len().max(1)
    }

    /// Builds one navigation card as a [`ListItem`]: a context line over a detail
    /// line, or the detail line alone when the card is collapsed (see
    /// [`Switcher::card_collapsed`]). The context line is the address column +
    /// `{host}/{mux}` in the one text colour (the mux segment only when known - a
    /// just-created session is stamped by the next enumeration; just `{host}` on a
    /// host-state card). The detail line is the address column + the session name in
    /// the accent, the one card element that leaves the text colour; a host-state card
    /// is the host/mux name alone on its row, with the unreachable mark (`⚠`) riding
    /// ahead of the host and the mux taking the accent, or a spinner in the level a
    /// scanning host has not resolved. The focused-window part a session card used to
    /// carry is gone, so no card has a second level of content below the session name.
    /// Ahead of both lines runs the ADDRESS column: the card's dim 0-based number, the
    /// thing `prefix <digit>` types. It sits on the DETAIL line, never the context
    /// line, so it reads beside the session it names and a collapsed card (detail line
    /// only) puts it in the same place as an expanded one. A host-state card is the
    /// exception: its number sits beside the host/mux name, because its row is a word
    /// about the host, not the thing the number names.
    ///
    /// On the SELECTED card that column holds the mark instead of a number, because
    /// "you are here" answers the same question the number answers, and one column pays
    /// for both. Every card's name therefore starts at the same screen column whatever
    /// the selection is doing - a name that shifts as the cursor passes is what makes a
    /// list twitch.
    ///
    /// The surface background comes from the List's `highlight_style`, so no per-span
    /// background is baked in here.
    fn nav_row_lines(
        &self,
        i: usize,
        num_w: usize,
        spinner_glyph: char,
        collapsed: bool,
        next_hangs: bool,
    ) -> Vec<Line<'static>> {
        let row = &self.rows[i];
        let selected = self.list_state.selected() == Some(i);
        let accent = Style::default().fg(palette::get().accent);
        let number = Style::default().fg(color_number());
        let separator = Style::default().fg(color_separator());
        let connector = Style::default().fg(color_connector());
        // `numbered` is the detail line, the only line the address column writes on: a
        // context line spends the same width blank so the two stay in one column.
        let address = move |numbered: bool| -> Vec<Span<'static>> {
            if !numbered {
                return vec![Span::raw(" ".repeat(num_w + 1))];
            }
            if selected {
                vec![Span::styled(format!("{SELECTED_MARK:>num_w$} "), accent)]
            } else {
                vec![Span::styled(format!("{i:>num_w$} "), number)]
            }
        };

        // Host-state card: a settled host (reachable empty or unreachable) reads as a
        // single row - the host name over nothing, because the only word a settled host
        // had (`⚠ unreachable`) now rides on the host row itself as a mark. The mark is
        // danger and sits flush after the host name, so the card names
        // WHAT it is (host/mux) while the mark colours its state; the mux - the lowest
        // level the card displays - takes the accent. A card still SCANNING has no
        // settled mux to accent: the spinner stands in the level that has not resolved,
        // in that one level only, and the mux stays text beside it.
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
            let one_line = !*scanning;
            let mut line1 = address(true);
            line1.push(Span::styled(
                host.to_string(),
                Style::default().fg(color_text()),
            ));
            if *unreachable {
                // The mark rides the host row flush after the host name.
                // Danger keeps its colour: an unreachable host is still a failure, the
                // card just says so with a mark instead of a second row of text.
                line1.push(Span::styled(
                    "⚠",
                    Style::default().fg(palette::get().danger),
                ));
            }
            if !mux.is_empty() {
                line1.push(Span::styled("/", separator));
                // A settled host's mux is the lowest level it displays, so it takes the
                // accent (there is no session to take it); a scanning host's mux is
                // context still being pinned down, so it stays text beside the spinner.
                let mux_style = if *scanning {
                    Style::default().fg(color_text())
                } else {
                    accent
                };
                line1.push(Span::styled(mux.to_string(), mux_style));
            } else if *scanning {
                // A source id names its mux only when its machine serves several, so on
                // the rest the mux is genuinely not known yet: the scan stamps it onto
                // the sessions it finds. The spinner sits where that name will land.
                line1.push(Span::styled("/", separator));
                line1.push(Span::styled(spinner_glyph.to_string(), pending));
            }
            line1.push(Span::raw(" "));
            if one_line {
                return vec![Line::from(line1)];
            }
            // A scanning host's second row holds the session-level spinner once its mux
            // is known; with the mux unknown the spinner already stands on row one.
            let mut line2 = address(false);
            if !mux.is_empty() {
                line2.push(Span::styled(spinner_glyph.to_string(), pending));
                line2.push(Span::raw(" "));
            }
            return vec![Line::from(line1), Line::from(line2)];
        }

        // Session / loading card.
        let (host, mux, sess) = context_of(row);
        let mut lines: Vec<Line> = Vec::new();
        if !collapsed {
            let mut context: Vec<Span> = address(false);
            context.push(Span::styled(
                host.to_string(),
                Style::default().fg(color_text()),
            ));
            if !mux.is_empty() {
                context.push(Span::styled("/", separator));
                context.push(Span::styled(
                    mux.to_string(),
                    Style::default().fg(color_text()),
                ));
            }
            context.push(Span::raw(" "));
            lines.push(Line::from(context));
        }
        // The detail line is the session name alone - the focused-window part is gone,
        // so there is no `/window` and no spinner to stand in for it. The session name
        // is the lowest level the card displays, so it takes the accent and stays bold;
        // the connector hangs it under the context line, or under the shared context of
        // the collapsed run above.
        let mut detail = address(true);
        let connector_glyph = if next_hangs { "├ " } else { "└ " };
        detail.push(Span::styled(connector_glyph, connector));
        detail.push(Span::styled(
            sess.to_string(),
            accent.add_modifier(Modifier::BOLD),
        ));
        detail.push(Span::raw(" "));
        lines.push(Line::from(detail));
        lines
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

    /// Draws the active centered modal popup (help / confirm / input) shifted by
    /// `popup_offset`, through the shared opaque `render_popup`, and caches its rect
    /// for drag hit-testing. The single `popup` Option makes these mutually
    /// exclusive.
    fn render_modal_popup(&mut self, frame: &mut Frame, area: Rect, state: &crate::state::State) {
        let (title, lines) = match &state.modal {
            Some(Modal::Help) => modal::help_lines(&state.chrome.ui_prefix),
            Some(Modal::Input(input)) => modal::input_lines(input),
            None => {
                self.popup_geo.rect = Rect::default();
                return;
            }
        };
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
