use super::*;

impl Switcher {
    // --- mouse --------------------------------------------------------------

    /// Begins a popup drag against the rectangle painted for the latest frame. A press on
    /// one of the help's tabs selects that tab instead and starts no drag: the tabs are
    /// the one thing in a popup that takes a click, so the rest of the box, the gaps on
    /// the tab row included, stays its handle.
    pub fn begin_popup_drag_in_plan(
        &mut self,
        plan: &RenderPlan,
        col: u16,
        row: u16,
        state: &mut crate::state::State,
    ) -> bool {
        if Self::click_help_tab(plan, col, row, state) {
            return false;
        }
        let key_list = plan.key_list.as_ref().map(|(rect, _)| *rect);
        self.popup_geo.rect = if plan.popup_rect.is_empty() {
            key_list.unwrap_or_default()
        } else {
            plan.popup_rect
        };
        let open = state.is_modal_popup_open() || key_list.is_some();
        self.begin_popup_drag(col, row, open)
    }

    /// Selects the help tab under `(col, row)` on the popup the plan painted, scrolling its
    /// section's title to the top of the body. Returns whether the press was on a tab.
    fn click_help_tab(
        plan: &RenderPlan,
        col: u16,
        row: u16,
        state: &mut crate::state::State,
    ) -> bool {
        let rect = plan.popup_rect;
        let (prefix, position) = (&state.chrome.ui_prefix, state.chrome.nav_position);
        let Some(Modal::Help {
            query, scroll, tab, ..
        }) = &mut state.modal
        else {
            return false;
        };
        let inner = Rect::new(
            rect.x.saturating_add(1),
            rect.y.saturating_add(1),
            rect.width.saturating_sub(2),
            rect.height.saturating_sub(2),
        );
        if row != inner.y.saturating_add(modal::HELP_TAB_ROW)
            || !inner.contains(Position { x: col, y: row })
        {
            return false;
        }
        let Some(chosen) = modal::help_tab_at(
            prefix,
            position,
            query,
            *scroll,
            *tab,
            inner.width,
            inner.height,
            col - inner.x,
        ) else {
            return false;
        };
        let map = modal::help_map(prefix, position, query, inner.width, inner.height);
        *tab = Some(chosen);
        *scroll = map.scroll_to(chosen);
        true
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
