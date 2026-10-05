use super::*;
use crate::model::keys::KeyCommand;
use crate::state::notify::{Level, Note};
use crate::state::PaletteChoice;

impl Switcher {
    // --- key handling -------------------------------------------------------

    /// The info key: the selection goes to the source the selected card is about, so its
    /// screen states the source's sessions and how they are kept current.
    fn select_host_section(&mut self) {
        let Some(source) = self.current_source() else {
            return;
        };
        self.note_user_move();
        self.select_node(Node::Source(source));
    }

    /// Open the modal keys help modal. In tree focus any key then dismisses it (see
    /// `handle_key`); [`toggle_help`] is the focus-independent open/close entry point.
    pub fn show_help(&mut self, state: &mut crate::state::State) {
        self.dismiss_modals(state);
        state.modal = Some(Modal::Help {
            query: String::new(),
            scroll: 0,
            tab: None,
            hover: None,
            decoder: crate::display::decode::KeyDecoder::new(),
        });
    }

    /// Toggle the keys help modal. Driven by `prefix ?` in EITHER focus so help opens
    /// and closes the same way regardless of which pane holds focus.
    pub fn toggle_help(&mut self, state: &mut crate::state::State) {
        if matches!(state.modal, Some(Modal::Help { .. })) {
            state.modal = None;
        } else {
            self.show_help(state);
        }
    }

    /// Toggles the history (`prefix m`) in either focus. Opening it takes every toast down:
    /// the history holds each of them, so the toasts have been read where they are kept.
    pub fn toggle_history(&mut self, state: &mut crate::state::State) {
        if matches!(state.modal, Some(Modal::History { .. })) {
            state.modal = None;
        } else {
            self.dismiss_modals(state);
            state.notify.dismiss_all();
            state.modal = Some(Modal::History { scroll: 0 });
        }
    }

    pub fn toggle_palette(&mut self, state: &mut crate::state::State) {
        if matches!(state.modal, Some(Modal::Palette { .. })) {
            state.modal = None;
        } else {
            self.dismiss_modals(state);
            state.modal = Some(Modal::Palette {
                query: String::new(),
                selected: 0,
                hover: None,
                open: false,
                decoder: crate::display::decode::KeyDecoder::new(),
            });
        }
    }

    pub(crate) fn palette_entries(
        &self,
        state: &crate::state::State,
        query: &str,
    ) -> Vec<(String, PaletteChoice)> {
        use KeyCommand::*;
        let commands = [
            Filter,
            FocusTerminal,
            FocusNav,
            NewSession,
            Rescan,
            RescanHost,
            Logout,
            Check,
            Collapse,
            AutoHide,
            Position,
            History,
            Help,
            Quit,
        ];
        let mut entries: Vec<_> = commands
            .into_iter()
            .filter_map(|command| {
                let entry = crate::model::keys::entry_for(command)?;
                let name = format!(
                    "{}  {}",
                    entry.long,
                    entry.full_label(&state.chrome.ui_prefix, state.chrome.nav_position)
                );
                Some((name, PaletteChoice::Command(command)))
            })
            .collect();
        entries.extend(
            self.check_entries(state)
                .into_iter()
                .filter(|entry| entry.kind != crate::model::FailureKind::ListFailed)
                .map(|entry| {
                    (
                        format!("log in to {}", entry.label),
                        PaletteChoice::Login(entry.source),
                    )
                }),
        );
        let terms: Vec<String> = query
            .split_whitespace()
            .map(|term| term.to_lowercase().replace('-', ""))
            .collect();
        entries.retain(|(name, choice)| {
            let mut text = name.to_lowercase().replace(['-', ' '], "");
            if let PaletteChoice::Login(source) = choice {
                text.push_str(&source.to_lowercase().replace(['-', ' '], ""));
            }
            terms.iter().all(|term| text.contains(term))
        });
        entries
    }

    pub(crate) fn take_palette_choice(
        &mut self,
        state: &mut crate::state::State,
    ) -> Option<PaletteChoice> {
        let Some(Modal::Palette {
            query,
            selected,
            open: true,
            ..
        }) = &state.modal
        else {
            return None;
        };
        let choice = self
            .palette_entries(state, query)
            .get(*selected)
            .map(|(_, choice)| choice.clone());
        if choice.is_some() {
            state.modal = None;
        } else if let Some(Modal::Palette { open, .. }) = &mut state.modal {
            *open = false;
        }
        choice
    }

