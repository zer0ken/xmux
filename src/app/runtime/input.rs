use super::*;

impl Runtime {
    /// Processes a batch of NAV-focus input bytes through ONE path - used for both real
    /// stdin and bytes replayed after a terminal→nav switch. Handles prefix arming
    /// (`C-g` then `q` → quit, `Ctrl+←` → move the border left, `Ctrl+→` → right,
    /// the nav width following the placement),
    /// Enter → focus terminal (unless an input popup is open),
    /// ←/→ navigate the nav; then the off-loop op dispatch, ensure-current-host, and
    /// the `R` re-scan. Returns `(focus_terminal, quit, width_delta, toggle_auto_hide)`.
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
            // ANY modal popup (not just an input) makes a modal OWN its keys: the
            // help and an input both swallow prefix/Enter, so `prefix q`
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
                    // Enter focuses the terminal view. For a locked machine that view holds
                    // the locked panel, whose own fields take the keys once focused; the
                    // unlock is a feature of that panel, not a modal this opens.
                    focus_terminal = true;
                }
                Some(Action::Quit) => quit = true,
                Some(Action::Width(d)) => width_delta = d,
                Some(Action::Height(d)) => height_delta = d,
                Some(Action::ToggleAutoHide) => toggle_auto_hide = true,
                Some(Action::CycleNavPosition) => cycle_position = true,
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
/// gesture gates (nav border drag, popup drag, modal swallow, nav border grab, idle
/// hover) in the SAME order, then the focus×position routing. Mutates `st`
/// (the gesture latches), `state.focus` (mid-loop focus toggles - routing re-reads focus
/// per event, so deferring would change behavior), and the byte-loop accumulators
/// (`mouse_focus_toggle`, `wheel_scrolled`, and the `quit` and `width_changed` a click
/// on a popup item can raise). Returns whether a redraw is needed for this event.
impl Runtime {
    pub(super) fn handle_mouse_event(
        &mut self,
        ev: &crate::display::mouse::MouseEvent,
        selection: &Selection,
        mouse_focus_toggle: &mut bool,
        wheel_scrolled: &mut bool,
        quit: &mut bool,
        width_changed: &mut bool,
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
        // Grabbing the key list to move it is not an action on anything behind it: a left
        // press on the box, and the drag it starts up to the release, keep the prefix.
        let at = ratatui::layout::Position {
            x: ev.col.saturating_sub(1),
            y: ev.row.saturating_sub(1),
        };
        let grabs_key_list = self.model.switcher.popup_drag_active()
            || (ev.pressed
                && (ev.cb & 0x63) == 0
                && self
                    .model
                    .render_plan
                    .key_list
                    .as_ref()
                    .is_some_and(|(rect, _)| rect.contains(at)));
        if !idle_motion
            && !grabs_key_list
            && (self.model.mouse_state.nav_armed
                || self.term_input.is_armed()
                || self.model.mouse_state.resizing)
        {
            let effects = update(&mut self.model, Msg::SetMouseNavArmed(false));
            debug_assert!(effects.is_empty());
            let effects = update(&mut self.model, Msg::SetResizing(false));
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
        // View border drag: grab the nav border rule (the column at the effective
        // nav width, only when the nav is shown) with the left button and
        // drag to resize. Once grabbed it owns every mouse event until the
        // button is released. Sets the NATURAL width; the loop-top reconcile
        // applies it and resizes the PTYs (same path as prefix Ctrl-←/→).
        let col0 = ev.col.saturating_sub(1); // 1-based SGR → 0-based screen col
        let row0 = ev.row.saturating_sub(1);
        // The nav border rect from the one shared geometry, so the grab / hover works in
        // any placement: a vertical rule in a column, a horizontal rule in a horizontal nav. The
        // drag then resizes the nav WIDTH (column, by column) or HEIGHT (horizontal nav, by row).
        let full = self.model.render_plan.screen_area;
        let regions = self.model.render_plan.regions;
        let on_nav_border = !self.model.render_plan.nav_hidden
            && regions
                .nav_border
                .contains(ratatui::layout::Position { x: col0, y: row0 });
        let top_layout = regions.layout == crate::ui::switcher::ViewLayout::Horizontal;
        if self.model.mouse_state.dragging_nav_border {
            if !ev.pressed {
                // Button up ends the drag; persist the final size once (motion resizes live
                // but does not write per cell). A horizontal nav drags the height, a column the width.
                let effects = update(
                    &mut self.model,
                    Msg::EndNavDrag {
                        horizontal: top_layout,
                    },
                );
                let _ = self.execute_effects(effects);
            } else if !is_wheel {
                // The DRAG measures from the near edge: a horizontal nav drags the height (from the
                // top edge, or the bottom edge when pinned there), a column the width (from
                // the left edge, or the right one) - the same per-side math the resize keys
                // follow (their direction is the border's movement). A drag clamps at the
                // nav's minimum, so it can never leave the nav with no room.
                let position = self.model.render_plan.nav_position;
                let target = if top_layout {
                    nav_border_drag_height(
                        ev.row,
                        full.height,
                        position == crate::ui::switcher::NavPosition::Bottom,
                    )
                } else {
                    nav_border_drag_width(
                        ev.col,
                        &self.env.ui_prefix,
                        full.width,
                        position == crate::ui::switcher::NavPosition::Right,
                    )
                };
                if top_layout && target != self.model.nav_height {
                    let effects = update(&mut self.model, Msg::SetNavHeight(target));
                    debug_assert!(effects.is_empty());
                    dirty = true;
                }
                if !top_layout && target != self.model.nav_width_natural {
                    let effects = update(&mut self.model, Msg::SetNavNaturalWidth(target));
                    debug_assert!(effects.is_empty());
                    dirty = true;
                }
            }
            return dirty;
        }
        // A drag that started in the terminal view keeps reaching the session past the
        // view: its motion at the view's nearest edge, and its release wherever it lands,
        // so the session never stays mid-drag or mid-selection. Motion without a button
        // means the release was lost, and ends the drag here.
        if self.model.mouse_state.view_drag && !is_wheel {
            let motion = ev.cb & 0x20 != 0;
            let held = ev.cb & 0x03 != 0x03;
            if !ev.pressed || (motion && held) {
                let (gc, gr) = clamp_to_grid(regions.terminal, ev.col, ev.row);
                self.forward_mouse(ev, gc, gr, selection);
                if !ev.pressed {
                    let effects = update(&mut self.model, Msg::SetViewDrag(false));
                    debug_assert!(effects.is_empty());
                }
                return dirty;
            }
            if motion {
                let effects = update(&mut self.model, Msg::SetViewDrag(false));
                debug_assert!(effects.is_empty());
            }
        }
        let is_left_press = is_press && (ev.cb & 0x03) == 0;
        // The key list and a modal popup move when dragged from anywhere on them. Once
        // grabbed the drag owns every mouse event until release, like the nav border
        // drag above. A release on the cell the press grabbed is a click, which executes
        // the popup item under it as Enter would.
        if self.model.switcher.popup_drag_active() {
            if !ev.pressed {
                let effects = update(&mut self.model, Msg::EndPopupDrag);
                let (q, w, _) = self.execute_effects(effects);
                *quit |= q;
                *width_changed |= w;
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
        // event that is not its drag (handled above) is swallowed,
        // so clicks, wheels, nav border grabs, and hovers never reach the
        // nav/terminal/nav border behind it. Bare motion sets the popup's
        // hover: the help tab or the list item under the pointer.
        if self.model.state.is_modal_popup_open() {
            if idle_motion {
                let before = self.model.state.modal_hover();
                let effects = update(
                    &mut self.model,
                    Msg::HoverPopup {
                        col: col0,
                        row: row0,
                    },
                );
                debug_assert!(effects.is_empty());
                dirty |= self.model.state.modal_hover() != before;
            }
            return dirty;
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
        // A horizontal nav's overflow count stands on the nav border for the hidden card
        // nearest the visible ones: a click selects that card, so the nav scrolls to it.
        if is_left_press && self.model.render_plan.overflow_target(col0, row0).is_some() {
            let effects = update(
                &mut self.model,
                Msg::MouseSelect {
                    col: col0,
                    row: row0,
                    execute: false,
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
        if is_left_press && on_nav_border {
            let effects = update(&mut self.model, Msg::SetMouseDragging(true));
            debug_assert!(effects.is_empty()); // grabbed the nav border
            return dirty;
        }
        // Idle motion (motion bit set, no button held) - reported only
        // because any-motion tracking (1003h) is on. Over the nav border it
        // lights the hover cue and is consumed (nothing under it to forward).
        // Elsewhere it falls through to the routing below, so a hover over the
        // terminal view IS forwarded to the child (the inner app gets hover); over
        // the nav it is harmlessly dropped.
        if idle_motion {
            let over_nav_border = on_nav_border;
            if over_nav_border != self.model.mouse_state.hovered_nav_border {
                let effects = update(&mut self.model, Msg::SetMouseHovered(over_nav_border));
                debug_assert!(effects.is_empty());
                dirty = true;
            }
            // The hover follows the pointer: a nav target while the nav holds
            // the focus, a screen link while the terminal view does.
            let before = self.model.switcher.hover_targets();
            let effects = update(
                &mut self.model,
                Msg::Hover {
                    col: col0,
                    row: row0,
                },
            );
            debug_assert!(effects.is_empty());
            if self.model.switcher.hover_targets() != before {
                dirty = true;
            }
            if over_nav_border {
                return dirty;
            }
        }
        // A click on a link of the machine or host screen the terminal view shows opens
        // it, the same as Enter on the selected link. The landing screen's links
        // take a click from either view's focus.
        let landing = self.model.switcher.landing_open();
        if is_left_press
            && (self.model.state.focus.is_terminal_focused() || landing)
            && self.model.render_plan.view_screen.is_some()
        {
            if let Some(link) =
                crate::ui::switcher::Switcher::link_at(&self.model.render_plan, col0, row0)
            {
                let effects = update(&mut self.model, Msg::OpenLink(Some(link)));
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
        }
        // Off its links the landing screen has no hover, so a click there
        // executes nothing.
        if is_left_press && landing && in_mux.is_some() {
            return dirty;
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
            ChainAction::FocusTerminal => {
                model_msg = Some(Msg::Action(crate::model::Action::FocusToggle));
                *mouse_focus_toggle = true;
            }
            ChainAction::SelectRow => {
                // Left-click a nav target executes it: it becomes the selection and
                // its screen takes the focus, as Enter does. The loop top commits the new
                // selection (attach); ensure the clicked row's host connects so its
                // subtree streams in.
                model_msg = Some(Msg::MouseSelect {
                    col: col0,
                    row: ev.row.saturating_sub(1),
                    execute: true,
                });
                *mouse_focus_toggle = true;
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
                    self.forward_mouse(ev, gc, gr, selection);
                    // A button press starts a drag the session follows to its release.
                    let button = ev.cb & 0x60 == 0;
                    if button && ev.pressed != self.model.mouse_state.view_drag {
                        let effects = update(&mut self.model, Msg::SetViewDrag(ev.pressed));
                        debug_assert!(effects.is_empty());
                    }
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
    /// Sends `ev` at the 1-based grid cell `(col, row)` to the session the terminal view
    /// shows, in the form its client's mouse modes ask for, or nothing when they do not
    /// ask for this event.
    fn forward_mouse(
        &mut self,
        ev: &crate::display::mouse::MouseEvent,
        col: u16,
        row: u16,
        selection: &Selection,
    ) {
        let key = display_key(&self.hosts, selection);
        let modes = self.registry.input_modes(&key);
        if let Some(bytes) = crate::display::mouse::encode_for(ev, col, row, &modes) {
            self.registry.input(&key, bytes);
        }
    }

    /// Applies a nav-resize delta on ONE axis, gated to the layout that actually shows that
    /// axis so a key never resizes a dimension the user cannot see: `horizontal` (Ctrl-←/→)
    /// resizes the WIDTH only in a column, `!horizontal` (↑/↓) the HEIGHT only in a horizontal nav; the
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
    /// perpendicular axis of the current layout) and start the resize mode, so the next
    /// bare Ctrl-arrows keep resizing without re-pressing the prefix. Returns whether the
    /// size changed (for the debounced persist).
    fn resize_and_repeat(&mut self, horizontal: bool, delta: i32) -> bool {
        if delta == 0 {
            return false;
        }
        let changed = self.resize_axis(horizontal, delta);
        let effects = update(&mut self.model, Msg::SetResizing(true));
        debug_assert!(effects.is_empty());
        changed
    }

    /// The whole `stdin_rx` arm body, lifted. Scans the read for SGR mouse sequences
    /// (routed via [`Runtime::handle_mouse_event`]) vs a non-mouse byte stream, runs the
    /// lost-release watchdogs, the resize mode, and the help-modal / nav-focus /
    /// terminal-view focus routing - in the SAME order as the inline arm. The final focus
    /// toggles (+ replay) run on `self.model.state.focus`, so the caller only acts on the returned
    /// `dirty`/`quit`. No behavior change.
    pub(super) fn handle_stdin_bytes(
        &mut self,
        bytes: &[u8],
        selection: &Selection,
    ) -> StdinOutcome {
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
                // A focus report says what happened to xmux's window, not what the user
                // typed: it is taken out here like a mouse report, and the loop tells the
                // session about it.
                let rest = &bytes[i..];
                if rest.starts_with(crate::display::term::FOCUS_IN)
                    || rest.starts_with(crate::display::term::FOCUS_OUT)
                {
                    self.window_focused = rest.starts_with(crate::display::term::FOCUS_IN);
                    i += crate::display::term::FOCUS_IN.len();
                    continue;
                }
                if let Some((ev, len)) = crate::display::mouse::parse_sgr_mouse(&bytes[i..]) {
                    if self.handle_mouse_event(
                        &ev,
                        selection,
                        &mut mouse_focus_toggle,
                        &mut wheel_scrolled,
                        quit,
                        width_changed,
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
        // Watchdog: a nav border drag is normally ended by the button-up event, but a
        // release can be lost (split across reads, released off-window, or a terminal
        // that omits it) - which would strand `dragging_nav_border` and eat all later
        // mouse input. Any non-mouse byte (a keystroke, or the split release's own
        // leftover bytes) ends the drag and persists the final width, so the user is
        // never trapped past the next input.
        if self.model.mouse_state.dragging_nav_border && !non_mouse.is_empty() {
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
            let effects = update(&mut self.model, Msg::AbandonPopupDrag);
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
        // Resize mode: after a prefix-driven resize, a bare Ctrl-arrow (no prefix, in
        // either focus) keeps resizing while the key list names the resize keys, until
        // another key ends the mode. Gated on NOT being mid-prefix (an armed prefix's
        // next key is a command, not a repeat - else skipping the input path would
        // leave the prefix armed and mis-read the following key). A pure-mouse read
        // (empty non_mouse) leaves the mode alone. Leading Ctrl-arrows are peeled off
        // (handles a coalesced autorepeat burst); any remaining bytes end the mode and
        // fall through to the normal nav/terminal routing below, so the key that ended
        // it does what it would have done.
        let mut consumed_by_repeat = false;
        if self.model.mouse_state.resizing
            && !self.model.mouse_state.nav_armed
            && !self.term_input.is_armed()
            && !non_mouse.is_empty()
        {
            let mut n = 0;
            while let Some((horizontal, d, len)) = leading_ctrl_arrow(&non_mouse[n..]) {
                if d != 0 && self.resize_axis(horizontal, d) {
                    *width_changed = true;
                }
                n += len;
            }
            if n > 0 {
                non_mouse.drain(0..n);
                *dirty = true;
                if non_mouse.is_empty() {
                    consumed_by_repeat = true;
                } else {
                    let effects = update(&mut self.model, Msg::SetResizing(false));
                    debug_assert!(effects.is_empty()); // trailing non-arrow bytes end + route below
                }
            } else {
                let effects = update(&mut self.model, Msg::SetResizing(false));
                debug_assert!(effects.is_empty()); // first key isn't a Ctrl-arrow → end the mode
            }
        }
        if !consumed_by_repeat
            && !non_mouse.is_empty()
            && crate::state::is_reader(&self.model.state.modal)
        {
            // The table of machine problems acts on Enter: it selects a host and may hand
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
            // A click earlier in this read may already have quit (a popup item run by
            // the mouse arm above), so the keys' verdict adds to it, never replaces it.
            *quit |= q;
            // A prefix-driven resize: width (Ctrl-←/→) or height (Ctrl-↑/↓); each applies only in
            // its layout, and starts the resize mode.
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
                    // on Enter) instead of a session. Otherwise forward to the selected
                    // session, held until its attachment exists (`forward_input`).
                    Action::Forward(f) => {
                        let login_running = self.model.state.login_run.as_ref().is_some_and(|l| {
                            self.model.switcher.current_host().as_deref() == Some(&l.host)
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
                        } else if self.model.switcher.login_pane_shown(&self.model.state) {
                            if let Some(host) = self.model.switcher.current_host() {
                                let effects =
                                    update(&mut self.model, Msg::FeedLogin { host, bytes: f });
                                let (cq, cwc, _) = self.execute_effects(effects);
                                *quit |= cq;
                                if cwc {
                                    *width_changed = true;
                                }
                                *dirty = true;
                            }
                        } else if self
                            .model
                            .switcher
                            .current_view_screen(&self.model.state)
                            .is_some()
                        {
                            // A machine's or a host's screen takes its own keys.
                            for key in crate::state::decode_keys(&f) {
                                let unreachable = self
                                    .model
                                    .switcher
                                    .current_unreachable_screen(&self.model.state);
                                let Some(msg) = screen_msg(key, unreachable) else {
                                    continue;
                                };
                                let opens = matches!(msg, Msg::OpenLink(_));
                                let effects = update(&mut self.model, msg);
                                let _ = self.execute_effects(effects);
                                if opens {
                                    ensure_current_host(
                                        &mut self.mgr,
                                        &self.hosts,
                                        &self.model.switcher,
                                        self.cols,
                                        self.body_rows,
                                        self.model.nav_width,
                                    );
                                }
                                *dirty = true;
                            }
                        } else {
                            self.forward_input(f);
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
                    // Same resize + repeat-window as the nav path, so a resize started from
                    // the terminal view chains with bare Ctrl-arrows too. Width = ←/→ (column),
                    // height = ↑/↓ (horizontal nav).
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
            // nav border colour changes), so clearing would blank the screen and
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

    /// Routes one paste. Pasted text is data for wherever it goes, never a key: the
    /// focused session reads it as a paste, a text field types it, and with no field to
    /// take it, as over the nav or a machine or host screen, it is dropped. A prefix
    /// waiting for its key and the resize mode end, as on any input that is not their
    /// key.
    pub(super) fn handle_paste(&mut self, text: Vec<u8>) -> StdinOutcome {
        let mut outcome = StdinOutcome {
            dirty: true,
            ..StdinOutcome::default()
        };
        if self.model.mouse_state.nav_armed || self.term_input.is_armed() {
            let effects = update(&mut self.model, Msg::SetMouseNavArmed(false));
            debug_assert!(effects.is_empty());
            self.term_input.disarm();
        }
        if self.model.mouse_state.resizing {
            let effects = update(&mut self.model, Msg::SetResizing(false));
            debug_assert!(effects.is_empty());
        }
        let field = crate::display::paste::field_text(&text);
        if crate::state::is_reader(&self.model.state.modal) {
            let effects = update(
                &mut self.model,
                Msg::ReaderBytes {
                    bytes: field,
                    prefix: self.prefix,
                },
            );
            let (quit, width_changed, _) = self.execute_effects(effects);
            outcome.quit = quit;
            outcome.width_changed = width_changed;
        } else if self.model.state.focus.is_nav_focused() || self.model.state.focus.is_modal() {
            if self.model.state.is_inputting() {
                let (_, quit, ..) = self.handle_nav_bytes(&field, &mut outcome.width_changed);
                outcome.quit = quit;
            }
        } else {
            let login_running =
                self.model.state.login_run.as_ref().is_some_and(|l| {
                    self.model.switcher.current_host().as_deref() == Some(&l.host)
                });
            // A running login takes no input, and a screen without the login pane has
            // no field.
            if login_running {
                return outcome;
            }
            if self.model.switcher.login_pane_shown(&self.model.state) {
                if let Some(host) = self.model.switcher.current_host() {
                    let effects = update(&mut self.model, Msg::FeedLogin { host, bytes: field });
                    let (quit, width_changed, _) = self.execute_effects(effects);
                    outcome.quit = quit;
                    outcome.width_changed = width_changed;
                }
            } else if self
                .model
                .switcher
                .current_view_screen(&self.model.state)
                .is_none()
            {
                self.forward_paste(text);
            }
        }
        self.flush_rescan();
        outcome
    }

    /// Forwards a paste to the session [`input_route`] names, wrapped in the paste
    /// markers when that session's client enabled bracketed paste.
    fn forward_paste(&mut self, text: Vec<u8>) {
        let bracketed = match input_route(&self.model.state, &self.hosts) {
            InputRoute::Selected(key) | InputRoute::Shown(key) => {
                self.registry.input_modes(&key).bracketed_paste
            }
            InputRoute::Hold => false,
        };
        self.forward_input(crate::display::paste::for_client(&text, bracketed));
    }

    /// Tells the attachment in the terminal view when it gains or loses the focus, in
    /// the form a terminal reports its own, and only when its client enabled focus
    /// reports. It holds the focus while xmux's window does and the terminal view holds
    /// xmux's focus with no popup and no machine or host screen over it, so moving to
    /// the nav, opening a popup, and switching to another session each read as a focus
    /// out for it. Runs on every loop pass, comparing with the attachment it last told,
    /// so every path that moves the focus is reported the same way.
    pub(super) fn sync_child_focus(&mut self) {
        let focused = self.keys_attachment().filter(|_| self.window_focused);
        if focused == self.child_focus {
            return;
        }
        if let Some(old) = self.child_focus.take() {
            if self.registry.input_modes(&old).focus_events {
                self.registry
                    .input(&old, crate::display::term::FOCUS_OUT.to_vec());
            }
        }
        if let Some(new) = &focused {
            if self.registry.input_modes(new).focus_events {
                self.registry
                    .input(new, crate::display::term::FOCUS_IN.to_vec());
            }
        }
        self.child_focus = focused;
    }

    /// The attachment the keys typed now reach: the session in the terminal view while
    /// the terminal view holds xmux's focus with no popup and no machine or host screen
    /// over it.
    fn keys_attachment(&self) -> Option<String> {
        (self.model.state.focus.is_terminal_focused()
            && !self.model.state.is_modal_popup_open()
            && self
                .model
                .switcher
                .current_view_screen(&self.model.state)
                .is_none())
        .then(|| display_key(&self.hosts, &self.model.state.displayed))
    }

    /// The bytes that bring xmux's terminal to the kitty keyboard protocol flags of the
    /// session the keys reach, so the terminal encodes each key the way that session's
    /// client asked and xmux forwards it unchanged; flags 0, the legacy keys, while the
    /// keys reach xmux itself. Empty when the flags are already in force or the terminal
    /// has no protocol. The first update pushes an entry of xmux's own on the
    /// terminal's flag stack, which the terminal guard pops on exit.
    pub(super) fn keyboard_update(&mut self) -> Vec<u8> {
        use crate::display::keyboard;
        if !keyboard::supported() {
            return Vec::new();
        }
        let mut out = Vec::new();
        if !self.keyboard_pushed {
            out.extend_from_slice(keyboard::PUSH);
            self.keyboard_pushed = true;
        }
        let flags = self
            .keys_attachment()
            .map_or(0, |key| self.registry.input_modes(&key).keyboard_flags);
        if flags != self.keyboard_flags {
            out.extend(keyboard::set_flags(flags));
            self.keyboard_flags = flags;
        }
        out
    }

    /// Forwards terminal input to the session [`input_route`] names, behind any input
    /// still held for the same selection so the order typed is the order delivered.
    pub(super) fn forward_input(&mut self, bytes: Vec<u8>) {
        if let Some(held) = self
            .held_input
            .as_mut()
            .filter(|held| held.selection == self.model.state.selection)
        {
            held.bytes.extend(bytes);
            self.flush_held_input();
            return;
        }
        self.flush_held_input();
        match input_route(&self.model.state, &self.hosts) {
            InputRoute::Selected(key) if !self.awaits_first_frame(&key) => {
                self.registry.input(&key, bytes)
            }
            InputRoute::Shown(key) => self.registry.input(&key, bytes),
            InputRoute::Selected(_) | InputRoute::Hold => {
                self.held_input = Some(HeldInput {
                    selection: self.model.state.selection.clone(),
                    since: std::time::Instant::now(),
                    bytes,
                });
            }
        }
    }

    /// Whether input for the attachment under `key` waits for its first frame: the host's
    /// mux drops the keys its client reads before that frame, and the attachment input
    /// reaches there has drawn nothing yet.
    fn awaits_first_frame(&self, key: &str) -> bool {
        self.hosts
            .get(host_of_key(key))
            .is_some_and(|h| h.mux.drops_input_before_first_frame())
            && !self.registry.input_target_painted(key)
    }

    /// Delivers held input once the selection's attachment exists, and drops it when
    /// the selection moved away or the attachment it waits for is not arriving. Runs on
    /// every loop pass, because an attachment arrives on an event that carries no input.
    pub(super) fn flush_held_input(&mut self) {
        let Some(held) = self.held_input.take() else {
            return;
        };
        let current = held.selection == self.model.state.selection;
        match input_route(&self.model.state, &self.hosts) {
            InputRoute::Selected(key) if current && !self.awaits_first_frame(&key) => {
                self.registry.input(&key, held.bytes)
            }
            InputRoute::Selected(_) | InputRoute::Hold
                if current && held.since.elapsed() < HELD_INPUT_MAX =>
            {
                self.held_input = Some(held);
            }
            _ => tracing::info!(
                session = %held.selection.session,
                bytes = held.bytes.len(),
                "held_input_dropped"
            ),
        }
    }
}

/// What a key does on a machine's or a host's screen in the terminal view: the arrows
/// and the tabs step through its links, Enter opens the selected one, and `d` unfolds an
/// unreachable machine's details. The key table's screen section names every key this reads.
pub(super) fn screen_msg(key: crate::state::Key, unreachable: bool) -> Option<Msg> {
    use crate::state::Key;
    match key {
        Key::Up | Key::BackTab => Some(Msg::StepLink(-1)),
        Key::Down | Key::Tab => Some(Msg::StepLink(1)),
        Key::Enter => Some(Msg::OpenLink(None)),
        Key::Char('d') if unreachable => Some(Msg::Key(ratatui::crossterm::event::KeyEvent::new(
            ratatui::crossterm::event::KeyCode::Char('d'),
            ratatui::crossterm::event::KeyModifiers::NONE,
        ))),
        _ => None,
    }
}
