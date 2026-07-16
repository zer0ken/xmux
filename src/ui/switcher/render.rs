use super::*;

/// Braille spinner frames for pending states (connecting session, loading panes).
const SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// The rendered display width of a tree row: the 2-col digit gutter, the level indent, the
/// padded label, and any trailing status annotation. Sizes a Top-layout host column to its
/// content so labels are not clipped.
fn row_render_width(row: &Row) -> u16 {
    let label = UnicodeWidthStr::width(pad_label(&row.label).as_str());
    let status = row
        .status
        .as_ref()
        .map(|s| UnicodeWidthStr::width(s.as_str()) + 1)
        .unwrap_or(0);
    (2 + row.indent + label + status) as u16
}

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
        // One geometry source for the whole frame (compute_regions), shared with the PTY
        // sizing and mouse hit-testing so they never diverge: the hint bar spans the bottom
        // full width, and the tree / terminal split horizontally (Side) or vertically (Top,
        // for a portrait screen), parted by the view border. The hint bar is normally one
        // row; a long flash wraps, so size it to the wrapped line count (never clipped).
        let hint_bar_h = state.chrome.hint_bar_lines(area.width, state).len().max(1) as u16;
        let r = compute_regions(area, tree_width, tree_height, hint_bar_h);
        self.render_tree(frame, r.tree, r.layout, state);
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

    fn render_tree(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        layout: ViewLayout,
        state: &crate::state::State,
    ) {
        // No border box: the tree fills its region outright and a single rule
        // (render_view_border) separates it from the terminal view.
        self.tree_inner = area;

        let spinner_glyph = SPINNER[state.chrome.spinner_frame % SPINNER.len()];
        let jump_digit = self.jump_digits();
        match layout {
            // Side: one vertical list; `list_state` carries the scroll offset so the
            // selected row stays visible as the tree grows past the column height.
            ViewLayout::Side => {
                let items: Vec<ListItem> = (0..self.rows.len())
                    .map(|i| self.tree_row_item(i, jump_digit[i], spinner_glyph, state))
                    .collect();
                let list = List::new(items);
                frame.render_stateful_widget(list, area, &mut self.list_state);
            }
            // Top (portrait): one column per host, paged horizontally to the selection.
            ViewLayout::Top => {
                self.render_tree_columns(frame, area, &jump_digit, spinner_glyph, state)
            }
        }
    }

    /// The quick-jump digit for each row: the first nine SELECTABLE rows (in flatten
    /// order, matching `move_to`) get a dim 1..9; the rest `None`. A 2-col gutter is
    /// reserved on every row so numbering never reflows the tree.
    fn jump_digits(&self) -> Vec<Option<char>> {
        let sel = self.selectable_indices();
        let mut jump_digit: Vec<Option<char>> = vec![None; self.rows.len()];
        for (pos, &ri) in sel.iter().enumerate().take(9) {
            jump_digit[ri] = Some((b'1' + pos as u8) as char);
        }
        jump_digit
    }

    /// Builds one tree row's list item — the reserved digit gutter, the level indent,
    /// the level-coloured (bold+italic when active) label, plus any connecting spinner
    /// or dim status annotation. Shared by the Side list and the Top per-host columns so
    /// a row looks identical in either layout; `i == self.selected` drives the
    /// reverse-video highlight.
    fn tree_row_item(
        &self,
        i: usize,
        digit: Option<char>,
        spinner_glyph: char,
        state: &crate::state::State,
    ) -> ListItem<'static> {
        let row = &self.rows[i];
        let selected = i == self.selected;
        let mut gutter_style = Style::default().add_modifier(Modifier::DIM);
        if selected {
            gutter_style = gutter_style.add_modifier(Modifier::REVERSED);
        }
        let gutter = match digit {
            Some(d) => format!("{d} "),
            None => "  ".to_string(),
        };
        let indent = " ".repeat(row.indent);
        // The pane-loading placeholder is an animated progress spinner, not the word "loading".
        if matches!(row.reference, RowRef::Loading) {
            return ListItem::new(Line::from(vec![
                Span::styled(gutter, gutter_style),
                Span::raw(indent),
                Span::styled(spinner_glyph.to_string(), Style::default().fg(COLOR_HINT)),
            ]));
        }
        // Colour is a pure function of the row's level, derived here so the tree model
        // (`tree::Row`) stays terminal-free. Loading returns above.
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
        // The active window / pane reads BOLD+ITALIC — no "(active)" text marker — the
        // currently-displayed window of each session.
        if row.active {
            style = style.add_modifier(Modifier::BOLD | Modifier::ITALIC);
        }
        let mut spans = vec![
            Span::styled(gutter, gutter_style),
            Span::raw(indent),
            Span::styled(pad_label(&row.label), style),
        ];
        // Spinner glyph: shown right of the session name when connecting.
        if matches!(&row.reference, RowRef::Session(s) if state.chrome.spinner.contains(&s.address()))
        {
            spans.push(Span::styled(
                spinner_glyph.to_string(),
                Style::default().fg(COLOR_HINT),
            ));
        }
        if let Some(status) = &row.status {
            let mut status_style = Style::default().fg(COLOR_HINT);
            if selected {
                status_style = status_style.add_modifier(Modifier::REVERSED);
            }
            spans.push(Span::styled(format!("{status} "), status_style));
        }
        ListItem::new(Line::from(spans))
    }

    /// Top (portrait) layout: the tree becomes one column per host, each column that
    /// host's Host→Session→Window subtree laid left-to-right. Host segments are read
    /// straight off the flat rows (each starts at a Host row and runs to the next), so
    /// no separate model is needed. When more hosts than fit, the columns page
    /// horizontally so the selected host's column stays on screen; within a column the
    /// per-column `ListState` scrolls vertically to keep the selection visible.
    // ponytail: whole-page paging (stateless), not smooth sliding — swap for a remembered
    // offset only if the page-flip on a boundary crossing feels abrupt in real use.
    /// The Top-layout column geometry for `area`: the per-host row segments
    /// (`[start, next_host_start)`), the column width, and how many columns fit per page.
    /// Columns are as wide as the widest row (gutter + indent + label + status) so full
    /// labels show without a fixed cap; when hosts overflow the width the caller pages
    /// horizontally. Shared by the renderer and mouse hit-testing so a click maps to the
    /// same columns that were drawn.
    pub(super) fn top_columns(&self, area: Rect) -> (Vec<(usize, usize)>, u16, usize) {
        let starts = self.host_starts();
        let segs: Vec<(usize, usize)> = starts
            .iter()
            .enumerate()
            .map(|(k, &s)| (s, *starts.get(k + 1).unwrap_or(&self.rows.len())))
            .collect();
        let natural = self
            .rows
            .iter()
            .map(row_render_width)
            .max()
            .unwrap_or(1)
            .max(1);
        let col_natural = natural.min(area.width.max(1)).max(1);
        let per_page = (area.width / col_natural).max(1) as usize;
        let col_w = (area.width / per_page as u16).max(1);
        (segs, col_w, per_page)
    }

    /// The host segment the selection sits in, given the column segments.
    pub(super) fn selected_host_index(&self, segs: &[(usize, usize)]) -> usize {
        segs.iter()
            .position(|&(s, e)| self.selected >= s && self.selected < e)
            .unwrap_or(0)
    }

    fn render_tree_columns(
        &self,
        frame: &mut Frame,
        area: Rect,
        jump_digit: &[Option<char>],
        spinner_glyph: char,
        state: &crate::state::State,
    ) {
        let (segs, col_w, per_page) = self.top_columns(area);
        if segs.is_empty() {
            return;
        }
        // Whole-page paging (stateless), not smooth sliding — the page holding the selection.
        let first = (self.selected_host_index(&segs) / per_page) * per_page;
        let visible = &segs[first..(first + per_page).min(segs.len())];

        let constraints: Vec<Constraint> =
            visible.iter().map(|_| Constraint::Length(col_w)).collect();
        let cols = Layout::horizontal(constraints).split(area);
        for (ci, &(s, e)) in visible.iter().enumerate() {
            let items: Vec<ListItem> = (s..e)
                .map(|i| self.tree_row_item(i, jump_digit[i], spinner_glyph, state))
                .collect();
            let list = List::new(items);
            let mut cstate = ListState::default();
            if self.selected >= s && self.selected < e {
                cstate.select(Some(self.selected - s));
            }
            frame.render_stateful_widget(list, cols[ci], &mut cstate);
        }
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