    /// Closes any open modal. The single `popup` Option already makes the modals
    /// mutually exclusive (opening one drops the rest); this is the explicit close used
    /// by every opener and on dismissal. The drag position stays: a popup a prefix key
    /// opens takes the place the key list was dragged to.
    fn dismiss_modals(&mut self, state: &mut crate::state::State) {
        state.modal = None;
    }

    /// Returns the key list and the popups to their anchored position once neither is
    /// on screen, so the next prefix interaction starts where the key list opens.
    pub fn settle_popup_position(&mut self, state: &crate::state::State) {
        if !state.chrome.armed && state.modal.is_none() && !self.popup_geo.drag_active() {
            self.popup_geo.reset();
        }
    }

    /// True while the key list or a modal popup is being dragged; the app routes every
    /// mouse event here until release, like the view border drag / menu hold.
    pub fn popup_drag_active(&self) -> bool {
        self.popup_geo.drag_active()
    }

    /// A left press anywhere on the key list or the active modal popup begins a
    /// move-drag. `open` is whether one of them is on screen. Returns true iff it
    /// grabbed (so the app consumes the event).
    pub fn begin_popup_drag(&mut self, col: u16, row: u16, open: bool) -> bool {
        self.popup_geo.begin_drag(col, row, open)
    }

    /// Updates the popup offset from the selection while a drag is active.
    pub fn drag_popup(&mut self, col: u16, row: u16) {
        self.popup_geo.drag(col, row);
    }

    /// Ends a drag.
    pub fn end_popup_drag(&mut self) {
        self.popup_geo.end_drag();
    }

