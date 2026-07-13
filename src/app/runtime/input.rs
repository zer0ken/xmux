use super::*;

impl Runtime {
    /// Processes a batch of TREE-focus input bytes through ONE path — used for both real
    /// stdin and bytes replayed after a terminal→tree switch. Handles prefix arming
    /// (`C-g` then `q` → quit, `h`/`Ctrl+←` → shrink tree, `l`/`Ctrl+→` → grow tree),
    /// Enter → focus terminal (unless an inline input is open),
    /// ←/→ navigate the tree; then the off-loop op dispatch, ensure-current-host, and
    /// the `r` re-scan. Returns `(focus_terminal, quit, width_delta, toggle_auto_hide)`.
    /// The selection is committed at the loop top, so this only drives navigation +
    /// metadata, not the display. `width_changed` is the caller's out-flag.
    pub(super) fn handle_tree_bytes(
        &mut self,
        bytes: &[u8],
        width_changed: &mut bool,
    ) -> (bool, bool, i32, i32, bool) {
        // Split-borrow the world state into the loose names the body uses (a tree read
        // touches most of it: decoder, switcher/state, host orchestration, width prefs).
        let Self {
            tree_decoder,
            switcher,
            state,
            mgr,
            env,
            hosts,
            detecting,
            panes_requested,
            ops,
            op_tx,
            tree_width_natural,
            auto_hide_tree,
            cols,
            body_rows: rows,
            tree_width,
            mouse_state,
            prefix,
            ..
        } = self;
        let tree_armed = &mut mouse_state.tree_armed;
        let (prefix, cols, rows, tree_width) = (*prefix, *cols, *rows, *tree_width);
        let mut focus_terminal = false;
        let mut quit = false;
        let mut width_delta = 0i32;
        let mut height_delta = 0i32;
        let mut toggle_auto_hide = false;
        let mut key_cmds: Vec<crate::model::Command> = Vec::new();
        for key in tree_decoder.feed(bytes) {
            // Re-query per key: opening a modal popup (via a TreeKey applied below) flips
            // this, which changes how the next key in this same read resolves. Gating on
            // ANY modal popup (not just the inline input) makes a modal OWN its keys — a
            // kill-confirm swallows prefix/Enter, so `prefix q` can't quit and Enter can't
            // focus the terminal while a confirm is on screen; only y/n/Esc act on it.
            let is_inputting = state.is_modal_popup_open();
            match resolve_tree_key(key, tree_armed, prefix, is_inputting) {
                // A committed input/kill confirm folds through State::apply, which returns
                // its Commands; collect them and dispatch the whole batch below.
                Some(Action::TreeKey(k)) => key_cmds.extend(switcher.handle_key(k, state)),
                Some(Action::FocusTerminal) => focus_terminal = true,
                Some(Action::Quit) => quit = true,
                Some(Action::Width(d)) => width_delta = d,
                Some(Action::Height(d)) => height_delta = d,
                Some(Action::ToggleAutoHide) => toggle_auto_hide = true,
                Some(Action::ShowHelp) => switcher.toggle_help(state),
                // resolve_tree_key never emits the mux-only or terminal-only variants
                // (Forward/FocusTree/KillActivePane); None = armed/consumed.
                Some(Action::Forward(_))
                | Some(Action::FocusTree(_))
                | Some(Action::KillActivePane)
                | None => {}
            }
        }
        // Route the FULL command batch through the single dispatcher (not just RunOp): a
        // switcher key emits only RunOp today, but dispatch_commands handles every variant
        // so a future non-RunOp command is acted on, never silently dropped. quit/
        // width-change it reports merge into this function's outputs.
        let (cmd_quit, cmd_width_changed) = dispatch_commands(
            key_cmds,
            switcher,
            state,
            tree_width_natural,
            auto_hide_tree,
            &env.xmux_dir,
            (&*ops, &*op_tx),
        );
        quit |= cmd_quit;
        if cmd_width_changed {
            *width_changed = true;
        }
        ensure_current_host(mgr, hosts, switcher, cols, rows, tree_width);
        kick_rescan(switcher, hosts, detecting, mgr, panes_requested, cols, rows);
        (
            focus_terminal,
            quit,
            width_delta,
            height_delta,
            toggle_auto_hide,
        )
    }
}

