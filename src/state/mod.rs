//! Runtime domain state: the single source of truth the new architecture's
//! components read from. Carries the app loop's inventory, selection,
//! display-truth, focus, and the open modal popup.
use crate::model::Selection;
use crate::ui::tree::Group;
use std::collections::{HashMap, HashSet};
use std::time::Instant;

/// The app's canonical runtime state.
#[derive(Default)]
pub struct State {
    /// Inventory - hosts → sessions (all reachable). The single
    /// source of truth every component reads, instead of reaching into the tree.
    // ponytail: flat fields, not an Inventory sub-struct - bundle them if a reader
    // ever needs the whole group at once.
    pub groups: Vec<Group>,
    /// Sources whose `list-sessions` has not yet returned (host shows scanning…).
    pub scanning: HashSet<String>,
    /// MACHINES the user has logged in to successfully in this run, which hiding never
    /// drops however they answer afterwards.
    ///
    /// A locked host is kept because it is actionable, its login pane being the one entry
    /// point. Succeeding at that login does not make it less actionable: it is the host
    /// the user just chose, and whatever it answers next is the answer they are waiting
    /// for. Without this, the one action a card offers is the action that makes the card
    /// vanish - the login stops being blocked, so nothing keeps it any more. Keyed by
    /// machine because a login authenticates the machine, not the one mux that carried it.
    pub logged_in: HashSet<String>,
    /// How many times in a row each source has failed to enumerate, reset to zero the
    /// moment it answers. Written at the single result-apply site and read only to be
    /// SHOWN: the unreachable screen states it, because one failed sweep and a host that
    /// has not answered since launch are different problems behind the same message.
    pub failure_runs: HashMap<String, u32>,
    /// Active fuzzy-filter text (drives the visible tree + the hint_bar).
    pub filter: String,
    /// What the tree selection points at - the session to show.
    pub selection: Selection,
    /// The address whose content is confirmed live in the on-screen terminal view -
    /// the single display truth, and the target of both rendering and input. The
    /// terminal view always shows THIS session's grid; on a switch it stays on the
    /// prior session until the new one is confirmed (stale-while-revalidate), then
    /// advances. Set only at confirmation (a synchronous in-place switch, or
    /// DisplayReady). Empty before the first confirmation → the view is blank.
    pub displayed: Selection,
    /// When set, a settled selection is attached once this instant passes.
    pub attach_deadline: Option<Instant>,
    /// A selection moved and has not yet armed its debounce deadline. The next
    /// [`Action::Tick`] (re)arms `attach_deadline` from this - re-armed on EVERY
    /// pending selection so rapid navigation coalesces into one trailing attach
    /// instead of a per-step storm of switch-client repaints (the freeze).
    pub attach_pending: bool,
    /// The session last persisted as the user's last-selected.
    pub last_saved_session: crate::session::Address,
    /// The app's focus state machine - which pane keys go to and whether a
    /// modal is open. The single source of truth for focus.
    pub focus: crate::app::focus::Focus,
    /// The single open modal, if any (help / inline input / kill confirm / context
    /// menu). One Option - not four independent fields - so the modals' mutual
    /// exclusion is structural: opening one drops whatever was open, and two can
    /// never coexist. The switcher owns the modal behavior and the transient popup
    /// geometry (drag offset / drawn rect); this owns which modal is open + its content.
    pub(crate) modal: Option<crate::ui::modal::Modal>,
    /// The switcher's chrome view-state: the tree|terminal view border, the tree-column
    /// hint bar (help / status / wrapped flash), and the host screens,
    /// plus their inputs (flash, spinner set + frame, auto-hide/hover cues, view border
    /// colours, ssh-config text, prefix). Owned here (the [`Modal`](crate::ui::modal::Modal)
    /// precedent) and fed by the app each frame; the switcher's `render` reads it off
    /// `&state`.
    pub(crate) chrome: crate::ui::chrome::Chrome,
    /// The login draft for the blocked host whose panel is on screen: the connection
    /// values the user is entering INTO the login pane (the terminal view) and which
    /// element the keys drive. It is NOT a modal - it never routes through the nav input
    /// path - it is a feature of the login pane, driven only while the terminal view
    /// holds a blocked host. `source` pins it to that host so moving to another card
    /// starts a fresh draft. The password lives here and in the transient login command
    /// only; it is drawn masked and never logged or serialized.
    pub login: Option<LoginDraft>,
    /// The login that is RUNNING: once the pane is submitted, ssh has the conversation on
    /// its own thread with the values the draft collected, and this is the handle that
    /// ends it. Present only while that conversation runs, so its presence is what tells
    /// the pane to say a login is under way instead of offering one.
    pub login_run: Option<crate::link::unlock::RunningLogin>,
}

/// What the login pane does with the values once the connection works. The two are one
/// choice, not two switches: a draft either leaves nothing behind or writes a stanza.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Remember {
    #[default]
    Nothing,
    SshConfig,
}

/// Which element of the login pane the keys drive. Every interactive element is one
/// stop, so Tab and the vertical arrows walk the pane the same way whatever is on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LoginFocus {
    #[default]
    Address,
    Port,
    Username,
    Password,
    RememberNothing,
    RememberSshConfig,
    Pubkey,
    Submit,
}

/// The login pane's draft: what the user is entering for a host that would not answer
/// with the values ssh resolves on its own. Held on [`State`] (not the modal set)
/// because the pane is a feature of the terminal view, so nothing in the nav path
/// drives it.
///
/// The three connection values start at what ssh WOULD use, and those starting values
/// are kept beside them: the remember choice is only worth offering once the user has
/// changed something, since a stanza repeating what ssh already resolves says nothing.
#[derive(Debug, Clone, Default)]
pub struct LoginDraft {
    /// The blocked source this draft belongs to; a different current source resets it.
    pub source: String,
    pub address: String,
    pub port: String,
    pub username: String,
    pub password: String,
    pub remember: Remember,
    pub pubkey: bool,
    pub focus: LoginFocus,
    pub default_address: String,
    pub default_port: String,
    pub default_username: String,
}

impl LoginDraft {
    /// True once a connection value differs from what ssh would have used. Only then is
    /// there anything a stanza could record.
    pub fn changed(&self) -> bool {
        self.address != self.default_address
            || self.port != self.default_port
            || self.username != self.default_username
    }

    /// The pane's focus stops in reading order. The remember choice is absent until the
    /// user changes a value, and a stop that is not drawn is not one the keys land on.
    pub fn stops(&self) -> Vec<LoginFocus> {
        let mut v = vec![
            LoginFocus::Address,
            LoginFocus::Port,
            LoginFocus::Username,
            LoginFocus::Password,
        ];
        if self.changed() {
            v.push(LoginFocus::RememberNothing);
            v.push(LoginFocus::RememberSshConfig);
        }
        v.push(LoginFocus::Pubkey);
        v.push(LoginFocus::Submit);
        v
    }

    /// Moves the focus `delta` stops, wrapping. A focus left on a stop that is no longer
    /// drawn (the user undid their edit) lands on the first stop rather than nowhere.
    fn move_focus(&mut self, delta: isize) {
        let stops = self.stops();
        let at = stops.iter().position(|s| *s == self.focus).unwrap_or(0) as isize;
        let n = stops.len() as isize;
        self.focus = stops[(at + delta).rem_euclid(n) as usize];
    }

    /// The text field the focus is on, or `None` when the focus is on a choice.
    fn field_mut(&mut self) -> Option<&mut String> {
        match self.focus {
            LoginFocus::Address => Some(&mut self.address),
            LoginFocus::Port => Some(&mut self.port),
            LoginFocus::Username => Some(&mut self.username),
            LoginFocus::Password => Some(&mut self.password),
            _ => None,
        }
    }

    /// What Enter does: submit from the button, and pass the focus on from anywhere
    /// else. One meaning for the whole pane, so filling it top to bottom with Enter alone
    /// ends on the button and never toggles something on the way past.
    fn enter(&mut self) -> bool {
        if self.focus == LoginFocus::Submit {
            return true;
        }
        self.move_focus(1);
        false
    }

    /// What Space does: pick the focused choice, leaving the focus where it is so the
    /// user can see what they picked. A text field takes it as the character it is.
    fn pick(&mut self) {
        match self.focus {
            LoginFocus::RememberNothing => self.remember = Remember::Nothing,
            LoginFocus::RememberSshConfig => self.remember = Remember::SshConfig,
            LoginFocus::Pubkey => self.pubkey = !self.pubkey,
            _ => {}
        }
    }
}

/// One key the login pane understands, decoded from the terminal's bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Key {
    Char(char),
    Enter,
    Tab,
    BackTab,
    Up,
    Down,
    Backspace,
}

