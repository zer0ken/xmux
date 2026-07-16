use super::*;

/// Braille spinner frames for pending states (connecting session, loading panes).
const SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

impl Switcher {
    pub fn render(
        &mut self,
        frame: &mut Frame,
        grid: Option<&crate::display::grid::Grid>,
        terminal_focused: bool,
        tree_width: u16,
        tree_height: u16,
        state: &crate::state::State,
    ) {
        let area = frame.area();
        self.screen_area = area;
        // Cache the stacking so key handling routes the arrows to match what is on screen.
        self.layout = view_layout(area, tree_width);
        // Reset the buffer before painting. The widgets below do not all fill every cell
        // they own — the mux grid only paints its top-left clip (cells past the grid size
        // are skipped), the view border rule sets fg only, and the nav list leaves blank
        // rows — so when the tree width changes (drag / prefix h·l) cells that switched
        // panes would otherwise keep stale content (the residue seen while resizing).
        // Clearing first makes every unpainted cell default; ratatui still diffs against
        // the last frame, so static content writes nothing (no flicker).
        frame.render_widget(Clear, area);
        // tree_width == 0 is the "nav hidden" sentinel (terminal view focused + auto-hide):
        // the terminal view owns the whole area — no nav list, no hint_bar, no view border.
        if tree_width == 0 {
            self.tree_inner = Rect::default();
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
        let r = compute_regions(area, tree_width, tree_height, hint_bar_h);
        self.render_tree(frame, r.tree, state);
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
    fn render_tree(&mut self, frame: &mut Frame, area: Rect, state: &crate::state::State) {
        // No border box: the list fills its region outright and a single rule
        // (render_view_border) separates it from the terminal view.
        self.tree_inner = area;

        let spinner_glyph = SPINNER[state.chrome.spinner_frame % SPINNER.len()];
        let jump_digit = self.jump_digits();
        let items: Vec<ListItem> = (0..self.rows.len())
            .map(|i| self.tree_row_item(i, jump_digit[i], spinner_glyph))
            .collect();
        let list =
            List::new(items).highlight_style(Style::default().add_modifier(Modifier::REVERSED));
        frame.render_stateful_widget(list, area, &mut self.list_state);
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

    /// Builds one navigation card as a 2-line [`ListItem`]. Line 1 is the digit gutter +
    /// context (`{host}/{session}` with the host yellow and the session green, or just
    /// `{host}` yellow for a host-state card). Line 2 is a blank gutter + detail (a window
    /// card's `{n}:{name}` in window magenta, bold+italic when active; a host-state card's
    /// dim state; a loading card's spinner). The selection highlight comes from the List's
    /// `highlight_style`, so no per-span reverse-video is baked in here.
    fn tree_row_item(
        &self,
        i: usize,
        digit: Option<char>,
        spinner_glyph: char,
    ) -> ListItem<'static> {
        let row = &self.rows[i];
        let dim = Style::default().add_modifier(Modifier::DIM);
        let gutter = match digit {
            Some(d) => format!("{d} "),
            None => "  ".to_string(),
        };

        // Line 1: the {host}/{session} (or {host}) context.
        let mut line1: Vec<Span> = vec![Span::styled(gutter, dim)];
        if matches!(row.reference, RowRef::Host { .. }) {
            line1.push(Span::styled(
                pad_label(&row.line1),
                Style::default().fg(COLOR_HOST),
            ));
        } else {
            let (host, sess) = row
                .line1
                .split_once('/')
                .unwrap_or((row.line1.as_str(), ""));
            line1.push(Span::raw(" "));
            line1.push(Span::styled(
                host.to_string(),
                Style::default().fg(COLOR_HOST),
            ));
            if !sess.is_empty() {
                line1.push(Span::styled("/", dim));
                line1.push(Span::styled(
                    sess.to_string(),
                    Style::default().fg(COLOR_SESSION),
                ));
            }
            line1.push(Span::raw(" "));
        }

        // Line 2: the detail line, under a blank-aligned gutter.
        let mut line2: Vec<Span> = vec![Span::styled("  ".to_string(), dim)];
        match &row.reference {
            RowRef::Window { .. } => {
                let mut style = Style::default().fg(COLOR_WINDOW);
                if row.active {
                    style = style.add_modifier(Modifier::BOLD | Modifier::ITALIC);
                }
                line2.push(Span::styled(pad_label(&row.line2), style));
            }
            RowRef::Loading { .. } => {
                line2.push(Span::styled(
                    format!(" {spinner_glyph} "),
                    Style::default().fg(COLOR_HINT),
                ));
            }
            RowRef::Host { .. } => {
                line2.push(Span::styled(
                    pad_label(&row.line2),
                    Style::default().fg(COLOR_HINT),
                ));
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
                let style = if menu.hovered == Some(i) {
                    Style::default().add_modifier(Modifier::REVERSED)
                } else {
                    Style::default()
                };
                Line::from(Span::styled(format!(" {:<pad$} ", it.label()), style))
            })
            .collect();
        modal::render_popup(frame, self.screen_area, rect, &menu.title, lines);
    }
}