/// Applies ONE parsed SGR mouse event to the gesture state + tree/registry — the body
/// of the inline `while i < bytes.len()` mouse branch, lifted verbatim. Runs the modal/
/// gesture gates (menu, view border drag, popup drag, modal swallow, view border grab, idle
/// hover, menu open) in the SAME order, then the focus×position routing. Mutates `st`
/// (the gesture latches), `state.focus` (mid-loop focus toggles — routing re-reads focus
/// per event, so deferring would change behavior), and the byte-loop accumulators
/// (`non_mouse`, `mouse_focus_toggle`, `wheel_scrolled`). Returns whether a redraw is
/// needed for this event.
impl Runtime {
    pub(super) fn handle_mouse_event(
        &mut self,
        ev: &crate::display::mouse::MouseEvent,
        selection: &Selection,
        non_mouse: &mut Vec<u8>,
        mouse_focus_toggle: &mut bool,
        wheel_scrolled: &mut bool,
        term_area: ratatui::layout::Rect,
    ) -> bool {
        // Split-borrow the world state into the loose names the (verbatim) gesture body uses.
        let Self {
            mouse_state: st,
            switcher,
            state,
            registry,
            mgr,
            env,
            hosts,
            detecting,
            panes_requested,
            tree_width_natural,
            tree_height,
            cols,
            body_rows,
            tree_width,
            ..
        } = self;
        let (cols, body_rows, tree_width) = (*cols, *body_rows, *tree_width);
        let mut dirty = false;
        let in_mux = to_grid_local(term_area, ev.col, ev.row);
        // A LEFT-button press in the UNFOCUSED view switches focus to that
        // view — focus only; the click is not delivered. Right-click is
        // reserved for the tree context menu, so it never moves focus.
        // Within the focused terminal view, the click forwards.
        let is_press = ev.pressed && (ev.cb & 0x60) == 0;
        // Wheel events carry the 0x40 bit (cb 64=up, 65=down; +16=Ctrl).
        let is_wheel = ev.pressed && (ev.cb & 0x40) != 0;
        // View border drag: grab the view border rule (the column at the effective
        // tree width, only when the tree is shown) with the left button and
        // drag to resize. Once grabbed it owns every mouse event until the
        // button is released. Sets the NATURAL width; the loop-top reconcile
        // applies it and resizes the PTYs (same path as prefix h/l).
        let col0 = ev.col.saturating_sub(1); // 1-based SGR → 0-based screen col
        let row0 = ev.row.saturating_sub(1);
        // The view border rect from the one shared geometry, so the grab / hover works in
        // either layout: a vertical rule in Side, a horizontal rule in Top. The drag then
        // resizes the tree WIDTH (Side, by column) or HEIGHT (Top, by row).
        let full = ratatui::layout::Rect::new(0, 0, cols, body_rows.saturating_add(1));
        let regions = crate::ui::switcher::compute_regions(full, tree_width, *tree_height, 1);
        let on_view_border = tree_width > 0
            && regions
                .view_border
                .contains(ratatui::layout::Position { x: col0, y: row0 });
        let top_layout = regions.layout == crate::ui::switcher::ViewLayout::Top;
        // A context menu owns every mouse event until the right
        // button is released (press-hold-release), exactly like the
        // view border drag below. Motion sets the hovered item; button-up
        // acts on it (or cancels if released off-menu).
        if state.menu_active() {
            if !ev.pressed {
                match switcher.menu_release(state) {
                    crate::ui::modal::MenuOutcome::FocusTerminal => {
                        // Connect the target's host (mirrors the left-click
                        // select path) so its control client streams, then
                        // focus the terminal on the now-selected session.
                        ensure_current_host(mgr, hosts, switcher, cols, body_rows, tree_width);
                        // Focus state is `Menu{prior}` here; set the restore view to the terminal
                        // so closing the menu (next loop-top sync_modal(None)) lands on it.
                        state.apply(crate::model::Action::Focus(
                            crate::model::FocusTarget::Terminal,
                        ));
                    }
                    crate::ui::modal::MenuOutcome::Handled => {
                        // A menu item only OPENS the next modal (input / kill confirm) — the
                        // actual mux op is committed later from that modal (Enter / y), which
                        // returns its RunOp through handle_key. Here just consume any re-scan
                        // (reconnect) kick and ensure the target's host is connected.
                        kick_rescan(
                            switcher,
                            hosts,
                            detecting,
                            mgr,
                            panes_requested,
                            cols,
                            body_rows,
                        );
                        ensure_current_host(mgr, hosts, switcher, cols, body_rows, tree_width);
                    }
                    crate::ui::modal::MenuOutcome::None => {}
                }
                dirty = true;
            } else if !is_wheel {
                switcher.menu_hover(col0, ev.row.saturating_sub(1), state);
                dirty = true;
            }
            return dirty;
        }
        if st.dragging_view_border {
            if !ev.pressed {
                // Button up ends the drag; persist the final size once (motion resizes live
                // but does not write per cell). Top drags the height, Side the width.
                st.dragging_view_border = false;
                if top_layout {
                    crate::prefs::save_tree_height(&env.xmux_dir, *tree_height);
                } else {
                    crate::prefs::save_tree_width(&env.xmux_dir, *tree_width_natural);
                }
            } else if !is_wheel {
                if top_layout {
                    let target = view_border_drag_height(ev.row);
                    if target != *tree_height {
                        *tree_height = target;
                        dirty = true;
                    }
                } else {
                    let target = view_border_drag_width(ev.col);
                    if target != *tree_width_natural {
                        *tree_width_natural = target;
                        dirty = true;
                    }
                }
            }
            return dirty;
        }
        let is_left_press = is_press && (ev.cb & 0x03) == 0;
        // A modal popup (help/input/confirm) moves when its border is
        // dragged. Once grabbed it owns every mouse event until release,
        // like the view border drag / menu hold above.
        if switcher.popup_drag_active() {
            if !ev.pressed {
                switcher.end_popup_drag();
            } else if !is_wheel {
                switcher.drag_popup(col0, ev.row.saturating_sub(1));
            }
            dirty = true;
            return dirty;
        }
        if is_left_press && switcher.begin_popup_drag(col0, ev.row.saturating_sub(1), state) {
            dirty = true;
            return dirty;
        }
        // A modal popup is mouse-modal: while one is open, every mouse
        // event that is not its border-drag (handled above) is swallowed,
        // so clicks, wheels, view border grabs, and hovers never reach the
        // tree/terminal/view border behind it.
        if state.is_modal_popup_open() {
            return dirty;
        }
        if is_left_press && on_view_border {
            st.dragging_view_border = true; // grabbed the view border
            return dirty;
        }
        // Idle motion (motion bit set, no button held) — reported only
        // because any-motion tracking (1003h) is on. Over the view border it
        // lights the hover cue and is consumed (nothing under it to forward).
        // Elsewhere it falls through to the routing below, so a hover over the
        // terminal view IS forwarded to the child (the inner app gets hover); over
        // the tree it is harmlessly dropped.
        if ev.pressed && (ev.cb & 0x23) == 0x23 {
            let over_view_border = on_view_border;
            if over_view_border != st.hovered_view_border {
                st.hovered_view_border = over_view_border;
                dirty = true;
            }
            if over_view_border {
                return dirty;
            }
        }
        // Right-button press over a selectable tree row opens its context
        // menu (press-hold-release). Tree-focus only: the menu acts on a
        // tree row, so it is tree-view input, not a global — a right-click
        // while the terminal view is focused (or over the terminal view) does not open it
        // and does not move focus. The menu's keyboard actions (rename input,
        // kill confirm) thus always run in tree focus, so a confirmed kill
        // can't quit the app out from under the mux.
        let is_right_press = is_press && (ev.cb & 0x03) == 2;
        if tree_menu_may_open(
            is_right_press,
            state.focus.is_tree_focused(),
            in_mux.is_some(),
        ) && switcher.menu_open(col0, ev.row.saturating_sub(1), state)
        {
            dirty = true;
            return dirty;
        }
        let down = (ev.cb & 0x01) != 0;
        let ctrl = (ev.cb & 0x10) != 0;
        match resolve_mouse_chain(
            is_wheel,
            ctrl,
            down,
            is_left_press,
            state.focus.is_tree_focused(),
            in_mux.is_some(),
        ) {
            ChainAction::ScrollTree(down) => {
                // Plain wheel → scroll the selection LINEARLY through every row
                // (move_selection), like any list. NOT sibling-cycle: arrows do
                // that (move_sibling), but it wraps within a level, so a 2-sibling
                // level just bounces — the "two notches per move" report.
                switcher.mouse_scroll(down, state);
                *wheel_scrolled = true;
                dirty = true;
            }
            ChainAction::LevelChange(down) => {
                // Ctrl+wheel → change level (↑ ascend / ↓ descend); inject the
                // arrow so the tree path (decode → handle_key → ensure) drives it.
                non_mouse.extend_from_slice(if down { b"\x1b[C" } else { b"\x1b[D" });
            }
            // The unfocused view was clicked → switch focus to it (no content
            // delivered); toggle flips Focus::Tree⇄Focus::Terminal either direction.
            ChainAction::FocusTerminal | ChainAction::FocusTree => {
                state.apply(crate::model::Action::FocusToggle);
                *mouse_focus_toggle = true;
            }
            ChainAction::SelectRow => {
                // Left-click a tree row → move the selection to it (select). The
                // loop top commits the new selection (attach); ensure the
                // clicked row's host connects so its subtree streams in.
                switcher.mouse_select(col0, ev.row.saturating_sub(1), state);
                ensure_current_host(mgr, hosts, switcher, cols, body_rows, tree_width);
                dirty = true;
            }
            ChainAction::ForwardToMux => {
                if let Some((gc, gr)) = in_mux {
                    registry.input(
                        &display_key(hosts, selection),
                        crate::display::mouse::encode_sgr_mouse(ev, gc, gr),
                    );
                }
            }
            ChainAction::Nothing => {}
        }
        dirty
    }
}