/// Decodes a terminal input chunk into the keys the login pane acts on, dropping every
/// other escape sequence whole so its bytes can never land in a field as text.
///
/// Only the sequences the pane uses are recognised: the vertical arrows walk its stops
/// and back-tab walks them backwards. A horizontal arrow is dropped rather than mapped,
/// because the fields are edited from their end and there is no caret for it to move.
fn decode_keys(bytes: &[u8]) -> Vec<Key> {
    let text = String::from_utf8_lossy(bytes);
    let mut out = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => {
                // CSI runs to a final byte; SS3 is one byte past its introducer; anything
                // else is a lone escape. Each form is consumed WHOLE, so no tail of a
                // sequence can be mistaken for typing.
                match chars.peek() {
                    Some('[') => {
                        chars.next();
                        let mut last = None;
                        for c in chars.by_ref() {
                            last = Some(c);
                            if c.is_ascii_alphabetic() || c == '~' {
                                break;
                            }
                        }
                        match last {
                            Some('A') => out.push(Key::Up),
                            Some('B') => out.push(Key::Down),
                            Some('Z') => out.push(Key::BackTab),
                            _ => {}
                        }
                    }
                    Some('O') => {
                        chars.next();
                        chars.next();
                    }
                    Some(_) => {
                        chars.next();
                    }
                    None => {}
                }
            }
            '\r' | '\n' => out.push(Key::Enter),
            '\t' => out.push(Key::Tab),
            '\u{7f}' | '\u{8}' => out.push(Key::Backspace),
            c if c.is_control() => {}
            c => out.push(Key::Char(c)),
        }
    }
    out
}

impl State {
    /// True while a modal owns the screen (the help popup or the inline input) is
    /// open. These drive [`ModalKind::Popup`]; the context
    /// menu is separate (pointer-anchored).
    ///
    /// [`ModalKind::Popup`]: crate::app::focus::ModalKind::Popup
    pub fn is_modal_popup_open(&self) -> bool {
        crate::ui::modal::is_popup_open(&self.modal)
    }

    /// True while an inline input (filter / rename / new) is open. The app
    /// routes every key to the switcher then, with no focus-switch hijack.
    pub fn is_inputting(&self) -> bool {
        crate::ui::modal::is_inputting(&self.modal)
    }

    /// Which kind of modal is open - the focus machine derives its modal dimension
    /// from this each loop-top, so [`Focus`] can never mirror-and-desync from the
    /// open popup. A centered popup and the context menu are mutually exclusive.
    ///
    /// [`Focus`]: crate::app::focus::Focus
    pub(crate) fn modal_kind(&self) -> Option<crate::app::focus::ModalKind> {
        crate::ui::modal::modal_kind(&self.modal)
    }

    /// Feeds terminal-view keystrokes into the login pane for `source`.
    ///
    /// The pane is a form: printable characters land in the focused text field, Tab and
    /// the vertical arrows walk the stops, Enter activates the focused one, and Space
    /// picks a choice. Enter on a text field passes the focus on, so filling the pane top
    /// to bottom with Enter alone ends on the button, where Enter submits.
    ///
    /// A draft for a different source is reset first, and a fresh draft starts at the
    /// values ssh would have used, so the pane opens showing what just failed. On submit
    /// the password is taken out of the draft (it rides only the transient command), so
    /// nothing keeps it.
    pub fn feed_login(&mut self, source: &str, bytes: &[u8]) -> Option<crate::model::Command> {
        let (address, port, username) = self.chrome.login_defaults(source);
        let draft = match &mut self.login {
            Some(d) if d.source == source => d,
            _ => {
                self.login = Some(LoginDraft {
                    source: source.to_string(),
                    address: address.clone(),
                    port: port.clone(),
                    username: username.clone(),
                    default_address: address,
                    default_port: port,
                    default_username: username,
                    ..Default::default()
                });
                self.login.as_mut().unwrap()
            }
        };
        let mut submit = false;
        for key in decode_keys(bytes) {
            match key {
                Key::Tab => draft.move_focus(1),
                Key::BackTab | Key::Up => draft.move_focus(-1),
                Key::Down => draft.move_focus(1),
                Key::Enter => submit |= draft.enter(),
                Key::Backspace => {
                    draft.field_mut().map(String::pop);
                }
                // Space picks a choice; in a text field it is a character like any other.
                Key::Char(' ') if draft.field_mut().is_none() => draft.pick(),
                Key::Char(c) => {
                    if let Some(f) = draft.field_mut() {
                        f.push(c);
                    }
                }
            }
        }
        if !submit {
            return None;
        }
        let port = draft.port.trim().parse::<u16>().ok();
        Some(crate::model::Command::RunLogin {
            source: draft.source.clone(),
            login: crate::transport::Login {
                address: (!draft.address.trim().is_empty()).then(|| draft.address.trim().into()),
                port,
                user: (!draft.username.trim().is_empty()).then(|| draft.username.trim().into()),
            },
            password: std::mem::take(&mut draft.password),
            remember: draft.remember,
            pubkey: draft.pubkey,
        })
    }

    /// Builds the inventory from a complete snapshot: every host is resolved
    /// (reachable or unreachable per its `err`) and every session is present. Other
    /// state fields stay default.
    pub fn from_scan(scan: crate::ui::switcher::Scan) -> State {
        State {
            groups: scan.groups,
            ..State::default()
        }
    }

    /// Seeds the inventory from the resolved source list alone - no probing - so
    /// the first frame paints host-skeleton rows, each in a scanning state. Other
    /// state fields stay default.
    pub fn from_sources(aliases: Vec<String>) -> State {
        let scanning = aliases.iter().cloned().collect();
        let groups = aliases
            .into_iter()
            .map(|source| Group {
                source,
                err: None,
                sessions: Vec::new(),
            })
            .collect();
        State {
            scanning,
            groups,
            ..State::default()
        }
    }

    /// Resolves a `switch` target against the current inventory - the set the nav
    /// shows. `Ok` when a session with exactly that source and session is listed;
    /// `Err` names which half is missing (an absent source, or a present source with no
    /// matching session). The answer the ctl `switch` verb replies with: resolution,
    /// not attach success, which is async and confirms later.
    pub fn resolve_switch_address(&self, address: &crate::session::Address) -> Result<(), String> {
        let found = self.groups.iter().any(|g| {
            g.sessions
                .iter()
                .any(|s| s.source == address.source && s.name == address.session)
        });
        if found {
            return Ok(());
        }
        if self.groups.iter().any(|g| g.source == address.source) {
            Err(format!(
                "no such session {:?} on source {:?}",
                address.session, address.source
            ))
        } else {
            Err(format!("no such source {:?}", address.source))
        }
    }

    /// The single domain-mutation site. Folds one [`Action`] into the state and
    /// returns the side effects to run as [`Command`]s. `apply` touches only `State`;
    /// every external effect (switcher selection move, attach, prefs persist, quit) is
    /// returned for the run loop to dispatch, so the intent → state → effect flow has
    /// exactly one mutation point.
    ///
    /// The clock and the runtime attach facts enter ONLY as data on [`Action::Tick`]
    /// (`now`/`in_flight`); `apply` never reads `Instant::now()` or any
    /// registry/host state itself.
    ///
    /// [`Action`]: crate::model::Action
    /// [`Command`]: crate::model::Command
    pub fn apply(&mut self, action: crate::model::Action) -> Vec<crate::model::Command> {
        use crate::model::{Action, Command, FocusTarget, MuxOp};
        use std::time::Duration;
        match action {
            Action::Switch(address) => vec![Command::SelectAddress(address)],
            Action::Focus(FocusTarget::Terminal) => {
                self.focus
                    .set_view_focus(crate::app::focus::ViewFocus::Terminal);
                Vec::new()
            }
            Action::Focus(FocusTarget::Nav) => {
                self.focus.set_view_focus(crate::app::focus::ViewFocus::Nav);
                Vec::new()
            }
            Action::FocusToggle => {
                self.focus.toggle();
                Vec::new()
            }
            Action::ConfirmDisplay(sel) => {
                self.displayed = sel;
                Vec::new()
            }
            Action::ClearDisplay => {
                self.displayed = Selection::default();
                Vec::new()
            }
            Action::RearmAttachNow { now } => {
                self.attach_deadline = Some(now);
                Vec::new()
            }
            Action::Rescan => vec![Command::Rescan],
            Action::NavWidth(d) => vec![Command::AdjustNavWidth(d)],
            Action::ToggleAutoHide => vec![Command::ToggleAutoHide],
            Action::Quit => vec![Command::Quit],
            Action::Select(target) => {
                // Mark the attach pending; do NOT arm the deadline or attach here.
                // The trailing Tick arms the debounce, so rapid navigation coalesces.
                self.selection = target;
                self.attach_pending = true;
                Vec::new()
            }
            Action::Tick {
                now,
                in_flight,
                display_astray,
            } => {
                // RE-ARM on every pending selection: a fresh Select between ticks
                // pushes the deadline out, so only the trailing selection attaches
                // (one switch, not a per-step storm - the freeze fix). Re-arming and
                // firing are mutually exclusive on a tick: a just-armed deadline is
                // always in the future, so the elapsed check below cannot fire it.
                if self.attach_pending {
                    self.attach_pending = false;
                    self.attach_deadline = Some(now + Duration::from_millis(ATTACH_DEBOUNCE_MS));
                    return Vec::new();
                }
                // ARM ON THE CONDITION, not on a change. A display sitting away from the
                // selection is a state, so it is answered for as long as it lasts and
                // nothing has to be remembered from the moment it began: a selection that
                // never moved again would arm nothing if only its MOVE could. The two ways
                // it can sit away are one condition here, because the gate below fires on
                // them: the client left for another session of the selected host
                // (`display_astray`), or the confirmed display is another session
                // altogether. Armed only with the debounce idle, so a navigation burst
                // still coalesces into one trailing attach, and only with nothing in
                // flight, so the attach already carrying the display there is not
                // restarted under itself.
                //
                // A display PTY that DIED while the selection stands arms nothing. Every
                // re-attach is a fresh connection to that machine, and an attach that
                // answers a death that the attach itself caused is a loop the machine sees
                // as a client hammering it. The pane keeps what it last drew and the user
                // decides, by selecting the card again or re-scanning.
                let display_needs_carry = display_astray || self.selection != self.displayed;
                if display_needs_carry
                    && !self.selection.is_empty()
                    && self.attach_deadline.is_none()
                    && !in_flight
                {
                    self.attach_deadline = Some(now + Duration::from_millis(ATTACH_DEBOUNCE_MS));
                    return Vec::new();
                }
                // The debounce deadline has elapsed.
                if self.attach_deadline.is_none_or(|d| now < d) {
                    return Vec::new();
                }
                self.attach_deadline = None;
                if self.selection.is_empty() {
                    return Vec::new();
                }
                let mut cmds = Vec::new();
                // Persist the settled session as last-selected - INDEPENDENT of the
                // attach gate, so it records even when the attach is suppressed (e.g.
                // an in-flight attach on the same shared host while the selection moves to
                // another of its sessions). Only on an address change.
                let addr =
                    crate::session::Address::new(&self.selection.source, &self.selection.session);
                if addr != self.last_saved_session {
                    self.last_saved_session = addr.clone();
                    cmds.push(Command::PersistLastSession(addr));
                }
                // Fire the attach only when the gate holds (the selection differs from the
                // confirmed display, or the display is astray) and nothing is in flight -
                // the freeze invariant depends on this gate, so it stays exactly as is.
                if self.should_attach(in_flight, display_astray) {
                    cmds.push(Command::Attach(self.selection.clone()));
                }
                cmds
            }
            // The session-lifecycle intent is a pure effect emitter: it folds into the
            // MuxOp the run loop runs off-loop. `apply` mutates no domain state - the
            // inventory change arrives later as the OpResult.
            Action::CreateSession { source, name } => {
                vec![Command::RunOp(MuxOp::Create { source, name })]
            }
        }
    }

