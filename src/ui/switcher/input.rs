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
        // ←/→ step one host, PageUp/Down jump ten, Home/End go to the ends (prefix
        // →/Enter focuses the terminal at the app layer). `n` starts a session on
        // the selected host; a digit opens the jump popup seeded with it (the app only
        // forwards a digit here behind the prefix).
        match ev.code {
            KeyCode::Enter => {}
            // The two axes name the two things the list is made of: ↑/↓ walk the cards,
            // ←/→ walk the hosts, landing on the first card of the previous/next source.
            // Neither is defined by where a card sits on screen, so both mean the same
            // thing in the side column and in the portrait band, which flows its cards
            // down a column and then right.
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
                state.modal = Some(Modal::Input(Input::new(
                    mode,
                    " filter sessions".into(),
                    state.filter.clone(),
                    None,
                )));
            }
            // New is opened by `open_new`, Jump by `open_jump` (both capture context
            // the mode alone does not carry).
            InputMode::New | InputMode::Jump => {}
        }
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
        if self.current_host_unreachable() {
            state.flash("host unreachable, cannot create here");
            return;
        }
        let Some(source) = self.current_source() else {
            return;
        };
        state.modal = Some(Modal::Input(Input::new(
            InputMode::New,
            " new session name (empty = auto)".into(),
            String::new(),
            Some(source),
        )));
    }

    /// The row `number` addresses, or `None` when no card carries it: the nth
    /// SELECTABLE card, section titles excepted. The numbers run `0..selectable_count`,
    /// so a number that names nothing today can never be extended into one either (any
    /// extra digit only makes it larger). That is what lets the jump VET a keystroke
    /// instead of holding a dead number: see [`Switcher::jump_accepts`].
    fn jump_row(&self, number: &str) -> Option<usize> {
        let n = number.trim().parse::<usize>().ok()?;
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.selectable())
            .nth(n)
            .map(|(i, _)| i)
    }

    /// Whether the jump would accept `number`, i.e. some card carries it. An empty
    /// buffer is not acceptable as a jump target but is a legal editing state, so it is
    /// handled by the caller, not here.
    fn jump_accepts(&self, number: &str) -> bool {
        self.jump_row(number).is_some()
    }

    /// Opens the jump popup seeded with `digit`, remembering the session to return to.
    /// The digit is applied immediately, so `prefix 4` lands on 4 and the popup stays
    /// open only to let the number grow (4 → 41 → 417) or be cancelled. A digit no
    /// card carries never opens it at all: with nine sessions, `prefix 9` is refused
    /// with a flash rather than opening a popup that cannot go anywhere.
    pub(super) fn open_jump(&mut self, digit: char, state: &mut crate::state::State) {
        state.chrome.flash.clear();
        let seed = digit.to_string();
        let last = self.selectable_count().saturating_sub(1);
        if !self.jump_accepts(&seed) {
            state.flash(format!("no session {seed} (0 - {last})"));
            return;
        }
        let restore = self.current_ref().cloned();
        self.dismiss_modals(state);
        let mut input = Input::new(
            InputMode::Jump,
            format!(" jump to a session (0 - {last})"),
            seed,
            None,
        );
        input.restore = restore;
        state.modal = Some(Modal::Input(input));
        self.apply_jump(state);
    }

    /// Moves the selection to the session the open jump popup's buffer names. Only an
    /// empty buffer leaves the selection alone: every digit the popup accepted keeps the
    /// number addressing a real card, so one, two, and three digit numbers all behave
    /// the same - the buffer is never showing a number you cannot land on.
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
        match ev.code {
            KeyCode::Enter => {
                let (mode, val, source) = {
                    let Some(Modal::Input(input)) = &state.modal else {
                        return Vec::new();
                    };
                    (
                        input.mode,
                        input.buffer.trim().to_string(),
                        input.source.clone(),
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
                    // The jump already happened on every edit, so Enter just commits by
                    // closing - there is nothing left to apply.
                    InputMode::Jump => Vec::new(),
                }
            }
            KeyCode::Esc => {
                // A cancelled jump must undo the moves it already made; every other mode
                // has changed nothing yet, so closing is the whole cancel.
                let restore = match &state.modal {
                    Some(Modal::Input(i)) if i.mode == InputMode::Jump => i.restore.clone(),
                    _ => None,
                };
                self.close_input(state);
                self.restore_jump(restore, state);
                Vec::new()
            }
            // All other keys edit the buffer at the caret. Grab the input once so each
            // editing key routes through the same borrow. The byte decoder delivers
            // Ctrl-letters as their control char (like the C-g prefix), so Ctrl-U / Ctrl-W
            // match the raw NAK / ETB bytes, not Char('u')/Char('w') + a modifier.
            code => {
                let mut jumping = false;
                // Vetting a digit needs the selectable count while `state.modal` is
                // mutably borrowed, so capture the predicate's input up front. Section
                // titles take no number, so the bound is the card count, not the row
                // count.
                let cards = self.selectable_count();
                let accepts =
                    |buf: String| matches!(buf.trim().parse::<usize>(), Ok(n) if n < cards);
                if let Some(Modal::Input(input)) = state.modal.as_mut() {
                    jumping = input.mode == InputMode::Jump;
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
                        // dropped rather than making the buffer unparseable. A digit is
                        // vetted by its RESULT: one that would take the number past the
                        // last card is refused, so the buffer always names a session you
                        // can land on and a two- or three-digit number behaves exactly
                        // like a one-digit one. Control chars are ignored in every mode
                        // so a stray C-g never lands as text.
                        KeyCode::Char(c) if jumping => {
                            if c.is_ascii_digit() && accepts(input.buffer_with(c)) {
                                input.insert(c);
                            }
                        }
                        KeyCode::Char(c) if !c.is_control() => input.insert(c),
                        _ => {}
                    }
                }
                // The jump acts WHILE open: re-target after every edit so the selection
                // tracks the number being typed.
                if jumping {
                    self.apply_jump(state);
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
        }
    }

    pub(super) fn row_of_session(&self, address: &str) -> Option<usize> {
        self.rows
            .iter()
            .position(|r| session_addr_of(&r.reference).as_deref() == Some(address))
    }
}