    /// Read-only popup input (the help and the history), tmux view-mode style. While one
    /// is open it captures the whole key read (returns true ⇒ consumed - nothing reaches
    /// the tree or the terminal view); Esc closes either, `q` closes the history, the
    /// history scrolls on its arrows, the help takes typing as its search and scrolls on
    /// its arrows, and every other key is swallowed. The keys that open the two popups toggle
    /// them here too: `prefix` then `m` toggles the history and `prefix` then `?` the help,
    /// with `armed` carrying a prefix that ended one read into the next. After the prefix
    /// any other key reads as it would alone. Returns false when neither is open, so the
    /// read falls through to normal routing. The single owner of their dismissal - the
    /// app calls it above the tree/terminal split, so the behavior is identical in both
    /// focuses.
    /// `help_inner` is the help popup's inner width and height as last painted, so the
    /// help's keys move over the rows and tabs its paint shows.
    pub fn feed_reader_key(
        &mut self,
        bytes: &[u8],
        prefix: u8,
        armed: &mut bool,
        help_inner: (u16, u16),
        state: &mut crate::state::State,
    ) -> bool {
        if !crate::state::is_reader(&state.modal) {
            return false;
        }
        let mut rest = bytes;
        while !rest.is_empty() && crate::state::is_reader(&state.modal) {
            if std::mem::take(armed) {
                match rest[0] {
                    b'm' => self.toggle_history(state),
                    b'?' => self.toggle_help(state),
                    b'h' => self.toggle_check(state),
                    b':' => self.toggle_palette(state),
                    _ => continue,
                }
                rest = &rest[1..];
                continue;
            }
            let (keys, after) = match rest.iter().position(|&b| b == prefix) {
                Some(i) => (&rest[..i], Some(&rest[i + 1..])),
                None => (rest, None),
            };
            if !keys.is_empty() {
                let (prefix, position) = (&state.chrome.ui_prefix, state.chrome.nav_position);
                let help = |query: &str| {
                    modal::help_map(prefix, position, query, help_inner.0, help_inner.1)
                };
                modal::feed_reader(&mut state.modal, keys, &help);
            }
            match after {
                Some(after) if crate::state::is_reader(&state.modal) => {
                    *armed = true;
                    rest = after;
                }
                _ => break,
            }
        }
        // The history scrolls no further than its oldest record, and the help no further
        // than the offset that shows the last page of what its search matches, the same
        // limit its paint holds, so a scroll back up moves the view at once.
        let last = state.notify.history.len().saturating_sub(1);
        let checks = match state.modal {
            Some(Modal::Check { .. }) => self.check_entries(state).len(),
            _ => 0,
        };
        let palette_count = match &state.modal {
            Some(Modal::Palette { query, .. }) => self.palette_entries(state, query).len(),
            _ => 0,
        };
        match state.modal.as_mut() {
            Some(Modal::History { scroll }) => *scroll = (*scroll).min(last),
            Some(Modal::Check { selected, .. }) => {
                *selected = (*selected).min(checks.saturating_sub(1))
            }
            Some(Modal::Palette { selected, .. }) => {
                *selected = (*selected).min(palette_count.saturating_sub(1))
            }
            Some(Modal::Help { query, scroll, .. }) => {
                let map = modal::help_map(
                    &state.chrome.ui_prefix,
                    state.chrome.nav_position,
                    query,
                    help_inner.0,
                    help_inner.1,
                );
                *scroll = (*scroll).min(map.max_scroll);
            }
            _ => {}
        }
        true
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
        state.chrome.clear_flash();
        // The flat card list has no levels or host columns: ↑/↓ (and k/j) step one card,
        // ←/→ step one category, PageUp/Down jump ten, Home/End go to the ends (prefix
        // →/Enter focuses the terminal at the app layer). `n` starts a session on
        // the selected host; a digit opens the jump popup seeded with it (the app only
        // forwards a digit here behind the prefix).
        let ctrl = ev
            .modifiers
            .contains(ratatui::crossterm::event::KeyModifiers::CONTROL);
        match ev.code {
            KeyCode::Enter => {}
            // Ctrl+↑/↓ walk the hierarchy rather than the list: up from a session to its
            // source and its host, and back down to where the walk came from.
            KeyCode::Up if ctrl => self.ascend(),
            KeyCode::Down if ctrl => self.descend(state),
            // ↑/↓ and ←/→ (and the vim hjkl pair) name the two things the list is made of:
            // ↑/↓ walk the cards, ←/→ walk the categories, landing on the first card of the
            // previous/next one. Neither is defined by where a card sits on screen, so both
            // mean the same thing in the side column and in the portrait band, which flows
            // its cards down a column and then right.
            KeyCode::Up | KeyCode::Char('k') => self.nav_vertical(-1),
            KeyCode::Down | KeyCode::Char('j') => self.nav_vertical(1),
            KeyCode::Left | KeyCode::Char('h') => self.nav_horizontal(-1),
            KeyCode::Right | KeyCode::Char('l') => self.nav_horizontal(1),
            KeyCode::PageUp => self.move_selection(-10),
            KeyCode::PageDown => self.move_selection(10),
            KeyCode::Home => self.move_to(0),
            KeyCode::End => self.move_to(-1),
            // An applied filter is cleared by Esc (the hint bar advertises it); with no
            // filter there is nothing to clear and Esc does nothing.
            KeyCode::Esc if !state.filter.is_empty() => {
                state.filter.clear();
                self.rebuild(state);
            }
            KeyCode::Char(c) => match c {
                'd' if self.current_unreachable_screen(state) => {
                    if let Some(source) = self.current_source() {
                        if !state.host_details.insert(source.clone()) {
                            state.host_details.remove(&source);
                        }
                    }
                }
                '/' => self.open_input(InputMode::Filter, state),
                'n' => self.open_new(state),
                'r' => return vec![Command::Rescan],
                'R' => return self.rescan_host(state),
                'L' => self.open_logout(state),
                'i' => self.select_host_section(),
                // Jump: the digit opens the jump popup already holding it, so the
                // number can be extended (4 → 41) without a second keystroke.
                '0'..='9' => self.open_jump(c, state),
                _ => {}
            },
            _ => {}
        }
        Vec::new()
    }

    /// The `prefix R` re-scan of the selected card's host alone. Refused while any source
    /// of that machine is still scanning, since a machine is asked one thing at a time.
    fn rescan_host(&mut self, state: &mut crate::state::State) -> Vec<Command> {
        let Some(source) = self.current_source() else {
            return Vec::new();
        };
        let machine = crate::session::machine_of(&source).to_string();
        let busy = state
            .scanning
            .iter()
            .any(|s| crate::session::machine_of(s) == machine);
        if busy {
            state.flash(format!("{machine} is still being scanned"));
            return Vec::new();
        }
        vec![Command::RescanHost(machine)]
    }

