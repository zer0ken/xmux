use super::*;

use ratatui::style::Modifier;
use ratatui::widgets::Paragraph;

use crate::ui::palette;

/// Where the hint bar actually paints. At rest it is the prefix indicator's rect: a
/// column's bottom row, or the right end of a band's view border row (empty when the nav
/// is hidden, so the mux keeps every row).
///
/// Floating, it opens from the indicator toward the terminal view and leaves the
/// indicator itself in place: across the terminal view's columns on a side column's
/// bottom row, on the rows below a top band's seam, and on the rows above a bottom band's
/// seam. A multi-row bar grows away from the indicator. With the nav hidden there is no
/// indicator, so it borrows the window's bottom rows. Only the paint moves; the layout is
/// untouched, so nothing reflows.
pub(super) fn hint_bar_rect(
    indicator: Rect,
    terminal: Rect,
    area: Rect,
    hint_bar_h: u16,
    floating: bool,
    position: NavPosition,
) -> Rect {
    if !floating {
        return indicator;
    }
    let h = hint_bar_h.min(area.height);
    if indicator.height == 0 {
        // Nav hidden: no row was reserved, so borrow the window's bottom rows.
        return Rect {
            x: area.x,
            y: area.y + area.height - h,
            width: area.width,
            height: h,
        };
    }
    match position {
        NavPosition::Left | NavPosition::Right => Rect {
            x: terminal.x,
            y: indicator.bottom().saturating_sub(h).max(area.y),
            width: terminal.width,
            height: h,
        },
        NavPosition::Top => Rect {
            x: area.x,
            y: indicator.bottom().min(area.bottom() - h),
            width: area.width,
            height: h,
        },
        NavPosition::Bottom => Rect {
            x: area.x,
            y: indicator.y.saturating_sub(h).max(area.y),
            width: area.width,
            height: h,
        },
    }
}

/// The glyph marking the SELECTED card, in its address column.
///
/// A SHAPE, never a solid block. The selected card is reverse video, which swaps that
/// cell's own pair, so a filled block inverts into a background-coloured half-cell and is
/// absorbed into the inverted row's left edge - the mark vanishes exactly where it is
/// needed. An outline keeps its silhouette either way round.
pub(crate) const SELECTED_MARK: &str = "\u{276f}";
pub(crate) const MIDDLE_ELLIPSIS: char = '…';

const MIN_SCREEN_WIDTH: u16 = 24;
const MIN_SCREEN_HEIGHT: u16 = 4;

struct NavRowPaint<'a> {
    width: u16,
    filter: &'a str,
    palette: &'a palette::Palette,
    reserve_state_word: bool,
}

fn middle_ellipsize(text: &str, width: usize) -> String {
    if UnicodeWidthStr::width(text) <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    if width == 1 {
        return MIDDLE_ELLIPSIS.into();
    }
    let chars: Vec<char> = text.chars().collect();
    let front_budget = (width - 1).div_ceil(2);
    let back_budget = width - 1 - front_budget;
    let mut front = String::new();
    let mut used = 0;
    for ch in &chars {
        let cw = unicode_width::UnicodeWidthChar::width(*ch).unwrap_or(0);
        if used + cw > front_budget {
            break;
        }
        front.push(*ch);
        used += cw;
    }
    let mut back = String::new();
    let mut used = 0;
    for ch in chars.iter().rev() {
        let cw = unicode_width::UnicodeWidthChar::width(*ch).unwrap_or(0);
        if used + cw > back_budget {
            break;
        }
        back.insert(0, *ch);
        used += cw;
    }
    format!("{front}{MIDDLE_ELLIPSIS}{back}")
}

fn remaining_filter(prefix: &str, filter: &str) -> String {
    let lower_filter = filter.to_lowercase();
    let mut wanted = lower_filter.chars().peekable();
    for ch in prefix.chars() {
        if wanted
            .peek()
            .is_some_and(|next| ch.to_lowercase().next() == Some(*next))
        {
            wanted.next();
        }
    }
    wanted.collect()
}

