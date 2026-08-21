use super::*;

use ratatui::widgets::{Scrollbar, ScrollbarOrientation, ScrollbarState};

use crate::ui::palette;

/// Where the hint bar actually paints. At rest it is the nav-local rect
/// `compute_regions` derived; while the prefix is ARMED it spans the whole window width
/// on those same rows, so the cheatsheet reads as one bar floating over the whole app
/// rather than a column note - and it can say more than a nav column has room for.
/// Only the paint widens; the layout is untouched, so nothing reflows on arm.
fn hint_bar_rect(nav_local: Rect, area: Rect, state: &crate::state::State) -> Rect {
    if !state.chrome.armed || nav_local.height == 0 {
        return nav_local;
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
        // the terminal view owns the whole area - no nav list, no hint_bar, no view border.
        if nav_width == 0 {
            self.nav_inner = Rect::default();
            self.render_terminal_view(frame, area, grid);
            if let Some(g) = grid {
                if !g.hide_cursor() {
                    frame.set_cursor_position(terminal_cursor_pos(area, g.cursor()));
                }
            }
            self.render_modal_popup(frame, area, state);
            return;
        }
        // One geometry source for the whole frame (compute_regions), shared with the PTY
        // sizing and mouse hit-testing so they never diverge: the nav list / terminal split
        // horizontally (Side) or vertically (Top, for a portrait screen), parted by the view
        // border, and the hint bar takes the nav's bottom rows. The hint bar is normally one
        // row; a long flash wraps, so size it to the wrapped line count (never clipped).
        // Measured at the width it will RENDER at: the nav column normally, the whole
        // window while the prefix is armed (see `hint_bar_rect`).
        let bar_w = if state.chrome.armed {
            area.width
        } else {
            nav_width
        };
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
        // The hint bar paints LAST of the two views, so an armed bar can float over the
        // terminal view. At rest it stays inside the nav (its own status line); armed it
        // widens to the whole window on the same row, covering the view border and the
        // grid beneath it - the layout never reflows, only the paint reaches further, so
        // arming the prefix cannot shift a single card.
        state
            .chrome
            .render_hint_bar(frame, hint_bar_rect(r.hint_bar, area, state), state);
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
        let jump_digit = self.jump_digits();
        let items: Vec<ListItem> = (0..self.rows.len())
            .map(|i| self.nav_row_item(i, jump_digit[i], spinner_glyph))
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

    /// The quick-jump digit for each card: the first nine cards (in list order, matching
    /// `move_to`) get a dim 1..9; the rest `None`. A 2-col gutter is reserved on every
    /// card so numbering never reflows the list.
    fn jump_digits(&self) -> Vec<Option<char>> {
        let sel = self.selectable_indices();
        let mut jump_digit: Vec<Option<char>> = vec![None; self.rows.len()];
        for (pos, &ri) in sel.iter().enumerate().take(9) {
            jump_digit[ri] = Some((b'1' + pos as u8) as char);
        }
        jump_digit
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
    /// (pending / danger / muted). The gutter carries the selected card's accent ▌
    /// bar on every line (replacing its jump digit - the selection needs no jump
    /// target); an unselected card shows its dim digit on its first line. The
    /// surface background comes from the List's `highlight_style`, so no per-span
    /// background is baked in here.
    fn nav_row_item(
        &self,
        i: usize,
        digit: Option<char>,
        spinner_glyph: char,
    ) -> ListItem<'static> {
        let row = &self.rows[i];
        let muted = Style::default().fg(color_hint());
        let selected = self.list_state.selected() == Some(i);
        let bar = Style::default().fg(palette::get().accent);
        let gutter = |first: bool| -> Span<'static> {
            if selected {
                Span::styled("▌ ", bar)
            } else {
                match digit.filter(|_| first) {
                    Some(d) => Span::styled(format!("{d} "), muted),
                    None => Span::raw("  "),
                }
            }
        };

        // Host-state card: the host name over its status, coloured by kind -
        // in-flight scanning is pending (soft yellow), a dead host is danger
        // (soft red), a settled empty host is muted.
        if let RowRef::Host { unreachable, .. } = &row.reference {
            let (host, _, _) = context_of(&row.reference);
            let line1 = vec![
                gutter(true),
                Span::styled(pad_label(host), Style::default().fg(color_host())),
            ];
            let style = if *unreachable {
                Style::default().fg(palette::get().danger)
            } else if row.line2.starts_with("scanning") {
                Style::default().fg(palette::get().pending)
            } else {
                muted
            };
            let line2 = vec![gutter(false), Span::styled(pad_label(&row.line2), style)];
            return ListItem::new(vec![Line::from(line1), Line::from(line2)]);
        }

        // Session / loading card.
        let (host, mux, sess) = context_of(&row.reference);
        let collapsed = self.card_collapsed(i);
        let mut lines: Vec<Line> = Vec::new();
        if !collapsed {
            let mut context: Vec<Span> = vec![gutter(true), Span::raw(" ")];
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
        let mut detail = vec![gutter(collapsed), Span::raw(" ")];
        if !selected {
            let connector = if self.card_collapsed(i + 1) {
                "├ "
            } else {
                "└ "
            };
            detail.push(Span::styled(connector, muted));
        }
        detail.extend([
            Span::styled(sess.to_string(), Style::default().fg(color_session())),
            Span::styled("/", muted),
            window_part,
            Span::raw(" "),
        ]);
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