    fn open_logout(&mut self, state: &mut crate::state::State) {
        let Some(source) = self.current_source() else {
            return;
        };
        let machine = crate::session::machine_of(&source);
        if !state
            .chrome
            .source_reach
            .get(&source)
            .or_else(|| state.chrome.source_reach.get(machine))
            .is_some_and(|reach| reach.ssh)
        {
            state.flash("this host does not use SSH");
            return;
        }
        let session = match self.current_ref() {
            Some(RowRef::Session { sess }) => Some(sess.address()),
            _ => None,
        };
        let method = session
            .as_ref()
            .and_then(|address| state.display_auth_methods.get(&address.source))
            .or_else(|| {
                session
                    .is_none()
                    .then(|| state.auth_methods.get(machine))
                    .flatten()
            })
            .map(|method| method.label())
            .unwrap_or("not observed");
        let subject = session.map_or_else(|| machine.to_owned(), |address| address.display());
        let password = (method != "public key").then_some("held password is cleared");
        let mut facts = vec![("session", subject), ("SSH login", method.to_owned())];
        facts.extend(password.map(|p| ("password", p.to_owned())));
        facts.push((
            "key",
            format!("removed from {machine}; asks first if xmux did not add it"),
        ));
        facts.push(("connections", format!("closes {machine} connections")));
        self.dismiss_modals(state);
        let mut input = Input::new(InputMode::Logout, String::new(), Some(source));
        input.facts = facts;
        state.modal = Some(Modal::Input(Box::new(input)));
    }

    /// Opens the logout's second confirmation for `machine`, whose key files hold this
    /// machine's key in lines xmux did not add: `unmarked` names the file of each such
    /// line, and `marked` counts the lines xmux added, which go whatever the answer is.
    /// The input carries the machine, so the answer lands on the logout that asked.
    pub(crate) fn open_logout_keys(
        &mut self,
        machine: &str,
        unmarked: &[&str],
        marked: usize,
        state: &mut crate::state::State,
    ) {
        let mut files: Vec<&str> = unmarked.to_vec();
        files.dedup();
        let lines = |n: usize| {
            if n == 1 {
                "1 line".to_string()
            } else {
                format!("{n} lines")
            }
        };
        let facts = vec![
            (
                "key",
                format!(
                    "{} of this PC's key not added by xmux",
                    lines(unmarked.len())
                ),
            ),
            ("file", files.join(", ")),
            ("remove", "ssh outside xmux loses this key too".to_string()),
            (
                "keep",
                if marked == 0 {
                    format!("the key stays on {machine}")
                } else {
                    format!("only the {} xmux added go", lines(marked))
                },
            ),
            ("logout", "goes on either way".to_string()),
        ];
        self.dismiss_modals(state);
        let mut input = Input::new(
            InputMode::LogoutKeys,
            String::new(),
            Some(machine.to_owned()),
        );
        input.facts = facts;
        state.modal = Some(Modal::Input(Box::new(input)));
    }

    // --- the hosts to check -------------------------------------------------

    /// Toggles the table of the hosts to check (`prefix h`) in either focus.
    pub fn toggle_check(&mut self, state: &mut crate::state::State) {
        if matches!(state.modal, Some(Modal::Check { .. })) {
            state.modal = None;
        } else {
            self.dismiss_modals(state);
            state.modal = Some(Modal::Check {
                selected: 0,
                hover: None,
                open: false,
            });
        }
    }

    /// Every host in a problem state, grouped by cause in the order the table reads them
    /// (login needed, unreachable, list failed) and by list order inside a cause. A host
    /// still scanning is in no state yet and is left out.
    pub(crate) fn check_entries(&self, state: &crate::state::State) -> Vec<CheckEntry> {
        use crate::model::FailureKind;
        let mut entries = Vec::new();
        for kind in [
            FailureKind::Blocked,
            FailureKind::Unreachable,
            FailureKind::ListFailed,
        ] {
            for g in &state.groups {
                if state.scanning.contains(&g.source) || g.failure() != Some(kind) {
                    continue;
                }
                let reason = g
                    .err
                    .as_deref()
                    .and_then(|e| e.lines().map(str::trim).find(|l| !l.is_empty()))
                    .unwrap_or_default()
                    .to_string();
                entries.push(CheckEntry {
                    label: state.chrome.source_label_when(&g.source, false),
                    source: g.source.clone(),
                    kind,
                    reason,
                });
            }
        }
        entries
    }

