use super::*;

impl Runtime {
    /// Processes a batch of NAV-focus input bytes through ONE path - used for both real
    /// stdin and bytes replayed after a terminal→nav switch. Handles prefix arming
    /// (`C-g` then `q` → quit, `Ctrl+←` → move the border left, `Ctrl+→` → right,
    /// the nav width following the placement),
    /// Enter → focus terminal (unless an inline input is open),
    /// ←/→ navigate the nav; then the off-loop op dispatch, ensure-current-host, and
    /// the `r` re-scan. Returns `(focus_terminal, quit, width_delta, toggle_auto_hide)`.
    /// The selection is committed at the loop top, so this only drives navigation +
    /// metadata, not the display. `width_changed` is the caller's out-flag.
    pub(super) fn handle_nav_bytes(
        &mut self,
        bytes: &[u8],
        width_changed: &mut bool,
    ) -> (bool, bool, i32, i32, bool, bool) {
        let keys = self.nav_decoder.feed(bytes);
        let mut nav_armed = self.model.mouse_state.nav_armed;
        let (prefix, cols, rows, nav_width) =
            (self.prefix, self.cols, self.body_rows, self.model.nav_width);
        let mut focus_terminal = false;
        let mut quit = false;
        let mut width_delta = 0i32;
        let mut height_delta = 0i32;
        let mut toggle_auto_hide = false;
        let mut cycle_position = false;
        let mut effects = Vec::new();
        for key in keys {
            // Re-query per key: opening a modal popup (via a NavKey applied below) flips
            // this, which changes how the next key in this same read resolves. Gating on
            // ANY modal popup (not just the inline input) makes a modal OWN its keys: the
            // help modal and the inline input both swallow prefix/Enter, so `prefix q`
            // can't quit and Enter can't focus the terminal while one is on screen.
            let is_inputting = self.model.state.is_modal_popup_open();
            match resolve_nav_key(
                key,
                &mut nav_armed,
                prefix,
                is_inputting,
                self.model.nav_position,
            ) {
                // A committed input/kill confirm folds through State::apply, which returns
                // its Commands; collect them and dispatch the whole batch below.
                Some(Action::NavKey(k)) => effects.extend(update(&mut self.model, Msg::Key(k))),
                Some(Action::FocusTerminal) => {
                    // Enter focuses the terminal view. For a locked host that view holds
                    // the locked panel, whose own fields take the keys once focused; the
                    // unlock is a feature of that panel, not a modal this opens.
                    focus_terminal = true;
                }
                Some(Action::Quit) => quit = true,
                Some(Action::Width(d)) => width_delta = d,
                Some(Action::Height(d)) => height_delta = d,
                Some(Action::ToggleAutoHide) => toggle_auto_hide = true,
                Some(Action::CycleNavPosition) => cycle_position = true,
                Some(Action::ToggleCollapse) => {
                    effects.extend(update(&mut self.model, Msg::ToggleNavCollapsed));
                }
                Some(Action::ShowHelp) => {
                    effects.extend(update(&mut self.model, Msg::ToggleHelp));
                }
                Some(Action::ShowHistory) => {
                    effects.extend(update(&mut self.model, Msg::ToggleHistory));
                }
                Some(Action::ShowCheck) => {
                    effects.extend(update(&mut self.model, Msg::ToggleCheck));
                }
                Some(Action::ShowPalette) => {
                    effects.extend(update(&mut self.model, Msg::TogglePalette));
                }
                Some(Action::CycleNavScope) => {
                    effects.extend(update(&mut self.model, Msg::CycleNavScope));
                }
                // resolve_nav_key never emits the mux-only or terminal-only variants
                // (Forward/FocusNav); None = armed/consumed.
                Some(Action::Forward(_)) | Some(Action::FocusNav(_)) | None => {}
            }
        }
        // One stdin read owns at most one discovery pass, even when it contains several
        // complete prefix+r pairs. The nav state still applies every key in order.
        let mut rescan_seen = false;
        effects.retain(|effect| {
            if matches!(effect, Effect::Command(crate::model::Command::Rescan)) {
                let keep = !rescan_seen;
                rescan_seen = true;
                keep
            } else {
                true
            }
        });
        let armed_effects = update(&mut self.model, Msg::SetMouseNavArmed(nav_armed));
        debug_assert!(armed_effects.is_empty());
        // Route the full command batch through the runtime executor so every command a
        // switcher key produces is acted on. Merge its loop signals into this input read.
        let (cmd_quit, cmd_width_changed, _) = self.execute_effects(effects);
        quit |= cmd_quit;
        if cmd_width_changed {
            *width_changed = true;
        }
        ensure_current_host(
            &mut self.mgr,
            &self.hosts,
            &self.model.switcher,
            cols,
            rows,
            nav_width,
        );
        self.flush_rescan();
        (
            focus_terminal,
            quit,
            width_delta,
            height_delta,
            toggle_auto_hide,
            cycle_position,
        )
    }
}

