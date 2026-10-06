//! Runtime domain state: the single source of truth the new architecture's
//! components read from. Carries the app loop's inventory, selection,
//! display-truth, focus, and the open modal popup.
pub(crate) mod chrome;
mod focus;
mod modal;
pub(crate) mod notify;
mod view;

pub use chrome::{Chrome, SourceReach, ViewBorderColors};
pub use focus::{Focus, ModalKind, ViewFocus};
pub(crate) use modal::{
    feed_reader, is_inputting, is_popup_open, is_reader, modal_kind, HelpMap, Input, InputMode,
    Modal, PaletteChoice,
};
pub(crate) use view::RowRef;
pub use view::{OpFollow, Scan};

use crate::model::SECRET_INPUT_CAPACITY;
pub use crate::model::{AfterLogin, SecretInput};
use crate::model::{Group, LoginOutcome, OpResult, RegistrationOutcome, Selection};
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
    /// The time each outstanding source scan must have answered by.
    pub(crate) scan_deadlines: HashMap<String, std::time::Instant>,
    /// MACHINES the user has logged in to successfully in this run, which hiding never
    /// drops however they answer afterwards.
    ///
    /// A locked host is kept because it is actionable, its login pane being the one entry
    /// point. Succeeding at that login does not make it less actionable: it is the host
    /// the user just chose, and whatever it answers next is the answer they are waiting
    /// for. Without this, the one action a card offers is the action that makes the card
    /// vanish - the login stops being blocked, so nothing keeps it any more. Keyed by
    /// machine because a login authenticates the machine, not the one mux that carried it.
    /// The set follows credential presence: a machine whose held credential is gone
    /// leaves it.
    pub logged_in: HashSet<String>,
    /// SSH authentication reported by the connection that last reached each machine.
    pub auth_methods: HashMap<String, crate::model::AuthMethod>,
    /// Authentication reported by the current live display attachment of each source.
    pub display_auth_methods: HashMap<String, crate::model::AuthMethod>,
    /// Machines whose known authentication was invalidated until the user asks again.
    /// Losing a held password, or a refusal of a machine whose method was known, lands
    /// here: the reported method goes and the machine's metadata and display connections
    /// close, so nothing reconnects it before another explicit connection attempt.
    pub invalid_auth: HashSet<String>,
    /// The last login attempt for each machine. It is separate from probe failures so a
    /// follow-up probe cannot replace the authentication diagnosis the user needs.
    pub login_reports: HashMap<String, LoginOutcome>,
    /// The last requested public-key registration result for each machine. It outlives
    /// the login pane so a later host screen can still state what happened.
    pub registration_reports: HashMap<String, RegistrationOutcome>,
    /// How many times in a row each source has failed to enumerate, reset to zero the
    /// moment it answers. Written at the single result-apply site and read only to be
    /// SHOWN: the unreachable screen states it, because one failed sweep and a host that
    /// has not answered since launch are different problems behind the same message.
    pub failure_runs: HashMap<String, u32>,
    /// Last successful enumeration of each source in this run.
    pub last_reached: HashMap<String, std::time::SystemTime>,
    /// Sources whose metadata push channel currently answers.
    pub live_sources: HashSet<String>,
    /// Sources whose host-screen diagnostic rows are expanded.
    pub(crate) host_details: HashSet<String>,
    /// Active fuzzy-filter text (drives the visible tree + the hint_bar).
    pub filter: String,
    /// What the tree selection points at - the session to show.
    pub selection: Selection,
    /// The address whose content is confirmed live in the on-screen terminal view -
    /// the single display truth, and the target of both rendering and input. The
    /// terminal view always shows THIS session's grid; on a switch it stays on the
    /// prior session until the new one is confirmed (stale-while-revalidate), then
    /// advances. Set only at confirmation (a synchronous in-place switch, or an
    /// attachment whose paint gate opened). Empty before the first confirmation means
    /// the view can show the initial scan animation.
    pub displayed: Selection,
    /// When set, a settled selection is attached once this instant passes.
    pub attach_deadline: Option<Instant>,
    /// A selection moved and has not yet armed its debounce deadline. The next
    /// [`Action::Tick`] (re)arms `attach_deadline` from this - re-armed on EVERY
    /// pending selection so rapid navigation coalesces into one trailing attach
    /// instead of a per-step storm of switch-client repaints (the freeze).
    pub attach_pending: bool,
    /// The session last persisted as the user's last-selected, so stepping within the
    /// same session does not rewrite the preference file on every settle.
    pub last_saved_session: crate::session::Address,
    /// The app's focus state machine - which pane keys go to and whether a
    /// modal is open. The single source of truth for focus.
    pub focus: Focus,
    /// The single open modal, if any (an input, the palette, the help, the hosts to
    /// check, the history). One Option - not four independent fields - so the modals' mutual
    /// exclusion is structural: opening one drops whatever was open, and two can
    /// never coexist. The switcher owns the modal behavior and the transient popup
    /// geometry (drag offset / drawn rect); this owns which modal is open + its content.
    pub(crate) modal: Option<Modal>,
    /// The switcher's chrome view-state: the tree|terminal view border, the tree-column
    /// hint bar (help / status / wrapped flash), and the host screens,
    /// plus their inputs (flash, spinner set + frame, auto-hide/hover cues, view border
    /// colours, ssh-config text, prefix). Owned here beside the modal data and fed by
    /// the app each frame; the switcher's `render` reads it off
    /// `&state`.
    pub(crate) chrome: Chrome,
    /// The toasts on screen and the history behind them, read by the switcher's render
    /// and by the `prefix m` history.
    pub(crate) notify: notify::Notifications,
    /// The login draft for the blocked host whose panel is on screen: the connection
    /// values the user is entering INTO the login pane (the terminal view) and which
    /// element the keys drive. It is NOT a modal - it never routes through the nav input
    /// path - it is a feature of the login pane, driven only while the terminal view
    /// holds a blocked host. `source` pins it to that host so moving to another card
    /// starts a fresh draft. The password moves from here into the process-memory
    /// credential store; it is drawn masked and never logged or serialized.
    pub login: Option<LoginDraft>,
    /// The login that is RUNNING: once the pane is submitted, ssh validates the held
    /// credential on its own thread, and this is the handle that ends it. Present only
    /// while that validation runs, so its presence is what tells
    /// the pane to say a login is under way instead of offering one.
    pub login_run: Option<crate::link::unlock::RunningLogin>,
    /// The steps of each source's last login attempt and where each stands. They outlive
    /// the running handle, because the mux search a working login starts runs after the
    /// verdict, and a failed login keeps the step it stopped at on screen until the
    /// machine is looked at again.
    pub login_progress: HashMap<String, crate::model::LoginProgress>,
    /// The last login attempt number handed out, so each submission is told apart.
    pub login_attempts: u64,
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
    AfterNothing,
    AfterSshConfig,
    AfterPublicKey,
    Submit,
    /// The choice that unfolds the failure's full ssh text and host facts. A stop only
    /// while the pane states a failure.
    Details,
}