    /// Acts on an Enter the check table took: closes the table and selects the chosen
    /// host's card. The filter is cleared if it excludes the selected host. A host whose login pane
    /// answers it hands the focus to the terminal view, where the pane takes the keys.
    /// Returns whether the focus goes to the terminal view.
    pub fn open_checked_host(&mut self, state: &mut crate::state::State) -> bool {
        let Some(Modal::Check {
            selected,
            open: true,
            ..
        }) = state.modal
        else {
            return false;
        };
        let Some(entry) = self.check_entries(state).into_iter().nth(selected) else {
            state.modal = None;
            return false;
        };
        state.modal = None;
        self.open_host(&entry.source, state)
    }

    /// Selects the card standing for `source` from the hosts to check: the source's own
    /// card, or its host's card while the host is down. A login answers a failure that is
    /// not a listing failure, so the host's screen then opens its login pane.
    pub(crate) fn open_host(&mut self, source: &str, state: &mut crate::state::State) -> bool {
        let login_needed = state
            .groups
            .iter()
            .find(|group| group.source == source)
            .and_then(crate::model::Group::failure)
            .is_some_and(|kind| kind != crate::model::FailureKind::ListFailed);
        let machine = crate::session::machine_of(source);
        let host_row = |sw: &Switcher| {
            sw.rows.iter().position(|r| match &r.reference {
                RowRef::Host {
                    source: row_source, ..
                } => row_source == source,
                RowRef::Machine { machine: m, .. } => m == machine,
                _ => false,
            })
        };
        let row = match host_row(self) {
            Some(i) => Some(i),
            None => {
                state.filter.clear();
                self.rebuild(state);
                host_row(self)
            }
        };
        let Some(i) = row else {
            return false;
        };
        self.note_user_move();
        if login_needed {
            // The host's own card, or the host half of this source's card while the host
            // is up, so the pane logs in through the source that was chosen.
            let host = Node::Host(machine.to_owned());
            if let Some((row, part)) = self.target_of(&host, Some(source)) {
                self.set_target(Target {
                    row,
                    part,
                    deep: None,
                });
            }
            self.login_target = Some(source.to_owned());
        } else {
            self.set_selected(i);
        }
        login_needed
    }

    // --- input row ----------------------------------------------------------

    /// Opens the fuzzy filter input. The only input popup the switcher opens by
    /// mode; `new session` is opened by [`Switcher::open_new`], which needs the
    /// selected host captured up front.
    pub(super) fn open_input(&mut self, mode: InputMode, state: &mut crate::state::State) {
        state.chrome.clear_flash();
        self.dismiss_modals(state);
        match mode {
            InputMode::Filter => {
                let mut input = Input::new(mode, state.filter.clone(), None);
                // The filter the input opened from: Esc restores it, undoing every
                // live edit made while the input was open.
                input.restore_filter = Some(state.filter.clone());
                state.modal = Some(Modal::Input(Box::new(input)));
            }
            // New is opened by `open_new` and Jump by `open_jump` (both capture context
            // the mode alone does not carry). The unlock is not a modal: it lives in the
            // locked panel (see `State::feed_unlock`).
            InputMode::New | InputMode::Jump | InputMode::Logout | InputMode::LogoutKeys => {}
        }
    }

    /// The `n` action: a new SESSION on the selected card's host/mux. Every card
    /// names a source - a session or loading card by its session, a host card by
    /// itself - so `n` adds a session to the source in front of the user, not only
    /// to an empty host. (xmux does not edit a session's windows, so there is
    /// nothing else `n` could add.) The source is captured up front so a streamed
    /// selection move cannot retarget it.
    pub(super) fn open_new(&mut self, state: &mut crate::state::State) {
        state.chrome.clear_flash();
        self.dismiss_modals(state);
        // A session lives in a source, so a host names none to create it in.
        if let Some(Node::Host(machine)) = self.selected_node() {
            if host_failure(state, &machine).is_some() {
                state.flash("host unreachable, cannot create here");
            } else {
                state.flash(format!("select a source of {machine} to start a session"));
            }
            return;
        }
        let Some(source) = self.current_source() else {
            return;
        };
        // A blocked source is unreachable too (its failure is one of unreachable's), so
        // the one check covers both: nothing can be created over a connection that is
        // not up.
        if state.groups.iter().any(|g| {
            g.source == source
                && matches!(
                    g.failure(),
                    Some(
                        crate::model::FailureKind::Blocked | crate::model::FailureKind::Unreachable
                    )
                )
        }) {
            state.flash("host unreachable, cannot create here");
            return;
        }
        state.modal = Some(Modal::Input(Box::new(Input::new(
            InputMode::New,
            String::new(),
            Some(source),
        ))));
    }