fn highlighted(text: String, filter: &str, style: Style) -> Vec<Span<'static>> {
    if filter.is_empty() {
        return vec![Span::styled(text, style)];
    }
    let lower_filter = filter.to_lowercase();
    let mut wanted = lower_filter.chars().peekable();
    text.chars()
        .map(|ch| {
            let matched = wanted
                .peek()
                .is_some_and(|next| ch.to_lowercase().next() == Some(*next));
            if matched {
                wanted.next();
            }
            Span::styled(
                ch.to_string(),
                if matched {
                    style.add_modifier(Modifier::BOLD)
                } else {
                    style
                },
            )
        })
        .collect()
}

/// The thick segment of a side nav's seam: where the cards on screen sit in the whole
/// list, as a scrollbar thumb would, drawn on the seam rather than in a column of its own,
/// so the cards keep the nav's full width. Counted in cards over the placement the cards
/// were painted with. Empty when everything fits.
fn seam_thumb(track: Rect, total: usize, offset: usize, visible: usize) -> Rect {
    if track.height == 0 || total == 0 || visible >= total {
        return Rect::default();
    }
    let t = track.height as usize;
    let len = (t * visible / total).clamp(1, t);
    let y = if offset + visible >= total {
        t - len
    } else {
        (t * offset / total).min(t - len)
    };
    Rect {
        y: track.y + y as u16,
        height: len as u16,
        ..track
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NavRule {
    Horizontal(Rect),
    Vertical(Rect),
}

/// A band's count of the cards scrolled off one side, written on the seam: `‹ 5` at the
/// left end and `7 ›` at the right end, before the prefix. `target` is the hidden card
/// nearest the visible ones, which a click on the count selects so the band scrolls to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct OverflowMark {
    rect: Rect,
    count: usize,
    left: bool,
    target: Option<usize>,
}

impl OverflowMark {
    fn width(count: usize) -> u16 {
        count.to_string().len() as u16 + 2
    }
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
    /// Where the prefix indicator keeps the prefix while the bar floats away from it;
    /// empty while the bar rests or the nav is hidden.
    prefix_label: Rect,
    /// Each toast on screen and the rect it floats in, newest first. A click inside one
    /// takes it down.
    pub(crate) toasts: Vec<(u64, Rect)>,
    /// The cells a click on a collapsed nav expands it from: the whole collapsed column
    /// with its seam, or a collapsed band's seam row. Empty while the nav is expanded.
    pub expand_area: Rect,
    overflow_marks: Vec<OverflowMark>,
    /// The repeated title on the top row of each band column that continues a section,
    /// paired with the title's row index.
    title_repeats: Vec<(usize, Rect)>,
    nav_rule: Option<NavRule>,
    pub(super) seam_thumb: Rect,
    floating_hint_bar: bool,
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
            prefix_label: Rect::default(),
            toasts: Vec::new(),
            expand_area: Rect::default(),
            overflow_marks: Vec::new(),
            title_repeats: Vec::new(),
            nav_rule: None,
            seam_thumb: Rect::default(),
            floating_hint_bar: false,
            nav_hidden: true,
            nav_collapsed: false,
        }
    }
}

impl RenderPlan {
    /// The toast a click at `(col, row)` lands on, if any.
    pub(crate) fn toast_at(&self, col: u16, row: u16) -> Option<u64> {
        let at = Position { x: col, y: row };
        self.toasts
            .iter()
            .find(|(_, rect)| rect.contains(at))
            .map(|(id, _)| *id)
    }

