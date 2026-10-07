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
        // The plan is frame-gated, so a box a keystroke closed can still have a rect in
        // it; only a box that is live can be grabbed. The modal popup paints above the
        // key list, so it is hit first.
        let modal = state
            .is_modal_popup_open()
            .then_some((modal::PopupSurface::Modal, plan.popup_rect));
        let key_list = plan
            .key_list
            .as_ref()
            .map(|(rect, _)| (modal::PopupSurface::KeyList, *rect));
        let boxes: Vec<_> = modal.into_iter().chain(key_list).collect();
        self.popup_geo.begin_drag(col, row, &boxes)
    }

    /// Ends a popup drag. A press released on the cell it grabbed is a click, and a click
    /// executes the help tab or the list item under it the way Enter executes the
    /// selection: a tab becomes the selection and scrolls its section's title to the
    /// top of the body, and an item becomes the selection and is marked for the
    /// switcher to act on as an Enter.
    pub fn end_popup_drag_in_plan(&mut self, plan: &RenderPlan, state: &mut crate::state::State) {
        let Some((col, row)) = self.popup_geo.end_drag() else {
            // A drag moved the popup under the pointer, so the hover set before
            // it names a cell the pointer may no longer be on. The next motion sets it
            // again from where the popup now is.
            if let Some(
                Modal::Help { hover, .. }
                | Modal::Check { hover, .. }
                | Modal::Palette { hover, .. },
            ) = &mut state.modal
            {
                *hover = None;
            }
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

    /// Sets the hover of the open popup to the help tab or the list item under
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
                let (target, line) = lines.get((row - inner.y) as usize)?;
                crate::ui::palette::standalone_bounds(line, inner.width)
                    .contains(&(col - inner.x))
                    .then_some(*target)
                    .flatten()
            }
            _ => None,
        }
    }

    fn in_tree(plan: &RenderPlan, col: u16, row: u16) -> bool {
        plan.nav_inner.contains(Position { x: col, y: row })
    }

    /// The nav target under a 0-based screen `(col, row)`: a card, or one half of a
    /// section title or of a host card's `{machine}/{mux}`. `None` outside the nav or on
    /// none of its targets (the gap between the bands, the band rule, the indent, a
    /// title's blank tail, the rows past the last card). A band's overflow count on the
    /// seam stands for the hidden card nearest the visible ones.
    ///
    /// Neither layout puts cards on a fixed row pitch - the side list parts its groups
    /// and its card heights vary, the portrait flow runs them into columns - so the plan
    /// records each card's rect and each half's, and the hit-test reads those back. One
    /// geometry, so a click cannot land on a target the renderer put elsewhere.
    fn target_at(&self, plan: &RenderPlan, col: u16, row: u16) -> Option<(usize, Part)> {
        if let Some(target) = plan.overflow_target(col, row) {
            return Some((target, Part::Card));
        }
        if !Self::in_tree(plan, col, row) {
            return None;
        }
        let at = Position { x: col, y: row };
        if let Some(&(i, part, _)) = plan.nav_parts.iter().find(|(_, _, rect)| rect.contains(at)) {
            return Some((i, part));
        }
        plan.nav_cells
            .iter()
            .find(|(_, rect)| rect.contains(at))
            .map(|(i, _)| *i)
            .filter(|&i| {
                !matches!(
                    self.rows.get(i).map(|r| &r.reference),
                    Some(RowRef::Section { .. })
                )
            })
            .map(|i| (i, Part::Card))
    }

    /// A click on a nav target: the target becomes the selection. Returns whether the
    /// click landed on one, so the caller can execute it.
    pub fn mouse_select(&mut self, plan: &RenderPlan, col: u16, row: u16) -> bool {
        let Some((idx, part)) = self.target_at(plan, col, row) else {
            return false;
        };
        if self.rows.get(idx).is_none() {
            return false;
        }
        self.note_user_move();
        self.hover = None;
        let before = self.selected_node();
        let node = node_of(&self.rows[idx].reference, part);
        if let Some(child) = before.filter(|b| b.parent().as_ref() == Some(&node)) {
            self.trail.insert(node, child);
        }
        self.set_target(Target {
            row: idx,
            part,
            deep: None,
        });
        true
    }

    /// The pointer resting at `(col, row)` while the nav holds the focus: the nav target
    /// under it becomes the hover, whose screen the terminal view shows. Off any
    /// target the hover ends and the selection's screen shows again.
    /// Returns whether the hover changed.
    pub fn mouse_hover(&mut self, plan: &RenderPlan, col: u16, row: u16) -> bool {
        let hover = if self.terminal_view {
            None
        } else {
            self.target_at(plan, col, row)
                .filter(|_| plan.overflow_target(col, row).is_none())
                .map(|(i, part)| (self.rows[i].reference.clone(), part))
        };
        let same = match (&self.hover, &hover) {
            (Some((a, p)), Some((b, q))) => p == q && same_node(a, b),
            (None, None) => true,
            _ => false,
        };
        if same {
            return false;
        }
        self.hover = hover;
        self.on_focus_changed();
        true
    }

    /// The link of the shown screen under `(col, row)`, as the latest frame painted it.
    pub fn link_at(plan: &RenderPlan, col: u16, row: u16) -> Option<usize> {
        let at = Position { x: col, y: row };
        plan.view_links
            .iter()
            .find(|(_, rect)| rect.contains(at))
            .map(|(i, _)| *i)
    }

    /// The pointer resting at `(col, row)` while the terminal view holds the focus: the
    /// link under it is the screen's hover. Returns whether it changed.
    pub fn link_hover_at(&mut self, plan: &RenderPlan, col: u16, row: u16) -> bool {
        // The landing screen is pickable from either view's focus.
        let hover = if self.terminal_view || self.landing {
            Self::link_at(plan, col, row)
        } else {
            None
        };
        if hover == self.link_hover {
            return false;
        }
        self.link_hover = hover;
        true
    }

    /// Scroll wheel: move the selection exactly as ↑/↓ do (`nav_vertical`) - one card up
    /// or down the flat list - so the wheel and the card step never diverge.
    pub fn mouse_scroll(&mut self, down: bool) {
        self.nav_vertical(if down { 1 } else { -1 });
    }
}