    /// The row of the card carrying `number`, or `None` when no card on the list carries
    /// it. The buffer is read as its value, spelling included, so 01 is 1: the values no
    /// card carries are 0, a vacant number (its card ended or is not on the list), and
    /// everything past the highest. The jump reads it on every edit to move the selection
    /// while the number names a card, and at Enter to decide whether to land or refuse: see
    /// [`Switcher::jump_accepts`].
    pub(super) fn jump_row(&self, number: &str) -> Option<usize> {
        let n = number.trim().parse::<usize>().ok()?;
        (0..self.rows.len()).find(|&i| self.rows[i].selectable() && self.card_number(i) == n)
    }

    /// Whether the jump would land on `number`, i.e. some card carries it. Read at
    /// Enter only: every digit is taken while typing, and a number that names no card
    /// just leaves the selection alone until Enter, which refuses it in the popup. An empty
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
        state.chrome.clear_flash();
        let seed = digit.to_string();
        let restore = self.current_ref().cloned();
        self.dismiss_modals(state);
        let mut input = Input::new(InputMode::Jump, seed, None);
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
        self.note_user_move();
        self.set_selected(n);
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
    fn restore_jump(&mut self, restore: Option<RowRef>) {
        let Some(target) = restore else {
            return;
        };
        if let Some(i) = self.row_matching(&target) {
            self.set_selected(i);
        }
    }

    pub(super) fn close_input(&mut self, state: &mut crate::state::State) {
        state.modal = None;
    }