    /// The card a click at `(col, row)` on a band's overflow count selects.
    pub(crate) fn overflow_target(&self, col: u16, row: u16) -> Option<usize> {
        let at = Position { x: col, y: row };
        self.overflow_marks
            .iter()
            .find(|m| m.rect.contains(at))
            .and_then(|m| m.target)
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
        let band = nav.position.layout() == ViewLayout::Band;
        // The resting indicator is one row, so the layout is cut for one row whatever the
        // bar says: a floating bar only paints further, it never takes a row from the nav.
        let regions = compute_regions(area, nav, 1);
        let bar_w = if !floating {
            nav.width
        } else if band || nav.width == 0 || regions.terminal.width == 0 {
            area.width
        } else {
            regions.terminal.width
        };
        let hint_bar_h = state.chrome.hint_bar_lines(bar_w, state).len().max(1) as u16;
        // At rest the prefix indicator is a label on the column's bottom row, and the right
        // end of the seam row in a band. While the bar floats away from it, the indicator
        // keeps the prefix alone.
        let prefix_w = collapsed_nav_width(&state.chrome.ui_prefix);
        let resting_bar = if band && !regions.hint_bar.is_empty() {
            let chip = if nav.collapsed || floating {
                prefix_w
            } else {
                state
                    .chrome
                    .hint_bar_chip_width(regions.hint_bar.width, state)
            }
            .min(regions.hint_bar.width);
            Rect {
                x: regions.hint_bar.right() - chip,
                width: chip,
                ..regions.hint_bar
            }
        } else {
            regions.hint_bar
        };
        let prefix_label = if floating && !regions.hint_bar.is_empty() {
            Rect {
                width: prefix_w.min(resting_bar.width),
                ..resting_bar
            }
        } else {
            Rect::default()
        };
        let hint_bar_rect = hint_bar_rect(
            resting_bar,
            regions.terminal,
            area,
            hint_bar_h,
            floating,
            nav.position,
        );
        let seam = regions.view_border;
        let expand_area = if nav.collapsed && nav.width > 0 {
            match nav.position {
                NavPosition::Left => Rect {
                    width: seam.right().saturating_sub(area.x),
                    ..area
                },
                NavPosition::Right => Rect {
                    x: seam.x,
                    width: area.right().saturating_sub(seam.x),
                    ..area
                },
                NavPosition::Top | NavPosition::Bottom => seam,
            }
        } else {
            Rect::default()
        };
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
            prefix_label,
            toasts: crate::ui::toast::place_toasts(
                &state.notify,
                regions.terminal,
                area,
                nav.position,
            ),
            expand_area,
            floating_hint_bar: floating,
            nav_hidden: nav.width == 0,
            nav_collapsed: nav.collapsed,
            ..RenderPlan::default()
        };
        if !plan.nav_inner.is_empty() {
            // The band's overflow counts share the seam row with the prefix, so they get
            // what the prefix leaves. A floating bar opens off the seam and leaves them be.
            let track = if band {
                Rect {
                    width: resting_bar.x.saturating_sub(seam.x),
                    ..seam
                }
            } else {
                Rect::default()
            };
            self.layout_nav(&mut plan, state, track);
        }
        plan
    }

    fn layout_nav(&self, plan: &mut RenderPlan, state: &crate::state::State, track: Rect) {
        let spinner_glyph = crate::ui::spinner_glyph(state.chrome.spinner_frame);
        let num_w = self.number_width();
        match plan.layout {
            ViewLayout::Column => self.layout_nav_list(plan),
            ViewLayout::Band => self.layout_nav_columns(plan, num_w, spinner_glyph, track),
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
        let cards = plan.nav_inner;
        plan.nav_row_offset = flow.offset;
        plan.nav_cells = flow
            .slots
            .iter()
            .map(|slot| {
                let indent = if self.starts_run(slot.idx) {
                    0
                } else {
                    CARD_INDENT
                };
                (
                    slot.idx,
                    Rect {
                        x: cards.x + indent,
                        y: cards.y + slot.y,
                        width: cards.width.saturating_sub(indent),
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
        if flow.scrolls {
            let seam = plan.regions.view_border;
            plan.seam_thumb = seam_thumb(
                Rect {
                    x: seam.x,
                    y: cards.y,
                    width: 1,
                    height: cards.height,
                },
                self.painted_rows(),
                flow.offset,
                flow.visible,
            );
        }
    }

    fn layout_nav_columns(
        &self,
        plan: &mut RenderPlan,
        num_w: usize,
        spinner_glyph: char,
        track: Rect,
    ) {
        let palette = self.palette;
        let band = plan.nav_inner;
        let indent = if band.height == 1 { 0 } else { CARD_INDENT };
        let cards: Vec<columns::Card> = (0..self.painted_rows())
            .map(|i| self.flow_card(i, num_w, spinner_glyph, &palette, indent))
            .collect();
        let boundary = self.painted_boundary().unwrap_or(cards.len());
        let placed = columns::place(&cards, band.height, boundary);
        let continuations = columns::continuations(&cards, &placed);
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
            let p = &placed[cell.idx];
            if p.y == 1 {
                if let Some(&(_, title)) = continuations.iter().find(|(c, _)| *c == p.col) {
                    plan.title_repeats.push((
                        title,
                        Rect {
                            y: band.y,
                            height: 1,
                            ..cell.rect
                        },
                    ));
                }
            }
            let indent = if self.starts_run(cell.idx) { 0 } else { indent };
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
        if shown >= n || track.is_empty() {
            return;
        }
        let first = plan.nav_col_offset;
        let selectable = |i: &usize| self.rows.get(*i).is_some_and(Row::selectable);
        let (left, right) =
            columns::hidden_counts(&placed, bcol, parting, first, shown, |i| selectable(&i));
        let dcol = |i: usize| columns::display_col(placed[i].col, bcol, parting);

        // Each count sits a cell in from its end of the track, the right one clear of the
        // prefix, and is dropped rather than clipped when the track cannot hold it.
        let mut used = 1u16;
        if left > 0 {
            let w = OverflowMark::width(left);
            if used + w < track.width {
                plan.overflow_marks.push(OverflowMark {
                    rect: Rect {
                        x: track.x + used,
                        width: w,
                        ..track
                    },
                    count: left,
                    left: true,
                    target: (0..placed.len())
                        .filter(|&i| dcol(i) < first)
                        .filter(selectable)
                        .max(),
                });
                used += w + 1;
            }
        }
        if right > 0 {
            let w = OverflowMark::width(right);
            if used + w < track.width {
                plan.overflow_marks.push(OverflowMark {
                    rect: Rect {
                        x: track.right() - 1 - w,
                        width: w,
                        ..track
                    },
                    count: right,
                    left: false,
                    target: (0..placed.len())
                        .filter(|&i| dcol(i) >= first + shown)
                        .find(selectable),
                });
            }
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
        if area.width < MIN_SCREEN_WIDTH || area.height < MIN_SCREEN_HEIGHT {
            frame.render_widget(
                Paragraph::new(vec![
                    Line::from(format!("xmux need {MIN_SCREEN_WIDTH}x{MIN_SCREEN_HEIGHT}")),
                    Line::from(format!("current {}x{}", area.width, area.height)),
                ]),
                area,
            );
            return;
        }
        // nav_width == 0 is the "nav hidden" sentinel (terminal view focused + auto-hide):
        // the terminal view owns the whole area - no nav list, no view border, and no
        // prefix indicator of its own, since the user asked for the whole screen to be the mux.
        if plan.nav_hidden {
            self.render_terminal_view(frame, area, grid);
            if let Some(g) = grid {
                if !g.hide_cursor() {
                    frame.set_cursor_position(terminal_cursor_pos(area, g.cursor()));
                }
            }
            // The bar still floats for the states that must be seen even here: an armed
            // prefix, open input, or refusal flash. Hiding the nav hides the prefix indicator,
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
            self.render_toasts(frame, state, plan, &palette);
            // The modal stacks above the bar: a popup is a stronger claim on the screen.
            self.render_modal_popup(frame, area, state, plan.popup_rect, &palette);
            return;
        }
        // One geometry source for the whole frame (compute_regions), shared with the PTY
        // sizing and mouse hit-testing so they never diverge: the nav list / terminal split
        // side by side (Column) or stacked (Band), parted by the view
        // border, and the hint bar rests on a column's bottom row or a band's view border
        // row. The hint bar is normally one row; a long flash wraps, so size it to the
        // wrapped line count (never clipped). Measured at the width it will RENDER at: the
        // nav column at rest in a column, and once the bar floats the terminal view's width
        // beside a column or the whole window across a band (see `hint_bar_floats` /
        // `hint_bar_rect`).
        self.render_nav(frame, state, plan, &palette, terminal_focused);
        // The seam is the one line the nav draws: its colour says which view holds the
        // focus, and a side nav's overflow thickens the stretch beside the cards on screen.
        state
            .chrome
            .render_view_border(frame, plan.regions.view_border, terminal_focused);
        state
            .chrome
            .render_seam_thumb(frame, plan.seam_thumb, terminal_focused);
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
        // terminal view. At rest it is the prefix indicator, a label sized to what it says
        // on the column's bottom row or at the right end of a band's seam; floating, it
        // opens from there toward the terminal view while the indicator keeps the prefix -
        // the layout never reflows, only the paint reaches further, so arming the prefix
        // cannot shift a single card. A band's overflow counts share the seam with the
        // indicator.
        for mark in &plan.overflow_marks {
            Self::render_overflow_mark(frame, *mark, &palette);
        }
        if plan.floating_hint_bar {
            state
                .chrome
                .render_collapsed_hint_bar(frame, plan.prefix_label, &palette);
            state.chrome.render_hint_bar(
                frame,
                plan.hint_bar_rect,
                state,
                crate::ui::chrome::BarFill::Row,
                &palette,
            );
        } else if plan.nav_collapsed {
            state
                .chrome
                .render_collapsed_hint_bar(frame, plan.hint_bar_rect, &palette);
        } else {
            state.chrome.render_hint_bar(
                frame,
                plan.hint_bar_rect,
                state,
                crate::ui::chrome::BarFill::Content,
                &palette,
            );
        }
        self.render_toasts(frame, state, plan, &palette);
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
    /// Nothing but cards, titles and the band parting is painted inside the nav: what is
    /// off screen is said on the seam. The selected card is reverse video while the nav
    /// holds the focus and keeps only its mark while the terminal does, so the selection
    /// and the seam colour say the same thing about the focus.
    fn render_nav(
        &self,
        frame: &mut Frame,
        state: &crate::state::State,
        plan: &RenderPlan,
        palette: &palette::Palette,
        terminal_focused: bool,
    ) {
        let spinner_glyph = crate::ui::spinner_glyph(state.chrome.spinner_frame);
        let num_w = self.number_width();
        let dim = Style::default().fg(palette.decoration);
        for &(title, rect) in &plan.title_repeats {
            let room = (rect.width as usize).saturating_sub(CONTINUED.chars().count() + 1);
            let text = format!(
                "{}{CONTINUED}",
                middle_ellipsize(&self.section_title(title), room)
            );
            frame.render_widget(Paragraph::new(Line::from(Span::styled(text, dim))), rect);
        }
        for &(idx, rect) in &plan.nav_cells {
            let lines = self.nav_row_lines(
                idx,
                num_w,
                spinner_glyph,
                NavRowPaint {
                    width: rect.width,
                    filter: &state.filter,
                    palette,
                    reserve_state_word: false,
                },
            );
            frame.render_widget(Paragraph::new(lines), rect);
            if self.selected == idx && !terminal_focused {
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

    /// Writes one overflow count on the band's seam: `‹ 5` or `7 ›`, the angle pointing
    /// the way the cards went and the count, the thing a user reaches for, bold.
    fn render_overflow_mark(frame: &mut Frame, mark: OverflowMark, palette: &palette::Palette) {
        let style = Style::default().fg(palette.decoration);
        let bold = style.add_modifier(Modifier::BOLD);
        let n = mark.count.to_string();
        let spans = if mark.left {
            vec![Span::styled("\u{2039} ", style), Span::styled(n, bold)]
        } else {
            vec![Span::styled(n, bold), Span::styled(" \u{203a}", style)]
        };
        frame.render_widget(Paragraph::new(Line::from(spans)), mark.rect);
    }

    /// A section title's `{host}/{mux}`, or the host alone when no mux is confirmed.
    fn section_title(&self, i: usize) -> String {
        let (host, mux, _) = context_of(&self.rows[i]);
        if mux.is_empty() {
            host.to_string()
        } else {
            format!("{host}/{mux}")
        }
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
        indent: u16,
    ) -> columns::Card {
        let lines = self.nav_row_lines(
            i,
            num_w,
            spinner_glyph,
            NavRowPaint {
                width: 0,
                filter: "",
                palette,
                reserve_state_word: true,
            },
        );
        let w = |n: usize| lines.get(n).map_or(0, |l: &Line| l.width() as u16);
        let starts_run = self.starts_run(i);
        // A session card is indented under its title, so the column has to be wide
        // enough for both.
        let indent = if starts_run { 0 } else { indent };
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
    /// a host-state card is the host/mux name on its row, with its state glyph in a
    /// fixed slot, or a spinner in
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
        paint: NavRowPaint<'_>,
    ) -> Vec<Line<'static>> {
        let NavRowPaint {
            width,
            filter,
            palette,
            reserve_state_word,
        } = paint;
        let row = &self.rows[i];
        let selected = self.selected == i;
        let accent = Style::default().fg(palette.accent);
        let number = Style::default().fg(palette.decoration);
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

        // Section title: `{host}/{mux}`, dim, alone on its row. Not a card - no number,
        // not selectable, and the selection can never land on it. The cards under it are
        // indented, which is what marks the group at every nav position.
        if let RowRef::Section { .. } = &row.reference {
            let title = self.section_title(i);
            let title = if width == 0 {
                title
            } else {
                middle_ellipsize(&title, width.saturating_sub(1) as usize)
            };
            return vec![Line::from(vec![
                Span::styled(title, Style::default().fg(palette.decoration)),
                Span::raw(" "),
            ])];
        }
        // Host-state cards keep one fixed glyph slot after the host/mux identity. The
        // selected card adds its state word after that slot. Column measurement reserves
        // the word on every host card, so moving the selection changes paint but never
        // moves the columns.
        if let RowRef::Host {
            unreachable,
            blocked,
            list_failed,
            scanning,
            ..
        } = &row.reference
        {
            let (host, mux, _) = context_of(row);
            let pending = Style::default().fg(palette.warning);
            let word =
                crate::ui::tree::host_state_word(*scanning, *blocked, *list_failed, *unreachable);
            // A host-state card's number sits on the host/mux line: the row is a word
            // about the host, not the thing the number names.
            let (glyph, glyph_style) = if *scanning {
                (spinner_glyph.to_string(), pending)
            } else if *blocked {
                (
                    crate::ui::chrome::BLOCK_MARK.to_string(),
                    Style::default().fg(palette.warning),
                )
            } else if *list_failed {
                (
                    crate::ui::chrome::LIST_FAILED_MARK.to_string(),
                    Style::default().fg(palette.primary),
                )
            } else if *unreachable {
                (
                    crate::ui::chrome::UNREACHABLE_MARK.to_string(),
                    Style::default().fg(palette.error),
                )
            } else {
                (" ".into(), Style::default())
            };
            let identity = if mux.is_empty() {
                host.to_string()
            } else {
                format!("{host}/{mux}")
            };
            let suffix_w = 2 + if selected || reserve_state_word {
                word.len() + 1
            } else {
                0
            };
            let identity_w = if width == 0 {
                usize::MAX
            } else {
                (width as usize).saturating_sub(num_w + 1 + suffix_w + 1)
            };
            let mut line = address();
            let identity = middle_ellipsize(&identity, identity_w);
            let identity = if filter.is_empty() {
                if let Some((host, mux)) = identity.split_once('/') {
                    vec![
                        Span::styled(host.to_string(), Style::default().fg(palette.secondary)),
                        Span::styled("/", Style::default().fg(palette.decoration)),
                        Span::styled(mux.to_string(), Style::default().fg(palette.secondary)),
                    ]
                } else {
                    vec![Span::styled(
                        identity,
                        Style::default().fg(palette.secondary),
                    )]
                }
            } else {
                let mut spans =
                    highlighted(identity, filter, Style::default().fg(palette.secondary));
                for span in &mut spans {
                    if span.content == "/" {
                        span.style = Style::default().fg(palette.decoration);
                    }
                }
                spans
            };
            line.extend(identity);
            line.push(Span::raw(" "));
            line.push(Span::styled(glyph, glyph_style));
            if selected || reserve_state_word {
                line.push(Span::styled(
                    format!(" {word}"),
                    Style::default().fg(palette.secondary),
                ));
            }
            line.push(Span::raw(" "));
            return vec![Line::from(line)];
        }

        // Session card: the address column + the session name on a single detail line.
        // The `{host}/{mux}` it used to restate now lives on the section title above it.
        // The session name is the lowest level the card displays, so it takes the accent
        // and stays bold.
        //
        // The indent a session card hangs at under its title is NOT part of the card;
        // what a card holds is what a card holds at every position.
        let (_, _, sess) = context_of(row);
        let source = match &row.reference {
            RowRef::Session { sess } => sess.source.as_str(),
            _ => "",
        };
        let mut detail = address();
        let available = if width == 0 {
            usize::MAX
        } else {
            (width as usize).saturating_sub(num_w + 2)
        };
        let session_style = if filter.is_empty() {
            accent.add_modifier(Modifier::BOLD)
        } else {
            accent
        };
        detail.extend(highlighted(
            middle_ellipsize(sess, available),
            &remaining_filter(&format!("{source}/"), filter),
            session_style,
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
        match &state.modal {
            Some(Modal::Help) => {
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
            Some(Modal::History { scroll }) => {
                let w = history_popup_width(area);
                let (_, lines) = crate::ui::toast::history_lines(
                    &state.notify,
                    *scroll,
                    w.saturating_sub(2),
                    &self.palette,
                );
                let h = (lines.len() as u16 + 2).min(area.height.max(1));
                modal::offset_centered(w, h, area, self.popup_geo.offset)
            }
            _ => Rect::default(),
        }
    }

    /// Paints every toast the plan placed, oldest first, so the newest lands on top.
    fn render_toasts(
        &self,
        frame: &mut Frame,
        state: &crate::state::State,
        plan: &RenderPlan,
        palette: &palette::Palette,
    ) {
        for (id, rect) in plan.toasts.iter().rev() {
            if let Some(toast) = state.notify.toasts.iter().find(|t| t.id == *id) {
                crate::ui::toast::render_toast(
                    frame,
                    *rect,
                    toast,
                    state.notify.now,
                    &state.chrome.ui_prefix,
                    palette,
                );
            }
        }
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
        let (title, lines) = match &state.modal {
            Some(Modal::Help) => {
                modal::help_lines(&state.chrome.ui_prefix, state.chrome.nav_position, palette)
            }
            Some(Modal::History { scroll }) => crate::ui::toast::history_lines(
                &state.notify,
                *scroll,
                rect.width.saturating_sub(2),
                palette,
            ),
            _ => return,
        };
        modal::render_popup(frame, area, rect, &title, lines, palette);
    }
}

/// The history popup's width: most of the window, capped so a record reads as one line
/// on a wide screen.
fn history_popup_width(area: Rect) -> u16 {
    area.width
        .saturating_sub(4)
        .clamp(24, 84)
        .min(area.width.max(1))
}