impl Runtime {
    /// Applies a tree-resize delta on ONE axis, gated to the layout that actually shows that
    /// axis so a key never resizes a dimension the user cannot see: `horizontal` (←/→ · h/l)
    /// resizes the WIDTH only in Side, `!horizontal` (↑/↓) the HEIGHT only in Top; the
    /// perpendicular axis is a no-op. Height is seeded from the effective auto height the
    /// first time (while `tree_height == 0`) so a relative step starts from what is on screen,
    /// clamped so the terminal keeps room, and persisted; width defers to `apply_width_delta`
    /// (the caller schedules the debounced persist). Returns whether the size changed.
    pub(super) fn resize_axis(&mut self, horizontal: bool, delta: i32) -> bool {
        let top = self.switcher.layout() == crate::ui::switcher::ViewLayout::Top;
        match (horizontal, top) {
            (true, false) => apply_width_delta(delta, &mut self.tree_width_natural),
            (false, true) => {
                let base = if self.tree_height == 0 {
                    crate::ui::switcher::default_tree_height(self.body_rows)
                } else {
                    self.tree_height
                };
                let ceil = self
                    .body_rows
                    .saturating_sub(2)
                    .clamp(TREE_HEIGHT_MIN, TREE_HEIGHT_MAX);
                let next = (base as i32 + delta).clamp(TREE_HEIGHT_MIN as i32, ceil as i32) as u16;
                if next == self.tree_height {
                    return false;
                }
                self.tree_height = next;
                crate::prefs::save_tree_height(&self.env.xmux_dir, self.tree_height);
                true
            }
            _ => false, // perpendicular axis for this layout: nothing to resize
        }
    }

