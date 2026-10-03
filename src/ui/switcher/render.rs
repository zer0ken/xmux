use super::*;

use ratatui::style::Modifier;
use ratatui::widgets::{Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState};

use crate::ui::palette;

/// Where the hint bar actually paints. At rest it is the nav-local rect
/// `compute_regions` derived (empty when the nav is hidden, so the mux keeps every row).
/// Floating, it spans the whole window width. A multi-row bar grows down from a collapsed
/// top nav and up from every other visible edge. Only the paint moves; the layout is
/// untouched, so nothing reflows.
pub(super) fn hint_bar_rect(nav_local: Rect, area: Rect, hint_bar_h: u16, floating: bool) -> Rect {
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
    let h = hint_bar_h.min(area.height);
    let y = if nav_local.y == area.y {
        area.y
    } else {
        nav_local.bottom().saturating_sub(h).max(area.y)
    };
    Rect {
        x: area.x,
        y,
        width: area.width,
        height: h,
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ScrollbarPlan {
    area: Rect,
    content_len: usize,
    position: usize,
    viewport_len: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NavRule {
    Horizontal(Rect),
    Vertical(Rect),
}

/// Immutable geometry for one rendered frame. The app retains the latest plan so paint
/// and mouse input consume the same card, popup, and split-view rectangles.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderPlan {
    pub screen_area: Rect,
    pub layout: ViewLayout,
    pub nav_position: NavPosition,
    pub regions: Regions,
    pub nav_inner: Rect,
    pub nav_cells: Vec<(usize, Rect)>,
    pub nav_row_offset: usize,
    pub nav_col_offset: usize,
    pub popup_rect: Rect,
    hint_bar_rect: Rect,
    pub collapse_button: Rect,
    hidden_counts: Option<(usize, usize)>,
    hidden_counts_rect: Rect,
    connectors: Vec<Rect>,
    nav_rule: Option<NavRule>,
    scrollbar: ScrollbarPlan,
    floating_hint_bar: bool,
    fill_hint_bar_row: bool,
    pub nav_hidden: bool,
    pub nav_collapsed: bool,
}

impl Default for RenderPlan {
    fn default() -> Self {
        Self {
            screen_area: Rect::default(),
            layout: ViewLayout::Column,
            nav_position: NavPosition::Left,
            regions: Regions::default(),
            nav_inner: Rect::default(),
            nav_cells: Vec::new(),
            nav_row_offset: 0,
            nav_col_offset: 0,
            popup_rect: Rect::default(),
            hint_bar_rect: Rect::default(),
            collapse_button: Rect::default(),
            hidden_counts: None,
            hidden_counts_rect: Rect::default(),
            connectors: Vec::new(),
            nav_rule: None,
            scrollbar: ScrollbarPlan::default(),
            floating_hint_bar: false,
            fill_hint_bar_row: false,
            nav_hidden: true,
            nav_collapsed: false,
        }
    }
}

impl Switcher {
    /// Computes the immutable geometry for one frame from the prior frame's scroll
    /// positions. No switcher state is changed.
    pub fn layout(
        &self,
        area: Rect,
        nav: NavSize,
        state: &crate::state::State,
        previous: &RenderPlan,
    ) -> RenderPlan {
        let floating = hint_bar_floats(state);
        let bar_w = if floating { area.width } else { nav.width };
        let hint_bar_h = state.chrome.hint_bar_lines(bar_w, state).len().max(1) as u16;
        let regions = compute_regions(area, nav, hint_bar_h);
        let fill_hint_bar_row = floating || !state.chrome.flash.is_empty();
        let collapse_button = if floating || nav.width == 0 {
            Rect::default()
        } else {
            collapse_button_rect(regions.hint_bar, nav.position, nav.collapsed)
        };
        let paint_button = if nav.collapsed {
            Rect::default()
        } else {
            collapse_button
        };
        let resting_bar = if paint_button.is_empty() {
            regions.hint_bar
        } else {
            Rect {
                width: paint_button.x.saturating_sub(regions.hint_bar.x),
                ..regions.hint_bar
            }
        };
        let hint_bar_rect = hint_bar_rect(resting_bar, area, hint_bar_h, floating);
        let mut plan = RenderPlan {
            screen_area: area,
            layout: regions.layout,
            nav_position: nav.position,
            regions,
            nav_inner: if nav.width == 0 || nav.collapsed {
                Rect::default()
            } else {
                regions.tree
            },
            nav_row_offset: previous.nav_row_offset,
            nav_col_offset: previous.nav_col_offset,
            popup_rect: self.modal_popup_rect(area, state),
            hint_bar_rect,
            collapse_button,
            floating_hint_bar: floating,
            fill_hint_bar_row,
            nav_hidden: nav.width == 0,
            nav_collapsed: nav.collapsed,
            ..RenderPlan::default()
        };
        if !plan.nav_inner.is_empty() {
            self.layout_nav(&mut plan, state);
        }
        if plan.hidden_counts.is_some() {
            let chip = state
                .chrome
                .hint_bar_chip_width(resting_bar.width, state)
                .min(resting_bar.width);
            plan.hidden_counts_rect = Rect {
                x: resting_bar.x + chip,
                width: resting_bar.width - chip,
                height: 1,
                ..resting_bar
            };
        }
        plan
    }

    fn layout_nav(&self, plan: &mut RenderPlan, state: &crate::state::State) {
        let spinner_glyph = crate::ui::spinner_glyph(state.chrome.spinner_frame);
        let num_w = self.number_width();
        match plan.layout {
            ViewLayout::Column => self.layout_nav_list(plan),
            ViewLayout::Band => self.layout_nav_columns(plan, num_w, spinner_glyph),
        }
    }

    fn layout_nav_list(&self, plan: &mut RenderPlan) {
        let heights = vec![1u16; self.painted_rows()];
        let flow = side::place(
            &heights,
            self.painted_boundary(),
            plan.nav_inner.height,
            plan.nav_row_offset,
            self.selected,
            self.selected_section_title(),
        );
        let (cards, bar) = reserve_bar(plan.nav_inner, flow.scrolls, false);
        plan.nav_row_offset = flow.offset;
        plan.nav_cells = flow
            .slots
            .iter()
            .map(|slot| {
                (
                    slot.idx,
                    Rect {
                        x: cards.x,
                        y: cards.y + slot.y,
                        width: cards.width,
                        height: slot.h,
                    },
                )
            })
            .collect();
        plan.nav_rule = flow.rule_y.map(|y| {
            NavRule::Horizontal(Rect {
                x: cards.x,
                y: cards.y + y,
                width: cards.width,
                height: 1,
            })
        });
        if !bar.is_empty() {
            plan.scrollbar = ScrollbarPlan {
                area: bar,
                content_len: self.painted_rows().saturating_sub(flow.visible),
                position: flow.offset,
                viewport_len: flow.visible,
            };
        }
    }

    fn layout_nav_columns(&self, plan: &mut RenderPlan, num_w: usize, spinner_glyph: char) {
        let palette = self.palette;
        let cards: Vec<columns::Card> = (0..self.painted_rows())
            .map(|i| self.flow_card(i, num_w, spinner_glyph, &palette))
            .collect();
        let band = plan.nav_inner;
        let boundary = self.painted_boundary().unwrap_or(cards.len());
        let placed = columns::place(&cards, band.height, boundary);
        let mut home_col = vec![false; cards.len()];
        let mut head_col = None;
        for (i, flag) in home_col.iter_mut().enumerate() {
            if self.starts_run(i) {
                head_col = placed.get(i).map(|p| p.col);
            }
            *flag = matches!((head_col, placed.get(i)), (Some(c), Some(p)) if c == p.col);
        }
        let widths = columns::widths(&cards, &placed, band.width);
        let bcol = columns::boundary_col(&placed, boundary);
        let parting = columns::parting(&widths, bcol, band.width, COL_GUTTER);
        let sel_col = placed.get(self.selected).map_or(0, |p| p.col);
        plan.nav_col_offset = match parting {
            Some(columns::Parting::Gap) => 0,
            Some(columns::Parting::Rule) => {
                let dw = columns::display_widths(&widths, bcol, columns::Parting::Rule);
                let sel = columns::display_col(sel_col, bcol, parting);
                columns::scroll_to(&dw, band.width, COL_GUTTER, plan.nav_col_offset, sel)
            }
            None => columns::scroll_to(
                &widths,
                band.width,
                COL_GUTTER,
                plan.nav_col_offset,
                sel_col,
            ),
        };
        let (cells, rule) = columns::cells(
            &placed,
            &widths,
            bcol,
            parting,
            band,
            plan.nav_col_offset,
            COL_GUTTER,
        );
        for cell in cells {
            let indent = if self.starts_run(cell.idx) {
                0
            } else {
                CONNECTOR_W
            };
            if indent > 0 && home_col[cell.idx] {
                plan.connectors.push(cell.rect);
            }
            plan.nav_cells.push((
                cell.idx,
                Rect {
                    x: cell.rect.x + indent,
                    width: cell.rect.width.saturating_sub(indent),
                    ..cell.rect
                },
            ));
        }
        plan.nav_rule = rule.map(NavRule::Vertical);
        let (shown, n) = match parting {
            Some(columns::Parting::Gap) => (widths.len(), widths.len()),
            Some(columns::Parting::Rule) => {
                let dw = columns::display_widths(&widths, bcol, columns::Parting::Rule);
                (
                    columns::visible_cols(&dw, band.width, plan.nav_col_offset, COL_GUTTER),
                    dw.len(),
                )
            }
            None => (
                columns::visible_cols(&widths, band.width, plan.nav_col_offset, COL_GUTTER),
                widths.len(),
            ),
        };
        if shown < n {
            plan.hidden_counts = Some(columns::hidden_counts(
                &placed,
                bcol,
                parting,
                plan.nav_col_offset,
                shown,
            ));
        }
    }

    pub fn render(
        &self,
        frame: &mut Frame,
        grid: Option<&crate::display::grid::Grid>,
        terminal_focused: bool,
        state: &crate::state::State,
        plan: &RenderPlan,
    ) {
        let area = plan.screen_area;
        let palette = self.palette;
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
        if plan.nav_hidden {
            self.render_terminal_view(frame, area, grid);
            if let Some(g) = grid {
                if !g.hide_cursor() {
                    frame.set_cursor_position(terminal_cursor_pos(area, g.cursor()));
                }
            }
            // The bar still floats for the states that must be seen even here: an armed
            // prefix, open input, or refusal flash. Hiding the nav hides the status line,
            // not xmux's ability to answer a keypress.
            if plan.floating_hint_bar {
                state.chrome.render_hint_bar(
                    frame,
                    plan.hint_bar_rect,
                    state,
                    crate::ui::chrome::BarFill::Row,
                    &palette,
                );
            }
            // The modal stacks above the bar: a popup is a stronger claim on the screen.
            self.render_modal_popup(frame, area, state, plan.popup_rect, &palette);
            return;
        }
        // One geometry source for the whole frame (compute_regions), shared with the PTY
        // sizing and mouse hit-testing so they never diverge: the nav list / terminal split
        // side by side (Column) or stacked (Band), parted by the view
        // border, and the hint bar takes the nav's bottom rows. The hint bar is normally one
        // row; a long flash wraps, so size it to the wrapped line count (never clipped).
        // Measured at the width it will RENDER at: the nav column normally, the whole
        // window whenever the bar floats (see `hint_bar_floats` / `hint_bar_rect`).
        self.render_nav(frame, state, plan, &palette);
        // The view border marks focus between the two views (vertical in a column, horizontal in a band).
        state
            .chrome
            .render_view_border(frame, plan.regions.view_border, terminal_focused);
        let term_area = plan.regions.terminal;
        // A selected host with no session to show has no live grid to mirror: its host
        // screen fills the region instead, so neither state is ever a blank view with no
        // next step. One call for both, because they are one screen in two states.
        if let Some(kind) = self.current_view_screen(state) {
            let address = self.view_screen_address(state, kind);
            state.chrome.render_view_screen(
                frame,
                term_area,
                state,
                crate::ui::chrome::ViewScreenRender {
                    address: &address,
                    kind,
                    focused: terminal_focused,
                },
                &palette,
            );
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
        let fill = if plan.fill_hint_bar_row {
            crate::ui::chrome::BarFill::Row
        } else {
            crate::ui::chrome::BarFill::Content
        };
        if let (Some(counts), crate::ui::chrome::BarFill::Content) = (plan.hidden_counts, fill) {
            Self::render_hidden_counts(frame, plan.hidden_counts_rect, counts, &palette);
        }
        if plan.nav_collapsed && !plan.floating_hint_bar {
            state.chrome.render_collapsed_hint_bar(
                frame,
                plan.hint_bar_rect,
                plan.nav_position,
                &palette,
            );
        } else {
            state
                .chrome
                .render_hint_bar(frame, plan.hint_bar_rect, state, fill, &palette);
            if !plan.collapse_button.is_empty() {
                state.chrome.render_collapse_button(
                    frame,
                    plan.regions.hint_bar,
                    plan.nav_position,
                    false,
                    &palette,
                );
            }
        }
        // In the terminal view, place the real cursor at the grid's cursor so typing in the
        // mux is visible and tracks. Skipped when the child hid its cursor.
        if terminal_focused {
            if let Some(g) = grid {
                if !g.hide_cursor() {
                    frame.set_cursor_position(terminal_cursor_pos(term_area, g.cursor()));
                }
            }
        }
        self.render_modal_popup(frame, area, state, plan.popup_rect, &palette);
    }

    #[cfg(test)]
    pub(crate) fn render_test(
        &self,
        frame: &mut Frame,
        grid: Option<&crate::display::grid::Grid>,
        terminal_focused: bool,
        nav: NavSize,
        state: &crate::state::State,
    ) {
        let plan = self.layout(frame.area(), nav, state, &RenderPlan::default());
        self.render(frame, grid, terminal_focused, state, &plan);
    }

    /// The navigation cards. A column stacks them in one vertically-scrolling list; a
    /// band flows them into columns (see [`columns`]), because a wide,
    /// short region shows three cards as a list and twenty as a grid.
    ///
    /// Either way a scrollbar, when one is needed, gets its own row or column of the
    /// region rather than sitting over the cards: the selected card is painted by
    /// inverting its whole rect, and a thumb inside that rect inverts with it into a
    /// hole in the bar.
    fn render_nav(
        &self,
        frame: &mut Frame,
        state: &crate::state::State,
        plan: &RenderPlan,
        palette: &palette::Palette,
    ) {
        let spinner_glyph = crate::ui::spinner_glyph(state.chrome.spinner_frame);
        let num_w = self.number_width();
        for rect in &plan.connectors {
            Self::render_card_connector(frame, *rect, palette);
        }
        for &(idx, rect) in &plan.nav_cells {
            let lines =
                self.nav_row_lines(idx, num_w, spinner_glyph, rect.width, plan.layout, palette);
            frame.render_widget(Paragraph::new(lines), rect);
            if self.selected == idx {
                frame
                    .buffer_mut()
                    .set_style(rect, palette::selection_style(palette));
            }
        }
        match plan.nav_rule {
            Some(NavRule::Horizontal(rect)) => Self::render_band_rule(frame, rect, palette),
            Some(NavRule::Vertical(rect)) => Self::render_column_rule(frame, rect, palette),
            None => {}
        }
        self.render_nav_scrollbar(frame, plan.scrollbar, palette);
    }

    /// The rule parting the side list's two bands once they scroll as one run. A single
    /// light horizontal line across the nav: it says the cards below it are a different
    /// kind of thing, which is all the blank gap says while both bands fit on screen.
    fn render_band_rule(frame: &mut Frame, rect: Rect, palette: &palette::Palette) {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                BAND_RULE.repeat(rect.width as usize),
                Style::default().fg(palette.decoration),
            ))),
            rect,
        );
    }

    /// The vertical rule parting the two bands in the portrait flow once they cannot
    /// stay apart by a gap. A single light vertical line across the band, the same
    /// statement the side list's horizontal rule makes.
    fn render_column_rule(frame: &mut Frame, rect: Rect, palette: &palette::Palette) {
        let style = Style::default().fg(palette.decoration);
        let buf = frame.buffer_mut();
        for y in rect.y..rect.y + rect.height {
            let cell = &mut buf[(rect.x, y)];
            cell.set_symbol("│");
            cell.set_style(style);
        }
    }

    /// The connector down the left of one session card, marking the title that owns it.
    /// Painted OUTSIDE the card, in the strip the column reserves for it, so the
    /// selection's inversion of the card rect cannot reach it.
    fn render_card_connector(frame: &mut Frame, rect: Rect, palette: &palette::Palette) {
        let cell = &mut frame.buffer_mut()[(rect.x, rect.y)];
        cell.set_symbol(CARD_CONNECTOR);
        cell.set_style(Style::default().fg(palette.decoration));
    }

    /// Writes the offscreen-card counts in `track` - the hint bar's row minus the cells the
    /// bar's own label takes - at the ends the hidden columns are behind: `<< 5 more` on the
    /// left, `7 more >>` on the right. The arrows point the way the cards went, and the
    /// count says how many, which a scrollbar thumb cannot. Dropped, not clipped, when the
    /// row is too narrow to hold them.
    fn render_hidden_counts(
        frame: &mut Frame,
        track: Rect,
        (left, right): (usize, usize),
        palette: &palette::Palette,
    ) {
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
        let more_style = Style::default().fg(palette.decoration);
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
    fn render_nav_scrollbar(
        &self,
        frame: &mut Frame,
        plan: ScrollbarPlan,
        palette: &palette::Palette,
    ) {
        if plan.area.is_empty() {
            return;
        }
        let mut sb = ScrollbarState::new(plan.content_len)
            .position(plan.position)
            .viewport_content_length(plan.viewport_len);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .track_symbol(None)
                .thumb_symbol("▐")
                .thumb_style(Style::default().fg(palette.decoration)),
            plan.area,
            &mut sb,
        );
    }

    /// How many columns the card numbers need: the digit count of the highest card
    /// number. One width for the whole frame, so the names stay aligned with each
    /// other instead of stepping right as the numbers gain a digit, and the numbers
    /// themselves line up by units place. Section titles carry no number, so the width
    /// counts the SELECTABLE cards only.
    fn number_width(&self) -> usize {
        self.selectable_count().to_string().len().max(1)
    }

    /// One row measured for the column flow: whether it opens a unit, how wide its
    /// content paints, and how many rows it takes. A section title measures its
    /// `{host}/{mux}` alone, which is the whole of what it paints in the band: the
    /// trailing rule belongs to the side list.
    fn flow_card(
        &self,
        i: usize,
        num_w: usize,
        spinner_glyph: char,
        palette: &palette::Palette,
    ) -> columns::Card {
        let lines = self.nav_row_lines(i, num_w, spinner_glyph, 0, ViewLayout::Band, palette);
        let w = |n: usize| lines.get(n).map_or(0, |l: &Line| l.width() as u16);
        let starts_run = self.starts_run(i);
        // A session card is pushed right by the connector's strip, so the column has to
        // be wide enough for both. The strip is reserved whether or not the glyph is
        // painted there: the widths are measured before the flow decides columns, so a
        // card that reserved nothing could not be given the strip afterwards, and every
        // card of a section reads at one offset inside its column wherever it landed.
        let indent = if starts_run { 0 } else { CONNECTOR_W };
        columns::Card {
            starts_run,
            width: w(0) + indent,
            lines: 1,
        }
    }

    /// Builds one navigation row's lines. A session card is the address column + the
    /// session name on a single detail line; a section title is the `{host}/{mux}`
    /// header (dim, with a rule filling the row's width in the side list) and carries
    /// no address column;
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
        layout: ViewLayout,
        palette: &palette::Palette,
    ) -> Vec<Line<'static>> {
        let row = &self.rows[i];
        let selected = self.selected == i;
        let accent = Style::default().fg(palette.accent);
        let number = Style::default().fg(palette.decoration);
        let separator = Style::default().fg(palette.decoration);
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

        // Section title: `{host}/{mux}` in the quiet header role. Not a card - no
        // number, not selectable, and the selection can never land on it.
        //
        // A rule fills the rest of the row in the SIDE list only, where the nav is one
        // full-width run and the rule reads as the group's own underline. The portrait
        // band's columns are each only as wide as their widest card and stand side by
        // side, so a rule there would run into the gutter and read as a bar parting the
        // columns rather than as anything about the group: the title stands alone.
        if let RowRef::Section { .. } = &row.reference {
            let (host, mux, _) = context_of(row);
            let header = Style::default().fg(palette.secondary);
            let title = if mux.is_empty() {
                host.to_string()
            } else {
                format!("{host}/{mux}")
            };
            let title_w = UnicodeWidthStr::width(title.as_str()) as u16;
            let rule_w = match layout {
                ViewLayout::Column => width.saturating_sub(title_w.saturating_add(1)),
                ViewLayout::Band => 0,
            };
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
            blocked,
            scanning,
            ..
        } = &row.reference
        {
            let (host, mux, _) = context_of(row);
            let pending = Style::default().fg(palette.warning);
            // A host-state card's number sits on the host/mux line: the row is a word
            // about the host, not the thing the number names.
            let mut line = address();
            line.push(Span::styled(
                host.to_string(),
                Style::default().fg(palette.secondary),
            ));
            if *blocked {
                // The block mark rides the host row flush after the host name. A blocked
                // host is a failure the user can act on, so it keeps the warning colour
                // like the unreachable mark.
                line.push(Span::styled(
                    crate::ui::chrome::BLOCK_MARK,
                    Style::default().fg(palette.warning),
                ));
            } else if *unreachable {
                // The mark rides the host row flush after the host name.
                // Danger keeps its colour: an unreachable host is still a failure, the
                // card just says so with a mark instead of a second row of text.
                line.push(Span::styled("⚠", Style::default().fg(palette.warning)));
            }
            if !mux.is_empty() {
                line.push(Span::styled("/", separator));
                // The mux is confirmed whenever it is shown, so it stays with the host:
                // both halves of the group identity read in secondary even while the host
                // still scans for its sessions.
                line.push(Span::styled(
                    mux.to_string(),
                    Style::default().fg(palette.secondary),
                ));
            }
            if *scanning {
                line.push(Span::styled(format!(" {spinner_glyph}"), pending));
            }
            line.push(Span::raw(" "));
            return vec![Line::from(line)];
        }

        // Session card: the address column + the session name on a single detail line.
        // The `{host}/{mux}` it used to restate now lives on the section title above it.
        // The session name is the lowest level the card displays, so it takes the accent
        // and stays bold.
        //
        // The connector the portrait band draws down a session card's left is NOT part
        // of the card and is painted separately; what a card holds is what a card holds
        // in either layout.
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

    fn modal_popup_rect(&self, area: Rect, state: &crate::state::State) -> Rect {
        let Some(Modal::Help) = &state.modal else {
            return Rect::default();
        };
        let (_, lines) = modal::help_lines(
            &state.chrome.ui_prefix,
            state.chrome.nav_position,
            &self.palette,
        );
        let inner_w = lines.iter().map(Line::width).max().unwrap_or(0) as u16;
        let w = (inner_w + 3).max(24).min(area.width.max(1));
        let h = (lines.len() as u16 + 2).min(area.height.max(1));
        modal::offset_centered(w, h, area, self.popup_geo.offset)
    }

    /// Draws the active modal at the rectangle supplied by the frame's plan.
    fn render_modal_popup(
        &self,
        frame: &mut Frame,
        area: Rect,
        state: &crate::state::State,
        rect: Rect,
        palette: &palette::Palette,
    ) {
        let Some(Modal::Help) = &state.modal else {
            return;
        };
        let (title, lines) =
            modal::help_lines(&state.chrome.ui_prefix, state.chrome.nav_position, palette);
        modal::render_popup(frame, area, rect, &title, lines, palette);
    }
}
