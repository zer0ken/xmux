use super::*;

use ratatui::widgets::{Scrollbar, ScrollbarOrientation, ScrollbarState};

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

/// Braille spinner frames for pending states (connecting session, loading panes).
const SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

impl Switcher {
    pub fn render(
        &mut self,
        frame: &mut Frame,
        grid: Option<&crate::display::grid::Grid>,
        terminal_focused: bool,
        nav_width: u16,
        nav_height: u16,
        state: &crate::state::State,
    ) {
        let area = frame.area();
        self.screen_area = area;
        // Cache the stacking so key handling routes the arrows to match what is on screen.
        self.layout = view_layout(area, nav_width);
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
                state.chrome.render_hint_bar(frame, rect, state);
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
        let r = compute_regions(area, nav_width, nav_height, hint_bar_h);
        self.render_nav(frame, r.tree, state);
        // The view border marks focus between the two views (vertical in Side, horizontal in Top).
        state
            .chrome
            .render_view_border(frame, r.view_border, terminal_focused);
        let term_area = r.terminal;
        // An unreachable host has no live grid; show an info panel (ssh config stanza
        // + failure reason) in the terminal view instead of the blank grid.
        if self.current_host_unreachable() {
            let source = self.current_source().unwrap_or_default();
            state
                .chrome
                .render_host_info(frame, term_area, state, &source);
        } else if self.current_host_empty(state) {
            // A reachable host with no sessions yet: a calm landing panel (name +
            // how to start one) rather than a blank grid with no next step.
            let source = self.current_source().unwrap_or_default();
            state.chrome.render_host_landing(frame, term_area, &source);
        } else {
            self.render_terminal_view(frame, term_area, grid);
        }
        // The hint bar paints LAST of the two views, so a floating bar can cover the
        // terminal view. At rest it stays inside the nav (its own status line); floating,
        // it widens to the whole window - the layout never reflows, only the paint reaches
        // further, so arming the prefix cannot shift a single card.
        state.chrome.render_hint_bar(
            frame,
            hint_bar_rect(r.hint_bar, area, hint_bar_h, floating),
            state,
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

    /// The navigation list: one flat, vertically-scrolling list of cards in both
    /// layouts (the Side column and the portrait Top band differ only in where the
    /// region sits). Card heights vary - two rows expanded, one collapsed (see
    /// [`Switcher::card_collapsed`]) - which ratatui's `List` handles per item;
    /// `list_state` carries the scroll offset so the selected card stays visible,
    /// and the selection highlight is ratatui's `highlight_style` over the whole
    /// card area.
    fn render_nav(&mut self, frame: &mut Frame, area: Rect, state: &crate::state::State) {
        // No border box: the list fills its region outright and a single rule
        // (render_view_border) separates it from the terminal view.
        self.nav_inner = area;

        let spinner_glyph = SPINNER[state.chrome.spinner_frame % SPINNER.len()];
        let num_w = self.number_width();
        let items: Vec<ListItem> = (0..self.rows.len())
            .map(|i| self.nav_row_item(i, num_w, spinner_glyph))
            .collect();
        // The selection highlight is a quiet raised surface (plus the accent ▌ bar the
        // card itself draws in its gutter), not reverse video: the card's own level
        // colours stay readable while selected.
        let list = List::new(items).highlight_style(Style::default().bg(palette::get().surface));
        frame.render_stateful_widget(list, area, &mut self.list_state);
        self.render_nav_scrollbar(frame, area);
    }

    /// A minimal scrollbar on the nav list's right edge, drawn only when the cards
    /// overflow the region - the offscreen-content cue the flat list otherwise lacks.
    /// Thumb only (no track / arrows) so it reads as a position marker, not furniture.
    /// Counted in cards (not screen rows) over the variable card heights, reading the
    /// offset ratatui settled while rendering the list just before.
    fn render_nav_scrollbar(&mut self, frame: &mut Frame, area: Rect) {
        let total = self.rows.len();
        let total_h: u16 = (0..total).map(|i| self.card_height(i)).sum();
        if total_h <= area.height || area.width == 0 {
            return;
        }
        // The cards fully visible from the settled offset.
        let mut visible = 0usize;
        let mut used = 0u16;
        for i in self.list_state.offset()..total {
            used += self.card_height(i);
            if used > area.height {
                break;
            }
            visible += 1;
        }
        let mut sb = ScrollbarState::new(total.saturating_sub(visible))
            .position(self.list_state.offset())
            .viewport_content_length(visible);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .track_symbol(None)
                .thumb_symbol("▐")
                .thumb_style(Style::default().fg(palette::get().overlay)),
            area,
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
    /// [`Switcher::card_collapsed`]). The context line is the gutter +
    /// `{host}/{mux}` in the host / mux colours (the mux segment only when known -
    /// a just-created session is stamped by the next enumeration; just `{host}` on
    /// a host-state card). The detail line is the gutter + detail: a session
    /// card's `{session}/{index}:{window-name}` - the focused (active) window,
    /// what the mux shows on attach - in the session / window colours, a loading
    /// card's `{session}/` + spinner, a host-state card's state coloured by kind
    /// (pending / danger / muted). The gutter is the selection bar column, then the
    /// card's dim 0-based number, then a space: EVERY card shows its number, the
    /// selected one included, because the number is the card's address and an address
    /// that disappears when you land on it cannot be read back. The number sits on the
    /// DETAIL line, never the context line: the number addresses a session, so it reads
    /// beside the session it names, and a collapsed card (detail line only) then puts it
    /// in the same place as an expanded one. The surface background comes from the List's
    /// `highlight_style`, so no per-span background is baked in here.
    fn nav_row_item(&self, i: usize, num_w: usize, spinner_glyph: char) -> ListItem<'static> {
        let row = &self.rows[i];
        let muted = Style::default().fg(color_hint());
        let selected = self.list_state.selected() == Some(i);
        let bar = Style::default().fg(palette::get().accent);
        // `numbered` is the detail line: the selection bar runs down every line of the
        // card, the number appears once, next to the session it addresses.
        let gutter = move |numbered: bool| -> Vec<Span<'static>> {
            let mark = if selected {
                Span::styled("▌", bar)
            } else {
                Span::raw(" ")
            };
            let number = if numbered {
                Span::styled(format!("{i:>num_w$} "), muted)
            } else {
                Span::raw(" ".repeat(num_w + 1))
            };
            vec![mark, number]
        };

        // Host-state card: the host name over its status, coloured by kind -
        // in-flight scanning is pending (soft yellow), a dead host is danger
        // (soft red), a settled empty host is muted.
        if let RowRef::Host { unreachable, .. } = &row.reference {
            let (host, _, _) = context_of(&row.reference);
            let mut line1 = gutter(false);
            line1.push(Span::styled(
                pad_label(host),
                Style::default().fg(color_host()),
            ));
            let style = if *unreachable {
                Style::default().fg(palette::get().danger)
            } else if row.line2.starts_with("scanning") {
                Style::default().fg(palette::get().pending)
            } else {
                muted
            };
            let mut line2 = gutter(true);
            line2.push(Span::styled(pad_label(&row.line2), style));
            return ListItem::new(vec![Line::from(line1), Line::from(line2)]);
        }

        // Session / loading card.
        let (host, mux, sess) = context_of(&row.reference);
        let collapsed = self.card_collapsed(i);
        let mut lines: Vec<Line> = Vec::new();
        if !collapsed {
            let mut context: Vec<Span> = gutter(false);
            context.push(Span::styled(
                host.to_string(),
                Style::default().fg(color_host()),
            ));
            if !mux.is_empty() {
                context.push(Span::styled("/", muted));
                context.push(Span::styled(
                    mux.to_string(),
                    Style::default().fg(color_mux()),
                ));
            }
            context.push(Span::raw(" "));
            lines.push(Line::from(context));
        }
        let window_part = match &row.reference {
            RowRef::Loading { .. } => Span::styled(
                spinner_glyph.to_string(),
                Style::default().fg(palette::get().pending),
            ),
            _ => Span::styled(row.line2.clone(), Style::default().fg(color_window())),
        };
        // The connector hangs the detail line under its context line; on a
        // collapsed card it hangs under the SHARED context above, so a run of
        // collapsed cards reads as siblings of one context: ├ while a collapsed
        // sibling follows below, └ on the run's last line. The selected card
        // drops it - the accent bar and surface already bind its two lines.
        let mut detail = gutter(true);
        if !selected {
            let connector = if self.card_collapsed(i + 1) {
                "├ "
            } else {
                "└ "
            };
            detail.push(Span::styled(connector, muted));
        }
        detail.push(Span::styled(
            sess.to_string(),
            Style::default().fg(color_session()),
        ));
        // No window row to show (the mux named none): the card is the session alone,
        // without a trailing separator standing in for something absent.
        if !matches!(&row.reference, RowRef::Session { .. }) || !row.line2.is_empty() {
            detail.push(Span::styled("/", muted));
            detail.push(window_part);
        }
        detail.push(Span::raw(" "));
        lines.push(Line::from(detail));
        ListItem::new(lines)
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
