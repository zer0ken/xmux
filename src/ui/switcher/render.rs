use super::*;

use ratatui::widgets::{Scrollbar, ScrollbarOrientation, ScrollbarState};

use crate::ui::palette;

/// Braille spinner frames for pending states (connecting session, loading panes).
const SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// The line1 context parts of a card: `(host, mux, session)`. A host-state card
/// carries only its host; a session/loading card names its session's host, mux
/// kind (empty when not yet known), and session name.
fn context_of(reference: &RowRef) -> (&str, &str, &str) {
    match reference {
        RowRef::Host { source, .. } => (source, "", ""),
        RowRef::Session { sess } | RowRef::Loading { sess } => {
            (&sess.source, &sess.mux, &sess.name)
        }
    }
}

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
            self.render_menu(frame, state);
            return;
        }
        // One geometry source for the whole frame (compute_regions), shared with the PTY
        // sizing and mouse hit-testing so they never diverge: the hint bar spans the bottom
        // full width, and the nav list / terminal split horizontally (Side) or vertically
        // (Top, for a portrait screen), parted by the view border. The hint bar is normally
        // one row; a long flash wraps, so size it to the wrapped line count (never clipped).
        let hint_bar_h = state.chrome.hint_bar_lines(area.width, state).len().max(1) as u16;
        let r = compute_regions(area, nav_width, nav_height, hint_bar_h);
        self.render_nav(frame, r.tree, state);
        state.chrome.render_hint_bar(frame, r.hint_bar, state);
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
        self.render_menu(frame, state);
    }

    /// The navigation list: one flat, vertically-scrolling list of 2-row cards in both
    /// layouts (the Side column and the portrait Top band differ only in where the region
    /// sits). `list_state` carries the scroll offset so the selected card stays visible;
    /// the selection highlight is ratatui's `highlight_style` over the whole card area.
    fn render_nav(&mut self, frame: &mut Frame, area: Rect, state: &crate::state::State) {
        // No border box: the list fills its region outright and a single rule
        // (render_view_border) separates it from the terminal view.
        self.nav_inner = area;

        // Settle the scroll offset BEFORE building the items: the first-visible
        // card's elision exemption reads it, and ratatui adjusts the offset only
        // DURING render (which would leave the exemption one frame behind on a
        // scroll). Mirrors the List's keep-selected-visible policy.
        let visible = (area.height / CARD_H).max(1) as usize;
        let mut first = self
            .list_state
            .offset()
            .min(self.rows.len().saturating_sub(1));
        if let Some(sel) = self.list_state.selected() {
            if sel < first {
                first = sel;
            } else if sel >= first + visible {
                first = sel + 1 - visible;
            }
        }
        *self.list_state.offset_mut() = first;

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
    fn render_nav_scrollbar(&mut self, frame: &mut Frame, area: Rect) {
        let visible = (area.height / CARD_H) as usize;
        let total = self.rows.len();
        if total <= visible || area.width == 0 {
            return;
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

    /// Builds one navigation card as a 2-line [`ListItem`]. Line 1 is the gutter +
    /// context: `{host}/{mux}/{session}` in the host / mux / session colours (or just
    /// `{host}` for a host-state card), with consecutive-context elision - a card
    /// whose host matches the previous card's omits the host, and matching mux too
    /// omits both, so runs on one server read grouped. The first VISIBLE card is
    /// never elided (the card it would continue from is offscreen). Line 2 is the
    /// gutter + detail (a session card's focused window name in the window colour; a
    /// host-state card's state coloured by kind - pending / danger / muted; a loading
    /// card's spinner). The gutter carries the selected card's accent ▌ bar on both
    /// lines (replacing its jump digit - the selection needs no jump target);
    /// unselected cards show their dim digit. The surface background comes from the
    /// List's `highlight_style`, so no per-span background is baked in here.
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
        let gutter = |line1: bool| -> Span<'static> {
            if selected {
                Span::styled("▌ ", bar)
            } else {
                match digit.filter(|_| line1) {
                    Some(d) => Span::styled(format!("{d} "), muted),
                    None => Span::raw("  "),
                }
            }
        };

        // Line 1: the {host}/{mux}/{session} (or {host}) context.
        let mut line1: Vec<Span> = vec![gutter(true)];
        if matches!(row.reference, RowRef::Host { .. }) {
            let (host, _, _) = context_of(&row.reference);
            line1.push(Span::styled(
                pad_label(host),
                Style::default().fg(color_host()),
            ));
        } else {
            let (host, mux, sess) = context_of(&row.reference);
            // The previous card's context, for elision - treated as absent at the
            // viewport top so the first visible card always carries full context.
            let prev = (i > self.list_state.offset())
                .then(|| self.rows.get(i - 1))
                .flatten()
                .map(|r| context_of(&r.reference));
            let same_host = prev.is_some_and(|(h, _, _)| h == host);
            let same_mux = same_host && prev.is_some_and(|(_, m, _)| m == mux);
            line1.push(Span::raw(" "));
            if !same_host {
                line1.push(Span::styled(
                    host.to_string(),
                    Style::default().fg(color_host()),
                ));
                line1.push(Span::styled("/", muted));
            }
            // The mux segment renders only when known (a just-created session is
            // stamped by the next enumeration).
            if !same_mux && !mux.is_empty() {
                line1.push(Span::styled(
                    mux.to_string(),
                    Style::default().fg(color_mux()),
                ));
                line1.push(Span::styled("/", muted));
            }
            line1.push(Span::styled(
                sess.to_string(),
                Style::default().fg(color_session()),
            ));
            line1.push(Span::raw(" "));
        }

        // Line 2: the detail line, under the gutter (bar when selected, blank otherwise).
        let mut line2: Vec<Span> = vec![gutter(false)];
        match &row.reference {
            RowRef::Session { .. } => {
                // The focused (active) window's name - what the mux shows on attach -
                // so the card needs no per-window active marker.
                line2.push(Span::styled(
                    format!(" {} ", row.line2),
                    Style::default().fg(color_window()),
                ));
            }
            RowRef::Loading { .. } => {
                line2.push(Span::styled(
                    format!(" {spinner_glyph} "),
                    Style::default().fg(palette::get().pending),
                ));
            }
            RowRef::Host { unreachable, .. } => {
                // The host-state colour language: in-flight scanning is pending
                // (soft yellow), a dead host is danger (soft red), a settled empty
                // host is muted.
                let style = if *unreachable {
                    Style::default().fg(palette::get().danger)
                } else if row.line2.starts_with("scanning") {
                    Style::default().fg(palette::get().pending)
                } else {
                    muted
                };
                line2.push(Span::styled(pad_label(&row.line2), style));
            }
        }

        ListItem::new(vec![Line::from(line1), Line::from(line2)])
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
    /// exclusive; the context menu is drawn separately by `render_menu`.
    fn render_modal_popup(&mut self, frame: &mut Frame, area: Rect, state: &crate::state::State) {
        let (title, lines) = match &state.modal {
            Some(Modal::Help) => modal::help_lines(&state.chrome.ui_prefix),
            Some(Modal::Kill(armed)) => modal::confirm_lines(armed),
            Some(Modal::Input(input)) => modal::input_lines(input),
            _ => {
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

    /// Draws the open context menu as a bordered popup at its anchored rect: the target's
    /// name in the title (like tmux's menu title), the hovered item reversed. Shares the
    /// opaque, tmux-edge popup renderer with the help modal.
    fn render_menu(&self, frame: &mut Frame, state: &crate::state::State) {
        let Some(Modal::Menu(menu)) = &state.modal else {
            return;
        };
        let rect = menu.rect;
        let pad = rect.width.saturating_sub(4) as usize;
        let lines: Vec<Line> = menu
            .items
            .iter()
            .enumerate()
            .map(|(i, it)| {
                // The menu highlight matches the nav's selection language: a quiet
                // surface background, not reverse video.
                let style = if menu.hovered == Some(i) {
                    Style::default().bg(palette::get().surface)
                } else {
                    Style::default()
                };
                Line::from(Span::styled(format!(" {:<pad$} ", it.label()), style))
            })
            .collect();
        modal::render_popup(frame, self.screen_area, rect, &menu.title, lines);
    }
}
