use super::*;

impl Switcher {
    // --- mouse --------------------------------------------------------------

    /// Begins a popup drag against the rectangle painted for the latest frame.
    pub fn begin_popup_drag_in_plan(
        &mut self,
        plan: &RenderPlan,
        col: u16,
        row: u16,
        state: &crate::state::State,
    ) -> bool {
        self.popup_geo.rect = plan.popup_rect;
        self.begin_popup_drag(col, row, state)
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
