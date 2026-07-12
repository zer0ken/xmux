use super::*;

impl Switcher {
    // --- key handling -------------------------------------------------------

    /// Open the modal keys help modal. In tree focus any key then dismisses it (see
    /// `handle_key`); [`toggle_help`] is the focus-independent open/close entry point.
    pub fn show_help(&mut self, state: &mut crate::state::State) {
        self.dismiss_modals(state);
        state.modal = Some(Modal::Help);
    }

    /// Toggle the keys help modal. Driven by `prefix ?` in EITHER focus so help opens
    /// and closes the same way regardless of which pane holds focus.
    pub fn toggle_help(&mut self, state: &mut crate::state::State) {
        if matches!(state.modal, Some(Modal::Help)) {
            state.modal = None;
        } else {
            self.dismiss_modals(state);
            state.modal = Some(Modal::Help);
        }
    }

    /// Closes any open modal and resets the popup drag position. The single `popup`
    /// Option already makes the modals mutually exclusive (opening one drops the rest);
    /// this is the explicit close + drag reset used by every opener and on dismissal.
    fn dismiss_modals(&mut self, state: &mut crate::state::State) {
        state.modal = None;
        self.popup_geo.reset();
    }

    /// True while a modal popup is being border-dragged; the app routes every
    /// mouse event here until release, like the view border drag / menu hold.
    pub fn popup_drag_active(&self) -> bool {
        self.popup_geo.drag_active()
    }

    /// A left press on the active modal popup's border begins a move-drag. Returns
    /// true iff it grabbed (so the app consumes the event).
    pub fn begin_popup_drag(&mut self, col: u16, row: u16, state: &crate::state::State) -> bool {
        self.popup_geo
            .begin_drag(col, row, state.is_modal_popup_open())
    }

    /// Updates the popup offset from the selection while a border-drag is active.
    pub fn drag_popup(&mut self, col: u16, row: u16) {
        self.popup_geo.drag(col, row);
    }

    /// Ends a border-drag.
    pub fn end_popup_drag(&mut self) {
        self.popup_geo.end_drag();
    }

    /// Modal help input, tmux view-mode style. While the modal is open it captures
    /// the whole key read (returns true ⇒ consumed — nothing reaches the tree or the
    /// terminal view); `q` or Esc closes it, every other key is swallowed. Returns false
    /// when help is closed, so the read falls through to normal routing. The single
    /// owner of help dismissal — the app calls it above the tree/terminal split, so the
    /// behavior is identical in both focuses.
    pub fn feed_help_key(&mut self, bytes: &[u8], state: &mut crate::state::State) -> bool {
        modal::feed_help(&mut state.modal, bytes)
    }