    /// The single event-driven mutation site: folds one mux [`HostEvent`] into the
    /// domain state and returns the mux follow-ups as [`EventEffect`]s. The mirror
    /// of [`apply`](State::apply) for the inbound (mux → state) direction - every
    /// `%`-notification, metadata reply, poll result, and reap routes through here, so
    /// State owns the event-driven mutations just as `apply` owns the intent-driven ones.
    ///
    /// `apply_event` performs only the mutations whose data is SELF-CONTAINED in the
    /// event (a poll enumeration, the
    /// unreachable mark) - driven through the switcher, which rebuilds the tree against
    /// `&mut State`. The follow-ups that need a mux handle the state layer must not
    /// hold (the single-owner inventory fold into `model::Host`, a control-mode probe,
    /// the attach registry, the detection dispatch) are returned as [`EventEffect`]s for
    /// the run loop - the sole executor - to carry out (the AGENTS rule: no IO/registry
    /// mutation here).
    ///
    /// `connected` (the run loop's once-connected set) enters as data, like the clock on
    /// `Tick`: an `Exited` of a once-connected host is a transient drop that keeps the
    /// last-known tree; otherwise it resolves the host's real state.
    ///
    /// [`HostEvent`]: crate::link::HostEvent
    /// [`EventEffect`]: crate::model::EventEffect
    pub fn apply_event(
        &mut self,
        ev: crate::link::HostEvent,
        switcher: &mut crate::ui::switcher::Switcher,
        connected: &mut std::collections::HashSet<String>,
    ) -> Vec<crate::model::EventEffect> {
        use crate::link::HostEvent;
        use crate::model::EventEffect;
        match ev {
            HostEvent::Connected { host, sessions } | HostEvent::Inventory { host, sessions } => {
                // The reader carries the parsed sessions on the event; record the
                // connected mark and hand the sessions to the loop, which folds them
                // into `model::Host.inventory` (the single owner) and applies the tree.
                connected.insert(host.clone());
                vec![EventEffect::ApplyInventory { host, sessions }]
            }
            HostEvent::Changed { host } => vec![EventEffect::Refetch { host }],
            HostEvent::Exited { host, reason } => {
                // Mark the host unreachable in the tree (unless a transient drop of a
                // once-connected host), then reap its dead client.
                crate::app::runtime::note_host_exited(switcher, self, connected, &host, reason);
                vec![EventEffect::ReapHost { host }]
            }
            HostEvent::ClientDetached { host, client } => {
                // The tty match against the host's recorded display tty + the registry
                // reap are loop-owned; forward the descriptor, mutate no State.
                vec![EventEffect::ReapDisplayAttach { host, client }]
            }
            HostEvent::ClientSessionChanged {
                host,
                client,
                session,
            } => {
                // The tty match against the host's recorded display tty, the display-belief
                // sync, and the nav follow are all loop-owned (the tty lives on `Host`);
                // forward the descriptor, mutate no State here.
                vec![EventEffect::FollowDisplaySession {
                    host,
                    client,
                    session,
                }]
            }
            HostEvent::DisplayTty { host, tty } => {
                // The tty lives on the Host (behind the loop's reach), so the state
                // layer forwards it as an effect for the loop to record.
                vec![EventEffect::RecordDisplayTty { host, tty }]
            }
            HostEvent::MuxesFound { machine, muxes } => {
                // Nothing to fold: which muxes the machine ALREADY serves lives in the
                // host registry, so the whole decision is the loop's.
                vec![EventEffect::AddDiscoveredSources { machine, muxes }]
            }
            HostEvent::RosterResolved { roster } => {
                // Nothing to fold: which machines the registries already hold is the
                // loop's to know, so the whole decision is the loop's.
                vec![EventEffect::ApplyRoster { roster }]
            }
            HostEvent::Scanned {
                source,
                detected,
                err,
            } => {
                // A detection probe on a CONNECTED machine resolved to no mux (the
                // configured mux is not there): settle the card out of scanning as
                // unreachable with the probe's error, exactly like the MachineProbed
                // err path. Only while the card is STILL scanning, so a stray detection
                // failure (the reconnect sweep retrying a settled host) cannot overwrite
                // a locked/unreachable reason already on the card. The host stays
                // undetected, so the sweep's retry recovers it when the mux appears.
                if detected.is_none() && self.scanning.contains(&source) {
                    let reason = err
                        .clone()
                        .unwrap_or_else(|| "mux not detected".to_string());
                    switcher.apply_source_result(source.clone(), Vec::new(), Some(reason), self);
                }
                vec![EventEffect::DispatchScanned {
                    source,
                    detected,
                    err,
                }]
            }
            HostEvent::MachineProbed {
                machine,
                err,
                shell,
                rescan,
            } => match err {
                Some(reason) => {
                    // The machine did not connect: every source it serves carries the
                    // same failure line, so each card classifies locked (ssh's
                    // auth-failure signature) or unreachable. No channel is opened - the
                    // loop's connected path is gated on this probe succeeding.
                    let sources: Vec<String> = self
                        .groups
                        .iter()
                        .map(|g| g.source.clone())
                        .filter(|s| crate::session::machine_of(s) == machine)
                        .collect();
                    for source in sources {
                        switcher.apply_source_result(
                            source,
                            Vec::new(),
                            Some(reason.clone()),
                            self,
                        );
                    }
                    Vec::new()
                }
                // Connected: which sources to resolve and how lives in the host
                // registry, so the whole decision is the loop's.
                None => vec![EventEffect::MachineConnected {
                    machine,
                    shell,
                    rescan,
                }],
            },
            HostEvent::Sessions {
                source,
                sessions,
                err,
            } => {
                // Apply the poll enumeration to the tree. On a SUCCESSFUL enumeration
                // hand the sessions back so the loop drops any stale attach + syncs the
                // PTY set; a transient failure shows the error but keeps attachments.
                let had_err = err.is_some();
                switcher.apply_source_result(source.clone(), sessions.clone(), err, self);
                if had_err {
                    Vec::new()
                } else {
                    vec![EventEffect::SyncPollSessions { source, sessions }]
                }
            }
        }
    }