    /// A keyboard resize step: apply the delta on its axis (no-op for zero, or for the
    /// perpendicular axis of the current layout) and open the bare-Ctrl-arrow repeat window
    /// so the next arrows keep resizing without re-pressing the prefix. Returns whether the
    /// size changed (for the debounced persist).
    fn resize_and_repeat(&mut self, horizontal: bool, delta: i32) -> bool {
        if delta == 0 {
            return false;
        }
        let changed = self.resize_axis(horizontal, delta);
        self.mouse_state.repeat_until =
            Some(std::time::Instant::now() + std::time::Duration::from_millis(RESIZE_REPEAT_MS));
        changed
    }

    /// The whole `stdin_rx` arm body, lifted. Scans the read for SGR mouse sequences
    /// (routed via [`Runtime::handle_mouse_event`]) vs a non-mouse byte stream, runs the
    /// lost-release watchdogs, the resize-repeat window, and the help-modal / tree-focus /
    /// terminal-view focus routing — in the SAME order as the inline arm. The final focus
    /// toggles (+ replay) run on `self.state.focus`, so the caller only acts on the returned
    /// `dirty`/`quit`. No behavior change.
    pub(super) fn handle_stdin_bytes(
        &mut self,
        bytes: &[u8],
        selection: &Selection,
    ) -> StdinOutcome {
        use std::time::Duration;
        let mut outcome = StdinOutcome::default();
        let StdinOutcome {
            quit,
            focus_terminal,
            focus_tree,
            dirty,
            tree_replay,
            width_changed,
        } = &mut outcome;
        // Scan for SGR mouse sequences BEFORE routing to Focus::Tree/Focus::Terminal branches.
        // Mouse capture is global, so mouse bytes arrive in both states; scanning here
        // prevents them from reaching handle_tree_bytes (which would mis-decode them)
        // or TermInput's prefix logic. Split into: mouse events + non-mouse byte stream.
        // Edge case: a sequence split across reads parses as None and falls into
        // non_mouse — rare in practice; no cross-read buffering in v1.
        // The terminal region from the one shared geometry, so a click lands on exactly
        // what was drawn in either layout (in Top the terminal sits below the tree, not
        // to the right of it).
        let full = ratatui::layout::Rect::new(0, 0, self.cols, self.body_rows.saturating_add(1));
        let term_area =
            crate::ui::switcher::compute_regions(full, self.tree_width, self.tree_height, 1)
                .terminal;
        let mut non_mouse: Vec<u8> = Vec::with_capacity(bytes.len());
        let mut mouse_focus_toggle = false;
        let mut wheel_scrolled = false;
        {
            let mut i = 0;
            while i < bytes.len() {
                if let Some((ev, len)) = crate::display::mouse::parse_sgr_mouse(&bytes[i..]) {
                    if self.handle_mouse_event(
                        &ev,
                        selection,
                        &mut non_mouse,
                        &mut mouse_focus_toggle,
                        &mut wheel_scrolled,
                        term_area,
                    ) {
                        *dirty = true;
                    }
                    i += len;
                } else {
                    non_mouse.push(bytes[i]);
                    i += 1;
                }
            }
        }
        // Watchdog: a view border drag is normally ended by the button-up event, but a
        // release can be lost (split across reads, released off-window, or a terminal
        // that omits it) — which would strand `dragging_view_border` and eat all later
        // mouse input. Any non-mouse byte (a keystroke, or the split release's own
        // leftover bytes) ends the drag and persists the final width, so the user is
        // never trapped past the next input.
        if self.mouse_state.dragging_view_border && !non_mouse.is_empty() {
            self.mouse_state.dragging_view_border = false;
            // The recovery doesn't track which axis was dragging; persist both (a no-op file
            // write for the unchanged one) so the final size is never lost.
            crate::prefs::save_tree_width(&self.env.xmux_dir, self.tree_width_natural);
            crate::prefs::save_tree_height(&self.env.xmux_dir, self.tree_height);
        }
        // Watchdog: a keystroke (or any non-mouse byte) during a held menu ends
        // the gesture without acting — mirrors the view border-drag watchdog, so a
        // missed button-up can't strand the menu and eat later input.
        if self.state.menu_active() && !non_mouse.is_empty() {
            self.switcher.menu_cancel(&mut self.state);
            non_mouse.clear();
            *dirty = true;
        }
        // Watchdog: same recovery for a popup border-drag — a lost button-up
        // must not strand `popup_drag` and eat all later mouse input.
        if self.switcher.popup_drag_active() && !non_mouse.is_empty() {
            self.switcher.end_popup_drag();
            *dirty = true;
        }
        if mouse_focus_toggle {
            *dirty = true;
        }
        if wheel_scrolled {
            // The plain-wheel scroll moved the selection; connect the host it landed on
            // so its subtree streams in (mirrors handle_tree_bytes's ensure step).
            ensure_current_host(
                &mut self.mgr,
                &self.hosts,
                &self.switcher,
                self.cols,
                self.body_rows,
                self.tree_width,
            );
        }
        // Resize-repeat: while the window from a prefix-driven resize is open, a
        // bare Ctrl+←/→ (no prefix, in either focus) keeps resizing and refreshes
        // the window. Gated on NOT being mid-prefix (an armed prefix's next key is
        // a command, not a repeat — else skipping the input path would leave the
        // prefix armed and mis-read the following key). A pure-mouse read (empty
        // non_mouse) leaves the window untouched. Leading Ctrl-arrows are peeled off
        // (handles a coalesced autorepeat burst); any remaining bytes end the window
        // and fall through to the normal tree/terminal routing below.
        let mut consumed_by_repeat = false;
        if self
            .mouse_state
            .repeat_until
            .is_some_and(|d| std::time::Instant::now() < d)
            && !self.mouse_state.tree_armed
            && !self.term_input.is_armed()
            && !non_mouse.is_empty()
        {
            let mut n = 0;
            while let Some((horizontal, d, len)) = leading_ctrl_arrow(&non_mouse[n..]) {
                if self.resize_axis(horizontal, d) {
                    *width_changed = true;
                }
                n += len;
            }
            if n > 0 {
                non_mouse.drain(0..n);
                *dirty = true;
                if non_mouse.is_empty() {
                    self.mouse_state.repeat_until =
                        Some(std::time::Instant::now() + Duration::from_millis(RESIZE_REPEAT_MS));
                    consumed_by_repeat = true;
                } else {
                    self.mouse_state.repeat_until = None; // trailing non-arrow bytes end + route below
                }
            } else {
                self.mouse_state.repeat_until = None; // first key isn't a Ctrl-arrow → end the window
            }
        }
        if !consumed_by_repeat
            && !non_mouse.is_empty()
            && self.switcher.feed_help_key(&non_mouse, &mut self.state)
        {
            // The help modal is modal (tmux view-mode style): while open it
            // captures every key in EITHER focus — q/Esc closes it, the rest are
            // swallowed — so nothing leaks to the tree or the terminal view. Above the
            // tree/terminal split so the behavior is identical regardless of focus.
            *dirty = true;
        } else if !consumed_by_repeat
            && (self.state.focus.is_tree_focused() || self.state.focus.is_modal())
        {
            // Tree view OR any modal: route to the switcher path. A modal popup (input /
            // kill-confirm) opened from EITHER view owns its keys here; the resolver gating
            // in handle_tree_bytes swallows everything but the modal's own keys, so a modal
            // never emits FocusTerminal/quit and the focus toggles below never fire mid-modal.
            let (ft, q, wd, hd, th) = self.handle_tree_bytes(&non_mouse, width_changed);
            *focus_terminal = ft;
            *quit = q;
            // A prefix-driven resize: width (←/→ · h/l) or height (↑/↓); each applies only in
            // its layout, and opens the bare-Ctrl-arrow repeat window.
            let rw = self.resize_and_repeat(true, wd);
            let rh = self.resize_and_repeat(false, hd);
            if rw || rh {
                *width_changed = true;
            }
            if th {
                toggle_auto_hide(&mut self.auto_hide_tree, &self.env.xmux_dir);
                *dirty = true;
            }
        } else if !consumed_by_repeat {
            // TERMINAL focus: forward raw bytes to the selected session's PTY;
            // TermInput intercepts the prefix (→ tree / quit / help / resize / literal).
            for action in self.term_input.feed(&non_mouse) {
                match action {
                    // Forward keystrokes to the VISIBLE session (`displayed`), not the
                    // selection: until the new session is ready the prior one is on screen,
                    // so input must reach what the user actually sees (no blind typing).
                    Action::Forward(f) => self
                        .registry
                        .input(&display_key(&self.hosts, &self.state.displayed), f),
                    Action::FocusTree(rest) => {
                        *focus_tree = true;
                        *tree_replay = rest;
                    }
                    Action::Quit => *quit = true,
                    Action::ShowHelp => {
                        self.switcher.toggle_help(&mut self.state);
                        *dirty = true;
                    }
                    // Same resize + repeat-window as the tree path, so a resize started from
                    // the terminal view chains with bare Ctrl-arrows too. Width = ←/→ (Side),
                    // height = ↑/↓ (Top).
                    Action::Width(d) => {
                        if self.resize_and_repeat(true, d) {
                            *width_changed = true;
                        }
                    }
                    Action::Height(d) => {
                        if self.resize_and_repeat(false, d) {
                            *width_changed = true;
                        }
                    }
                    Action::ToggleAutoHide => {
                        toggle_auto_hide(&mut self.auto_hide_tree, &self.env.xmux_dir);
                        *dirty = true;
                    }
                    // prefix n/R/x/r reach here from terminal focus: run them through the
                    // switcher exactly like the tree path. handle_key opens the modal on the
                    // displayed session (n/R/x) or arms the re-scan (r); a committing key
                    // (Enter / y) then routes via the modal path (is_modal) on the next read.
                    // `r` only sets the re-scan flag, so kick_rescan must fire it — the tree
                    // path (handle_tree_bytes) runs the same tail after every read.
                    Action::TreeKey(k) => {
                        let cmds = self.switcher.handle_key(k, &mut self.state);
                        let (cq, cwc) = dispatch_commands(
                            cmds,
                            &mut self.switcher,
                            &mut self.state,
                            &mut self.tree_width_natural,
                            &mut self.auto_hide_tree,
                            &self.env.xmux_dir,
                            (&self.ops, &self.op_tx),
                        );
                        *quit |= cq;
                        if cwc {
                            *width_changed = true;
                        }
                        kick_rescan(
                            &mut self.switcher,
                            &self.hosts,
                            &mut self.detecting,
                            &mut self.mgr,
                            &mut self.panes_requested,
                            self.cols,
                            self.body_rows,
                        );
                        *dirty = true;
                    }
                    // prefix x from the terminal view: arm a kill confirm for the ACTIVE
                    // pane of the displayed session (not the tree selection). The confirm
                    // draws over the terminal view and owns the next read; y/n routes
                    // through the modal path like any other kill confirm.
                    Action::KillActivePane => {
                        self.switcher.arm_kill_active_pane(&mut self.state);
                        *dirty = true;
                    }
                    // TermInput never emits FocusTerminal (that is the tree-focus path).
                    Action::FocusTerminal => {}
                }
            }
        }
        if *focus_terminal {
            self.state.apply(crate::model::Action::Focus(
                crate::model::FocusTarget::Terminal,
            ));
            // No term.clear(): both states draw the SAME split layout (only the
            // view border colour changes), so clearing would blank the screen and
            // force a full repaint for nothing.
        }
        if *focus_tree {
            self.state
                .apply(crate::model::Action::Focus(crate::model::FocusTarget::Tree));
            if !tree_replay.is_empty() {
                let (ft, q, wd, hd, th) = self.handle_tree_bytes(tree_replay, width_changed);
                if ft {
                    self.state.apply(crate::model::Action::Focus(
                        crate::model::FocusTarget::Terminal,
                    ));
                }
                *quit = *quit || q;
                // A prefix-driven resize on the replayed bytes: same as the direct path above.
                let rw = self.resize_and_repeat(true, wd);
                let rh = self.resize_and_repeat(false, hd);
                if rw || rh {
                    *width_changed = true;
                }
                if th {
                    toggle_auto_hide(&mut self.auto_hide_tree, &self.env.xmux_dir);
                    *dirty = true;
                }
            }
        }
        outcome
    }
}
