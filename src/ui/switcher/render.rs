use super::*;

impl Switcher {
    pub fn render(
        &mut self,
        frame: &mut Frame,
        grid: Option<&crate::display::grid::Grid>,
        terminal_focused: bool,
        tree_width: u16,
        state: &crate::state::State,
    ) {
        let area = frame.area();
        self.screen_area = area;
        // Reset the buffer before painting. The widgets below do not all fill every cell
        // they own — the mux grid only paints its top-left clip (cells past the grid size
        // are skipped), the view border rule sets fg only, and the tree leaves blank rows — so
        // when the tree width changes (drag / prefix h·l) cells that switched panes would
        // otherwise keep stale content (the residue seen while resizing). Clearing first
        // makes every unpainted cell default; ratatui still diffs against the last frame,
        // so static content writes nothing (no flicker).
        frame.render_widget(Clear, area);
        // tree_width == 0 is the "tree hidden" sentinel (terminal view focused + auto-hide-tree):
        // the terminal view owns the whole area — no tree, no input/hint_bar, no view border.
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
        // Split vertically first so the hint_bar spans the full terminal width, not just
        // the tree column: [body, hint_bar]. The hint_bar is normally one line; a long
        // flash wraps across several, so size it to the wrapped line count (never clipped).
        let hint_bar_h = state.chrome.hint_bar_lines(area.width, state).len().max(1) as u16;
        let rows = Layout::vertical([
            Constraint::Min(0),             // body: tree | view border | terminal view
            Constraint::Length(hint_bar_h), // full-width hint_bar (help / status / wrapped flash)
        ])
        .split(area);
        let cols = Layout::horizontal([
            Constraint::Length(tree_width),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(rows[0]);
        self.render_tree(frame, cols[0], state);
        state.chrome.render_hint_bar(frame, rows[1], state);
        // The tree|terminal view border marks focus between those two views.
        state
            .chrome
            .render_view_border(frame, cols[1], terminal_focused);
        let term_area = cols[2];
        // An unreachable host has no live grid; show an info panel (ssh config stanza
        // + failure reason) in the terminal view instead of the blank grid.
        if self.current_host_unreachable() {
            let source = self.current_source().unwrap_or_default();
            state
                .chrome
                .render_host_info(frame, term_area, state, &source);
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

    fn render_tree(&mut self, frame: &mut Frame, area: Rect, state: &crate::state::State) {
        // No border box: the tree fills its column outright and a single rule
        // (render_view_border) separates it from the terminal view.
        self.tree_inner = area;

        const SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
        let spinner_glyph = SPINNER[state.chrome.spinner_frame % SPINNER.len()];

        // Quick-jump gutter: the first nine SELECTABLE rows (in flatten order, matching
        // `move_to`) get a dim 1..9 digit; pressing that digit in tree focus jumps there.
        // A 2-col gutter is reserved on EVERY row so numbering never reflows the tree.
        let sel = self.selectable_indices();
        let mut jump_digit: Vec<Option<char>> = vec![None; self.rows.len()];
        for (pos, &ri) in sel.iter().enumerate().take(9) {
            jump_digit[ri] = Some((b'1' + pos as u8) as char);
        }

        let items: Vec<ListItem> = self
            .rows
            .iter()
            .enumerate()
            .map(|(i, row)| {
                let selected = i == self.selected;
                let mut gutter_style = Style::default().add_modifier(Modifier::DIM);
                if selected {
                    gutter_style = gutter_style.add_modifier(Modifier::REVERSED);
                }
                let gutter = match jump_digit[i] {
                    Some(d) => format!("{d} "),
                    None => "  ".to_string(),
                };
                let indent = " ".repeat(row.indent);
                // The pane-loading placeholder is an animated progress spinner,
                // not the word "loading".
                if matches!(row.reference, RowRef::Loading) {
                    return ListItem::new(Line::from(vec![
                        Span::styled(gutter, gutter_style),
                        Span::raw(indent),
                        Span::styled(spinner_glyph.to_string(), Style::default().fg(COLOR_HINT)),
                    ]));
                }
                // Colour is a pure function of the row's level, derived here so the
                // tree model (`tree::Row`) stays terminal-free. Loading returns above.
                let color = match &row.reference {
                    RowRef::Host { .. } => COLOR_HOST,
                    RowRef::Session(_) => COLOR_SESSION,
                    RowRef::Window { .. } => COLOR_WINDOW,
                    RowRef::Loading => COLOR_HINT,
                };
                let mut style = Style::default().fg(color);
                if selected {
                    style = style.add_modifier(Modifier::REVERSED);
                }
                // The active window / pane reads BOLD+ITALIC — no "(active)" text
                // marker — the currently-displayed window of each session.
                if row.active {
                    style = style.add_modifier(Modifier::BOLD | Modifier::ITALIC);
                }
                let mut spans = vec![
                    Span::styled(gutter, gutter_style),
                    Span::raw(indent),
                    Span::styled(pad_label(&row.label), style),
                ];
                // Spinner glyph: shown right of the session name when connecting.
                if matches!(&row.reference, RowRef::Session(s) if state.chrome.spinner.contains(&s.address())) {
                    let sp_style = Style::default().fg(COLOR_HINT);
                    spans.push(Span::styled(spinner_glyph.to_string(), sp_style));
                }
                if let Some(status) = &row.status {
                    let mut status_style = Style::default().fg(COLOR_HINT);
                    if selected {
                        status_style = status_style.add_modifier(Modifier::REVERSED);
                    }
                    spans.push(Span::styled(format!("{status} "), status_style));
                }
                ListItem::new(Line::from(spans))
            })
            .collect();

        let list = List::new(items);
        frame.render_stateful_widget(list, area, &mut self.list_state);
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