/// The login pane's draft: what the user is entering for a host that would not answer
/// with the values ssh resolves on its own. Held on [`State`] (not the modal set)
/// because the pane is a feature of the terminal view, so nothing in the nav path
/// drives it.
///
/// Address and port start at what ssh would use; username comes from an exact host
/// stanza or starts empty. The starting values stay beside the fields so the pane can
/// show where each value came from.
#[derive(Clone, Default)]
pub struct LoginDraft {
    /// The blocked source this draft belongs to; a different current source resets it.
    pub source: String,
    pub address: String,
    pub port: String,
    pub username: String,
    pub password: SecretInput,
    pub after_login: AfterLogin,
    pub focus: LoginFocus,
    /// Whether the failure's full ssh text and host facts are unfolded.
    pub details: bool,
    pub default_address: String,
    pub default_port: String,
    pub default_username: String,
    /// What ssh resolves for the host on its own, which the entered values are compared
    /// with to decide whether recording them would change anything.
    pub resolved: crate::transport::Login,
}

impl std::fmt::Debug for LoginDraft {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginDraft")
            .field("source", &self.source)
            .field("address", &self.address)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("password", &"[redacted]")
            .field("after_login", &self.after_login)
            .field("focus", &self.focus)
            .field("details", &self.details)
            .field("default_address", &self.default_address)
            .field("default_port", &self.default_port)
            .field("default_username", &self.default_username)
            .field("resolved", &self.resolved)
            .finish()
    }
}

