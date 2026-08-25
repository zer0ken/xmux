use super::*;

impl Runtime {
    /// Processes a batch of NAV-focus input bytes through ONE path — used for both real
    /// stdin and bytes replayed after a terminal→nav switch. Handles prefix arming
    /// (`C-g` then `q` → quit, `h`/`Ctrl+←` → shrink the nav, `l`/`Ctrl+→` → grow the nav),
    /// Enter → focus terminal (unless an inline input is open),
    /// ←/→ navigate the nav; then the off-loop op dispatch, ensure-current-host, and
    /// the `r` re-scan. Returns `(focus_terminal, quit, width_delta, toggle_auto_hide)`.
    /// The selection is committed at the loop top, so this only drives navigation +
    /// metadata, not the display. `width_changed` is the caller's out-flag.
    pub(super) fn handle_nav_bytes(
        &mut self,
        bytes: &[u8],
        width_changed: &mut bool,
    ) -> (bool, bool, i32, i32, bool) {
        // Split-borrow the world state into the loose names the body uses (a nav read
        // touches most of it: decoder, switcher/state, host orchestration, width prefs).
        let Self {
            nav_decoder,
            switcher,
            state,
            mgr,
            env,
            hosts,
            detecting,
            panes_requested,
            ops,
            op_tx,
            nav_width_natural,
            auto_hide_nav,
            cols,
            body_rows: rows,
            nav_width,
            mouse_state,
            prefix,
            ..
        } = self;
        let nav_armed = &mut mouse_state.nav_armed;
        let (prefix, cols, rows, nav_width) = (*prefix, *cols, *rows, *nav_width);
        let mut focus_terminal = false;
        let mut quit = false;
        let mut width_delta = 0i32;
        let mut height_delta = 0i32;
        let mut toggle_auto_hide = false;
        let mut key_cmds: Vec<crate::model::Command> = Vec::new();
        for key in nav_decoder.feed(bytes) {
            // Re-query per key: opening a modal popup (via a NavKey applied below) flips
            // this, which changes how the next key in this same read resolves. Gating on
            // ANY modal popup (not just the inline input) makes a modal OWN its keys: the
            // help modal and the inline input both swallow prefix/Enter, so `prefix q`
            // can't quit and Enter can't focus the terminal while one is on screen.
            let is_inputting = state.is_modal_popup_open();
            match resolve_nav_key(key, nav_armed, prefix, is_inputting) {
                // A committed input/kill confirm folds through State::apply, which returns
                // its Commands; collect them and dispatch the whole batch below.
                Some(Action::NavKey(k)) => key_cmds.extend(switcher.handle_key(k, state)),
                Some(Action::FocusTerminal) => focus_terminal = true,
                Some(Action::Quit) => quit = true,
                Some(Action::Width(d)) => width_delta = d,
                Some(Action::Height(d)) => height_delta = d,
                Some(Action::ToggleAutoHide) => toggle_auto_hide = true,
                Some(Action::ShowHelp) => switcher.toggle_help(state),
                // resolve_nav_key never emits the mux-only or terminal-only variants
                // (Forward/FocusNav); None = armed/consumed.
                Some(Action::Forward(_)) | Some(Action::FocusNav(_)) | None => {}
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
            nav_width_natural,
            auto_hide_nav,
            &env.xmux_dir,
            (&*ops, &*op_tx),
        );
        quit |= cmd_quit;
        if cmd_width_changed {
            *width_changed = true;
        }
        ensure_current_host(mgr, hosts, switcher, cols, rows, nav_width);
        kick_rescan(
            env,
            switcher,
            hosts,
            detecting,
            mgr,
            panes_requested,
            (cols, rows),
        );
        (
            focus_terminal,
            quit,
            width_delta,
            height_delta,
            toggle_auto_hide,
        )
    }
}

/// Applies ONE parsed SGR mouse event to the gesture state + nav/registry — the body
/// of the inline `while i < bytes.len()` mouse branch, lifted verbatim. Runs the modal/
/// gesture gates (view border drag, popup drag, modal swallow, view border grab, idle
/// hover) in the SAME order, then the focus×position routing. Mutates `st`
/// (the gesture latches), `state.focus` (mid-loop focus toggles — routing re-reads focus
/// per event, so deferring would change behavior), and the byte-loop accumulators
/// (`mouse_focus_toggle`, `wheel_scrolled`). Returns whether a redraw is
/// needed for this event.
impl Runtime {
    pub(super) fn handle_mouse_event(
        &mut self,
        ev: &crate::display::mouse::MouseEvent,
        selection: &Selection,
        mouse_focus_toggle: &mut bool,
        wheel_scrolled: &mut bool,
        term_area: ratatui::layout::Rect,
    ) -> bool {
        // Split-borrow the world state into the loose names the (verbatim) gesture body uses.
        let Self {
            mouse_state: st,
            term_input,
            switcher,
            state,
            registry,
            mgr,
            env,
            hosts,
            nav_width_natural,
            nav_height,
            cols,
            body_rows,
            nav_width,
            ..
        } = self;
        let (cols, body_rows, nav_width) = (*cols, *body_rows, *nav_width);
        let mut dirty = false;
        // A prefix is armed only until the next INPUT, and a mouse action is input. Mouse
        // bytes are scanned out of the stream before either focus path's key handling sees
        // them, so the disarm happens here or not at all - and a chord left half-open keeps
        // its cheatsheet floating over the window, then eats the next key as a command the
        // user meant for the pane. Bare hover is not an action: the pointer drifting across
        // the screen must not break a chord that is still being typed.
        let idle_motion = ev.pressed && (ev.cb & 0x23) == 0x23;
        if !idle_motion && (st.nav_armed || term_input.is_armed()) {
            st.nav_armed = false;
            term_input.disarm();
            dirty = true;
        }
        let in_mux = to_grid_local(term_area, ev.col, ev.row);
        // A LEFT-button press in the UNFOCUSED view switches focus to that
        // view: focus only, the click is not delivered. Within the focused
        // terminal view, the click forwards.
        let is_press = ev.pressed && (ev.cb & 0x60) == 0;
        // Wheel events carry the 0x40 bit (cb 64=up, 65=down; +16=Ctrl).
        let is_wheel = ev.pressed && (ev.cb & 0x40) != 0;
        // View border drag: grab the view border rule (the column at the effective
        // nav width, only when the nav is shown) with the left button and
        // drag to resize. Once grabbed it owns every mouse event until the
        // button is released. Sets the NATURAL width; the loop-top reconcile
        // applies it and resizes the PTYs (same path as prefix h/l).
        let col0 = ev.col.saturating_sub(1); // 1-based SGR → 0-based screen col
        let row0 = ev.row.saturating_sub(1);
        // The view border rect from the one shared geometry, so the grab / hover works in
        // either layout: a vertical rule in Side, a horizontal rule in Top. The drag then
        // resizes the nav WIDTH (Side, by column) or HEIGHT (Top, by row).
        let full = ratatui::layout::Rect::new(0, 0, cols, body_rows.saturating_add(1));
        let regions = crate::ui::switcher::compute_regions(
            full,
            crate::ui::switcher::NavSize {
                natural: *nav_width_natural,
                width: nav_width,
                height: *nav_height,
            },
            1,
        );
        let on_view_border = nav_width > 0
            && regions
                .view_border
                .contains(ratatui::layout::Position { x: col0, y: row0 });
        let top_layout = regions.layout == crate::ui::switcher::ViewLayout::Top;
        if st.dragging_view_border {
            if !ev.pressed {
                // Button up ends the drag; persist the final size once (motion resizes live
                // but does not write per cell). Top drags the height, Side the width.
                st.dragging_view_border = false;
                if top_layout {
                    crate::prefs::save_nav_height(&env.xmux_dir, *nav_height);
                } else {
                    crate::prefs::save_nav_width(&env.xmux_dir, *nav_width_natural);
                }
            } else if !is_wheel {
                if top_layout {
                    let target = view_border_drag_height(ev.row);
                    if target != *nav_height {
                        *nav_height = target;
                        dirty = true;
                    }
                } else {
                    let target = view_border_drag_width(ev.col);
                    if target != *nav_width_natural {
                        *nav_width_natural = target;
                        dirty = true;
                    }
                }
            }
            return dirty;
        }
        let is_left_press = is_press && (ev.cb & 0x03) == 0;
        // A modal popup (help/input/confirm) moves when its border is
        // dragged. Once grabbed it owns every mouse event until release,
        // like the view border drag above.
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
        // nav/terminal/view border behind it.
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
        // the nav it is harmlessly dropped.
        if idle_motion {
            let over_view_border = on_view_border;
            if over_view_border != st.hovered_view_border {
                st.hovered_view_border = over_view_border;
                dirty = true;
            }
            if over_view_border {
                return dirty;
            }
        }
        let down = (ev.cb & 0x01) != 0;
        match resolve_mouse_chain(
            is_wheel,
            down,
            is_left_press,
            state.focus.is_nav_focused(),
            in_mux.is_some(),
        ) {
            ChainAction::ScrollNav(down) => {
                // Plain wheel → scroll the selection LINEARLY through every row
                // (move_selection), like any list. NOT sibling-cycle: arrows do
                // that (move_sibling), but it wraps within a level, so a 2-sibling
                // level just bounces — the "two notches per move" report.
                switcher.mouse_scroll(down, state);
                *wheel_scrolled = true;
                dirty = true;
            }
            // The unfocused view was clicked → switch focus to it (no content
            // delivered); toggle flips Focus::Nav⇄Focus::Terminal either direction.
            ChainAction::FocusTerminal | ChainAction::FocusNav => {
                state.apply(crate::model::Action::FocusToggle);
                *mouse_focus_toggle = true;
            }
            ChainAction::SelectRow => {
                // Left-click a nav row → move the selection to it (select). The
                // loop top commits the new selection (attach); ensure the
                // clicked row's host connects so its subtree streams in.
                switcher.mouse_select(col0, ev.row.saturating_sub(1), state);
                ensure_current_host(mgr, hosts, switcher, cols, body_rows, nav_width);
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
    /// Applies a nav-resize delta on ONE axis, gated to the layout that actually shows that
    /// axis so a key never resizes a dimension the user cannot see: `horizontal` (←/→ · h/l)
    /// resizes the WIDTH only in Side, `!horizontal` (↑/↓) the HEIGHT only in Top; the
    /// perpendicular axis is a no-op. Height is seeded from the effective auto height the
    /// first time (while `nav_height == 0`) so a relative step starts from what is on screen,
    /// clamped so the terminal keeps room, and persisted; width defers to `apply_width_delta`
    /// (the caller schedules the debounced persist). Returns whether the size changed.
    pub(super) fn resize_axis(&mut self, horizontal: bool, delta: i32) -> bool {
        let top = self.switcher.layout() == crate::ui::switcher::ViewLayout::Top;
        match (horizontal, top) {
            (true, false) => apply_width_delta(delta, &mut self.nav_width_natural),
            (false, true) => {
                let base = if self.nav_height == 0 {
                    crate::ui::switcher::default_nav_height(self.body_rows)
                } else {
                    self.nav_height
                };
                let ceil = self
                    .body_rows
                    .saturating_sub(2)
                    .clamp(NAV_HEIGHT_MIN, NAV_HEIGHT_MAX);
                let next = (base as i32 + delta).clamp(NAV_HEIGHT_MIN as i32, ceil as i32) as u16;
                if next == self.nav_height {
                    return false;
                }
                self.nav_height = next;
                crate::prefs::save_nav_height(&self.env.xmux_dir, self.nav_height);
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
    /// lost-release watchdogs, the resize-repeat window, and the help-modal / nav-focus /
    /// terminal-view focus routing — in the SAME order as the inline arm. The final focus
    /// toggles (+ replay) run on `self.state.focus`, so the caller only acts on the returned
    /// `dirty`/`quit`. No behavior change.
    pub(super) fn handle_stdin_bytes(
        &mut self,
        bytes: &[u8],
        selection: &Selection,
    ) -> StdinOutcome {
        use std::time::Duration;
        // The hint bar swaps between the resting prefix and the armed cheatsheet, so an
        // arm/disarm is a VISIBLE change even when the read moves nothing else. Snapshot
        // it here and mark the frame dirty below if it flipped, or the cheatsheet would
        // only appear on the next unrelated redraw (a poll tick).
        let armed_before = self.armed();
        let mut outcome = StdinOutcome::default();
        let StdinOutcome {
            quit,
            focus_terminal,
            focus_nav,
            dirty,
            nav_replay,
            width_changed,
        } = &mut outcome;
        // Scan for SGR mouse sequences BEFORE routing to Focus::Nav/Focus::Terminal branches.
        // Mouse capture is global, so mouse bytes arrive in both states; scanning here
        // prevents them from reaching handle_nav_bytes (which would mis-decode them)
        // or TermInput's prefix logic. Split into: mouse events + non-mouse byte stream.
        // Edge case: a sequence split across reads parses as None and falls into
        // non_mouse — rare in practice; no cross-read buffering in v1.
        // The terminal region from the one shared geometry, so a click lands on exactly
        // what was drawn in either layout (in Top the terminal sits below the nav, not
        // to the right of it).
        let full = ratatui::layout::Rect::new(0, 0, self.cols, self.body_rows.saturating_add(1));
        let term_area = crate::ui::switcher::compute_regions(full, self.nav_size(), 1).terminal;
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
            crate::prefs::save_nav_width(&self.env.xmux_dir, self.nav_width_natural);
            crate::prefs::save_nav_height(&self.env.xmux_dir, self.nav_height);
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
            // so its subtree streams in (mirrors handle_nav_bytes's ensure step).
            ensure_current_host(
                &mut self.mgr,
                &self.hosts,
                &self.switcher,
                self.cols,
                self.body_rows,
                self.nav_width,
            );
        }
        // Resize-repeat: while the window from a prefix-driven resize is open, a
        // bare Ctrl+←/→ (no prefix, in either focus) keeps resizing and refreshes
        // the window. Gated on NOT being mid-prefix (an armed prefix's next key is
        // a command, not a repeat — else skipping the input path would leave the
        // prefix armed and mis-read the following key). A pure-mouse read (empty
        // non_mouse) leaves the window untouched. Leading Ctrl-arrows are peeled off
        // (handles a coalesced autorepeat burst); any remaining bytes end the window
        // and fall through to the normal nav/terminal routing below.
        let mut consumed_by_repeat = false;
        if self
            .mouse_state
            .repeat_until
            .is_some_and(|d| std::time::Instant::now() < d)
            && !self.mouse_state.nav_armed
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
            // swallowed — so nothing leaks to the nav or the terminal view. Above the
            // nav/terminal split so the behavior is identical regardless of focus.
            *dirty = true;
        } else if !consumed_by_repeat
            && (self.state.focus.is_nav_focused() || self.state.focus.is_modal())
        {
            // Nav view OR any modal: route to the switcher path. A modal popup opened
            // from EITHER view owns its keys here; the resolver gating in handle_nav_bytes
            // swallows everything but the modal's own keys, so a modal never emits
            // FocusTerminal/quit and the focus toggles below never fire mid-modal.
            let (ft, q, wd, hd, th) = self.handle_nav_bytes(&non_mouse, width_changed);
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
                toggle_auto_hide(&mut self.auto_hide_nav, &self.env.xmux_dir);
                *dirty = true;
            }
        } else if !consumed_by_repeat {
            // TERMINAL focus: forward raw bytes to the selected session's PTY;
            // TermInput intercepts the prefix (→ nav / quit / help / resize / literal).
            for action in self.term_input.feed(&non_mouse) {
                match action {
                    // Forward keystrokes to the VISIBLE session (`displayed`), not the
                    // selection: until the new session is ready the prior one is on screen,
                    // so input must reach what the user actually sees (no blind typing).
                    Action::Forward(f) => self
                        .registry
                        .input(&display_key(&self.hosts, &self.state.displayed), f),
                    Action::FocusNav(rest) => {
                        *focus_nav = true;
                        *nav_replay = rest;
                    }
                    Action::Quit => *quit = true,
                    Action::ShowHelp => {
                        self.switcher.toggle_help(&mut self.state);
                        *dirty = true;
                    }
                    // Same resize + repeat-window as the nav path, so a resize started from
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
                        toggle_auto_hide(&mut self.auto_hide_nav, &self.env.xmux_dir);
                        *dirty = true;
                    }
                    // prefix n/r reach here from terminal focus: run them through the
                    // switcher exactly like the nav path. handle_key opens the new-session
                    // input (n) or arms the re-scan (r); Enter then routes via the modal
                    // path (is_modal) on the next read. `r` only sets the re-scan flag, so
                    // kick_rescan must fire it: the nav path (handle_nav_bytes) runs the
                    // same tail after every read.
                    Action::NavKey(k) => {
                        let cmds = self.switcher.handle_key(k, &mut self.state);
                        let (cq, cwc) = dispatch_commands(
                            cmds,
                            &mut self.switcher,
                            &mut self.state,
                            &mut self.nav_width_natural,
                            &mut self.auto_hide_nav,
                            &self.env.xmux_dir,
                            (&self.ops, &self.op_tx),
                        );
                        *quit |= cq;
                        if cwc {
                            *width_changed = true;
                        }
                        kick_rescan(
                            &self.env,
                            &mut self.switcher,
                            &self.hosts,
                            &mut self.detecting,
                            &mut self.mgr,
                            &mut self.panes_requested,
                            (self.cols, self.body_rows),
                        );
                        *dirty = true;
                    }
                    // TermInput never emits FocusTerminal (that is the nav-focus path).
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
        if self.armed() != armed_before {
            *dirty = true;
        }
        if *focus_nav {
            self.state
                .apply(crate::model::Action::Focus(crate::model::FocusTarget::Nav));
            if !nav_replay.is_empty() {
                let (ft, q, wd, hd, th) = self.handle_nav_bytes(nav_replay, width_changed);
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
                    toggle_auto_hide(&mut self.auto_hide_nav, &self.env.xmux_dir);
                    *dirty = true;
                }
            }
        }
        outcome
    }
}