/// Applies ONE parsed SGR mouse event to the gesture state + nav/registry - the body
/// of the inline `while i < bytes.len()` mouse branch, lifted verbatim. Runs the modal/
/// gesture gates (view border drag, popup drag, modal swallow, view border grab, idle
/// hover) in the SAME order, then the focus×position routing. Mutates `st`
/// (the gesture latches), `state.focus` (mid-loop focus toggles - routing re-reads focus
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
    ) -> bool {
        let (cols, body_rows, nav_width) = (self.cols, self.body_rows, self.model.nav_width);
        let mut dirty = false;
        // A prefix is armed only until the next INPUT, and a mouse action is input. Mouse
        // bytes are scanned out of the stream before either focus path's key handling sees
        // them, so the disarm happens here or not at all - and a chord left half-open keeps
        // its key list floating over the window, then eats the next key as a command the
        // user meant for the pane. Bare hover is not an action: the pointer drifting across
        // the screen must not break a chord that is still being typed.
        let idle_motion = ev.pressed && (ev.cb & 0x23) == 0x23;
        if !idle_motion && (self.model.mouse_state.nav_armed || self.term_input.is_armed()) {
            let effects = update(&mut self.model, Msg::SetMouseNavArmed(false));
            debug_assert!(effects.is_empty());
            self.term_input.disarm();
            dirty = true;
        }
        let in_mux = to_grid_local(self.model.render_plan.regions.terminal, ev.col, ev.row);
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
        // applies it and resizes the PTYs (same path as prefix Ctrl-←/→).
        let col0 = ev.col.saturating_sub(1); // 1-based SGR → 0-based screen col
        let row0 = ev.row.saturating_sub(1);
        // The view border rect from the one shared geometry, so the grab / hover works in
        // any placement: a vertical rule in a column, a horizontal rule in a band. The
        // drag then resizes the nav WIDTH (column, by column) or HEIGHT (band, by row).
        let full = self.model.render_plan.screen_area;
        let regions = self.model.render_plan.regions;
        let on_view_border = !self.model.render_plan.nav_hidden
            && !self.model.render_plan.nav_collapsed
            && regions
                .view_border
                .contains(ratatui::layout::Position { x: col0, y: row0 });
        let top_layout = regions.layout == crate::ui::switcher::ViewLayout::Band;
        if self.model.mouse_state.dragging_view_border {
            if !ev.pressed {
                // Button up ends the drag; persist the final size once (motion resizes live
                // but does not write per cell). A band drags the height, a column the width.
                let effects = update(&mut self.model, Msg::EndNavDrag { band: top_layout });
                let _ = self.execute_effects(effects);
            } else if !is_wheel {
                // The DRAG measures from the near edge: a band drags the height (from the
                // top edge, or the bottom edge when pinned there), a column the width (from
                // the left edge, or the right one) - the same per-side math the resize keys
                // follow (their direction is the border's movement). A drag past the
                // minimum collapses the nav, and coming back out within the same drag
                // expands it at the width or height the pointer reached.
                let position = self.model.render_plan.nav_position;
                let target = if top_layout {
                    view_border_drag_height(
                        ev.row,
                        full.height,
                        position == crate::ui::switcher::NavPosition::Bottom,
                    )
                } else {
                    view_border_drag_width(
                        ev.col,
                        &self.env.ui_prefix,
                        full.width,
                        position == crate::ui::switcher::NavPosition::Right,
                    )
                };
                if target.is_none() != self.model.nav_collapsed {
                    let effects = update(&mut self.model, Msg::SetNavCollapsed(target.is_none()));
                    let _ = self.execute_effects(effects);
                    dirty = true;
                }
                match target {
                    Some(target) if top_layout && target != self.model.nav_height => {
                        let effects = update(&mut self.model, Msg::SetNavHeight(target));
                        debug_assert!(effects.is_empty());
                        dirty = true;
                    }
                    Some(target) if !top_layout && target != self.model.nav_width_natural => {
                        let effects = update(&mut self.model, Msg::SetNavNaturalWidth(target));
                        debug_assert!(effects.is_empty());
                        dirty = true;
                    }
                    _ => {}
                }
            }
            return dirty;
        }
        let is_left_press = is_press && (ev.cb & 0x03) == 0;
        // A modal popup (help/input/confirm) moves when its border is
        // dragged. Once grabbed it owns every mouse event until release,
        // like the view border drag above.
        if self.model.switcher.popup_drag_active() {
            if !ev.pressed {
                let effects = update(&mut self.model, Msg::EndPopupDrag);
                debug_assert!(effects.is_empty());
            } else if !is_wheel {
                let effects = update(
                    &mut self.model,
                    Msg::DragPopup {
                        col: col0,
                        row: ev.row.saturating_sub(1),
                    },
                );
                debug_assert!(effects.is_empty());
            }
            dirty = true;
            return dirty;
        }
        if is_left_press {
            let effects = update(
                &mut self.model,
                Msg::BeginPopupDrag {
                    col: col0,
                    row: ev.row.saturating_sub(1),
                },
            );
            debug_assert!(effects.is_empty());
            if self.model.switcher.popup_drag_active() {
                dirty = true;
                return dirty;
            }
        }
        // A modal popup is mouse-modal: while one is open, every mouse
        // event that is not its border-drag (handled above) is swallowed,
        // so clicks, wheels, view border grabs, and hovers never reach the
        // nav/terminal/view border behind it.
        if self.model.state.is_modal_popup_open() {
            return dirty;
        }
        // A collapsed nav is one target: a click anywhere on it, its seam included,
        // expands it, and is neither a focus move nor a drag.
        let at = ratatui::layout::Position { x: col0, y: row0 };
        if is_left_press
            && self.model.render_plan.nav_collapsed
            && self.model.render_plan.expand_area.contains(at)
        {
            let effects = update(&mut self.model, Msg::SetNavCollapsed(false));
            let _ = self.execute_effects(effects);
            return true;
        }
        // A toast is taken down by a click on it, and the click goes no further: the
        // toast covered whatever is beneath it.
        if is_left_press {
            if let Some(id) = self.model.render_plan.toast_at(col0, row0) {
                let effects = update(&mut self.model, Msg::DismissToast(id));
                debug_assert!(effects.is_empty());
                return true;
            }
        }
        // A band's overflow count stands on the seam for the hidden card nearest the
        // visible ones: a click selects that card, so the band scrolls to it.
        if is_left_press && self.model.render_plan.overflow_target(col0, row0).is_some() {
            let effects = update(
                &mut self.model,
                Msg::MouseSelect {
                    col: col0,
                    row: row0,
                },
            );
            let _ = self.execute_effects(effects);
            ensure_current_host(
                &mut self.mgr,
                &self.hosts,
                &self.model.switcher,
                cols,
                body_rows,
                nav_width,
            );
            return true;
        }
        if is_left_press && on_view_border {
            let effects = update(&mut self.model, Msg::SetMouseDragging(true));
            debug_assert!(effects.is_empty()); // grabbed the view border
            return dirty;
        }
        // Idle motion (motion bit set, no button held) - reported only
        // because any-motion tracking (1003h) is on. Over the view border it
        // lights the hover cue and is consumed (nothing under it to forward).
        // Elsewhere it falls through to the routing below, so a hover over the
        // terminal view IS forwarded to the child (the inner app gets hover); over
        // the nav it is harmlessly dropped.
        if idle_motion {
            let over_view_border = on_view_border;
            if over_view_border != self.model.mouse_state.hovered_view_border {
                let effects = update(&mut self.model, Msg::SetMouseHovered(over_view_border));
                debug_assert!(effects.is_empty());
                dirty = true;
            }
            if over_view_border {
                return dirty;
            }
        }
        let down = (ev.cb & 0x01) != 0;
        let mut model_msg = None;
        let mut ensure_after_update = false;
        match resolve_mouse_chain(
            is_wheel,
            down,
            is_left_press,
            self.model.state.focus.is_nav_focused(),
            in_mux.is_some(),
        ) {
            ChainAction::ScrollNav(down) => {
                // Plain wheel → scroll the selection LINEARLY through every row
                // (move_selection), like any list. NOT sibling-cycle: arrows do
                // that (move_sibling), but it wraps within a level, so a 2-sibling
                // level just bounces - the "two notches per move" report.
                model_msg = Some(Msg::MouseScroll { down });
                *wheel_scrolled = true;
                dirty = true;
            }
            // The unfocused view was clicked → switch focus to it (no content
            // delivered); toggle flips Focus::Nav⇄Focus::Terminal either direction.
            ChainAction::FocusTerminal | ChainAction::FocusNav => {
                model_msg = Some(Msg::Action(crate::model::Action::FocusToggle));
                *mouse_focus_toggle = true;
            }
            ChainAction::SelectRow => {
                // Left-click a nav row → move the selection to it (select). The
                // loop top commits the new selection (attach); ensure the
                // clicked row's host connects so its subtree streams in.
                model_msg = Some(Msg::MouseSelect {
                    col: col0,
                    row: ev.row.saturating_sub(1),
                });
                ensure_after_update = true;
                dirty = true;
            }
            ChainAction::ForwardToMux => {
                // Re-assert the CONIN capture bits at the instant each event is forwarded: a
                // ConPTY child spawn clears ENABLE_MOUSE_INPUT / re-enables quick-edit on the
                // parent CONIN at any moment, and the per-frame poll only restores capture one
                // loop later, so a drag that starts in the gap would hand the terminal back its
                // native drag-to-select. Re-asserting before the forward keeps capture alive for
                // the whole interaction, not just at the next poll. No-op off Windows.
                crate::display::term::ensure_mouse_capture();
                if let Some((gc, gr)) = in_mux {
                    self.registry.input(
                        &display_key(&self.hosts, selection),
                        crate::display::mouse::encode_sgr_mouse(ev, gc, gr),
                    );
                }
            }
            ChainAction::Nothing => {}
        }
        if let Some(msg) = model_msg {
            let effects = update(&mut self.model, msg);
            let _ = self.execute_effects(effects);
        }
        if ensure_after_update {
            ensure_current_host(
                &mut self.mgr,
                &self.hosts,
                &self.model.switcher,
                cols,
                body_rows,
                nav_width,
            );
        }
        dirty
    }
}