impl LoginDraft {
    /// The pane's focus stops in reading order. The ssh config choice appears only while
    /// recording would change something, and the details choice only while the pane
    /// states a failure.
    pub fn stops(&self, details: bool) -> Vec<LoginFocus> {
        let mut v = vec![
            LoginFocus::Address,
            LoginFocus::Port,
            LoginFocus::Username,
            LoginFocus::Password,
            LoginFocus::AfterNothing,
        ];
        if self.offers_ssh_config() {
            v.push(LoginFocus::AfterSshConfig);
        }
        v.push(LoginFocus::AfterPublicKey);
        v.push(LoginFocus::Submit);
        if details {
            v.push(LoginFocus::Details);
        }
        v
    }

    /// The connection values a submit hands to ssh, and the ones recording writes. A
    /// blank field or a port that is not a number names nothing, so ssh resolves it.
    pub fn login(&self) -> crate::transport::Login {
        let named = |v: &str| (!v.trim().is_empty()).then(|| v.trim().to_string());
        crate::transport::Login {
            address: named(&self.address),
            port: self.port.trim().parse::<u16>().ok(),
            user: named(&self.username),
        }
    }

    /// Whether recording the entered values in ssh config would change what ssh uses:
    /// some value the login names differs from what ssh resolves on its own. A host
    /// whose stanza already holds these values, xmux's own included, is not offered a
    /// recording that writes them again. ssh compares host names without case.
    pub fn offers_ssh_config(&self) -> bool {
        let login = self.login();
        let resolved = &self.resolved;
        login.address.is_some_and(|a| {
            !resolved
                .address
                .as_deref()
                .is_some_and(|r| r.eq_ignore_ascii_case(&a))
        }) || login.port.is_some_and(|p| resolved.port != Some(p))
            || login
                .user
                .is_some_and(|u| resolved.user.as_deref() != Some(u.as_str()))
    }

