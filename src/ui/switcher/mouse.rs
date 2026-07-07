use super::*;

impl Switcher {
    // --- mouse --------------------------------------------------------------

    fn in_tree(&self, col: u16, row: u16) -> bool {
        self.tree_inner.contains(Position { x: col, y: row })
    }

    /// Single click: move the selection to the clicked row (select; never attach).
    pub fn mouse_select(&mut self, col: u16, row: u16, state: &crate::state::State) {
        if !self.in_tree(col, row) {
            return;
        }
        let offset = self.list_state.offset();
        let idx = offset + (row.saturating_sub(self.tree_inner.y)) as usize;
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

    /// Scroll wheel: move the selection (panes skipped) in the given direction.
    pub fn mouse_scroll(&mut self, down: bool, state: &crate::state::State) {
        self.move_selection(if down { 1 } else { -1 }, state);
    }

    // --- context menu -------------------------------------------------------

    /// Right-button press at 0-based screen (col,row): opens that tree row's menu if
    /// it lands on a selectable row that has items. Does NOT move the tree selection —
    /// the gesture only remembers the target, so no background attach fires mid-hold.
    /// Returns true iff a menu opened (so the app knows to consume the event).
    pub fn menu_open(&mut self, col: u16, row: u16, state: &mut crate::state::State) -> bool {
        if !self.in_tree(col, row) {
            return false;
        }
        let offset = self.list_state.offset();
        let idx = offset + (row.saturating_sub(self.tree_inner.y)) as usize;
        let Some(target) = self
            .rows
            .get(idx)
            .filter(|r| r.selectable())
            .map(|r| r.reference.clone())
        else {
            return false;
        };
        let items = modal::menu_items(&target);
        if items.is_empty() {
            return false;
        }
        let title = modal::menu_title(&target);
        let rect = modal::menu_rect(col, row, &items, &title, self.screen_area);
        // No item is pre-highlighted, and the box opens just below the pointer (see
        // menu_rect) — so an accidental right-click that releases without dragging onto
        // an item does nothing. Selecting is a deliberate move down onto an item.
        state.modal = Some(Modal::Menu(Menu {
            target,
            title,
            rect,
            items,
            hovered: None,
        }));
        true
    }

    /// Mouse moved while the menu is held: highlight the item under the selection. Over the
    /// box but off an item (the title border) keeps the current highlight; only dragging
    /// fully OUTSIDE the box clears it, so releasing there cancels.
    pub fn menu_hover(&mut self, col: u16, row: u16, state: &mut crate::state::State) {
        modal::menu_hover(&mut state.modal, col, row);
    }

    /// Right-button up: act on the hovered item against the (re-located) target row,
    /// then close the menu. Released off-menu (no hovered item) cancels. The target is
    /// re-found by identity so a rebuild during the hold can't act on a stale node.
    pub fn menu_release(&mut self, state: &mut crate::state::State) -> MenuOutcome {
        let Some(Modal::Menu(menu)) = state.modal.take() else {
            return MenuOutcome::None;
        };
        let Some(i) = menu.hovered else {
            return MenuOutcome::None;
        };
        let item = menu.items[i];
        let Some(idx) = self
            .rows
            .iter()
            .position(|r| same_node(&r.reference, &menu.target))
        else {
            return MenuOutcome::None;
        };
        // The delegated methods act on the current selection, so land it on the target,
        // run the action (which CAPTURES the target by value), then for everything
        // EXCEPT focus restore the selection. A lingering selection move would change the
        // selection → trigger an attach and the events it spawns → rebuild the tree,
        // which clears an armed kill confirm (pending_kill) before the user can answer
        // y/n, and needlessly switches the displayed session. focus is the one item
        // that intends to move there.
        let prior = self.capture_focus();
        self.user_moved = true;
        self.set_selected(idx, state);
        match item {
            MenuItem::Focus => {
                // For a window, optimistically mark it active in the cache. Otherwise the
                // terminal-view selection-follow (`select_active_window`, run before the attach's
                // select-window lands) would yank the selection back to the session's previous
                // active window — so focusing a different window of the already-displayed
                // session did nothing. The real select-window follows from the selection.
                if let RowRef::Window { sess, window } = &menu.target {
                    self.set_active_window(&sess.source, &sess.name, *window, state);
                }
                MenuOutcome::FocusTerminal
            }
            MenuItem::NewSession | MenuItem::NewWindow => {
                self.open_new(state);
                self.restore_focus(prior, state);
                MenuOutcome::Handled
            }
            MenuItem::Rename => {
                self.open_input(InputMode::Rename, state);
                self.restore_focus(prior, state);
                MenuOutcome::Handled
            }
            MenuItem::Kill => {
                self.arm_kill(state);
                self.restore_focus(prior, state);
                MenuOutcome::Handled
            }
        }
    }

    /// Close the menu without acting (app watchdog: a keystroke ends the gesture).
    /// Only a menu is cleared — a centered popup, if somehow open, is left intact.
    pub fn menu_cancel(&mut self, state: &mut crate::state::State) {
        if matches!(state.modal, Some(Modal::Menu(_))) {
            state.modal = None;
        }
    }
}