    /// Whether to (re)issue an attach for the settled selection. Fire when the
    /// selection differs from what is confirmed on screen, or when the display client
    /// sits on another session (`display_astray`) - but never while an attach for the
    /// key is already in flight, so the async-attach window cannot spawn a storm of
    /// duplicates. The clock and these runtime facts enter as data on the Tick, never
    /// read here directly.
    ///
    /// The astray leg is why the two regions cannot settle on different sessions. The
    /// other leg compares the selection against xmux's OWN record of what it put on
    /// screen, which a session change the mux made never touches: the selection and the
    /// confirmed display agree and the client is somewhere else entirely. Only a fact
    /// about where the client actually is can say so.
    ///
    /// A display PTY that DIED while the selection stands is NOT re-attached. Each
    /// re-attach is a fresh connection to that machine, and an attach whose own EOF
    /// arms the next one is a chain the machine sees as a client hammering it - which
    /// is exactly what it looks like when the session is gone and every attempt dies
    /// the same way. The pane keeps what it last drew until the user selects the card
    /// again or re-scans, so recovering is something the user asks for.
    pub(crate) fn should_attach(&self, in_flight: bool, display_astray: bool) -> bool {
        let owed = self.selection != self.displayed || display_astray;
        owed && !in_flight
    }

    /// Folds a completed [`MuxOp`](crate::model::MuxOp)'s [`OpResult`] into the
    /// inventory - the single owner of `groups` - and returns an
    /// [`OpFollow`] telling the switcher how to rebuild its rows (and, for a create,
    /// which session to reselect). State owns the domain mutation here just as
    /// [`apply`](State::apply) / [`apply_event`](State::apply_event) own the intent- and
    /// event-driven ones; the row rebuild + cursor restore stay in the switcher. A
    /// `Failed` op mutates no inventory - its message is returned to flash.
    ///
    /// [`OpResult`]: crate::ui::ops::OpResult
    /// [`OpFollow`]: crate::ui::ops::OpFollow
    pub(crate) fn fold_op_result(
        &mut self,
        result: crate::ui::ops::OpResult,
    ) -> crate::ui::ops::OpFollow {
        use crate::ui::ops::{OpFollow, OpResult};
        use crate::ui::tree;
        match result {
            OpResult::Created { session, .. } => {
                let addr = session.address();
                self.groups = tree::add_session(&self.groups, session);
                OpFollow::Reselect(addr)
            }
            OpResult::Failed { message } => OpFollow::Flash(message),
            // The unlock verdict is no inventory mutation: the app reacts to it (re-probe
            // the unlocked machine on success, a flash on failure).
            OpResult::Login {
                source,
                login,
                outcome,
            } => {
                // The conversation is over however it ended, so the handle that would
                // have ended it goes with it and the pane offers a login again.
                self.login_run = None;
                OpFollow::LoginResult {
                    source,
                    login,
                    outcome,
                }
            }
        }
    }

    /// Flashes a transient message in the tree-column hint bar (an error or notice).
    /// The next tree key clears it (the switcher's `handle_key` clear path), and so does
    /// its own ten-second life, so the normal help/status hint bar returns whether or not
    /// the user presses anything. Delegates to the chrome's flash API.
    pub(crate) fn flash(&mut self, msg: impl Into<String>) {
        self.chrome.flash(msg);
    }
}