    /// Handles one key against the switcher. Navigation/modal-open keys mutate the
    /// switcher's own view state and `state.modal` directly and return no command;
    /// the keys that COMMIT a slow mux action (Enter on an input, `y` on a kill
    /// confirm) return the [`Command`]s `State::apply` produced for the run loop to
    /// dispatch (off-loop `run_op`). The caller dispatches the returned commands; an
    /// empty vec means there was no effect.
    pub fn handle_key(&mut self, ev: KeyEvent, state: &mut crate::state::State) -> Vec<Command> {
        if matches!(state.modal, Some(Modal::Input(_))) {
            return self.handle_input_key(ev, state);
        }
        if matches!(state.modal, Some(Modal::Kill(_))) {
            return self.resolve_kill(ev, state);
        }
        // A flash is a transient error/message — it lives only until the next key. Clear
        // it here so navigation (or any key) restores the normal help
        // hint_bar; actions below may set a fresh one, which survives because this runs first.
        state.chrome.flash.clear();
        // The arrow / hjkl semantics track the on-screen layout so the keys always follow
        // what the user sees. Side (a vertical list): ↑/↓ move between siblings at the
        // current tree level, →/← change level (→ descends to the first child / expands a
        // folded host, ← ascends / collapses). Top (per-host columns): ↑/↓ move WITHIN the
        // current host's column, ←/→ move BETWEEN host columns. Space folds in either.
        let top = self.layout() == ViewLayout::Top;
        match ev.code {
            KeyCode::Enter => {}
            KeyCode::Up | KeyCode::Char('k') => {
                if top {
                    self.move_within_host(-1, state)
                } else {
                    self.move_sibling(-1, state)
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if top {
                    self.move_within_host(1, state)
                } else {
                    self.move_sibling(1, state)
                }
            }
            KeyCode::Right | KeyCode::Char('l') => {
                if top {
                    self.move_host(1, state)
                } else {
                    self.expand_or_descend(state)
                }
            }
            KeyCode::Left | KeyCode::Char('h') => {
                if top {
                    self.move_host(-1, state)
                } else {
                    self.collapse_or_ascend(state)
                }
            }
            KeyCode::PageUp => self.move_selection(-10, state),
            KeyCode::PageDown => self.move_selection(10, state),
            KeyCode::Home => self.move_to(0, state),
            KeyCode::End => self.move_to(-1, state),
            KeyCode::Char(c) => match c {
                '/' => self.open_input(InputMode::Filter, state),
                'n' => self.open_new(state),
                'R' => self.open_input(InputMode::Rename, state),
                'x' => self.arm_kill(state),
                'r' => self.request_rescan(state),
                // Space folds/unfolds the selected host (a no-op on other rows).
                ' ' => self.toggle_host_fold(state),
                // Quick-jump: 1..9 select the Nth selectable row (the dim digit shown on
                // that row), reusing the normal selection/attach-debounce path.
                '1'..='9' => self.move_to((c as u8 - b'1') as isize, state),
                _ => {}
            },
            _ => {}
        }
        Vec::new()
    }

    // --- input row ----------------------------------------------------------

    pub(super) fn open_input(&mut self, mode: InputMode, state: &mut crate::state::State) {
        state.chrome.flash.clear();
        self.dismiss_modals(state);
        match mode {
            InputMode::Filter => {
                state.modal = Some(Modal::Input(Input::new(
                    mode,
                    " filter sessions".into(),
                    state.filter.clone(),
                    None,
                    None,
                    None,
                )));
            }
            InputMode::Rename => match self.current_ref().cloned() {
                Some(RowRef::Host { .. }) => {
                    state.flash("cannot rename a host");
                }
                Some(RowRef::Session(sess)) => {
                    state.modal = Some(Modal::Input(Input::new(
                        mode,
                        " rename session".into(),
                        sess.name.clone(),
                        None,
                        Some(sess),
                        None,
                    )));
                }
                Some(RowRef::Window { sess, window }) => {
                    let win_name = self
                        .window_name(&sess.address(), window, state)
                        .unwrap_or_default();
                    let target = crate::mux::window_target(&sess.name, window);
                    state.modal = Some(Modal::Input(Input::new(
                        mode,
                        " rename window".into(),
                        win_name,
                        Some(sess.source.clone()),
                        Some(sess),
                        Some(target),
                    )));
                }
                _ => {}
            },
            // New/NewWindow/SplitWindow are opened by `open_new` (level-aware).
            InputMode::New | InputMode::NewWindow | InputMode::SplitWindow => {}
        }
    }

    /// The level-aware `n` action: a new SESSION on a host row, a new WINDOW on a
    /// session row, or a new PANE (split) on a window row (prompting the split
    /// direction). The prompt context is captured up front so a streamed selection
    /// move cannot retarget it.
    pub(super) fn open_new(&mut self, state: &mut crate::state::State) {
        state.chrome.flash.clear();
        self.dismiss_modals(state);
        if self.current_host_unreachable() {
            state.flash("host unreachable, cannot create here");
            return;
        }
        let Some(reference) = self.current_ref().cloned() else {
            return;
        };
        state.modal = match reference {
            RowRef::Host { source, .. } => Some(Modal::Input(Input::new(
                InputMode::New,
                " new session name (empty = auto)".into(),
                String::new(),
                Some(source),
                None,
                None,
            ))),
            RowRef::Session(sess) => Some(Modal::Input(Input::new(
                InputMode::NewWindow,
                format!(" new window in {} (name optional)", sess.name),
                String::new(),
                Some(sess.source.clone()),
                Some(sess),
                None,
            ))),
            RowRef::Window { sess, window } => {
                let target = crate::mux::window_target(&sess.name, window);
                Some(Modal::Input(Input::new(
                    InputMode::SplitWindow,
                    " split [v]ertical / [h]orizontal (default v)".into(),
                    String::new(),
                    Some(sess.source.clone()),
                    Some(sess),
                    Some(target),
                )))
            }
            RowRef::Loading => None,
        };
    }

    pub(super) fn close_input(&mut self, state: &mut crate::state::State) {
        state.modal = None;
    }

    fn handle_input_key(&mut self, ev: KeyEvent, state: &mut crate::state::State) -> Vec<Command> {
        match ev.code {
            KeyCode::Enter => {
                let (mode, val, source, sess, target) = {
                    let Some(Modal::Input(input)) = &state.modal else {
                        return Vec::new();
                    };
                    (
                        input.mode,
                        input.buffer.trim().to_string(),
                        input.source.clone(),
                        input.sess.clone(),
                        input.target.clone(),
                    )
                };
                // Close the input first so a queue helper that early-returns on a
                // validation failure (empty/unchanged name) still dismisses the modal.
                self.close_input(state);
                match mode {
                    InputMode::Filter => {
                        state.filter = val;
                        self.rebuild(state);
                        Vec::new()
                    }
                    InputMode::New => self.queue_create(source, &val, state),
                    InputMode::NewWindow => self.queue_new_window(source, sess, &val, state),
                    InputMode::SplitWindow => self.queue_split(source, sess, target, &val, state),
                    InputMode::Rename => {
                        if target.is_some() {
                            self.queue_rename_window(source, sess, target, &val, state)
                        } else {
                            self.queue_rename(sess, &val, state)
                        }
                    }
                }
            }
            KeyCode::Esc => {
                self.close_input(state);
                Vec::new()
            }
            // All other keys edit the buffer at the caret. Grab the input once so each
            // editing key routes through the same borrow. The byte decoder delivers
            // Ctrl-letters as their control char (like the C-g prefix), so Ctrl-U / Ctrl-W
            // match the raw NAK / ETB bytes, not Char('u')/Char('w') + a modifier.
            code => {
                if let Some(Modal::Input(input)) = state.modal.as_mut() {
                    match code {
                        KeyCode::Backspace => input.backspace(),
                        KeyCode::Delete => input.delete(),
                        KeyCode::Left => input.left(),
                        KeyCode::Right => input.right(),
                        KeyCode::Home => input.home(),
                        KeyCode::End => input.end(),
                        KeyCode::Char('\u{15}') => input.clear_line(),
                        KeyCode::Char('\u{17}') => input.delete_word_before(),
                        // Ignore control chars so a stray C-g etc. never lands as text.
                        KeyCode::Char(c) if !c.is_control() => input.insert(c),
                        _ => {}
                    }
                }
                Vec::new()
            }
        }
    }

    /// Test/host hook: set the active input buffer directly.
    pub fn set_input_text(&mut self, text: &str, state: &mut crate::state::State) {
        if let Some(Modal::Input(input)) = state.modal.as_mut() {
            input.buffer = text.to_string();
            input.cursor = text.chars().count();
        }
    }

    /// Resolves a create into an [`Action::CreateSession`] and folds it through
    /// `State::apply`, returning the resulting [`Command`] (a `RunOp`) for the run
    /// loop to dispatch off-loop. The network call is NOT made here, so the
    /// key-handling path never blocks on an ssh round-trip; [`run_op`] performs it
    /// off-loop and [`Switcher::apply_op_result`] folds the result in.
    fn queue_create(
        &mut self,
        source: Option<String>,
        name: &str,
        state: &mut crate::state::State,
    ) -> Vec<Command> {
        let Some(source) = source else {
            return Vec::new();
        };
        state.apply(Action::CreateSession {
            source,
            name: name.to_string(),
        })
    }

    /// Resolves a new-window in the captured session (the `n` action on a session
    /// row) into an [`Action::NewWindow`]. An empty name lets the mux auto-name it.
    fn queue_new_window(
        &mut self,
        source: Option<String>,
        sess: Option<Session>,
        name: &str,
        state: &mut crate::state::State,
    ) -> Vec<Command> {
        let (Some(source), Some(sess)) = (source, sess) else {
            return Vec::new();
        };
        state.apply(Action::NewWindow {
            source,
            session: sess.name,
            name: name.to_string(),
        })
    }

    /// Resolves a split of the captured window target (the `n` action on a window
    /// row) into an [`Action::SplitWindow`]. The direction defaults to vertical
    /// unless the buffer starts with `h`.
    fn queue_split(
        &mut self,
        source: Option<String>,
        sess: Option<Session>,
        target: Option<String>,
        dir: &str,
        state: &mut crate::state::State,
    ) -> Vec<Command> {
        let (Some(source), Some(sess), Some(target)) = (source, sess, target) else {
            return Vec::new();
        };
        let vertical = !dir.trim().eq_ignore_ascii_case("h");
        state.apply(Action::SplitWindow {
            source,
            target,
            session: sess.name,
            vertical,
        })
    }

    /// The current name of window `index` under the session at `sess_addr`, if its panes are loaded.
    fn window_name(
        &self,
        sess_addr: &str,
        index: i64,
        state: &crate::state::State,
    ) -> Option<String> {
        state
            .panes
            .get(sess_addr)
            .and_then(|ws| ws.iter().find(|w| w.index == index))
            .map(|w| w.name.clone())
    }

    /// Resolves a rename into an [`Action::RenameSession`] after the synchronous
    /// validation that needs no network. See [`Switcher::queue_create`] for why the
    /// op is deferred off-loop.
    fn queue_rename(
        &mut self,
        sess: Option<Session>,
        new_name: &str,
        state: &mut crate::state::State,
    ) -> Vec<Command> {
        let Some(sess) = sess else {
            return Vec::new();
        };
        if new_name.is_empty() || new_name == sess.name {
            return Vec::new();
        }
        if new_name.starts_with('-') {
            // the mux silently no-ops a '-'-leading name (getopt eats it) — refuse.
            state.flash("rename: name cannot start with '-'");
            return Vec::new();
        }
        state.apply(Action::RenameSession {
            sess,
            new_name: new_name.to_string(),
        })
    }

    fn queue_rename_window(
        &mut self,
        source: Option<String>,
        sess: Option<Session>,
        target: Option<String>,
        new_name: &str,
        state: &mut crate::state::State,
    ) -> Vec<Command> {
        let (Some(source), Some(sess), Some(target)) = (source, sess, target) else {
            return Vec::new();
        };
        let cur = target
            .rsplit(':')
            .next()
            .and_then(|i| i.parse::<i64>().ok())
            .and_then(|idx| self.window_name(&sess.address(), idx, state));
        if new_name.is_empty() || cur.as_deref() == Some(new_name) {
            return Vec::new();
        }
        if new_name.starts_with('-') {
            state.flash("rename: name cannot start with '-'");
            return Vec::new();
        }
        state.apply(Action::RenameWindow {
            source,
            session: sess.name,
            target,
            new_name: new_name.to_string(),
        })
    }

    /// Applies a completed [`MuxOp`](crate::model::MuxOp)'s [`OpResult`] to the
    /// in-memory tree. The result is applied on the event loop after `run_op`
    /// returns off-loop, so a slow ssh round-trip never blocks rendering. State
    /// owns the inventory fold ([`State::fold_op_result`](crate::state::State::fold_op_result));
    /// the switcher only rebuilds its rows + restores the cursor per the returned
    /// [`OpFollow`].
    pub fn apply_op_result(&mut self, result: OpResult, state: &mut crate::state::State) {
        match state.fold_op_result(result) {
            OpFollow::Reselect(addr) => {
                self.rebuild(state);
                if let Some(i) = self.row_of_session(&addr) {
                    self.user_moved = true;
                    self.set_selected(i, state);
                }
            }
            OpFollow::Rebuild => self.rebuild(state),
            OpFollow::RebuildPreservingFocus => {
                let prior = self.capture_focus();
                self.rebuild(state);
                self.restore_focus(prior, state);
            }
            OpFollow::Flash(message) => {
                state.flash(message);
            }
        }
    }

    pub(super) fn row_of_session(&self, address: &str) -> Option<usize> {
        self.rows
            .iter()
            .position(|r| matches!(&r.reference, RowRef::Session(s) if s.address() == address))
    }

    // --- kill (confirm popup) -----------------------------------------------

    pub(super) fn arm_kill(&mut self, state: &mut crate::state::State) {
        self.dismiss_modals(state);
        match self.current_ref().cloned() {
            Some(RowRef::Host { .. }) => {
                state.flash("cannot kill a host");
            }
            Some(RowRef::Session(sess)) => {
                state.modal = Some(Modal::Kill(PendingKill::Session(sess)));
            }
            Some(RowRef::Window { sess, window }) => {
                let target = crate::mux::window_target(&sess.name, window);
                state.modal = Some(Modal::Kill(PendingKill::Window {
                    source: sess.source.clone(),
                    session: sess.name.clone(),
                    target,
                }));
            }
            _ => {}
        }
    }

    fn resolve_kill(&mut self, ev: KeyEvent, state: &mut crate::state::State) -> Vec<Command> {
        // tmux confirm-before semantics: only y/Y confirms; any other key — n, Esc, or
        // anything else — cancels (the pending confirm is taken either way).
        let confirmed = matches!(ev.code, KeyCode::Char('y') | KeyCode::Char('Y'));
        let Some(Modal::Kill(armed)) = state.modal.take() else {
            return Vec::new();
        };
        if !confirmed {
            return Vec::new();
        }
        let action = match armed {
            PendingKill::Session(sess) => Action::KillSession { sess },
            PendingKill::Window {
                source,
                session,
                target,
            } => Action::KillWindow {
                source,
                session,
                target,
            },
            PendingKill::Pane {
                source,
                session,
                target,
            } => Action::KillPane {
                source,
                session,
                target,
            },
        };
        state.apply(action)
    }

    /// Arms a kill confirm for the ACTIVE pane of the DISPLAYED session — the pane the
    /// terminal view is showing (tmux `prefix x` parity). Unlike [`arm_kill`], which
    /// targets the tree SELECTION, this reads `state.displayed` and resolves that
    /// session's active window from the cached pane data, so the confirmed kill hits the
    /// pane on screen regardless of where the tree cursor sits. A no-op flash when no
    /// session is displayed or its active pane is not yet known.
    pub fn arm_kill_active_pane(&mut self, state: &mut crate::state::State) {
        self.dismiss_modals(state);
        let sel = state.displayed.clone();
        if sel.is_empty() {
            state.flash("no session displayed");
            return;
        }
        let addr = crate::session::address_of(&sel.source, &sel.session);
        let Some(window) = state
            .panes
            .get(&addr)
            .and_then(|ws| ws.iter().find(|w| w.active))
            .map(|w| w.index)
        else {
            state.flash("no active pane to kill");
            return;
        };
        // session:window (not a bare session) so a numeric session name can't be
        // mis-parsed as a window index — the mux resolves the window's active pane.
        let target = crate::mux::window_target(&sel.session, window);
        state.modal = Some(Modal::Kill(PendingKill::Pane {
            source: sel.source,
            session: sel.session,
            target,
        }));
    }
}
