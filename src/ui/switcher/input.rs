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
    /// the whole key read (returns true ⇒ consumed - nothing reaches the tree or the
    /// terminal view); `q` or Esc closes it, every other key is swallowed. Returns false
    /// when help is closed, so the read falls through to normal routing. The single
    /// owner of help dismissal - the app calls it above the tree/terminal split, so the
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
        // A flash is a transient error/message - it lives only until the next key. Clear
        // it here so navigation (or any key) restores the normal help
        // hint_bar; actions below may set a fresh one, which survives because this runs first.
        state.chrome.flash.clear();
        // The flat card list has no levels or host columns: ↑/↓ (and k/j) step one card,
        // ←/→ step one category, PageUp/Down jump ten, Home/End go to the ends (prefix
        // →/Enter focuses the terminal at the app layer). `n` starts a session on
        // the selected host; a digit opens the jump popup seeded with it (the app only
        // forwards a digit here behind the prefix).
        match ev.code {
            KeyCode::Enter => {}
            // ↑/↓ and ←/→ name the two things the list is made of: ↑/↓ walk the cards,
            // ←/→ walk the categories, landing on the first card of the previous/next
            // one. Neither is defined by where a card sits on screen, so both mean the
            // same thing in the side column and in the portrait band, which flows its
            // cards down a column and then right.
            KeyCode::Up | KeyCode::Char('k') => self.nav_vertical(-1, state),
            KeyCode::Down | KeyCode::Char('j') => self.nav_vertical(1, state),
            KeyCode::Left => self.nav_horizontal(-1, state),
            KeyCode::Right => self.nav_horizontal(1, state),
            KeyCode::PageUp => self.move_selection(-10, state),
            KeyCode::PageDown => self.move_selection(10, state),
            KeyCode::Home => self.move_to(0, state),
            KeyCode::End => self.move_to(-1, state),
            KeyCode::Char(c) => match c {
                '/' => self.open_input(InputMode::Filter, state),
                'n' => self.open_new(state),
                'r' => self.request_rescan(state),
                // Jump: the digit opens the jump popup already holding it, so the
                // number can be extended (4 → 41) without a second keystroke.
                '0'..='9' => self.open_jump(c, state),
                _ => {}
            },
            _ => {}
        }
        Vec::new()
    }

    // --- input row ----------------------------------------------------------

    /// Opens the fuzzy filter input. The only inline input the switcher opens by
    /// mode; `new session` is opened by [`Switcher::open_new`], which needs the
    /// selected host captured up front.
    pub(super) fn open_input(&mut self, mode: InputMode, state: &mut crate::state::State) {
        state.chrome.flash.clear();
        self.dismiss_modals(state);
        match mode {
            InputMode::Filter => {
                let mut input =
                    Input::new(mode, " filter sessions".into(), state.filter.clone(), None);
                // The filter the input opened from: Esc restores it, undoing every
                // live edit made while the input was open.
                input.restore_filter = Some(state.filter.clone());
                state.modal = Some(Modal::Input(Box::new(input)));
            }
            // New is opened by `open_new`, Jump by `open_jump`, and the unlock steps
            // by `open_unlock_user` / `open_unlock_password` (all capture context the
            // mode alone does not carry).
            InputMode::New | InputMode::Jump | InputMode::User | InputMode::Password => {}
        }
    }

    /// The unlock entry: the USERNAME step of the two-step unlock input. The id is
    /// never guessed or prefilled - the user always types it, so what is submitted
    /// here is exactly what the unlock uses.
    pub(crate) fn open_unlock_user(&mut self, state: &mut crate::state::State) {
        state.chrome.flash.clear();
        self.dismiss_modals(state);
        let Some(source) = self.current_source() else {
            return;
        };
        state.modal = Some(crate::ui::modal::Modal::Input(Box::new(
            crate::ui::modal::Input::new(
                crate::ui::modal::InputMode::User,
                format!(" username for {source}"),
                String::new(),
                Some(source),
            ),
        )));
    }

    /// The masked PASSWORD step of the unlock, carrying the id the User step just
    /// submitted; the buffer renders masked either way.
    fn open_unlock_password(
        &mut self,
        source: Option<String>,
        user: &str,
        state: &mut crate::state::State,
    ) -> Vec<crate::model::Command> {
        let Some(source) = source else {
            return Vec::new();
        };
        let mut input = crate::ui::modal::Input::new(
            crate::ui::modal::InputMode::Password,
            format!(" password for {user}@{source}"),
            String::new(),
            Some(source),
        );
        input.unlock_user = Some(user.to_string());
        state.modal = Some(crate::ui::modal::Modal::Input(Box::new(input)));
        Vec::new()
    }

    /// The `n` action: a new SESSION on the selected card's host/mux. Every card
    /// names a source - a session or loading card by its session, a host card by
    /// itself - so `n` adds a session to the source in front of the user, not only
    /// to an empty host. (xmux does not edit a session's windows, so there is
    /// nothing else `n` could add.) The source is captured up front so a streamed
    /// selection move cannot retarget it.
    pub(super) fn open_new(&mut self, state: &mut crate::state::State) {
        state.chrome.flash.clear();
        self.dismiss_modals(state);
        if self.current_host_blocked() {
            state.flash("host locked or unreachable, cannot create here");
            return;
        }
        let Some(source) = self.current_source() else {
            return;
        };
        state.modal = Some(Modal::Input(Box::new(Input::new(
            InputMode::New,
            " new session name (empty = auto)".into(),
            String::new(),
            Some(source),
        ))));
    }

    /// The row `number` addresses, or `None` when no card carries it: the number-th
    /// SELECTABLE card, section titles excepted, counting from 1. The buffer is read
    /// as its value, spelling included, so 01 is 1: the values no card carries are 0
    /// alone and everything past the last card. The jump reads it on every edit to
    /// move the selection while the number names a card, and at Enter to decide
    /// whether to land or flash: see [`Switcher::jump_accepts`].
    fn jump_row(&self, number: &str) -> Option<usize> {
        let n = number.trim().parse::<usize>().ok()?;
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.selectable())
            .nth(n.checked_sub(1)?)
            .map(|(i, _)| i)
    }

    /// Whether the jump would land on `number`, i.e. some card carries it. Read at
    /// Enter only: every digit is taken while typing, and a number that names no card
    /// just leaves the selection alone until Enter, which flashes the range. An empty
    /// buffer is not acceptable as a jump target but is a legal editing state, so it is
    /// handled by the caller, not here.
    fn jump_accepts(&self, number: &str) -> bool {
        self.jump_row(number).is_some()
    }

    /// Opens the jump popup seeded with `digit`, remembering the session to return to.
    /// The digit is applied immediately when it names a card, so `prefix 4` lands on 4
    /// and the popup stays open only to let the number grow (4 → 41 → 417) or be
    /// cancelled. A digit no card carries still opens the popup holding it; the
    /// selection is only moved while the number names a card, so a dead number just
    /// waits for Enter to vet it.
    pub(super) fn open_jump(&mut self, digit: char, state: &mut crate::state::State) {
        state.chrome.flash.clear();
        let seed = digit.to_string();
        let last = self.selectable_count();
        let restore = self.current_ref().cloned();
        self.dismiss_modals(state);
        let mut input = Input::new(
            InputMode::Jump,
            format!(" jump to a session (1 - {last})"),
            seed,
            None,
        );
        input.restore = restore;
        state.modal = Some(Modal::Input(Box::new(input)));
        self.apply_jump(state);
    }

    /// Moves the selection to the card the open jump popup's buffer names. The move
    /// happens only while the number names a card and leaves the selection alone
    /// otherwise (an empty buffer, or a number past the last card), so the number reads
    /// as a live cursor rather than a value submitted at the end.
    fn apply_jump(&mut self, state: &mut crate::state::State) {
        let Some(Modal::Input(input)) = &state.modal else {
            return;
        };
        let Some(n) = self.jump_row(&input.buffer.clone()) else {
            return;
        };
        self.user_moved = true;
        self.set_selected(n, state);
    }

    /// Reflects the open filter input's buffer into the active filter and re-derives
    /// the list, so the filter applies as the user types rather than at Enter. A no-op
    /// when no filter input is open. The trimmed buffer is what the filter stores, so
    /// typing a trailing space does not change the active filter.
    fn apply_filter(&mut self, state: &mut crate::state::State) {
        let filter = match &state.modal {
            Some(Modal::Input(input)) if input.mode == InputMode::Filter => {
                input.buffer.trim().to_string()
            }
            _ => return,
        };
        if state.filter == filter {
            return;
        }
        state.filter = filter;
        self.rebuild(state);
    }

    /// Returns the selection to the card a cancelled jump started from, matched by
    /// identity so a rebuild mid-jump cannot land on the wrong card. A card that
    /// vanished meanwhile leaves the selection where the jump put it.
    fn restore_jump(&mut self, restore: Option<RowRef>, state: &mut crate::state::State) {
        let Some(target) = restore else {
            return;
        };
        if let Some(i) = self.row_matching(&target) {
            self.set_selected(i, state);
        }
    }

    pub(super) fn close_input(&mut self, state: &mut crate::state::State) {
        state.modal = None;
    }

    fn handle_input_key(&mut self, ev: KeyEvent, state: &mut crate::state::State) -> Vec<Command> {
        // A flash is a transient error/message - it lives only until the next key. Clear
        // it here so a key while an input is open (a fresh edit, a fresh Enter) restores
        // the input line; an action below may set a fresh one, which survives because
        // this runs first.
        state.chrome.flash.clear();
        match ev.code {
            KeyCode::Enter => {
                let (mode, val, source, unlock_user) = {
                    let Some(Modal::Input(input)) = &state.modal else {
                        return Vec::new();
                    };
                    (
                        input.mode,
                        input.buffer.trim().to_string(),
                        input.source.clone(),
                        input.unlock_user.clone(),
                    )
                };
                match mode {
                    // Enter on a jump lands only when the buffer names a card. A number
                    // no card carries flashes the range and keeps the popup open, so the
                    // user can find out how high the numbers go without closing; an
                    // empty buffer just keeps it open.
                    InputMode::Jump => {
                        if !val.is_empty() && self.jump_accepts(&val) {
                            self.close_input(state);
                        } else {
                            let last = self.selectable_count();
                            if !val.is_empty() {
                                state.flash(format!("no session {val} (1 - {last})"));
                            }
                        }
                        Vec::new()
                    }
                    // The filter applied on every edit, so Enter only closes it; the
                    // create input closes first so a queue helper that early-returns on a
                    // validation failure (empty/unchanged name) still dismisses the
                    // modal.
                    _ => {
                        self.close_input(state);
                        match mode {
                            InputMode::Filter => Vec::new(),
                            InputMode::New => self.queue_create(source, &val, state),
                            InputMode::Jump => Vec::new(),
                            // The unlock steps: User opens the masked password step
                            // carrying the id; Password submits the unlock with the
                            // carried id (never a guessed one).
                            InputMode::User => self.open_unlock_password(source, &val, state),
                            InputMode::Password => self.queue_unlock(source, unlock_user, &val),
                        }
                    }
                }
            }
            KeyCode::Esc => {
                // A cancelled jump must undo the moves it already made; a cancelled
                // filter must restore the filter it opened from (it applied live, so
                // every edit needs undoing); every other mode has changed nothing yet,
                // so closing is the whole cancel.
                let (restore, restore_filter) = match &state.modal {
                    Some(Modal::Input(i)) if i.mode == InputMode::Jump => (i.restore.clone(), None),
                    Some(Modal::Input(i)) if i.mode == InputMode::Filter => {
                        (None, i.restore_filter.clone())
                    }
                    _ => (None, None),
                };
                self.close_input(state);
                self.restore_jump(restore, state);
                if let Some(f) = restore_filter {
                    if state.filter != f {
                        state.filter = f;
                        self.rebuild(state);
                    }
                }
                Vec::new()
            }
            // All other keys edit the buffer at the caret. Grab the input once so each
            // editing key routes through the same borrow. The byte decoder delivers
            // Ctrl-letters as their control char (like the C-g prefix), so Ctrl-U / Ctrl-W
            // match the raw NAK / ETB bytes, not Char('u')/Char('w') + a modifier.
            code => {
                let mut jumping = false;
                let mut filtering = false;
                if let Some(Modal::Input(input)) = state.modal.as_mut() {
                    jumping = input.mode == InputMode::Jump;
                    filtering = input.mode == InputMode::Filter;
                    match code {
                        KeyCode::Backspace => input.backspace(),
                        KeyCode::Delete => input.delete(),
                        KeyCode::Left => input.left(),
                        KeyCode::Right => input.right(),
                        KeyCode::Home => input.home(),
                        KeyCode::End => input.end(),
                        KeyCode::Char('\u{15}') => input.clear_line(),
                        KeyCode::Char('\u{17}') => input.delete_word_before(),
                        // A session number is digits only, so a stray letter is
                        // dropped rather than making the buffer unparseable. Every digit
                        // is taken as typed: the number only has to name a card at Enter,
                        // and until then a dead number just leaves the selection alone.
                        // Control chars are ignored in every mode so a stray C-g never
                        // lands as text.
                        KeyCode::Char(c) if jumping => {
                            if c.is_ascii_digit() {
                                input.insert(c);
                            }
                        }
                        KeyCode::Char(c) if !c.is_control() => input.insert(c),
                        _ => {}
                    }
                }
                // Both live modes act WHILE open: a jump re-targets the selection after
                // every edit, and a filter re-derives the list after every edit.
                if jumping {
                    self.apply_jump(state);
                }
                if filtering {
                    self.apply_filter(state);
                }
                Vec::new()
            }
        }
    }

    /// Test/host hook: set the active input buffer directly. For the filter input the
    /// buffer is also applied to the active filter, matching the live apply that every
    /// keystroke performs, so the hook leaves the same state a real edit would.
    pub fn set_input_text(&mut self, text: &str, state: &mut crate::state::State) {
        if let Some(Modal::Input(input)) = state.modal.as_mut() {
            input.buffer = text.to_string();
            input.cursor = text.chars().count();
        }
        self.apply_filter(state);
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

    /// Submits the unlock: the id carried from the User step plus the entered
    /// password become the off-loop `RunUnlock` command. The password is never
    /// persisted or rendered (the field draws masked), and nothing is guessed - the
    /// id is exactly what was submitted in the User step.
    fn queue_unlock(
        &mut self,
        source: Option<String>,
        user: Option<String>,
        password: &str,
    ) -> Vec<Command> {
        let (Some(source), Some(user)) = (source, user) else {
            return Vec::new();
        };
        vec![Command::RunUnlock {
            source,
            user,
            password: password.to_string(),
        }]
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
            OpFollow::Flash(message) => {
                state.flash(message);
            }
            // A successful unlock established the authenticated ControlMaster: a
            // roster re-scan then re-enumerates the host over it. Any failure stays
            // locked and flashes why (the secret is still in memory for a retry).
            OpFollow::UnlockResult(outcome) => match outcome {
                crate::link::unlock::UnlockOutcome::Ok => self.request_rescan(state),
                crate::link::unlock::UnlockOutcome::AuthFailed => {
                    state.flash("authentication failed");
                }
                crate::link::unlock::UnlockOutcome::Timeout => {
                    state.flash("unlock timed out");
                }
                crate::link::unlock::UnlockOutcome::Unavailable => {
                    state.flash("unlock unavailable on this platform");
                }
                crate::link::unlock::UnlockOutcome::Failed(msg) => {
                    state.flash(format!("unlock failed: {msg}"));
                }
            },
        }
    }

    pub(super) fn row_of_session(&self, address: &str) -> Option<usize> {
        self.rows
            .iter()
            .position(|r| session_addr_of(&r.reference).as_deref() == Some(address))
    }
}