impl Runtime {
    /// Applies a nav-resize delta on ONE axis, gated to the layout that actually shows that
    /// axis so a key never resizes a dimension the user cannot see: `horizontal` (Ctrl-←/→)
    /// resizes the WIDTH only in a column, `!horizontal` (↑/↓) the HEIGHT only in a band; the
    /// perpendicular axis is a no-op. The delta is the key's SCREEN direction (+1 = right /
    /// down), and the nav-size effect follows the placement: on the left or above that
    /// direction grows the nav, on the right or below it shrinks it, because the border's
    /// far edge is the nav's in the first case and its near edge in the second. So the key
    /// always points where the border actually moves, the same flip as the focus-arrow pair
    /// and the border drag. Height is seeded from the effective auto height the first time
    /// (while `nav_height == 0`) so a relative step starts from what is on screen, clamped so
    /// the terminal keeps room, and persisted; width defers to `apply_width_delta` (the
    /// caller schedules the debounced persist). Returns whether the size changed.
    pub(super) fn resize_axis(&mut self, horizontal: bool, delta: i32) -> bool {
        let before = (self.model.nav_width_natural, self.model.nav_height);
        let effects = update(
            &mut self.model,
            Msg::ResizeNav {
                horizontal,
                delta,
                body_rows: self.body_rows,
                ui_prefix: self.env.ui_prefix.clone(),
            },
        );
        let _ = self.execute_effects(effects);
        before != (self.model.nav_width_natural, self.model.nav_height)
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
        let effects = update(
            &mut self.model,
            Msg::SetResizeRepeat(Some(
                std::time::Instant::now() + std::time::Duration::from_millis(RESIZE_REPEAT_MS),
            )),
        );
        debug_assert!(effects.is_empty());
        changed
    }

