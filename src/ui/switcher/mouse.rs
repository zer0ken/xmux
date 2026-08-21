use super::*;

impl Switcher {
    // --- mouse --------------------------------------------------------------

    fn in_tree(&self, col: u16, row: u16) -> bool {
        self.nav_inner.contains(Position { x: col, y: row })
    }

    /// The card index under a 0-based screen `(col, row)`, or `None` if it is outside the
    /// nav list or past its cards. Both layouts render one vertically-scrolling list of
    /// cards whose heights VARY (two rows expanded, one collapsed), so the screen-row
    /// delta walks `card_height` from the `list_state` offset (an item index). ratatui
    /// never partial-scrolls an item, so the first visible card always starts at the
    /// region's top edge.
    fn row_at(&self, col: u16, row: u16) -> Option<usize> {
        if !self.in_tree(col, row) {
            return None;
        }
        let mut row_in = row.saturating_sub(self.nav_inner.y);
        for i in self.list_state.offset()..self.rows.len() {
            let h = self.card_height(i);
            if row_in < h {
                return Some(i);
            }
            row_in -= h;
        }
        None
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
