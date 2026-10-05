use super::*;

impl Switcher {
    // --- mouse --------------------------------------------------------------

    /// Begins a popup drag against the rectangle painted for the latest frame. A press
    /// anywhere on the key list or a modal popup grabs it, a help tab and a list item
    /// included: the press becomes a drag once the pointer moves, and a click on the
    /// grabbed cell if it is released there (see [`Self::end_popup_drag_in_plan`]).
    pub fn begin_popup_drag_in_plan(
        &mut self,
        plan: &RenderPlan,
        col: u16,
        row: u16,
        state: &crate::state::State,
    ) -> bool {
        let key_list = plan.key_list.as_ref().map(|(rect, _)| *rect);
        self.popup_geo.rect = if plan.popup_rect.is_empty() {
            key_list.unwrap_or_default()
        } else {
            plan.popup_rect
        };
        let open = state.is_modal_popup_open() || key_list.is_some();
        self.begin_popup_drag(col, row, open)
    }

    /// Ends a popup drag. A press released on the cell it grabbed is a click, and a click
    /// executes the help tab or the list item under it the way Enter executes the hard
    /// selection: a tab becomes the hard selection and scrolls its section's title to the
    /// top of the body, and an item becomes the hard selection and is marked for the
    /// switcher to act on as an Enter.
    pub fn end_popup_drag_in_plan(&mut self, plan: &RenderPlan, state: &mut crate::state::State) {
        let Some((col, row)) = self.popup_geo.end_drag() else {
            return;
        };
        match (
            self.popup_target_at(plan, col, row, state),
            &mut state.modal,
        ) {
            (
                Some(chosen),
                Some(Modal::Help {
                    query, scroll, tab, ..
                }),
            ) => {
                let inner = Self::popup_inner(plan.popup_rect);
                let map = modal::help_map(
                    &state.chrome.ui_prefix,
                    state.chrome.nav_position,
                    query,
                    inner.width,
                    inner.height,
                );
                *tab = Some(chosen);
                *scroll = map.scroll_to(chosen);
            }
            (
                Some(chosen),
                Some(Modal::Check { selected, open, .. } | Modal::Palette { selected, open, .. }),
            ) => {
                *selected = chosen;
                *open = true;
            }
            _ => {}
        }
    }

    /// Sets the soft selection of the open popup to the help tab or the list item under
    /// `(col, row)`, or clears it when the pointer is on neither.
    pub fn hover_popup(
        &mut self,
        plan: &RenderPlan,
        col: u16,
        row: u16,
        state: &mut crate::state::State,
    ) {
        let target = self.popup_target_at(plan, col, row, state);
        if let Some(
            Modal::Help { hover, .. } | Modal::Check { hover, .. } | Modal::Palette { hover, .. },
        ) = &mut state.modal
        {
            *hover = target;
        }
    }

    /// The popup's inner rect: `rect` inside its border.
    fn popup_inner(rect: Rect) -> Rect {
        Rect::new(
            rect.x.saturating_add(1),
            rect.y.saturating_add(1),
            rect.width.saturating_sub(2),
            rect.height.saturating_sub(2),
        )
    }

    /// What the pointer at `(col, row)` would select on the popup the plan painted: a help
    /// tab's section, or the index of a list item (a host to check, a palette command).
    /// A cell between tabs, a cause title, the query field, and the border name nothing.
    fn popup_target_at(
        &self,
        plan: &RenderPlan,
        col: u16,
        row: u16,
        state: &crate::state::State,
    ) -> Option<usize> {
        let inner = Self::popup_inner(plan.popup_rect);
        if !inner.contains(Position { x: col, y: row }) {
            return None;
        }
        match &state.modal {
            Some(Modal::Help {
                query, scroll, tab, ..
            }) => {
                if row != inner.y.saturating_add(modal::HELP_TAB_ROW) {
                    return None;
                }
                modal::help_tab_at(
                    &state.chrome.ui_prefix,
                    state.chrome.nav_position,
                    query,
                    *scroll,
                    *tab,
                    inner.width,
                    inner.height,
                    col - inner.x,
                )
            }
            Some(Modal::Check { .. } | Modal::Palette { .. }) => {
                let (_, lines) = self.list_popup_lines(state, plan.popup_rect)?;
                lines.get((row - inner.y) as usize)?.0
            }
            _ => None,
        }
    }

    fn in_tree(plan: &RenderPlan, col: u16, row: u16) -> bool {
        plan.nav_inner.contains(Position { x: col, y: row })
    }

    /// The card index under a 0-based screen `(col, row)`, or `None` if it is outside the
    /// nav or on none of its cards (the gap between the bands, the band rule, a title, an
    /// indent, the rows past the last card). A band's overflow count on the seam stands
    /// for the hidden card nearest the visible ones.
    ///
    /// Neither layout puts cards on a fixed row pitch - the side list parts its groups
    /// and its card heights vary, the portrait flow runs them into columns - so the plan
    /// records each card's rect and the hit-test reads those back. One geometry, so a
    /// click cannot land on a card the renderer put elsewhere.
    fn row_at(plan: &RenderPlan, col: u16, row: u16) -> Option<usize> {
        if let Some(target) = plan.overflow_target(col, row) {
            return Some(target);
        }
        if !Self::in_tree(plan, col, row) {
            return None;
        }
        let at = Position { x: col, y: row };
        plan.nav_cells
            .iter()
            .find(|(_, rect)| rect.contains(at))
            .map(|(i, _)| *i)
    }

    /// Single click: move the selection to the clicked row (select; never attach).
    pub fn mouse_select(
        &mut self,
        plan: &RenderPlan,
        col: u16,
        row: u16,
        state: &crate::state::State,
    ) {
        let Some(idx) = Self::row_at(plan, col, row) else {
            return;
        };
        if self.rows.get(idx).is_some() {
            self.note_user_move();
            self.set_selected(idx, state);
        }
    }

    /// Double click: selects the clicked row (the preceding single click already
    /// moved the selection; with select=attach there is no separate attach action).
    pub fn mouse_attach(
        &mut self,
        plan: &RenderPlan,
        col: u16,
        row: u16,
        state: &crate::state::State,
    ) {
        self.mouse_select(plan, col, row, state);
    }

    /// Scroll wheel: move the selection exactly as ↑/↓ do (`nav_vertical`) - one card up
    /// or down the flat list - so the wheel and the card step never diverge.
    pub fn mouse_scroll(&mut self, down: bool, state: &crate::state::State) {
        self.nav_vertical(if down { 1 } else { -1 }, state);
    }
}