    /// The whole `stdin_rx` arm body, lifted. Scans the read for SGR mouse sequences
    /// (routed via [`Runtime::handle_mouse_event`]) vs a non-mouse byte stream, runs the
    /// lost-release watchdogs, the resize-repeat window, and the help-modal / nav-focus /
    /// terminal-view focus routing - in the SAME order as the inline arm. The final focus
    /// toggles (+ replay) run on `self.model.state.focus`, so the caller only acts on the returned
    /// `dirty`/`quit`. No behavior change.
    pub(super) fn handle_stdin_bytes(
        &mut self,
        bytes: &[u8],
        selection: &Selection,
    ) -> StdinOutcome {
        use std::time::Duration;
        // A live prefix opens the key list, so an arm/disarm is a VISIBLE change even when
        // the read moves nothing else. Snapshot it here and mark the frame dirty below if
        // it flipped, or the key list would only appear on the next unrelated redraw (a
        // poll tick).
        let armed_before = self.prefix_active();
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
        // non_mouse - rare in practice; no cross-read buffering in v1.
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
        // Any key ends the hint after a selection move, in either focus; a key below that
        // moves the selection again raises the next one.
        if !non_mouse.is_empty() && self.model.state.chrome.selection_hint.is_some() {
            let effects = update(&mut self.model, Msg::KeysRead);
            debug_assert!(effects.is_empty());
            *dirty = true;
        }
        // Watchdog: a view border drag is normally ended by the button-up event, but a
        // release can be lost (split across reads, released off-window, or a terminal
        // that omits it) - which would strand `dragging_view_border` and eat all later
        // mouse input. Any non-mouse byte (a keystroke, or the split release's own
        // leftover bytes) ends the drag and persists the final width, so the user is
        // never trapped past the next input.
        if self.model.mouse_state.dragging_view_border && !non_mouse.is_empty() {
            let effects = update(&mut self.model, Msg::SetMouseDragging(false));
            debug_assert!(effects.is_empty());
            // The recovery doesn't track which axis was dragging; persist both (a no-op file
            // write for the unchanged one) so the final size is never lost.
            let effects = update(&mut self.model, Msg::PersistNavSize);
            let _ = self.execute_effects(effects);
        }
        // Watchdog: same recovery for a popup border-drag - a lost button-up
        // must not strand `popup_drag` and eat all later mouse input.
        if self.model.switcher.popup_drag_active() && !non_mouse.is_empty() {
            let effects = update(&mut self.model, Msg::EndPopupDrag);
            debug_assert!(effects.is_empty());
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
                &self.model.switcher,
                self.cols,
                self.body_rows,
                self.model.nav_width,
            );
        }
        // Resize-repeat: while the window from a prefix-driven resize is open, a
        // bare Ctrl+←/→ (no prefix, in either focus) keeps resizing and refreshes
        // the window. Gated on NOT being mid-prefix (an armed prefix's next key is
        // a command, not a repeat - else skipping the input path would leave the
        // prefix armed and mis-read the following key). A pure-mouse read (empty
        // non_mouse) leaves the window untouched. Leading Ctrl-arrows are peeled off
        // (handles a coalesced autorepeat burst); any remaining bytes end the window
        // and fall through to the normal nav/terminal routing below.
        let mut consumed_by_repeat = false;
        if self
            .model
            .mouse_state
            .repeat_until
            .is_some_and(|d| std::time::Instant::now() < d)
            && !self.model.mouse_state.nav_armed
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
                    let effects = update(
                        &mut self.model,
                        Msg::SetResizeRepeat(Some(
                            std::time::Instant::now() + Duration::from_millis(RESIZE_REPEAT_MS),
                        )),
                    );
                    debug_assert!(effects.is_empty());
                    consumed_by_repeat = true;
                } else {
                    let effects = update(&mut self.model, Msg::SetResizeRepeat(None));
                    debug_assert!(effects.is_empty()); // trailing non-arrow bytes end + route below
                }
            } else {
                let effects = update(&mut self.model, Msg::SetResizeRepeat(None));
                debug_assert!(effects.is_empty()); // first key isn't a Ctrl-arrow → end the window
            }
        }
        if !consumed_by_repeat
            && !non_mouse.is_empty()
            && crate::state::is_reader(&self.model.state.modal)
        {
            // The table of the hosts to check acts on Enter: it selects a host and may hand
            // the focus to the terminal view, whose login pane then takes the keys.
            let effects = update(
                &mut self.model,
                Msg::ReaderBytes {
                    bytes: non_mouse,
                    prefix: self.prefix,
                },
            );
            let (cq, cwc, _) = self.execute_effects(effects);
            *quit |= cq;
            if cwc {
                *width_changed = true;
            }
            // The help and the history are modal (tmux view-mode style): while one is
            // open it captures every key in EITHER focus - q/Esc or the prefix key that
            // opened it closes it, the history scrolls, the rest are swallowed - so
            // nothing leaks to the nav or the terminal view. Above the nav/terminal split
            // so the behavior is identical regardless of focus.
            *dirty = true;
        } else if !consumed_by_repeat
            && (self.model.state.focus.is_nav_focused() || self.model.state.focus.is_modal())
        {
            // Nav view OR any modal: route to the switcher path. A modal popup opened
            // from EITHER view owns its keys here; the resolver gating in handle_nav_bytes
            // swallows everything but the modal's own keys, so a modal never emits
            // FocusTerminal/quit and the focus toggles below never fire mid-modal.
            let (ft, q, wd, hd, th, cp) = self.handle_nav_bytes(&non_mouse, width_changed);
            *focus_terminal = ft;
            *quit = q;
            // A prefix-driven resize: width (Ctrl-←/→) or height (Ctrl-↑/↓); each applies only in
            // its layout, and opens the bare-Ctrl-arrow repeat window.
            let rw = self.resize_and_repeat(true, wd);
            let rh = self.resize_and_repeat(false, hd);
            if rw || rh {
                *width_changed = true;
            }
            if th {
                let effects = update(
                    &mut self.model,
                    Msg::Action(crate::model::Action::ToggleAutoHide),
                );
                let _ = self.execute_effects(effects);
                *dirty = true;
            }
            if cp {
                let effects = update(&mut self.model, Msg::CycleNavPosition);
                let _ = self.execute_effects(effects);
                *dirty = true;
            }
        } else if !consumed_by_repeat {
            // TERMINAL focus: forward raw bytes to the selected session's PTY;
            // TermInput intercepts the prefix (→ nav / quit / help / resize / literal).
            for action in self.term_input.feed(&non_mouse, self.model.nav_position) {
                match action {
                    // A BLOCKED host has no PTY: its login pane in the terminal view owns the
                    // keys. Route them to that pane (edit a field, walk the stops, or submit
                    // on Enter) instead of a session. Otherwise forward to the
                    // VISIBLE session (`displayed`), not the selection: until a new session
                    // is ready the prior one is on screen, so input must reach what the user
                    // actually sees (no blind typing).
                    Action::Forward(f) => {
                        let login_running = self.model.state.login_run.as_ref().is_some_and(|l| {
                            self.model.switcher.current_source().as_deref() == Some(&l.source)
                        });
                        if login_running {
                            // The login is xmux's own conversation, so nothing typed here
                            // reaches it. A lone Esc ends it, which is the one thing the
                            // user can still say about a login that is going nowhere;
                            // every other key waits for the pane to come back.
                            if f.as_slice() == b"\x1b" {
                                let effects = update(&mut self.model, Msg::CancelRunningLogin);
                                let _ = self.execute_effects(effects);
                            }
                            *dirty = true;
                        } else if self.model.switcher.current_host_blocked() {
                            if let Some(source) = self.model.switcher.current_source() {
                                let effects =
                                    update(&mut self.model, Msg::FeedLogin { source, bytes: f });
                                let (cq, cwc, _) = self.execute_effects(effects);
                                *quit |= cq;
                                if cwc {
                                    *width_changed = true;
                                }
                                *dirty = true;
                            }
                        } else {
                            self.registry
                                .input(&display_key(&self.hosts, &self.model.state.displayed), f);
                        }
                    }
                    Action::FocusNav(rest) => {
                        *focus_nav = true;
                        *nav_replay = rest;
                    }
                    Action::Quit => *quit = true,
                    Action::ShowHelp => {
                        let effects = update(&mut self.model, Msg::ToggleHelp);
                        debug_assert!(effects.is_empty());
                        *dirty = true;
                    }
                    Action::ShowHistory => {
                        let effects = update(&mut self.model, Msg::ToggleHistory);
                        debug_assert!(effects.is_empty());
                        *dirty = true;
                    }
                    Action::ShowCheck => {
                        let effects = update(&mut self.model, Msg::ToggleCheck);
                        debug_assert!(effects.is_empty());
                        *dirty = true;
                    }
                    Action::ShowPalette => {
                        let effects = update(&mut self.model, Msg::TogglePalette);
                        debug_assert!(effects.is_empty());
                        *dirty = true;
                    }
                    Action::CycleNavScope => {
                        let effects = update(&mut self.model, Msg::CycleNavScope);
                        let _ = self.execute_effects(effects);
                        *dirty = true;
                    }
                    // Same resize + repeat-window as the nav path, so a resize started from
                    // the terminal view chains with bare Ctrl-arrows too. Width = ←/→ (column),
                    // height = ↑/↓ (band).
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
                        let effects = update(
                            &mut self.model,
                            Msg::Action(crate::model::Action::ToggleAutoHide),
                        );
                        let _ = self.execute_effects(effects);
                        *dirty = true;
                    }
                    Action::CycleNavPosition => {
                        let effects = update(&mut self.model, Msg::CycleNavPosition);
                        let _ = self.execute_effects(effects);
                        *dirty = true;
                    }
                    Action::ToggleCollapse => {
                        let effects = update(&mut self.model, Msg::ToggleNavCollapsed);
                        let _ = self.execute_effects(effects);
                        *dirty = true;
                    }
                    // prefix n/r reach here from terminal focus: run them through the
                    // switcher exactly like the nav path. handle_key opens the new-session
                    // input (n) or requests the re-scan (r); Enter then routes via the modal
                    // path (is_modal) on the next read. Discovery is flushed once after the
                    // full input batch has applied its other effects.
                    Action::NavKey(k) => {
                        let effects = update(&mut self.model, Msg::Key(k));
                        let (cq, cwc, _) = self.execute_effects(effects);
                        *quit |= cq;
                        if cwc {
                            *width_changed = true;
                        }
                        *dirty = true;
                    }
                    // TermInput never emits FocusTerminal (that is the nav-focus path).
                    Action::FocusTerminal => {}
                }
            }
        }
        if *focus_terminal {
            // Leaving the nav for the terminal: a nav-side pending prefix has no key-up
            // once the terminal owns stdin, so clear it here instead of waiting for a
            // release that is now delivered elsewhere.
            let effects = update(&mut self.model, Msg::SetMouseNavArmed(false));
            debug_assert!(effects.is_empty());
            let effects = update(
                &mut self.model,
                Msg::Focus(crate::model::FocusTarget::Terminal),
            );
            let _ = self.execute_effects(effects);
            // No term.clear(): both states draw the SAME split layout (only the
            // view border colour changes), so clearing would blank the screen and
            // force a full repaint for nothing.
        }
        if *focus_nav {
            // Leaving the terminal for the nav: the prefix key's release is delivered to
            // the nav path now, never to TermInput, so clear the terminal-side pending
            // prefix here instead of waiting for a release that will not arrive (a stale
            // prefix would keep the status bar up forever).
            self.term_input.disarm();
            let effects = update(&mut self.model, Msg::Focus(crate::model::FocusTarget::Nav));
            let _ = self.execute_effects(effects);
            if !nav_replay.is_empty() {
                let (ft, q, wd, hd, th, cp) = self.handle_nav_bytes(nav_replay, width_changed);
                if ft {
                    // The replayed bytes switch focus back to the terminal: clear the
                    // nav-side latches the replay may have armed, same as the direct
                    // terminal-focus path above.
                    let effects = update(&mut self.model, Msg::SetMouseNavArmed(false));
                    debug_assert!(effects.is_empty());
                    let effects = update(
                        &mut self.model,
                        Msg::Focus(crate::model::FocusTarget::Terminal),
                    );
                    let _ = self.execute_effects(effects);
                }
                *quit = *quit || q;
                // A prefix-driven resize on the replayed bytes: same as the direct path above.
                let rw = self.resize_and_repeat(true, wd);
                let rh = self.resize_and_repeat(false, hd);
                if rw || rh {
                    *width_changed = true;
                }
                if th {
                    let effects = update(
                        &mut self.model,
                        Msg::Action(crate::model::Action::ToggleAutoHide),
                    );
                    let _ = self.execute_effects(effects);
                    *dirty = true;
                }
                if cp {
                    let effects = update(&mut self.model, Msg::CycleNavPosition);
                    let _ = self.execute_effects(effects);
                    *dirty = true;
                }
            }
        }
        // The focus-switch latch drops above change prefix_active, so this read's bar
        // visibility is compared AFTER both focus blocks, not between them.
        if self.prefix_active() != armed_before {
            *dirty = true;
        }
        self.flush_rescan();
        outcome
    }
}
