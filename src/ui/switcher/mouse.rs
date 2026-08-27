use super::*;

impl Switcher {
    // --- mouse --------------------------------------------------------------

    fn in_tree(&self, col: u16, row: u16) -> bool {
        self.nav_inner.contains(Position { x: col, y: row })
    }

    /// The card index under a 0-based screen `(col, row)`, or `None` if it is outside the
    /// nav or on none of its cards (the gap between the bands, the band rule, the
    /// scrollbar strip, the rows past the last card).
    ///
    /// Neither layout puts cards on a fixed row pitch - the side list parts its two bands
    /// and its card heights vary, the portrait flow runs them into columns - so the paint
    /// records each card's rect and the hit-test reads those back. One geometry, so a
    /// click cannot land on a card the renderer put elsewhere.
    fn row_at(&self, col: u16, row: u16) -> Option<usize> {
        if !self.in_tree(col, row) {
            return None;
        }
        let at = Position { x: col, y: row };
        self.nav_cells
            .iter()
            .find(|(_, rect)| rect.contains(at))
            .map(|(i, _)| *i)
    }

    /// Single click: move the selection to the clicked row (select; never attach).
    pub fn mouse_select(&mut self, col: u16, row: u16, state: &crate::state::State) {
        let Some(idx) = self.row_at(col, row) else {
            return;
        };
        if self.rows.get(idx).is_some_and(Row::selectable) {
            self.user_moved = true;
            self.set_selected(idx, state);
        }
    }

    /// Double click: selects the clicked row (the preceding single click already
    /// moved the selection; with select=attach there is no separate attach action).
    pub fn mouse_attach(&mut self, col: u16, row: u16, state: &crate::state::State) {
        self.mouse_select(col, row, state);
    }

    /// Scroll wheel: move the selection exactly as ↑/↓ do (`nav_vertical`) - one card up
    /// or down the flat list - so the wheel and the arrows never diverge.
    pub fn mouse_scroll(&mut self, down: bool, state: &crate::state::State) {
        self.nav_vertical(if down { 1 } else { -1 }, state);
    }
}