    /// Moves the focus `delta` stops, wrapping.
    fn move_focus(&mut self, delta: isize, details: bool) {
        let stops = self.stops(details);
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
    fn enter(&mut self, details: bool) -> bool {
        if self.focus == LoginFocus::Submit {
            return true;
        }
        self.move_focus(1, details);
        false
    }

    /// What Space does: pick the focused choice, leaving the focus where it is so the
    /// user can see what they picked. A text field takes it as the character it is.
    fn pick(&mut self) {
        match self.focus {
            LoginFocus::AfterNothing => self.after_login = AfterLogin::Nothing,
            LoginFocus::AfterSshConfig => self.after_login = AfterLogin::SshConfig,
            LoginFocus::AfterPublicKey => self.after_login = AfterLogin::RegisterKey,
            LoginFocus::Details => self.details = !self.details,
            _ => {}
        }
    }
}

/// One key the login pane or a host's or source's screen understands, decoded from the
/// terminal's bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Key {
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
pub(crate) fn decode_keys(bytes: &[u8]) -> Vec<Key> {
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
    /// The current session-list update method, including a closed push channel.
    pub(crate) fn refresh_words(&self, source: &str) -> &str {
        let Some(reach) = self.chrome.source_reach.get(source) else {
            return "on request";
        };
        if reach.refresh == "live updates" && !self.live_sources.contains(source) {
            "last observed (channel closed)"
        } else {
            &reach.refresh
        }
    }

    /// True while a modal popup owns the screen. These drive [`ModalKind::Popup`]; the context
    /// menu is separate (pointer-anchored).
    ///
    /// [`ModalKind::Popup`]
    pub fn is_modal_popup_open(&self) -> bool {
        is_popup_open(&self.modal)
    }

    /// The open popup's soft selection: the help tab or the list item under the pointer.
    pub(crate) fn modal_hover(&self) -> Option<usize> {
        match &self.modal {
            Some(
                Modal::Help { hover, .. }
                | Modal::Check { hover, .. }
                | Modal::Palette { hover, .. },
            ) => *hover,
            _ => None,
        }
    }

    /// True while an input popup (filter / jump / new session / logout) is open. The app
    /// routes every key to the switcher then, with no focus-switch hijack.
    pub fn is_inputting(&self) -> bool {
        is_inputting(&self.modal)
    }

    /// Which kind of modal is open - the focus machine derives its modal dimension
    /// from this each loop-top, so [`Focus`] can never mirror-and-desync from the
    /// open popup. A popup and the context menu are mutually exclusive.
    ///
    pub(crate) fn modal_kind(&self) -> Option<ModalKind> {
        modal_kind(&self.modal)
    }

    /// Feeds terminal-view keystrokes into the login pane for `source`.
    ///
    /// The pane is a form: printable characters land in the focused text field, Tab and
    /// the vertical arrows walk the stops, Enter activates the focused one, and Space
    /// picks a choice. Enter on a text field passes the focus on, so filling the pane top
    /// to bottom with Enter alone ends on the button, where Enter submits.
    ///
    /// A draft for a different source is reset first. Address and port start at the
    /// values ssh would have used, while username needs input when no host stanza
    /// supplies it. On submit the password leaves the rendered draft and enters the
    /// process-only credential broker. A failed or replaced login removes that exact
    /// credential.
    pub fn feed_login(&mut self, source: &str, bytes: &[u8]) -> Option<crate::model::Command> {
        let details = self.login_failure(source).is_some();
        let defaults = self.chrome.login_defaults(source);
        let address = defaults.address.value;
        let port = defaults.port.value;
        let username = defaults.username.value;
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
                    resolved: defaults.resolved,
                    ..Default::default()
                });
                self.login.as_mut().unwrap()
            }
        };
        let mut submit = false;
        for key in decode_keys(bytes) {
            match key {
                Key::Tab => draft.move_focus(1, details),
                Key::BackTab | Key::Up => draft.move_focus(-1, details),
                Key::Down => draft.move_focus(1, details),
                Key::Enter => submit |= draft.enter(details),
                Key::Backspace => {
                    draft.field_mut().map(String::pop);
                }
                // Space picks a choice; in a text field it is a character like any other.
                Key::Char(' ') if draft.field_mut().is_none() => draft.pick(),
                Key::Char(c) => {
                    let password = draft.focus == LoginFocus::Password;
                    if let Some(f) = draft.field_mut() {
                        if !password || f.len() + c.len_utf8() <= SECRET_INPUT_CAPACITY {
                            f.push(c);
                        }
                    }
                }
            }
        }
        // A pick the pane no longer shows is not a pick: editing a value back to what ssh
        // resolves hides the ssh config choice, and the follow-up returns to doing nothing.
        if draft.after_login == AfterLogin::SshConfig && !draft.offers_ssh_config() {
            draft.after_login = AfterLogin::Nothing;
        }
        if !submit {
            return None;
        }
        if draft.username.trim().is_empty() {
            draft.focus = LoginFocus::Username;
            return None;
        }
        Some(crate::model::Command::RunLogin {
            source: draft.source.clone(),
            login: draft.login(),
            password: std::mem::take(&mut draft.password),
            after_login: draft.after_login,
        })
    }

    /// Builds the inventory from a complete snapshot: every host is resolved
    /// (reachable or unreachable per its `err`) and every session is present. Other
    /// state fields stay default.
    pub fn from_scan(scan: Scan) -> State {
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
                self.focus.set_view_focus(ViewFocus::Terminal);
                Vec::new()
            }
            Action::Focus(FocusTarget::Nav) => {
                self.focus.set_view_focus(ViewFocus::Nav);
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
    /// which session to reselect). The application update transition calls this
    /// reducer, while the row rebuild and cursor restore stay in the switcher. A
    /// `Failed` op mutates no inventory; its message is returned for a toast.
    ///
    /// [`OpResult`]: crate::model::OpResult
    pub(crate) fn fold_op_result(&mut self, result: OpResult) -> OpFollow {
        match result {
            OpResult::Created { session, .. } => {
                let addr = session.address();
                self.groups = crate::model::add_session(&self.groups, session);
                OpFollow::Reselect(addr)
            }
            OpResult::Failed { message } => OpFollow::Failed(message),
            // The unlock verdict is no inventory mutation: the app reacts to it (re-probe
            // the unlocked machine on success, a toast either way).
            OpResult::Login {
                source,
                login,
                attempt,
                outcome,
            } => {
                // The validation is over however it ended, so the handle that would
                // have ended it goes with it and the pane offers a login again. A result
                // from a replaced attempt leaves the newer handle and steps alone.
                if self
                    .login_run
                    .as_ref()
                    .is_some_and(|run| run.source == source && run.attempt == attempt)
                {
                    self.login_run = None;
                }
                if let Some(progress) = self
                    .login_progress
                    .get_mut(&source)
                    .filter(|p| p.attempt == attempt)
                {
                    progress.finish(&outcome);
                }
                OpFollow::LoginResult {
                    source,
                    login,
                    outcome,
                }
            }
            OpResult::LoginProgress {
                source,
                attempt,
                event,
            } => {
                if let Some(progress) = self
                    .login_progress
                    .get_mut(&source)
                    .filter(|p| p.attempt == attempt)
                {
                    progress.apply(&event);
                }
                OpFollow::Nothing
            }
            // A logout's key steps are no inventory mutation: the application update
            // transition reads them before the switcher sees any result.
            OpResult::HostKeysFound { .. } | OpResult::HostKeysRemoved { .. } => OpFollow::Nothing,
        }
    }

    /// Takes a machine probe's answer into the login steps on that machine. The probe a
    /// working login started settles or advances its mux search. Any other probe is a
    /// newer look at the machine, so steps that already settled describe an older state
    /// and go.
    pub(crate) fn login_probe_answered(&mut self, machine: &str, probe: u64, err: Option<&str>) {
        self.login_progress.retain(|source, progress| {
            if crate::session::machine_of(source) != machine {
                return true;
            }
            if probe != 0 && progress.probe_answered(probe, err) {
                return true;
            }
            progress.running()
        });
    }

    /// Takes the first mux answer after a working login's probe into its mux search. A
    /// search that found a mux has handed the pane to the sessions, so its steps go.
    /// Steps that settled earlier go too when the machine now answers with a mux, since
    /// they no longer describe it.
    pub(crate) fn login_mux_answered(&mut self, machine: &str, answer: &crate::model::MuxAnswer) {
        self.login_progress.retain(|source, progress| {
            if crate::session::machine_of(source) != machine {
                return true;
            }
            let was_running = progress.running();
            progress.found_mux(answer);
            if progress.state_of(crate::model::LoginStep::FindMux)
                == Some(crate::model::StepState::Done)
            {
                return false;
            }
            was_running || *answer != crate::model::MuxAnswer::Found
        });
    }

    /// The failure the login pane for `source` states: the machine's last login when it
    /// failed, else the probe failure that blocked the host. A first-seen key is a
    /// condition the form can answer, not a failed login. `None` when neither failed.
    ///
    /// The login's own categorized ssh reason is stated apart from, and ahead of, later
    /// probe errors, so a submitted login that fails keeps its own verdict.
    pub(crate) fn login_failure(&self, source: &str) -> Option<crate::model::LoginFailure> {
        let machine = crate::session::machine_of(source);
        if let Some(failure) = self
            .login_reports
            .get(machine)
            .and_then(crate::model::LoginFailure::of_login)
        {
            return Some(failure);
        }
        // While a login's steps still run, the probe failure that blocked the host is the
        // question the login is answering, not a failure of its own.
        if self
            .login_progress
            .get(source)
            .is_some_and(crate::model::LoginProgress::running)
        {
            return None;
        }
        self.groups
            .iter()
            .find(|g| g.source == source)
            .and_then(|g| g.err.as_deref())
            .map(crate::model::LoginFailure::of_probe)
            // The probe cannot ask about a first-seen key. The login form can answer
            // that condition, so it is not displayed as a failed login attempt.
            .filter(|failure| {
                failure.kind != Some(crate::link::unlock::FailureKind::HostKeyUnverified)
            })
    }

    /// Flashes a refused key's reason in the tree-column hint bar.
    /// The next tree key clears it (the switcher's `handle_key` clear path), and so does
    /// its own ten-second life, so the normal hint bar returns whether or not the user
    /// presses anything. Delegates to the chrome's flash API.
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
    use crate::model::{Action, Command, FocusTarget, Selection};
    use crate::session::Address;
    use crate::session::Session;
    use crate::state::Focus;
    use std::time::Duration;

    #[test]
    fn login_requires_a_manually_entered_username() {
        let mut state = State {
            login: Some(LoginDraft {
                source: "prod".into(),
                address: "prod.example".into(),
                port: "22".into(),
                focus: LoginFocus::Submit,
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(state.feed_login("prod", b"\r").is_none());
        assert_eq!(state.login.as_ref().unwrap().focus, LoginFocus::Username);
        assert!(state.feed_login("prod", b"alice").is_none());
        state.login.as_mut().unwrap().focus = LoginFocus::Submit;
        let command = state.feed_login("prod", b"\r").expect("login command");
        assert!(
            matches!(command, Command::RunLogin { login, .. } if login.user.as_deref() == Some("alice"))
        );
    }

    #[test]
    fn first_seen_host_key_is_not_a_failed_login() {
        let unknown = crate::transport::diagnostic::explain("Host key verification failed.", false);
        let mut state = State::from_scan(Scan {
            groups: vec![Group {
                source: "prod".into(),
                err: Some(unknown),
                sessions: vec![],
            }],
        });
        assert!(state.login_failure("prod").is_none());

        state.login_reports.insert(
            "prod".into(),
            crate::model::LoginOutcome {
                auth_method: None,
                connect: crate::link::unlock::UnlockOutcome::Failed {
                    kind: crate::link::unlock::FailureKind::HostKeyUnverified,
                    reason: "the host key could not be verified".into(),
                },
                output: "Host key verification failed.".into(),
                saved: None,
                registration: crate::model::RegistrationOutcome::NotRequested,
            },
        );
        assert!(state.login_failure("prod").is_some());
    }

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
        use crate::state::ViewFocus;
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
        // in-place attach or painted-attachment confirmation, folded at the single site.
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

    // --- fold_op_result: State owns the op-result inventory mutation ----------
    // A completed MuxOp's OpResult folds its inventory change (groups) into State;
    // the returned OpFollow tells the switcher only how to rebuild the rows + move
    // the cursor. State owns the domain mutation.

    #[test]
    fn fold_op_result_failed_reports_and_leaves_inventory_untouched() {
        use crate::model::OpResult;
        use crate::state::OpFollow;
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
            matches!(follow, OpFollow::Failed(m) if m == "create failed: boom"),
            "a failure carries its message to the switcher's toast"
        );
    }
}