    fn handle_input_key(&mut self, ev: KeyEvent, state: &mut crate::state::State) -> Vec<Command> {
        // A flash and a jump's refusal live only until the next key. Clear them here so a
        // key while an input is open (a fresh edit, a fresh Enter) restores the popup; an
        // action below may set a fresh one, which survives because this runs first.
        state.chrome.clear_flash();
        if let Some(Modal::Input(input)) = state.modal.as_mut() {
            input.refused = None;
        }
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
                match mode {
                    // Enter on a jump lands only when the buffer names a card. A number
                    // no card carries is refused in the popup, which stays open with the
                    // range in its top border; an empty buffer just keeps it open.
                    InputMode::Jump => {
                        if !val.is_empty() && self.jump_accepts(&val) {
                            self.close_input(state);
                        } else if !val.is_empty() {
                            if let Some(Modal::Input(input)) = state.modal.as_mut() {
                                input.refused = Some(val);
                            }
                        }
                        Vec::new()
                    }
                    InputMode::Logout if val != "logout" => {
                        state.flash("type logout to confirm");
                        Vec::new()
                    }
                    InputMode::LogoutKeys if val != "remove" => {
                        state.flash("type remove to confirm");
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
                            InputMode::Logout => {
                                let Some(source) = source else {
                                    return Vec::new();
                                };
                                vec![Command::Logout(
                                    crate::session::machine_of(&source).to_owned(),
                                )]
                            }
                            InputMode::LogoutKeys => source
                                .map(Command::RemoveUnmarkedKeys)
                                .into_iter()
                                .collect(),
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
                self.restore_jump(restore);
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

    /// Applies a completed [`MuxOp`](crate::model::MuxOp)'s [`OpResult`] to the
    /// in-memory tree. The result is applied on the event loop after `run_op`
    /// returns off-loop, so a slow ssh round-trip never blocks rendering. State
    /// owns the inventory fold ([`State::fold_op_result`](crate::state::State::fold_op_result));
    /// the switcher only rebuilds its rows + restores the cursor per the returned
    /// [`OpFollow`].
    ///
    /// Returns the source whose MACHINE the app should re-probe, and the connection
    /// values that reached it: `Some` only on a successful unlock, because that machine's
    /// reach state (locked → connected) is the only thing that changed, so re-probing the
    /// whole roster would be wasteful. Every other result returns `None`.
    pub fn apply_op_result(
        &mut self,
        result: OpResult,
        state: &mut crate::state::State,
    ) -> Option<(String, crate::transport::Login)> {
        match state.fold_op_result(result) {
            OpFollow::Reselect(addr) => {
                // The created session is what the user asked for, so its card takes the
                // selection the moment it appears.
                self.note_user_move();
                self.interest = Interest::Awaiting(addr.clone());
                self.rebuild(state);
                state.notify.toast(
                    "new session",
                    vec![Note::new(
                        Level::Success,
                        format!(
                            "{}/{} created",
                            crate::session::machine_of(&addr.source),
                            addr.session
                        ),
                    )],
                );
                None
            }
            OpFollow::Failed(message) => {
                state
                    .notify
                    .toast("new session", vec![Note::new(Level::Error, message)]);
                None
            }
            OpFollow::Nothing => None,
            // A successful unlock promoted this machine's credential. Only this
            // machine's reach changed, so the app re-probes just it. Either way one toast
            // reports the login and the follow-ups it ran; the user retypes the password
            // after a failure.
            OpFollow::LoginResult {
                source,
                login,
                outcome,
            } => {
                let machine = crate::session::machine_of(&source).to_string();
                state.login_reports.insert(machine.clone(), outcome.clone());
                if !matches!(
                    outcome.registration,
                    crate::ui::ops::RegistrationOutcome::NotRequested
                ) {
                    state
                        .registration_reports
                        .insert(machine.clone(), outcome.registration.clone());
                }
                state
                    .notify
                    .timed_toast(machine.clone(), login_notes(&outcome));
                match outcome.connect {
                    crate::link::unlock::UnlockOutcome::Ok => Some((source, login)),
                    crate::link::unlock::UnlockOutcome::Unavailable => None,
                    crate::link::unlock::UnlockOutcome::Failed { .. } => {
                        state.logged_in.remove(&machine);
                        None
                    }
                }
            }
        }
    }

    pub(super) fn row_of_session(&self, address: &crate::session::Address) -> Option<usize> {
        self.rows
            .iter()
            .position(|r| session_addr_of(&r.reference).as_ref() == Some(address))
    }
}

/// The report lines a finished login makes: the connection's verdict, then the public-key
/// registration it ran, then an ssh config recording that failed. A cancelled login reports nothing
/// about the connection, because the user ended it and knows how it ended.
pub(crate) fn login_notes(outcome: &crate::ui::ops::LoginOutcome) -> Vec<Note> {
    use crate::link::unlock::{FailureKind, UnlockOutcome};
    use crate::ui::ops::RegistrationOutcome;
    let mut notes = Vec::new();
    match &outcome.connect {
        UnlockOutcome::Ok => notes.push(Note::new(Level::Success, "logged in")),
        // Reached only for a machine there is nothing to log in TO: this box and its WSL
        // distributions are not behind ssh at all.
        UnlockOutcome::Unavailable => notes.push(Note::new(
            Level::Error,
            "this machine is reached without a login",
        )),
        UnlockOutcome::Failed { kind, reason } => {
            // The verdict's first line; the login pane keeps the whole of ssh's reason.
            if *kind != FailureKind::Cancelled {
                let verdict = reason.lines().next().unwrap_or_default();
                notes.push(Note::new(Level::Error, format!("login failed: {verdict}")));
            }
        }
    }
    match &outcome.registration {
        RegistrationOutcome::Registered => {
            notes.push(Note::new(Level::Success, "public key registered"))
        }
        RegistrationOutcome::Skipped(reason) => notes.push(Note::new(
            Level::Warning,
            format!("public key not registered: {reason}"),
        )),
        RegistrationOutcome::Failed(reason) => notes.push(Note::new(
            Level::Error,
            format!("public key not registered: {reason}"),
        )),
        RegistrationOutcome::NotRequested => {}
    }
    if let Some(Err(reason)) = &outcome.saved {
        notes.push(Note::new(
            Level::Error,
            format!("ssh config not written: {reason}"),
        ));
    }
    notes
}