/// Debounce before a settled selection move attaches/switches its session.
/// Rapid navigation must NOT switch-client per step: each switch makes the remote
/// mux send a full-screen repaint, and a storm of repaints floods
/// the draw - the single-threaded loop then spends all its time redrawing, which IS
/// the freeze. Deferring the attach until the selection settles keeps per-step redraws
/// to a cheap tree-only diff. The single source of this value: `apply`'s `Tick` re-arm and
/// its [`Action::RearmAttachNow`](crate::model::Action::RearmAttachNow) both read it, so
/// the two arming paths can never drift.
pub(crate) const ATTACH_DEBOUNCE_MS: u64 = 90;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::focus::Focus;
    use crate::model::{Action, Command, FocusTarget, Selection};
    use crate::session::Address;
    use std::time::Duration;

    #[test]
    fn default_state_is_empty() {
        let s = State::default();
        assert!(s.selection.is_empty());
        assert!(s.displayed.is_empty());
        assert!(s.attach_deadline.is_none());
        assert!(!s.attach_pending);
        assert_eq!(s.last_saved_session, Address::default());
        assert!(s.focus.is_nav_focused());
        assert!(s.modal.is_none());
        assert!(!s.is_modal_popup_open());
        assert!(!s.is_inputting());
        assert!(s.modal_kind().is_none());
    }

    fn sel(session: &str) -> Selection {
        Selection {
            source: "jup".into(),
            session: session.into(),
        }
    }

    #[test]
    fn apply_select_sets_selection_marks_pending_and_emits_no_command() {
        let mut s = State::default();
        let cmds = s.apply(Action::Select(sel("api")));
        assert_eq!(s.selection, sel("api"));
        assert!(s.attach_pending, "Select marks the attach pending");
        assert!(
            s.attach_deadline.is_none(),
            "Select does NOT arm the deadline - the trailing Tick does"
        );
        assert!(cmds.is_empty(), "Select emits no attach command");
    }

    #[test]
    fn apply_tick_arms_then_fires_one_attach_after_debounce() {
        let mut s = State::default();
        let t0 = Instant::now();
        s.apply(Action::Select(sel("api")));
        // Tick at t0 arms the deadline (no fire yet - now < deadline).
        let armed = s.apply(Action::Tick {
            now: t0,
            in_flight: false,
            display_astray: false,
        });
        assert_eq!(s.attach_deadline, Some(t0 + Duration::from_millis(90)));
        assert!(armed.is_empty(), "arming does not fire on the same tick");
        // Tick at t0+90ms (deadline reached) with no intervening Select fires once.
        let fired = s.apply(Action::Tick {
            now: t0 + Duration::from_millis(90),
            in_flight: false,
            display_astray: false,
        });
        assert_eq!(
            fired,
            vec![
                Command::PersistLastSession(Address::new("jup", "api")),
                Command::Attach(sel("api")),
            ],
            "the settled selection attaches exactly once"
        );
        assert!(
            s.attach_deadline.is_none(),
            "the deadline is cleared on fire"
        );
    }

    #[test]
    fn apply_select_between_ticks_rearms_so_rapid_nav_does_not_fire_early() {
        let mut s = State::default();
        let t0 = Instant::now();
        s.apply(Action::Select(sel("api")));
        s.apply(Action::Tick {
            now: t0,
            in_flight: false,
            display_astray: false,
        });
        assert_eq!(s.attach_deadline, Some(t0 + Duration::from_millis(90)));
        // A Select 30ms later (rapid nav) re-marks pending; the next Tick re-arms the
        // deadline PAST the original, so the original deadline does not fire.
        s.apply(Action::Select(sel("db")));
        let rearm = s.apply(Action::Tick {
            now: t0 + Duration::from_millis(30),
            in_flight: false,
            display_astray: false,
        });
        assert!(
            rearm.is_empty(),
            "re-arming a moved selection does not fire"
        );
        assert_eq!(
            s.attach_deadline,
            Some(t0 + Duration::from_millis(30 + 90)),
            "the deadline is pushed out by the re-arm"
        );
        // At the ORIGINAL deadline (t0+90) the now-later deadline (t0+120) has not
        // elapsed → no premature fire.
        let early = s.apply(Action::Tick {
            now: t0 + Duration::from_millis(90),
            in_flight: false,
            display_astray: false,
        });
        assert!(early.is_empty(), "no fire before the re-armed deadline");
        // Only at t0+120 does the trailing selection (db) attach, once.
        let fired = s.apply(Action::Tick {
            now: t0 + Duration::from_millis(120),
            in_flight: false,
            display_astray: false,
        });
        assert_eq!(
            fired,
            vec![
                Command::PersistLastSession(Address::new("jup", "db")),
                Command::Attach(sel("db")),
            ],
            "only the trailing selection attaches"
        );
    }

    #[test]
    fn apply_tick_does_not_fire_when_already_displayed_and_live() {
        // should_attach gate: selection == displayed AND key_live AND not in_flight
        // ⇒ nothing to do (already persisted, so no persist command either).
        let t0 = Instant::now();
        let mut s = State {
            selection: sel("api"),
            displayed: sel("api"),
            last_saved_session: Address::new("jup", "api"), // already persisted → no persist command
            attach_deadline: Some(t0),
            ..State::default()
        };
        let cmds = s.apply(Action::Tick {
            now: t0,
            in_flight: false,
            display_astray: false,
        });
        assert!(
            cmds.is_empty(),
            "no attach when the selection is already the confirmed display and live"
        );
        assert!(
            s.attach_deadline.is_none(),
            "the elapsed deadline is cleared"
        );
    }

    #[test]
    fn apply_tick_arms_when_the_display_sits_on_another_session_with_no_select() {
        // The display can move while the selection stands still: an attach lands for a
        // session nobody chose, or the mux carries the client away. `should_attach` fires
        // on that difference, so the ARMING answers the same condition - if only a
        // `Select` could arm, the gate would sit true with no deadline and the two
        // regions would stay split until the next selection move.
        let t0 = Instant::now();
        let mut s = State {
            selection: sel("api"),
            displayed: sel("db"),
            last_saved_session: Address::new("jup", "api"), // already persisted → no persist command
            ..State::default()
        };
        let armed = s.apply(Action::Tick {
            now: t0,
            in_flight: false,
            display_astray: false,
        });
        assert!(armed.is_empty(), "arming does not fire on the same tick");
        assert_eq!(
            s.attach_deadline,
            Some(t0 + Duration::from_millis(90)),
            "the difference alone arms the debounce"
        );
        let fired = s.apply(Action::Tick {
            now: t0 + Duration::from_millis(90),
            in_flight: false,
            display_astray: false,
        });
        assert_eq!(
            fired,
            vec![Command::Attach(sel("api"))],
            "the attach carries the display back to the selection"
        );
    }

    #[test]
    fn apply_tick_arms_nothing_while_nothing_is_selected() {
        // Until the scan puts a card under the cursor there is nowhere to carry the
        // display to, so an empty selection differing from whatever is displayed arms
        // nothing - it would only arm and clear a deadline every beat.
        let t0 = Instant::now();
        let mut s = State {
            displayed: sel("db"),
            ..State::default()
        };
        let cmds = s.apply(Action::Tick {
            now: t0,
            in_flight: false,
            display_astray: false,
        });
        assert!(cmds.is_empty(), "an empty selection attaches nothing");
        assert!(
            s.attach_deadline.is_none(),
            "an empty selection arms nothing"
        );
    }

    #[test]
    fn a_dead_display_pty_neither_arms_nor_attaches() {
        // The mirrored client detached: the display PTY is gone while the selection and
        // the confirmed display still name one session. Nothing arms and nothing fires.
        //
        // Re-attaching here is a fresh connection to that machine, raised by the death of
        // the connection before it. When the session is gone every attempt dies the same
        // way, so the chain does not stop on its own, and a machine on the far side reads
        // a client reconnecting on every failure as one attacking it. The pane keeps what
        // it last drew; the user recovers it by selecting the card again or re-scanning.
        let t0 = Instant::now();
        let mut s = State {
            groups: one_session_scan().groups,
            selection: sel("api"),
            displayed: sel("api"),
            last_saved_session: Address::new("jup", "api"),
            ..State::default()
        };
        let armed = s.apply(Action::Tick {
            now: t0,
            in_flight: false,
            display_astray: false,
        });
        assert!(armed.is_empty(), "a dead display fires nothing");
        assert!(
            s.attach_deadline.is_none(),
            "a dead display arms no deadline either, so no later tick can fire one"
        );
    }

    #[test]
    fn selecting_the_card_again_is_what_recovers_a_dead_display() {
        // The recovery path that remains is the user's. A selection carries the display
        // wherever it lands, including back onto the session whose PTY died: `Select`
        // marks the attach pending, the next tick arms the debounce, and the elapsed
        // deadline attaches.
        let t0 = Instant::now();
        let mut s = State {
            groups: one_session_scan().groups,
            selection: sel("api"),
            displayed: sel("api"),
            last_saved_session: Address::new("jup", "api"),
            ..State::default()
        };
        s.apply(Action::ClearDisplay); // the dead attachment is torn down
        s.apply(Action::Select(sel("api")));
        s.apply(Action::Tick {
            now: t0,
            in_flight: false,
            display_astray: false,
        });
        let fired = s.apply(Action::Tick {
            now: t0 + Duration::from_millis(90),
            in_flight: false,
            display_astray: false,
        });
        assert_eq!(
            fired,
            vec![Command::Attach(sel("api"))],
            "the user's own selection attaches the session again"
        );
    }

    #[test]
    fn apply_tick_does_not_fire_attach_while_in_flight_but_still_persists() {
        // The attach is suppressed while one is in flight (no storm), but the settled
        // session is still recorded as last-selected - the persist is independent of
        // the attach gate.
        let t0 = Instant::now();
        let mut s = State {
            selection: sel("db"),
            displayed: sel("api"),
            attach_deadline: Some(t0),
            ..State::default()
        };
        let cmds = s.apply(Action::Tick {
            now: t0,
            in_flight: true,
            display_astray: false,
        });
        assert!(
            !cmds.iter().any(|c| matches!(c, Command::Attach(_))),
            "never spawn a second attach while one is already in flight"
        );
        assert_eq!(
            cmds,
            vec![Command::PersistLastSession(Address::new("jup", "db"))],
            "the settled session is still persisted while the attach is suppressed"
        );
    }

    #[test]
    fn apply_tick_persists_second_session_of_same_host_while_first_attach_in_flight() {
        // Differential parity: settle on B → B attaches (its attach now in flight on
        // the shared host key) → settle on C of the SAME host → its Tick sees the key
        // still in flight, so the attach is suppressed, but C MUST still be persisted
        // as last-selected (else the next launch wrongly restores B).
        let mut s = State::default();
        let t0 = Instant::now();
        // Settle on B and let its attach fire (no in-flight yet, B differs from the
        // empty displayed).
        s.apply(Action::Select(sel("b")));
        s.apply(Action::Tick {
            now: t0,
            in_flight: false,
            display_astray: false,
        }); // arms
        let b_cmds = s.apply(Action::Tick {
            now: t0 + Duration::from_millis(90),
            in_flight: false,
            display_astray: false,
        });
        assert_eq!(
            b_cmds,
            vec![
                Command::PersistLastSession(Address::new("jup", "b")),
                Command::Attach(sel("b")),
            ],
        );
        // Move to C of the same host while B's attach is still in flight.
        s.apply(Action::Select(sel("c")));
        s.apply(Action::Tick {
            now: t0 + Duration::from_millis(100),
            in_flight: true,
            display_astray: false,
        }); // arms
        let c_cmds = s.apply(Action::Tick {
            now: t0 + Duration::from_millis(190),
            in_flight: true, // first attach (B) still in flight on the shared host key
            display_astray: false,
        });
        assert!(
            c_cmds.contains(&Command::PersistLastSession(Address::new("jup", "c"))),
            "C must be persisted even though its attach is suppressed by in_flight: {c_cmds:?}"
        );
        assert!(
            !c_cmds.iter().any(|c| matches!(c, Command::Attach(_))),
            "C's attach is suppressed while B's attach is in flight (no storm): {c_cmds:?}"
        );
        assert_eq!(s.last_saved_session, Address::new("jup", "c"));
    }

    #[test]
    fn apply_tick_with_empty_selection_does_nothing() {
        let t0 = Instant::now();
        let mut s = State {
            attach_deadline: Some(t0),
            ..State::default()
        };
        let cmds = s.apply(Action::Tick {
            now: t0,
            in_flight: false,
            display_astray: false,
        });
        assert!(cmds.is_empty(), "empty selection never attaches");
        assert!(s.attach_deadline.is_none());
    }

    #[test]
    fn apply_rearm_attach_now_arms_an_immediate_deadline() {
        // The `r` reattach-kick fires ASAP: it sets the deadline to `now` so the SAME
        // loop iteration's trailing Tick sees it elapsed and re-attaches immediately.
        let mut s = State::default();
        let t0 = Instant::now();
        let cmds = s.apply(Action::RearmAttachNow { now: t0 });
        assert_eq!(
            s.attach_deadline,
            Some(t0),
            "RearmAttachNow arms an already-elapsed (immediate) deadline"
        );
        assert!(cmds.is_empty(), "RearmAttachNow emits no command");
    }

    #[test]
    fn apply_focus_moves_focus_with_no_command() {
        let mut s = State::default();
        assert!(s.focus.is_nav_focused());
        assert!(s.apply(Action::Focus(FocusTarget::Terminal)).is_empty());
        assert_eq!(s.focus, Focus::Terminal);
        assert!(s.apply(Action::Focus(FocusTarget::Nav)).is_empty());
        assert_eq!(s.focus, Focus::Nav);
    }

    #[test]
    fn apply_focus_toggle_flips_the_view_and_delegates_to_focus_toggle() {
        use crate::app::focus::ViewFocus;
        let mut s = State::default(); // Tree
        assert!(
            s.apply(Action::FocusToggle).is_empty(),
            "FocusToggle emits no command"
        );
        assert_eq!(s.focus, Focus::Terminal, "toggle flips Tree → Terminal");
        s.apply(Action::FocusToggle);
        assert_eq!(s.focus, Focus::Nav, "toggle flips back Terminal → Tree");
        // During a modal, toggle flips the carried prior and keeps the modal open.
        s.focus = Focus::Popup {
            prior: ViewFocus::Nav,
        };
        s.apply(Action::FocusToggle);
        assert_eq!(
            s.focus,
            Focus::Popup {
                prior: ViewFocus::Terminal
            },
            "toggle during a modal flips prior, the modal stays open"
        );
    }

    #[test]
    fn apply_confirm_display_sets_displayed() {
        // ConfirmDisplay advances the display truth to the given selection - the
        // in-place-attach / DisplayReady confirmation, folded at the single site.
        let mut s = State::default();
        assert!(s.displayed.is_empty());
        let cmds = s.apply(Action::ConfirmDisplay(sel("api")));
        assert_eq!(
            s.displayed,
            sel("api"),
            "ConfirmDisplay sets the display truth"
        );
        assert!(cmds.is_empty(), "ConfirmDisplay emits no command");
    }

    #[test]
    fn apply_clear_display_empties_displayed() {
        // ClearDisplay blanks the display truth - the reattach-kick path (nothing
        // confirmed yet → blank view until the fresh attach lands).
        let mut s = State {
            displayed: sel("api"),
            ..State::default()
        };
        assert!(!s.displayed.is_empty());
        let cmds = s.apply(Action::ClearDisplay);
        assert!(
            s.displayed.is_empty(),
            "ClearDisplay blanks the display truth"
        );
        assert!(cmds.is_empty(), "ClearDisplay emits no command");
    }

    #[test]
    fn apply_switch_emits_select_address_command() {
        let mut s = State::default();
        assert_eq!(
            s.apply(Action::Switch(crate::session::Address::new("jup", "db"))),
            vec![Command::SelectAddress(crate::session::Address::new(
                "jup", "db"
            ))]
        );
    }

    #[test]
    fn resolve_switch_address_reports_which_half_is_missing() {
        use crate::session::Address;
        let s = State::from_scan(Scan {
            groups: vec![
                Group {
                    source: "jup".into(),
                    err: None,
                    sessions: vec![Session {
                        source: "jup".into(),
                        name: "api".into(),
                        ..Default::default()
                    }],
                },
                Group {
                    source: "local:psmux".into(),
                    err: None,
                    sessions: vec![Session {
                        source: "local:psmux".into(),
                        name: "swtarget".into(),
                        ..Default::default()
                    }],
                },
            ],
        });
        // A session the inventory lists resolves.
        assert_eq!(
            s.resolve_switch_address(&Address::new("jup", "api")),
            Ok(())
        );
        assert_eq!(
            s.resolve_switch_address(&Address::new("local:psmux", "swtarget")),
            Ok(()),
            "the qualified source pair the nav actually uses resolves"
        );
        // A session missing under an existing source is a session error.
        let err = s
            .resolve_switch_address(&Address::new("jup", "nope"))
            .unwrap_err();
        assert!(err.starts_with("no such session"), "{err}");
        // A source that does not exist is a source error - the issue's `local/swtarget`
        // when the real source is `local:psmux`, and a wholly unknown host.
        let err = s
            .resolve_switch_address(&Address::new("local", "swtarget"))
            .unwrap_err();
        assert!(err.starts_with("no such source"), "{err}");
        let err = s
            .resolve_switch_address(&Address::new("nosuchhost", "nosuchsession"))
            .unwrap_err();
        assert!(err.starts_with("no such source"), "{err}");
        // An address with no source on the roster is a source error, not a parse one.
        assert!(s
            .resolve_switch_address(&Address::new("noslash", ""))
            .is_err());
    }

    #[test]
    fn apply_rescan_emits_rescan_command() {
        let mut s = State::default();
        assert_eq!(s.apply(Action::Rescan), vec![Command::Rescan]);
    }

    #[test]
    fn apply_nav_width_emits_adjust_command() {
        let mut s = State::default();
        assert_eq!(
            s.apply(Action::NavWidth(-2)),
            vec![Command::AdjustNavWidth(-2)]
        );
    }

    #[test]
    fn apply_toggle_auto_hide_emits_toggle_command() {
        let mut s = State::default();
        assert_eq!(
            s.apply(Action::ToggleAutoHide),
            vec![Command::ToggleAutoHide]
        );
    }

    #[test]
    fn apply_quit_emits_quit_command() {
        let mut s = State::default();
        assert_eq!(s.apply(Action::Quit), vec![Command::Quit]);
    }

    // --- session-lifecycle intents fold into Command::RunOp ------------------
    // Each lifecycle Action is a pure intent → effect: apply mutates nothing and
    // returns the MuxOp descriptor the run loop runs off-loop. The OpResult flows
    // back over the op channel.

    fn a_sess(name: &str) -> crate::session::Session {
        crate::session::Session {
            source: "jup".into(),
            name: name.into(),
            ..Default::default()
        }
    }

    #[test]
    fn apply_create_session_emits_run_op_create() {
        use crate::model::MuxOp;
        let mut s = State::default();
        assert_eq!(
            s.apply(Action::CreateSession {
                source: "jup".into(),
                name: "api".into(),
            }),
            vec![Command::RunOp(MuxOp::Create {
                source: "jup".into(),
                name: "api".into(),
            })]
        );
    }

    #[test]
    fn apply_lifecycle_action_does_not_touch_selection_or_focus() {
        // The lifecycle intent is a pure effect emitter: it leaves domain state alone
        // (the OpResult that follows mutates the inventory, not apply).
        let mut s = State::default();
        let before_sel = s.selection.clone();
        s.apply(Action::CreateSession {
            source: "jup".into(),
            name: "api".into(),
        });
        assert_eq!(
            s.selection, before_sel,
            "create intent leaves selection alone"
        );
        assert!(s.focus.is_nav_focused(), "create intent leaves focus alone");
        assert!(s.modal.is_none(), "create intent leaves the popup alone");
    }

    // --- apply_event(HostEvent) -----------------------------------------------
    // State owns the EVENT-DRIVEN mutations: apply_event folds the self-contained
    // arms (Focus marker, Panes subtree, Sessions enumeration, Exited unreachable
    // mark) into State directly, and returns the mux follow-ups (refetch /
    // probe / reap / sync / scan-dispatch) as EventEffects for the run loop to run.
    use crate::link::HostEvent;
    use crate::model::EventEffect;
    use crate::session::Session;
    use crate::ui::switcher::{Scan, Switcher};
    use crate::ui::tree::Group;
    use std::collections::HashSet;

    fn one_session_scan() -> Scan {
        Scan {
            groups: vec![Group {
                source: "jup".into(),
                err: None,
                sessions: vec![Session {
                    source: "jup".into(),
                    name: "api".into(),
                    mux: "tmux".into(),
                    windows: 2,
                    attached: false,
                }],
            }],
        }
    }

    fn with_switcher(scan: Scan) -> (State, Switcher) {
        let mut state = State::from_scan(scan);
        let sw = Switcher::new(&mut state);
        (state, sw)
    }

    #[test]
    fn apply_event_connected_marks_connected_and_emits_apply_inventory() {
        // The reader carries the parsed sessions on Connected/Inventory; apply_event
        // records the connected mark and hands the sessions to the loop as an effect
        // (which folds them into `model::Host.inventory` - the single owner).
        let (mut state, mut sw) = with_switcher(one_session_scan());
        let mut connected = HashSet::new();
        let sessions = vec![crate::session::Session {
            source: "jup".into(),
            name: "api".into(),
            ..Default::default()
        }];
        let effects = state.apply_event(
            HostEvent::Connected {
                host: "jup".into(),
                sessions: sessions.clone(),
            },
            &mut sw,
            &mut connected,
        );
        assert!(connected.contains("jup"), "Connected records the host");
        assert!(
            matches!(effects.as_slice(), [EventEffect::ApplyInventory { host, sessions }] if host == "jup" && sessions.len() == 1),
            "Connected carries its sessions into one ApplyInventory effect: {effects:?}"
        );
        // Inventory behaves identically (the arm is shared).
        let effects = state.apply_event(
            HostEvent::Inventory {
                host: "jup".into(),
                sessions,
            },
            &mut sw,
            &mut connected,
        );
        assert!(
            matches!(effects.as_slice(), [EventEffect::ApplyInventory { host, sessions }] if host == "jup" && sessions.len() == 1),
        );
    }

    #[test]
    fn apply_event_changed_emits_refetch() {
        let (mut state, mut sw) = with_switcher(one_session_scan());
        let mut connected = HashSet::new();
        let effects = state.apply_event(
            HostEvent::Changed { host: "jup".into() },
            &mut sw,
            &mut connected,
        );
        assert!(
            matches!(effects.as_slice(), [EventEffect::Refetch { host }] if host == "jup"),
            "Changed returns one Refetch effect: {effects:?}"
        );
    }

    #[test]
    fn apply_event_client_detached_emits_reap_display_attach_with_no_state_change() {
        // The tty match + reap need the host registry (loop-owned); apply_event only
        // forwards the descriptor and touches no State.
        let (mut state, mut sw) = with_switcher(one_session_scan());
        let mut connected = HashSet::new();
        let before_groups = state.groups.len();
        let before_sessions = state.groups[0].sessions.len();
        let effects = state.apply_event(
            HostEvent::ClientDetached {
                host: "jup".into(),
                client: "/dev/pts/3".into(),
            },
            &mut sw,
            &mut connected,
        );
        assert!(
            matches!(
                effects.as_slice(),
                [EventEffect::ReapDisplayAttach { host, client }]
                    if host == "jup" && client == "/dev/pts/3"
            ),
            "ClientDetached forwards a ReapDisplayAttach effect: {effects:?}"
        );
        // ClientDetached mutates no State (the tree group set is untouched).
        assert_eq!(state.groups.len(), before_groups);
        assert_eq!(state.groups[0].sessions.len(), before_sessions);
        assert!(state.modal.is_none());
    }

    #[test]
    fn apply_event_client_session_changed_forwards_follow_effect_with_no_state_change() {
        // The tty match against Host.display_tty, the display-belief sync, and the nav
        // follow all need loop-owned state; apply_event only forwards the descriptor and
        // touches no State (the selection follow happens in the loop, gated on the match).
        let (mut state, mut sw) = with_switcher(one_session_scan());
        let mut connected = HashSet::new();
        let before_groups = state.groups.len();
        let before_sessions = state.groups[0].sessions.len();
        let effects = state.apply_event(
            HostEvent::ClientSessionChanged {
                host: "jup".into(),
                client: "/dev/pts/3".into(),
                session: "db".into(),
            },
            &mut sw,
            &mut connected,
        );
        assert!(
            matches!(
                effects.as_slice(),
                [EventEffect::FollowDisplaySession { host, client, session }]
                    if host == "jup" && client == "/dev/pts/3" && session == "db"
            ),
            "ClientSessionChanged forwards a FollowDisplaySession effect: {effects:?}"
        );
        // apply_event mutates no State (the tree group set is untouched); the tty match +
        // selection follow are loop-owned.
        assert_eq!(state.groups.len(), before_groups);
        assert_eq!(state.groups[0].sessions.len(), before_sessions);
    }

    #[test]
    fn apply_event_exited_marks_unreachable_and_emits_reap() {
        // A never-connected host exiting with a real failure marks the tree
        // unreachable (a State mutation) AND asks the loop to reap the client.
        let (mut state, mut sw) = with_switcher(one_session_scan());
        let mut connected = HashSet::new(); // not connected → not a transient drop
        let effects = state.apply_event(
            HostEvent::Exited {
                host: "jup".into(),
                reason: Some("connection refused".into()),
            },
            &mut sw,
            &mut connected,
        );
        assert!(
            matches!(effects.as_slice(), [EventEffect::ReapHost { host }] if host == "jup"),
            "Exited returns one ReapHost effect: {effects:?}"
        );
        let g = state.groups.iter().find(|g| g.source == "jup").unwrap();
        assert!(
            g.err.is_some(),
            "the host is marked unreachable in the tree"
        );
    }

    #[test]
    fn apply_event_exited_of_connected_host_keeps_tree_and_still_reaps() {
        // A transient drop of a once-connected host keeps its last-known tree (no
        // unreachable flash) but still reaps the dead client.
        let (mut state, mut sw) = with_switcher(one_session_scan());
        let mut connected = HashSet::new();
        connected.insert("jup".to_string());
        let effects = state.apply_event(
            HostEvent::Exited {
                host: "jup".into(),
                reason: None,
            },
            &mut sw,
            &mut connected,
        );
        assert!(matches!(effects.as_slice(), [EventEffect::ReapHost { host }] if host == "jup"),);
        assert!(
            !connected.contains("jup"),
            "the connected mark is cleared so a later failed reconnect resolves"
        );
        let g = state.groups.iter().find(|g| g.source == "jup").unwrap();
        assert!(
            g.err.is_none(),
            "a transient drop keeps the last-known tree"
        );
    }

    #[test]
    fn apply_event_sessions_applies_tree_and_emits_sync_on_success() {
        // A poll host's enumeration is self-contained: apply_event applies the
        // sessions to the tree and hands the sessions back for the stale-attach /
        // sync follow-up the loop owns.
        let mut state = State::from_sources(vec!["local".into()]);
        let mut sw = Switcher::from_sources(&mut state);
        let mut connected = HashSet::new();
        let sessions = vec![Session {
            source: "local".into(),
            name: "work".into(),
            mux: "tmux".into(),
            windows: 1,
            attached: false,
        }];
        let effects = state.apply_event(
            HostEvent::Sessions {
                source: "local".into(),
                sessions: sessions.clone(),
                err: None,
            },
            &mut sw,
            &mut connected,
        );
        assert!(
            !state.scanning.contains("local"),
            "the enumerated source is no longer scanning"
        );
        let g = state.groups.iter().find(|g| g.source == "local").unwrap();
        assert_eq!(g.sessions.len(), 1, "the session is in the tree");
        assert!(
            matches!(
                effects.as_slice(),
                [EventEffect::SyncPollSessions { source, sessions: s }]
                    if source == "local" && s.len() == 1
            ),
            "a successful enumeration syncs terminals: {effects:?}"
        );
    }

    #[test]
    fn apply_event_sessions_with_error_applies_tree_but_emits_no_sync() {
        // A transient enumeration failure shows the error in the tree but keeps
        // attachments (the keep-alive guarantee) - no sync effect.
        let mut state = State::from_sources(vec!["local".into()]);
        let mut sw = Switcher::from_sources(&mut state);
        let mut connected = HashSet::new();
        let effects = state.apply_event(
            HostEvent::Sessions {
                source: "local".into(),
                sessions: Vec::new(),
                err: Some("poll failed".into()),
            },
            &mut sw,
            &mut connected,
        );
        let g = state.groups.iter().find(|g| g.source == "local").unwrap();
        assert_eq!(g.err.as_deref(), Some("poll failed"));
        assert!(
            effects.is_empty(),
            "a failed enumeration keeps attachments - no sync effect: {effects:?}"
        );
    }

    #[test]
    fn feed_login_fills_the_pane_and_submits_from_the_button() {
        // Enter passes the focus on from a text field, so filling the pane top to bottom
        // with Enter alone ends on the button, where Enter submits. The password is taken
        // out of the draft on submit so nothing keeps it.
        let mut s = State::default();
        // address, port, username come prefilled; Enter walks past them.
        for _ in 0..3 {
            assert!(
                s.feed_login("prod", b"\r").is_none(),
                "a field passes focus on"
            );
        }
        assert!(s.feed_login("prod", b"hunter2").is_none(), "typing waits");
        assert!(
            s.feed_login("prod", b"\r").is_none(),
            "the password field passes focus on too"
        );
        // The focus is on the pubkey checkbox: Space picks it, Enter walks past.
        assert!(
            s.feed_login("prod", b" ").is_none(),
            "Space picks, never submits"
        );
        assert!(
            s.feed_login("prod", b"\r").is_none(),
            "Enter walks past the choice"
        );
        let cmd = s.feed_login("prod", b"\r").expect("the button submits");
        match cmd {
            crate::model::Command::RunLogin {
                source,
                password,
                pubkey,
                ..
            } => {
                assert_eq!(source, "prod");
                assert_eq!(password, "hunter2");
                assert!(pubkey, "the checkbox the user toggled rides along");
            }
            other => panic!("expected RunLogin, got {other:?}"),
        }
        assert_eq!(
            s.login.as_ref().unwrap().password,
            "",
            "the submitted password is taken out of the draft"
        );
    }

    #[test]
    fn feed_login_walks_its_stops_with_tab_and_the_vertical_arrows() {
        let mut s = State::default();
        s.feed_login("prod", b"\t");
        assert_eq!(s.login.as_ref().unwrap().focus, LoginFocus::Port);
        s.feed_login("prod", b"\x1b[B");
        assert_eq!(s.login.as_ref().unwrap().focus, LoginFocus::Username);
        s.feed_login("prod", b"\x1b[A");
        assert_eq!(s.login.as_ref().unwrap().focus, LoginFocus::Port);
        s.feed_login("prod", b"\x1b[Z");
        assert_eq!(s.login.as_ref().unwrap().focus, LoginFocus::Address);
    }

    #[test]
    fn feed_login_offers_the_remember_choice_only_after_a_value_changes() {
        // A stanza repeating what ssh already resolves records nothing, so the choice is
        // absent until the user changes a connection value, and the stops skip it.
        let mut s = State::default();
        s.feed_login("prod", b"x");
        let d = s.login.as_ref().unwrap();
        assert!(d.changed(), "the address was edited");
        assert!(d.stops().contains(&LoginFocus::RememberSshConfig));
        // Undoing the edit takes the choice away again.
        s.feed_login("prod", b"\x7f");
        let d = s.login.as_ref().unwrap();
        assert!(!d.changed());
        assert!(!d.stops().contains(&LoginFocus::RememberSshConfig));
    }

    #[test]
    fn feed_login_backspace_edits_and_a_new_source_resets_the_draft() {
        let mut s = State::default();
        s.feed_login("prod", b"X");
        s.feed_login("prod", b"\x7f");
        assert_eq!(s.login.as_ref().unwrap().address, "prod");
        // Moving to another blocked host starts a fresh draft (no stale value carried).
        s.feed_login("stage", b"");
        let d = s.login.as_ref().unwrap();
        assert_eq!(d.source, "stage");
        assert_eq!(
            d.address, "stage",
            "the fresh draft starts at its own defaults"
        );
    }

    #[test]
    fn feed_login_never_lets_an_escape_sequence_land_in_a_field() {
        // A function key xmux does not act on is still a key, not text: none of its bytes
        // reach a field.
        let mut s = State::default();
        s.feed_login("prod", b"\x7f\x7f\x7f\x7fab");
        s.feed_login("prod", b"\x1b[1;5C");
        s.feed_login("prod", b"\x1bOP");
        assert_eq!(s.login.as_ref().unwrap().address, "ab");
    }

    #[test]
    fn machine_probe_connected_forwards_the_connect_to_the_loop() {
        // A machine that answered `true` carries no reason; which of its sources to
        // resolve, and how, lives in the host registry, so the whole decision is the
        // loop's.
        let mut state = State::from_sources(vec!["prod".into()]);
        let mut sw = crate::ui::switcher::Switcher::from_sources(&mut state);
        let mut connected = HashSet::new();
        let effects = state.apply_event(
            HostEvent::MachineProbed {
                shell: None,
                machine: "prod".into(),
                err: None,
                rescan: false,
            },
            &mut sw,
            &mut connected,
        );
        assert!(
            matches!(
                &effects[..],
                [EventEffect::MachineConnected {
                    machine,
                    rescan: false,
                    ..
                }] if machine == "prod"
            ),
            "{effects:?}"
        );
    }

    #[test]
    fn machine_probe_auth_failure_marks_every_source_of_the_machine_locked() {
        // The reachability probe is the single classification site: an auth failure
        // (ssh's `Permission denied (` signature) marks EVERY source the machine serves
        // locked, and folds nothing itself for the loop to run - no channel is opened.
        let mut state = State::from_sources(vec!["prod".into(), "prod:zellij".into(), "db".into()]);
        let mut sw = crate::ui::switcher::Switcher::from_sources(&mut state);
        let mut connected = HashSet::new();
        let effects = state.apply_event(
            HostEvent::MachineProbed {
                shell: None,
                machine: "prod".into(),
                err: Some(
                    "command failed (exit 255): user@prod: Permission denied (publickey,password)."
                        .into(),
                ),
                rescan: false,
            },
            &mut sw,
            &mut connected,
        );
        assert!(
            effects.is_empty(),
            "a failed probe opens no channel: {effects:?}"
        );
        for source in ["prod", "prod:zellij"] {
            let g = state
                .groups
                .iter()
                .find(|g| g.source == source)
                .unwrap_or_else(|| panic!("{source} group"));
            assert!(
                g.err.as_deref().is_some_and(crate::mux::is_blocked),
                "{source} classifies locked: {:?}",
                g.err
            );
        }
        let other = state.groups.iter().find(|g| g.source == "db").unwrap();
        assert!(other.err.is_none(), "another machine is untouched");
    }

    #[test]
    fn machine_probe_unreachable_marks_the_machine_unreachable_not_locked() {
        // A reach failure (refused/timeout/no route) is unreachable, never locked: only
        // ssh's auth-failure signature earns locked, so a host that merely died stays a
        // plain unreachable card.
        let mut state = State::from_sources(vec!["prod".into()]);
        let mut sw = crate::ui::switcher::Switcher::from_sources(&mut state);
        let mut connected = HashSet::new();
        let _ = state.apply_event(
            HostEvent::MachineProbed {
                shell: None,
                machine: "prod".into(),
                err: Some("ssh: connect to host prod port 22: Connection refused".into()),
                rescan: false,
            },
            &mut sw,
            &mut connected,
        );
        let g = state.groups.iter().find(|g| g.source == "prod").unwrap();
        assert!(g.err.is_some(), "the card is unreachable");
        assert!(
            !g.err.as_deref().is_some_and(crate::mux::is_blocked),
            "a reach failure is not locked: {:?}",
            g.err
        );
    }

    #[test]
    fn apply_event_scanned_emits_dispatch_carrying_the_detection() {
        // The detection box + the host-channel dispatch are loop-owned; apply_event
        // forwards the descriptor. The host already has sessions (not scanning), so a
        // failed detection does not settle it - only a still-scanning card settles.
        let (mut state, mut sw) = with_switcher(one_session_scan());
        let mut connected = HashSet::new();
        let effects = state.apply_event(
            HostEvent::Scanned {
                source: "jup".into(),
                detected: None,
                err: Some("command failed (exit 127): sh: tmux: not found".into()),
            },
            &mut sw,
            &mut connected,
        );
        assert!(
            matches!(
                effects.as_slice(),
                [EventEffect::DispatchScanned {
                    source,
                    detected: None,
                    ..
                }] if source == "jup"
            ),
            "Scanned forwards a DispatchScanned effect: {effects:?}"
        );
        let g = state.groups.iter().find(|g| g.source == "jup").unwrap();
        assert!(
            g.err.is_none(),
            "a settled host keeps its state; a stray detection failure does not touch it"
        );
    }

    #[test]
    fn a_connected_machines_failed_detection_settles_the_scanning_card() {
        // A host that reached the connection stage (a local/WSL machine connected
        // inline, or a remote whose machine probe succeeded) but whose mux detection
        // failed must leave the scanning state: it settles as unreachable with the
        // probe's error instead of spinning forever (issue 226).
        let mut state = State::from_sources(vec!["jup".into()]);
        let mut sw = crate::ui::switcher::Switcher::from_sources(&mut state);
        let mut connected = HashSet::new();
        assert!(state.scanning.contains("jup"), "precondition: scanning");
        let effects = state.apply_event(
            HostEvent::Scanned {
                source: "jup".into(),
                detected: None,
                err: Some("command failed (exit 127): sh: tmux: not found".into()),
            },
            &mut sw,
            &mut connected,
        );
        assert!(
            !state.scanning.contains("jup"),
            "the failed detection settles the card out of scanning"
        );
        let g = state.groups.iter().find(|g| g.source == "jup").unwrap();
        assert_eq!(
            g.err.as_deref(),
            Some("command failed (exit 127): sh: tmux: not found"),
            "the card carries the detection error"
        );
        assert!(g.sessions.is_empty());
        assert!(
            matches!(
                effects.as_slice(),
                [EventEffect::DispatchScanned {
                    source,
                    detected: None,
                    ..
                }] if source == "jup"
            ),
            "the detection box still forwards to the loop: {effects:?}"
        );
    }

    #[test]
    fn a_stray_detection_failure_does_not_overwrite_a_settled_card() {
        // The reconnect sweep retries detection for undetected hosts even after they
        // settled unreachable/locked. That later failure must NOT overwrite the card's
        // existing reason - only a still-scanning card settles on detection failure.
        let mut state = State::from_sources(vec!["jup".into()]);
        let mut sw = crate::ui::switcher::Switcher::from_sources(&mut state);
        let mut connected = HashSet::new();
        let _ = state.apply_event(
            HostEvent::MachineProbed {
                shell: None,
                machine: "jup".into(),
                err: Some("hrlee@jup: Permission denied (publickey,password).".into()),
                rescan: false,
            },
            &mut sw,
            &mut connected,
        );
        assert!(
            !state.scanning.contains("jup"),
            "the machine probe settled the card first"
        );
        let _ = state.apply_event(
            HostEvent::Scanned {
                source: "jup".into(),
                detected: None,
                err: Some("command failed (exit 255)".into()),
            },
            &mut sw,
            &mut connected,
        );
        let g = state.groups.iter().find(|g| g.source == "jup").unwrap();
        assert_eq!(
            g.err.as_deref(),
            Some("hrlee@jup: Permission denied (publickey,password)."),
            "a settled reason is not overwritten by a stray detection failure"
        );
    }

    #[test]
    fn muxes_found_forwards_the_add_to_the_loop() {
        // Which muxes a machine ALREADY serves lives in the host registry, which this
        // layer does not hold, so the whole decision is forwarded rather than folded.
        let mut state = State::from_sources(vec!["prod".into()]);
        let mut sw = crate::ui::switcher::Switcher::from_sources(&mut state);
        let mut connected = HashSet::new();
        let before = state.groups.len();
        let effects = state.apply_event(
            HostEvent::MuxesFound {
                machine: "prod".into(),
                muxes: vec!["tmux".into(), "zellij".into()],
            },
            &mut sw,
            &mut connected,
        );
        assert!(
            matches!(
                &effects[..],
                [EventEffect::AddDiscoveredSources { machine, muxes }]
                    if machine == "prod" && muxes == &["tmux".to_string(), "zellij".to_string()]
            ),
            "{effects:?}"
        );
        assert_eq!(state.groups.len(), before, "and folds nothing itself");
    }

    // --- fold_op_result: State owns the op-result inventory mutation ----------
    // A completed MuxOp's OpResult folds its inventory change (groups) into State;
    // the returned OpFollow tells the switcher only how to rebuild the rows + move
    // the cursor. State owns the domain mutation.

    #[test]
    fn fold_op_result_failed_flashes_and_leaves_inventory_untouched() {
        use crate::ui::ops::{OpFollow, OpResult};
        let mut s = State::default();
        s.fold_op_result(OpResult::Created {
            session: a_sess("api"),
        });
        let before_groups = s.groups.len();
        let follow = s.fold_op_result(OpResult::Failed {
            message: "create failed: boom".into(),
        });
        assert_eq!(s.groups.len(), before_groups, "a failure mutates no groups");
        assert!(
            matches!(follow, OpFollow::Flash(m) if m == "create failed: boom"),
            "a failure carries its message to the switcher's flash"
        );
    }

    // --- chrome ownership: State owns the chrome view-state -------------------

    #[test]
    fn flash_sets_message_and_key_clears_it() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let (mut state, mut sw) = with_switcher(one_session_scan());
        state.flash("boom");
        assert_eq!(
            state.chrome.flash, "boom",
            "State::flash sets the chrome flash"
        );
        // A navigation key clears the flash (the switcher's handle_key clear path).
        sw.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &mut state);
        assert!(
            state.chrome.flash.is_empty(),
            "a key clears the flash so the normal hint bar returns"
        );
    }
}
