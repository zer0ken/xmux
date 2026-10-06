use std::collections::{HashMap, HashSet};

use ratatui::crossterm::event::KeyEvent;
use ratatui::crossterm::event::{KeyCode, KeyModifiers};

use crate::app::input::MouseState;
use crate::model::{Action, Command, EventEffect, Selection};
use crate::ui::switcher::{NavPosition, NavSize, RenderPlan, Switcher};

pub(crate) const NAV_WIDTH_MAX: u16 = 100;

/// The narrowest expanded side nav: a card's indent, a two-digit number with the cells
/// around it, and eight cells of name. Always wider than the padded prefix indicator, so
/// a wide configured prefix raises it. A seam dragged narrower than this collapses the nav.
pub(crate) fn nav_width_min(ui_prefix: &str) -> u16 {
    const CARD_FLOOR: u16 = 14;
    CARD_FLOOR.max(crate::ui::switcher::prefix_chip_width(ui_prefix) + 1)
}

/// The band-layout nav height drag range. A band one row tall still lists its cards
/// along that row, so the min is one row, and a seam dragged past it collapses the band;
/// compute_regions clamps the max down to the body so the terminal always keeps room.
pub(crate) const NAV_HEIGHT_MIN: u16 = 1;
pub(crate) const NAV_HEIGHT_MAX: u16 = 100;

pub(crate) fn adjust_nav_width(w: u16, delta: i32, ui_prefix: &str) -> u16 {
    (w as i32 + delta).clamp(nav_width_min(ui_prefix) as i32, NAV_WIDTH_MAX as i32) as u16
}

pub(crate) struct AppModel {
    pub(crate) state: crate::state::State,
    pub(crate) switcher: Switcher,
    pub(crate) render_plan: RenderPlan,
    pub(crate) nav_width: u16,
    pub(crate) nav_width_natural: u16,
    pub(crate) nav_collapsed: bool,
    pub(crate) nav_height: u16,
    pub(crate) nav_position: NavPosition,
    pub(crate) nav_position_pinned: Option<NavPosition>,
    pub(crate) nav_default: NavPosition,
    pub(crate) max_fps: u16,
    pub(crate) applied_nav_height: u16,
    pub(crate) applied_nav_collapsed: bool,
    pub(crate) auto_hide_nav: bool,
    pub(crate) nav_was_focused: bool,
    pub(crate) mouse_state: MouseState,
    pub(crate) connected: HashSet<String>,
    pub(crate) detecting: HashSet<String>,
    pub(crate) config_last_mtime: Option<std::time::SystemTime>,
    pub(crate) width_dirty: bool,
    pub(crate) width_flush_at: Option<std::time::Instant>,
    /// The re-scan whose summary toast is still owed, held until every host it asked
    /// and, for a full re-scan, the roster have answered.
    pub(crate) rescan: Option<RescanInFlight>,
    /// The logout still taking this PC's key off a machine. One runs at a time: a
    /// second logout is refused while this one is set.
    pub(crate) logout: Option<LogoutRun>,
    /// A cancel handle for every login whose result has not arrived, whichever machine it
    /// is on. The pane's handle names only the latest submission, so a logout of another
    /// machine finds its own login here.
    pub(crate) running_logins: Vec<crate::link::unlock::RunningLogin>,
    /// The recorded logins as the xmux directory last stored them, so the loop writes
    /// the record only when it changes.
    pub(crate) saved_logins: HashMap<String, crate::model::RecordedLogin>,
}

/// A logout that has not yet cleared its machine. The key comes off the machine first, over
/// the connection the login left, and the ssh config entries naming the machine go
/// after, so nothing of the machine's is cleared until both settle. What xmux added goes
/// without asking; a key line or an ssh config entry xmux did not add goes only when one
/// second confirmation, covering both, is answered yes.
#[derive(Debug)]
pub(crate) struct LogoutRun {
    machine: String,
    step: LogoutStep,
}

#[derive(Debug)]
enum LogoutStep {
    /// The machine's key files are being searched.
    Finding,
    /// The ssh config entries xmux did not write are being looked for. Carries what the
    /// key search found.
    FindingEntries(Result<Vec<crate::provision::env::MachineKeyLine>, String>),
    /// The second confirmation asks about the key lines and entries found, some of
    /// them not xmux's. `notes` report a key search that found nothing to remove. It
    /// waits, `shown` false, while another popup the user opened is open, so the
    /// logout's question never takes that popup's place.
    Asking {
        keys: Vec<crate::provision::env::MachineKeyLine>,
        entries: Vec<crate::provision::config::RemovedEntry>,
        notes: Vec<crate::state::notify::Note>,
        shown: bool,
    },
    /// The chosen key lines are being removed. `kept` reports what the answer keeps, and
    /// `unmarked` says whether the entries xmux did not write go next.
    Removing {
        kept: Vec<crate::state::notify::Note>,
        unmarked: bool,
    },
    /// The machine is being removed from the ssh config entries naming it. Carries what
    /// the key steps reported.
    RemovingEntries {
        notes: Vec<crate::state::notify::Note>,
    },
}

/// A re-scan the user asked for that has not reported yet.
#[derive(Debug)]
pub(crate) struct RescanInFlight {
    /// The inventory as it stood when the re-scan was asked for, which the summary
    /// compares against.
    before: crate::state::notify::ScanSnapshot,
    /// Whether the re-scan's roster answer is still out. The roster names the hosts that
    /// came and went, so the summary waits for it as it waits for every host.
    roster: bool,
    /// The machines whose held password ssh refused during this re-scan. Their cards keep
    /// no failure of their own, so the summary is told here.
    locked: HashSet<String>,
    /// The one machine a `prefix r` re-scan asked, or `None` for a full re-scan. The
    /// summary of a one-machine re-scan compares that machine's hosts alone.
    machine: Option<String>,
    /// A full re-scan can use an already running one-machine probe instead of asking
    /// that machine twice. The runtime consumes this when it starts discovery.
    skip_machine: Option<String>,
}

impl AppModel {
    pub(crate) fn take_rescan_skip_machine(&mut self) -> Option<String> {
        self.rescan.as_mut().and_then(|r| r.skip_machine.take())
    }

    #[cfg(test)]
    pub(crate) fn from_hosts(hosts: Vec<String>) -> Self {
        let mut state = crate::state::State::from_hosts(hosts);
        let switcher = Switcher::from_hosts(&mut state);
        Self {
            state,
            switcher,
            render_plan: RenderPlan::default(),
            nav_width: crate::ui::switcher::NAV_WIDTH,
            nav_width_natural: crate::ui::switcher::NAV_WIDTH,
            nav_collapsed: false,
            nav_height: 0,
            nav_position: NavPosition::Left,
            nav_position_pinned: None,
            nav_default: NavPosition::Left,
            max_fps: crate::provision::config::DEFAULT_MAX_FPS,
            applied_nav_height: u16::MAX,
            applied_nav_collapsed: true,
            auto_hide_nav: false,
            nav_was_focused: true,
            mouse_state: MouseState::default(),
            connected: HashSet::new(),
            detecting: HashSet::new(),
            config_last_mtime: None,
            width_dirty: false,
            width_flush_at: None,
            rescan: None,
            logout: None,
            running_logins: Vec::new(),
            saved_logins: HashMap::new(),
        }
    }

    pub(crate) fn nav_size(&self) -> NavSize {
        NavSize {
            natural: self.nav_width_natural,
            width: self.nav_width,
            height: self.nav_height,
            position: self.nav_position,
            collapsed: self.nav_collapsed,
        }
    }

    #[cfg(test)]
    fn layout_for_test(&self, area: ratatui::layout::Rect) -> RenderPlan {
        self.switcher
            .layout(area, self.nav_size(), &self.state, &self.render_plan)
    }
}

pub(crate) enum Msg {
    Action(Action),
    #[cfg(test)]
    Commands(Vec<Command>),
    SyncSelection,
    /// A read that carried keys arrived. Any key ends the hint after a selection move; a
    /// key that moves the selection again raises a new one as it is applied.
    KeysRead,
    Key(KeyEvent),
    /// A click on a nav target. `execute` opens the target's screen and gives the terminal
    /// view the focus, as Enter does; a band's overflow count only selects the card it
    /// stands for.
    MouseSelect {
        col: u16,
        row: u16,
        execute: bool,
    },
    /// The pointer resting at a cell: the soft selection follows it.
    Hover {
        col: u16,
        row: u16,
    },
    /// An arrow key on a machine's or a host's screen while the terminal view holds the
    /// focus: the hard-selected link moves by this many.
    StepLink(isize),
    /// Opens a link of the shown screen: the one clicked, or the hard-selected one.
    OpenLink(Option<usize>),
    MouseScroll {
        down: bool,
    },
    ToggleHelp,
    ToggleHistory,
    ToggleCheck,
    TogglePalette,
    DismissToast(u64),
    /// A key read while the help or the history is open, with the configured prefix byte
    /// so the prefix keys that open them can close them.
    ReaderBytes {
        bytes: Vec<u8>,
        prefix: u8,
    },
    OpResult {
        result: crate::ui::switcher::OpResult,
        logged_in: HashSet<String>,
    },
    LoginSettled {
        host: String,
        credential_held: bool,
        machine_has_hosts: bool,
        /// The machine probe the runtime starts for this login, whose answer alone
        /// settles the login's mux search.
        probe: u64,
    },
    CredentialInventory {
        held: HashSet<String>,
    },
    DisplayAuth {
        host: String,
        method: Option<crate::model::AuthMethod>,
    },
    ApplyInventory {
        host: String,
        sessions: Vec<crate::session::Session>,
        live: bool,
    },
    AddHost {
        host: String,
        scanning: bool,
    },
    /// Hosts one machine answered it serves, added together so the card the machine
    /// stood on gives way to all of them in one rebuild.
    AddHosts {
        hosts: Vec<String>,
    },
    RemoveHost {
        host: String,
        clear_tracking: bool,
    },
    /// A machine the roster now names.
    AddMachine {
        machine: String,
    },
    /// A machine the roster no longer names.
    RemoveMachine {
        machine: String,
    },
    /// What a machine answered as a whole while no host of it is known: why it could
    /// not be asked.
    ApplyMachineResult {
        machine: String,
        err: Option<String>,
    },
    /// A machine answered that it serves no mux xmux supports.
    SettleMuxless {
        machine: String,
    },
    /// The re-scan's roster answer has been reconciled into the registries and the nav.
    RescanRosterApplied,
    /// The launch roster has been reconciled into the registries and the nav.
    LaunchRosterApplied,
    DetectionFinished {
        host: String,
    },
    SetHostReach(HashMap<String, crate::state::HostReach>),
    SetRosterFacts {
        providers: HashMap<String, String>,
        login_defaults: HashMap<String, crate::provision::env::LoginDefaults>,
        ssh_stanzas: HashMap<String, String>,
        held_credentials: HashSet<String>,
        host_reach: HashMap<String, crate::state::HostReach>,
    },
    HostEvent {
        event: crate::link::HostEvent,
        logged_in: HashSet<String>,
    },
    Focus(crate::model::FocusTarget),
    FeedLogin {
        host: String,
        bytes: Vec<u8>,
    },
    SetMouseNavArmed(bool),
    SetMouseDragging(bool),
    EndNavDrag {
        band: bool,
    },
    SetMouseHovered(bool),
    SetResizeRepeat(Option<std::time::Instant>),
    /// The button-up that ends a popup drag: a release on the grabbed cell is a click.
    EndPopupDrag,
    /// Ends a popup drag whose button-up was lost, as no click.
    AbandonPopupDrag,
    /// Idle pointer motion over an open popup: sets its soft selection.
    HoverPopup {
        col: u16,
        row: u16,
    },
    DragPopup {
        col: u16,
        row: u16,
    },
    BeginPopupDrag {
        col: u16,
        row: u16,
    },
    ToggleNavCollapsed,
    SetNavCollapsed(bool),
    SetNavNaturalWidth(u16),
    SetNavHeight(u16),
    ResizeNav {
        horizontal: bool,
        delta: i32,
        body_rows: u16,
        ui_prefix: String,
    },
    CycleNavPosition,
    CancelRunningLogin,
    PersistNavSize,
    SyncFrame {
        spinner_frame: usize,
        animation_ms: u64,
        view_border_hovered: bool,
        prefix_active: bool,
    },
    ReconcileNav {
        width: u16,
        position: NavPosition,
    },
    ConsumeReattach {
        now: std::time::Instant,
    },
    MarkWidthDirty {
        flush_at: std::time::Instant,
    },
    FlushWidth {
        now: std::time::Instant,
        force: bool,
    },
    SetRenderPlan(RenderPlan),
    FollowDisplay(crate::session::Address),
    Tick {
        now: std::time::Instant,
        spinner: HashSet<String>,
    },
    ConfigObserved {
        mtime: Option<std::time::SystemTime>,
        ui: Option<
            Box<(
                crate::provision::config::UiConfig,
                crate::ui::palette::Palette,
            )>,
        >,
    },
    ConfigError(String),
    Notice(String),
    DetectionStarted(String),
    Shutdown,
}

pub(crate) enum Effect {
    Command(Command),
    Event(EventEffect),
    EventBatch(Vec<EventEffect>),
    LoginApplied {
        host: String,
        login: crate::transport::Login,
    },
    StartLogin {
        host: String,
        login: crate::transport::Login,
        password: crate::model::SecretInput,
        after_login: crate::model::AfterLogin,
        /// The submission this run is, carried on every report it sends back.
        attempt: u64,
        cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    },
    PersistNavWidth(u16),
    PersistNavHeight(u16),
    PersistNavCollapsed(bool),
    PersistNavPosition(Option<NavPosition>),
    PersistFirstKeyHelpSeen,
    PersistSshLogins(HashMap<String, crate::model::RecordedLogin>),
    ReattachDisplay(Selection),
    /// Searches the machine's key files for this machine's public keys, ending first the
    /// login the logout cancelled.
    FindMachineKeys {
        machine: String,
        cancel_login: Vec<crate::link::unlock::RunningLogin>,
    },
    RemoveMachineKeys {
        machine: String,
        lines: Vec<crate::provision::env::MachineKeyLine>,
    },
    FindSshConfigEntries {
        machine: String,
    },
    RemoveSshConfigEntries {
        machine: String,
        unmarked: bool,
    },
    /// Reads what ssh config says about the machine as the file is now, for the login
    /// pane and the machine screen.
    ReadSshFacts {
        machine: String,
    },
    LogoutMachine {
        machine: String,
        cancel_login: Vec<crate::link::unlock::RunningLogin>,
    },
    CancelLogin(crate::link::unlock::RunningLogin),
}

impl std::fmt::Debug for Effect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Command(command) => f.debug_tuple("Command").field(command).finish(),
            Self::Event(effect) => f.debug_tuple("Event").field(effect).finish(),
            Self::EventBatch(effects) => f.debug_tuple("EventBatch").field(effects).finish(),
            Self::LoginApplied { host, login } => f
                .debug_struct("LoginApplied")
                .field("host", host)
                .field("login", login)
                .finish(),
            Self::StartLogin {
                host,
                login,
                after_login,
                ..
            } => f
                .debug_struct("StartLogin")
                .field("host", host)
                .field("login", login)
                .field("password", &"[redacted]")
                .field("after_login", after_login)
                .finish(),
            Self::PersistNavWidth(width) => f.debug_tuple("PersistNavWidth").field(width).finish(),
            Self::PersistNavHeight(height) => {
                f.debug_tuple("PersistNavHeight").field(height).finish()
            }
            Self::PersistNavCollapsed(collapsed) => f
                .debug_tuple("PersistNavCollapsed")
                .field(collapsed)
                .finish(),
            Self::PersistNavPosition(position) => {
                f.debug_tuple("PersistNavPosition").field(position).finish()
            }
            Self::PersistFirstKeyHelpSeen => f.write_str("PersistFirstKeyHelpSeen"),
            Self::PersistSshLogins(logins) => {
                f.debug_tuple("PersistSshLogins").field(logins).finish()
            }
            Self::ReattachDisplay(selection) => {
                f.debug_tuple("ReattachDisplay").field(selection).finish()
            }
            Self::FindMachineKeys { machine, .. } => {
                f.debug_tuple("FindMachineKeys").field(machine).finish()
            }
            Self::RemoveMachineKeys { machine, lines } => f
                .debug_struct("RemoveMachineKeys")
                .field("machine", machine)
                .field("lines", &lines.len())
                .finish(),
            Self::FindSshConfigEntries { machine } => f
                .debug_tuple("FindSshConfigEntries")
                .field(machine)
                .finish(),
            Self::RemoveSshConfigEntries { machine, unmarked } => f
                .debug_struct("RemoveSshConfigEntries")
                .field("machine", machine)
                .field("unmarked", unmarked)
                .finish(),
            Self::ReadSshFacts { machine } => f.debug_tuple("ReadSshFacts").field(machine).finish(),
            Self::LogoutMachine { machine, .. } => {
                f.debug_tuple("LogoutMachine").field(machine).finish()
            }
            Self::CancelLogin(_) => f.write_str("CancelLogin"),
        }
    }
}

fn command_effect(model: &mut AppModel, command: Command) -> Option<Effect> {
    match command {
        Command::SelectAddress(address) => {
            model.switcher.select_address(&address);
            None
        }
        Command::Rescan => {
            model.state.invalid_auth.clear();
            // A re-scan asked for while one is still running keeps the first snapshot, so
            // its summary compares against what the user saw before any of them. Each one
            // re-resolves the roster, so the summary waits for that answer again.
            // A one-machine re-scan in flight gives way to the full one, whose summary
            // covers that machine too.
            let skip_machine = model.rescan.as_ref().and_then(|r| r.machine.clone());
            match model.rescan.as_mut() {
                Some(rescan) if rescan.machine.is_none() => rescan.roster = true,
                _ => {
                    model.rescan = Some(RescanInFlight {
                        before: crate::state::notify::ScanSnapshot::of(
                            &model.state,
                            &HashSet::new(),
                        ),
                        roster: true,
                        locked: HashSet::new(),
                        machine: None,
                        skip_machine,
                    })
                }
            }
            model.switcher.request_rescan(&mut model.state);
            // The roster answer can add hosts after every listed host has answered, so
            // the numbers stay open until it is in.
            model.switcher.hold_numbers(true, &model.state);
            let armed = model.switcher.take_rescan_kick();
            debug_assert!(armed);
            Some(Effect::Command(Command::Rescan))
        }
        Command::RescanMachine(machine) => {
            // One re-scan reports at a time: a second one waits for the first one's
            // summary, so each summary says what its own re-scan found.
            if model.rescan.is_some() {
                model.state.refuse(
                    format!("rescan machine {machine}"),
                    "a re-scan is still running",
                );
                return None;
            }
            model.state.invalid_auth.remove(&machine);
            model.rescan = Some(RescanInFlight {
                before: crate::state::notify::ScanSnapshot::of(&model.state, &HashSet::new())
                    .only_machine(&machine),
                roster: false,
                locked: HashSet::new(),
                machine: Some(machine.clone()),
                skip_machine: None,
            });
            model
                .switcher
                .mark_machine_scanning(&machine, &mut model.state);
            Some(Effect::Command(Command::RescanMachine(machine)))
        }
        Command::Logout(machine) => {
            if model.logout.is_some() {
                model
                    .state
                    .refuse(format!("logout {machine}"), "a logout is still running");
                return None;
            }
            // The confirm popup or the result toast is what the logout says next.
            let cancel_login = take_login_of(model, &machine);
            model.logout = Some(LogoutRun {
                machine: machine.clone(),
                step: LogoutStep::Finding,
            });
            Some(Effect::FindMachineKeys {
                machine,
                cancel_login,
            })
        }
        Command::RemoveUnmarked(machine) => {
            let run = model.logout.as_mut().filter(|run| {
                run.machine == machine && matches!(run.step, LogoutStep::Asking { shown: true, .. })
            })?;
            let LogoutStep::Asking { keys, notes, .. } =
                std::mem::replace(&mut run.step, LogoutStep::Finding)
            else {
                return None;
            };
            remove_logout_keys(model, keys, notes, Vec::new(), true)
                .into_iter()
                .next()
        }
        Command::Attach(selection)
            if model
                .state
                .invalid_auth
                .contains(crate::session::machine_of(&selection.host)) =>
        {
            None
        }
        Command::AdjustNavWidth(delta) => {
            let min = nav_width_min(&model.state.chrome.ui_prefix) as i32;
            let next =
                (model.nav_width_natural as i32 + delta).clamp(min, NAV_WIDTH_MAX as i32) as u16;
            if next == model.nav_width_natural {
                None
            } else {
                model.nav_width_natural = next;
                Some(Effect::Command(Command::AdjustNavWidth(delta)))
            }
        }
        Command::ToggleAutoHide => {
            model.auto_hide_nav = !model.auto_hide_nav;
            Some(Effect::Command(Command::ToggleAutoHide))
        }
        Command::Quit => Some(Effect::Command(Command::Quit)),
        Command::RunLogin {
            host,
            login,
            password,
            after_login,
        } => {
            let machine = crate::session::machine_of(&host);
            // A login's registration could put the key back right after the logout took
            // it off, so a machine being logged out takes no login until that ends.
            if model
                .logout
                .as_ref()
                .is_some_and(|run| run.machine == machine)
            {
                model
                    .state
                    .refuse(machine, format!("a logout of {machine} is running"));
                return None;
            }
            model.state.logged_in.remove(machine);
            model.state.login_reports.remove(machine);
            // The key report describes the latest login's follow-up, so a login that does
            // not register clears what an earlier one reported.
            model.state.registration_reports.remove(machine);
            model.state.login_attempts += 1;
            let attempt = model.state.login_attempts;
            model.state.login_progress.insert(
                host.clone(),
                crate::model::LoginProgress::start(
                    attempt,
                    &login,
                    !password.is_empty(),
                    after_login == crate::model::AfterLogin::SshConfig,
                    after_login == crate::model::AfterLogin::RegisterKey,
                ),
            );
            let (running, cancel) =
                crate::link::unlock::RunningLogin::pending(host.clone(), attempt);
            model.running_logins.push(running.clone());
            model.state.login_run = Some(running);
            Some(Effect::StartLogin {
                host,
                login,
                password,
                after_login,
                attempt,
                cancel,
            })
        }
        command => Some(Effect::Command(command)),
    }
}

fn sync_selection(model: &mut AppModel) {
    let target = model.switcher.terminal_view_target();
    let selection = Selection {
        host: target.host,
        session: target.target,
    };
    if selection != model.state.selection {
        model.state.apply(Action::Select(selection));
    }
}

/// Takes every running login on `machine`, whose results a logout must not let reopen it.
fn take_login_of(model: &mut AppModel, machine: &str) -> Vec<crate::link::unlock::RunningLogin> {
    let on_machine =
        |run: &crate::link::unlock::RunningLogin| crate::session::machine_of(&run.host) == machine;
    model
        .state
        .login_progress
        .retain(|host, _| crate::session::machine_of(host) != machine);
    if model.state.login_run.as_ref().is_some_and(on_machine) {
        model.state.login_run = None;
    }
    let (taken, kept) = std::mem::take(&mut model.running_logins)
        .into_iter()
        .partition(|run| on_machine(run));
    model.running_logins = kept;
    taken
}

/// Reads what the logout's search of the machine's key files found, then looks for the ssh
/// config entries naming the machine that xmux did not write, so one confirmation can ask
/// about both.
fn logout_keys_found(
    model: &mut AppModel,
    machine: String,
    result: Result<Vec<crate::provision::env::MachineKeyLine>, String>,
) -> Vec<Effect> {
    let Some(run) = model
        .logout
        .as_mut()
        .filter(|run| run.machine == machine && matches!(run.step, LogoutStep::Finding))
    else {
        return Vec::new();
    };
    run.step = LogoutStep::FindingEntries(result);
    vec![Effect::FindSshConfigEntries { machine }]
}

/// Reads which ssh config entries xmux did not write name the machine, and decides with
/// the key search whether to ask. Lines and entries xmux added go at once; a key line or
/// an entry it did not add asks first, because the user may reach the machine through it
/// from outside xmux. A key search that failed leaves the key where it is, and an ssh
/// config that cannot be read is reported by the removal step, which reads it again.
fn logout_entries_found(
    model: &mut AppModel,
    machine: String,
    result: Result<Vec<crate::provision::config::RemovedEntry>, String>,
) -> Vec<Effect> {
    let Some(run) = model
        .logout
        .as_mut()
        .filter(|run| run.machine == machine && matches!(run.step, LogoutStep::FindingEntries(_)))
    else {
        return Vec::new();
    };
    let LogoutStep::FindingEntries(keys) = std::mem::replace(&mut run.step, LogoutStep::Finding)
    else {
        return Vec::new();
    };
    let entries = result.unwrap_or_default();
    let (keys, notes) = match keys {
        Err(reason) => (
            Vec::new(),
            vec![crate::state::notify::Note::new(
                crate::state::notify::Level::Warning,
                format!("this PC's key was not removed from {machine}: {reason}"),
            )],
        ),
        Ok(found) if found.is_empty() => (
            Vec::new(),
            vec![crate::state::notify::Note::new(
                crate::state::notify::Level::Info,
                format!("{machine} holds no key of this PC"),
            )],
        ),
        Ok(found) => (found, Vec::new()),
    };
    if entries.is_empty() && keys.iter().all(|line| line.marked) {
        return remove_logout_keys(model, keys, notes, Vec::new(), false);
    }
    if let Some(run) = model.logout.as_mut() {
        run.step = LogoutStep::Asking {
            keys,
            entries,
            notes,
            shown: false,
        };
    }
    show_logout_question(model);
    Vec::new()
}

/// Opens the logout's second confirmation once no other popup is open. Read on every
/// update, so a question that waited behind a popup the user opened appears when that
/// popup closes.
fn show_logout_question(model: &mut AppModel) {
    if model.state.modal.is_some() {
        return;
    }
    let Some(run) = model.logout.as_mut() else {
        return;
    };
    let LogoutStep::Asking {
        keys,
        entries,
        shown,
        ..
    } = &mut run.step
    else {
        return;
    };
    if *shown {
        return;
    }
    *shown = true;
    let unmarked: Vec<&str> = keys
        .iter()
        .filter(|line| !line.marked)
        .map(|line| line.file.label())
        .collect();
    let marked = keys.iter().filter(|line| line.marked).count();
    model
        .switcher
        .open_logout_keys(&run.machine, &unmarked, marked, entries, &mut model.state);
}

/// Removes `keys` and then the ssh config entries, or goes straight to the entries when
/// no key line goes. `notes` report the key search, `kept` what the answer keeps, and
/// `unmarked` whether the entries xmux did not write go too.
fn remove_logout_keys(
    model: &mut AppModel,
    keys: Vec<crate::provision::env::MachineKeyLine>,
    notes: Vec<crate::state::notify::Note>,
    kept: Vec<crate::state::notify::Note>,
    unmarked: bool,
) -> Vec<Effect> {
    let Some(run) = model.logout.as_mut() else {
        return Vec::new();
    };
    if keys.is_empty() {
        let mut notes = notes;
        notes.extend(kept);
        return remove_logout_stanza(model, notes, unmarked);
    }
    run.step = LogoutStep::Removing { kept, unmarked };
    vec![Effect::RemoveMachineKeys {
        machine: run.machine.clone(),
        lines: keys,
    }]
}

/// Settles a second confirmation that closed without confirming: the key lines and the
/// ssh config entries xmux did not add stay, and only the ones it added go. Read on every
/// update, so a confirmation that closed any way at all, an Esc or another screen taking
/// its place, answers the logout.
fn settle_logout_choice(model: &mut AppModel) -> Vec<Effect> {
    let asking = matches!(
        model.state.modal.as_ref(),
        Some(crate::state::Modal::Input(input))
            if input.mode == crate::state::InputMode::LogoutKeys
    );
    let Some(run) = model
        .logout
        .as_mut()
        .filter(|run| !asking && matches!(run.step, LogoutStep::Asking { shown: true, .. }))
    else {
        return Vec::new();
    };
    let LogoutStep::Asking {
        keys,
        entries,
        notes,
        ..
    } = std::mem::replace(&mut run.step, LogoutStep::Finding)
    else {
        return Vec::new();
    };
    let machine = run.machine.clone();
    let mut kept = Vec::new();
    if keys.iter().any(|line| !line.marked) {
        kept.push(crate::state::notify::Note::new(
            crate::state::notify::Level::Info,
            format!("this PC's key that xmux did not add stays on {machine}"),
        ));
    }
    if !entries.is_empty() {
        let headers: Vec<&str> = entries.iter().map(|entry| entry.header.as_str()).collect();
        kept.push(crate::state::notify::Note::new(
            crate::state::notify::Level::Info,
            format!(
                "ssh config entries xmux did not add stay: {}",
                headers.join("; ")
            ),
        ));
    }
    let marked: Vec<_> = keys.into_iter().filter(|line| line.marked).collect();
    remove_logout_keys(model, marked, notes, kept, false)
}

/// Reads what removing the chosen key lines did, then goes on to the ssh config either
/// way.
fn logout_keys_removed(
    model: &mut AppModel,
    machine: String,
    result: Result<(), String>,
) -> Vec<Effect> {
    let Some(LogoutStep::Removing { kept, unmarked }) = model
        .logout
        .as_mut()
        .filter(|run| run.machine == machine)
        .map(|run| &mut run.step)
    else {
        return Vec::new();
    };
    let unmarked = *unmarked;
    let kept = std::mem::take(kept);
    let mut notes = vec![match result {
        Ok(()) => crate::state::notify::Note::new(
            crate::state::notify::Level::Success,
            format!("this PC's key removed from {machine}"),
        ),
        Err(reason) => crate::state::notify::Note::new(
            crate::state::notify::Level::Warning,
            format!("this PC's key remains on {machine}: {reason}"),
        ),
    }];
    notes.extend(kept);
    remove_logout_stanza(model, notes, unmarked)
}

/// Goes on to the ssh config step once the key steps settled. The entries go after the
/// key, because the key steps reach the machine through the values they hold. `notes`
/// report what happened to the key, and `unmarked` says whether the entries xmux did not
/// write go too.
fn remove_logout_stanza(
    model: &mut AppModel,
    notes: Vec<crate::state::notify::Note>,
    unmarked: bool,
) -> Vec<Effect> {
    let Some(run) = model.logout.as_mut() else {
        return Vec::new();
    };
    run.step = LogoutStep::RemovingEntries { notes };
    vec![Effect::RemoveSshConfigEntries {
        machine: run.machine.clone(),
        unmarked,
    }]
}

/// Reads what removing the machine from ssh config did, then finishes the logout either
/// way. The toast names every entry that changed; a machine no entry named adds nothing.
fn logout_stanza_removed(
    model: &mut AppModel,
    machine: String,
    result: Result<Vec<crate::provision::config::RemovedEntry>, String>,
) -> Vec<Effect> {
    let Some(LogoutStep::RemovingEntries { notes }) = model
        .logout
        .as_mut()
        .filter(|run| run.machine == machine)
        .map(|run| &mut run.step)
    else {
        return Vec::new();
    };
    let mut notes = std::mem::take(notes);
    // An entry that went changes what ssh config says about the machine, so the facts
    // the login pane and the machine screen show are read again.
    let edited = result.as_ref().is_ok_and(|removed| !removed.is_empty());
    match result {
        Ok(removed) if removed.is_empty() => {}
        Ok(removed) => {
            let entries: Vec<String> = removed
                .iter()
                .map(|entry| match entry.after {
                    None => format!("removed {}", entry.header),
                    Some(_) => format!("removed {machine} from {}", entry.header),
                })
                .collect();
            notes.push(crate::state::notify::Note::new(
                crate::state::notify::Level::Success,
                format!("ssh config: {}", entries.join("; ")),
            ))
        }
        Err(reason) => notes.push(crate::state::notify::Note::new(
            crate::state::notify::Level::Warning,
            format!("ssh config entries for {machine} remain: {reason}"),
        )),
    }
    let mut effects = finish_logout(model, notes);
    if edited {
        effects.push(Effect::ReadSshFacts { machine });
    }
    effects
}

/// Clears the logged-out machine once its key and ssh config steps settled: the held
/// password, the login state, and every card on it, then the connections through the
/// effect. `notes` report what happened to the key and the ssh config entries.
fn finish_logout(model: &mut AppModel, notes: Vec<crate::state::notify::Note>) -> Vec<Effect> {
    let Some(LogoutRun { machine, .. }) = model.logout.take() else {
        return Vec::new();
    };
    let cancel_login = take_login_of(model, &machine);
    model.state.forget_auth(&machine);
    model.state.invalid_auth.insert(machine.clone());
    model.state.logged_in.remove(&machine);
    model.state.registration_reports.remove(&machine);
    model
        .state
        .live_hosts
        .retain(|host| crate::session::machine_of(host) != machine);
    model
        .connected
        .retain(|host| crate::session::machine_of(host) != machine);
    if crate::session::machine_of(&model.state.displayed.host) == machine {
        model.state.displayed = Default::default();
        model.state.attach_deadline = None;
        model.state.attach_pending = false;
    }
    let hosts: Vec<_> = model
        .state
        .groups
        .iter()
        .filter(|group| crate::session::machine_of(&group.host) == machine)
        .map(|group| group.host.clone())
        .collect();
    for host in hosts {
        model.switcher.apply_host_result(
            host,
            Vec::new(),
            Some(crate::model::LOGGED_OUT.into()),
            &mut model.state,
        );
    }
    if model.state.stands_alone(&machine) {
        model.switcher.apply_machine_result(
            &machine,
            Some(crate::model::LOGGED_OUT.into()),
            &mut model.state,
        );
    }
    model.state.notify.toast(format!("logout {machine}"), notes);
    vec![Effect::LogoutMachine {
        machine,
        cancel_login,
    }]
}

fn host_event_effects(model: &mut AppModel, event: crate::link::HostEvent) -> Vec<EventEffect> {
    use crate::link::HostEvent;

    match event {
        HostEvent::AuthObserved {
            machine,
            method,
            connection,
            ..
        } => {
            if !model.state.invalid_auth.contains(&machine) {
                model.state.note_auth(machine, method, connection);
            }
            Vec::new()
        }
        HostEvent::SharedConnectionSeen {
            machine,
            identity,
            at,
        } => {
            model.state.see_shared_connection(machine, identity, at);
            Vec::new()
        }
        HostEvent::Connected { host, .. } | HostEvent::Inventory { host, .. }
            if model
                .state
                .invalid_auth
                .contains(crate::session::machine_of(&host)) =>
        {
            Vec::new()
        }
        HostEvent::Connected { host, sessions } => vec![
            EventEffect::MarkConnected { host: host.clone() },
            EventEffect::CheckSharedConnection {
                machine: crate::session::machine_of(&host).to_owned(),
            },
            EventEffect::ApplyInventory { host, sessions },
        ],
        HostEvent::Inventory { host, sessions } => vec![
            EventEffect::MarkConnected { host: host.clone() },
            EventEffect::ApplyInventory { host, sessions },
        ],
        HostEvent::Changed { host }
        | HostEvent::ClientDetached { host, .. }
        | HostEvent::ClientSessionChanged { host, .. }
        | HostEvent::DisplayTty { host, .. }
            if model
                .state
                .invalid_auth
                .contains(crate::session::machine_of(&host)) =>
        {
            Vec::new()
        }
        HostEvent::Changed { host } => vec![EventEffect::Refetch { host }],
        // The server detached a client that was answering (tmux does this when the
        // client's attached session is destroyed): the host still serves its other
        // sessions, so its card stands as the mux last reported it and the channel is
        // opened once more. The connected mark is cleared here, so only a reopened channel
        // that lists sessions again can make a later exit a detach: a reopen that fails
        // takes the ordinary exit path below and never opens a third channel.
        HostEvent::Exited { host, .. }
            if model
                .state
                .invalid_auth
                .contains(crate::session::machine_of(&host)) =>
        {
            vec![EventEffect::ReapHost { host }]
        }
        HostEvent::Exited {
            host,
            detached: true,
            ..
        } if model.connected.remove(&host) => {
            model.state.live_hosts.remove(&host);
            vec![
                EventEffect::ReapHost { host: host.clone() },
                EventEffect::ReopenHost { host },
            ]
        }
        HostEvent::Exited { host, reason, .. } => {
            model.state.live_hosts.remove(&host);
            vec![
                EventEffect::NoteHostExited {
                    host: host.clone(),
                    reason,
                },
                EventEffect::ReapHost { host },
            ]
        }
        HostEvent::ClientDetached { host, client } => {
            vec![EventEffect::ReapDisplayAttach { host, client }]
        }
        HostEvent::ClientSessionChanged {
            host,
            client,
            session,
        } => vec![EventEffect::FollowDisplaySession {
            host,
            client,
            session,
        }],
        HostEvent::DisplayTty { host, tty } => {
            vec![EventEffect::RecordDisplayTty { host, tty }]
        }
        HostEvent::MuxesFound { machine, .. } if model.state.invalid_auth.contains(&machine) => {
            Vec::new()
        }
        HostEvent::MuxesFound { machine, muxes } => {
            // Discovery that found nothing ends a login's mux search here: no host
            // result follows it, since the machine's card goes instead.
            match &muxes {
                Ok(found) if found.is_empty() => model
                    .state
                    .login_mux_answered(&machine, &crate::model::MuxAnswer::NoMux),
                Err(reason) => model
                    .state
                    .login_mux_answered(&machine, &crate::model::MuxAnswer::Failed(reason.clone())),
                Ok(_) => {}
            }
            vec![EventEffect::AddDiscoveredHosts { machine, muxes }]
        }
        HostEvent::RosterResolved { roster, rescan } => vec![EventEffect::ApplyRoster {
            roster,
            startup: None,
            rescan,
        }],
        HostEvent::RosterKept => {
            settle_rescan_roster(model);
            Vec::new()
        }
        HostEvent::StartupResolved {
            roster,
            own_session,
            force_askpass,
        } => vec![EventEffect::ApplyRoster {
            roster,
            startup: Some(crate::model::StartupFacts {
                own_session,
                force_askpass,
            }),
            rescan: false,
        }],
        HostEvent::Scanned { host, .. }
            if model
                .state
                .invalid_auth
                .contains(crate::session::machine_of(&host)) =>
        {
            Vec::new()
        }
        HostEvent::Scanned {
            host,
            detected,
            err,
        } => {
            // Detection that found no mux ends a login's mux search whether or not a
            // host result follows: a host no longer scanning gets none.
            if detected.is_none() {
                let answer = match &err {
                    Some(reason) => crate::model::MuxAnswer::Failed(reason.clone()),
                    None => crate::model::MuxAnswer::NoMux,
                };
                model
                    .state
                    .login_mux_answered(crate::session::machine_of(&host), &answer);
            }
            if detected.is_none() && model.state.scanning.contains(&host) {
                let reason = err.clone().unwrap_or_else(|| "mux not detected".to_owned());
                return vec![
                    EventEffect::ApplyHostResult {
                        host: host.clone(),
                        sessions: Vec::new(),
                        err: Some(reason),
                    },
                    EventEffect::DispatchScanned {
                        host,
                        detected,
                        err,
                    },
                ];
            }
            vec![EventEffect::DispatchScanned {
                host,
                detected,
                err,
            }]
        }
        HostEvent::MachineProbed {
            machine,
            err,
            shell,
            password_supplied,
            credential_rejection_generation,
            credential_held,
            credential_generation,
            current_credential_generation,
            rescan,
            probe,
        } => {
            // The login steps take every machine answer, including one the generation
            // check below sets aside: the probe a login started answers that login.
            model
                .state
                .login_probe_answered(&machine, probe, err.as_deref());
            let result_generation =
                credential_rejection_generation.unwrap_or(credential_generation);
            if result_generation != current_credential_generation {
                return Vec::new();
            }
            if model.state.invalid_auth.contains(&machine) {
                return Vec::new();
            }
            match err {
                Some(reason) => {
                    let auth_refused = crate::transport::diagnostic::contains_auth_refusal(&reason);
                    let known_auth = model.state.auth_methods.contains_key(&machine)
                        || model
                            .state
                            .display_auth_methods
                            .keys()
                            .any(|host| crate::session::machine_of(host) == machine);
                    let disconnect = if auth_refused && known_auth {
                        model.state.forget_auth(&machine);
                        model.state.invalid_auth.insert(machine.clone());
                        model
                            .state
                            .live_hosts
                            .retain(|host| crate::session::machine_of(host) != machine);
                        model
                            .connected
                            .retain(|host| crate::session::machine_of(host) != machine);
                        if crate::session::machine_of(&model.state.displayed.host) == machine {
                            model.state.displayed = Default::default();
                            model.state.attach_deadline = None;
                            model.state.attach_pending = false;
                        }
                        vec![EventEffect::DisconnectMachine {
                            machine: machine.clone(),
                        }]
                    } else {
                        Vec::new()
                    };
                    // Only a refusal of the held password leaves the cards to the login
                    // pane; a refusal that never received it stays visible on them.
                    if credential_held && password_supplied && auth_refused {
                        model
                            .state
                            .scanning
                            .retain(|host| crate::session::machine_of(host) != machine);
                        model.state.machine_scanning.remove(&machine);
                        if let Some(rescan) = model.rescan.as_mut() {
                            rescan.locked.insert(machine);
                        }
                        return disconnect;
                    }
                    let mut effects: Vec<_> = model
                        .state
                        .groups
                        .iter()
                        .filter(|group| crate::session::machine_of(&group.host) == machine)
                        .map(|group| EventEffect::ApplyHostResult {
                            host: group.host.clone(),
                            sessions: Vec::new(),
                            err: Some(reason.clone()),
                        })
                        .collect();
                    // The machine owns its reachability: with no host known, its own
                    // card states the failure.
                    if model.state.stands_alone(&machine) {
                        model.switcher.apply_machine_result(
                            &machine,
                            Some(reason.clone()),
                            &mut model.state,
                        );
                    }
                    effects.extend(disconnect);
                    effects
                }
                None => {
                    if let Some(m) = model.state.machine_mut(&machine) {
                        m.err = None;
                    }
                    model.state.login_reports.remove(&machine);
                    if let Some(rescan) = model.rescan.as_mut() {
                        rescan.locked.remove(&machine);
                    }
                    vec![EventEffect::MachineConnected {
                        machine,
                        shell,
                        rescan,
                    }]
                }
            }
        }
        HostEvent::Sessions { host, .. }
            if model
                .state
                .invalid_auth
                .contains(crate::session::machine_of(&host)) =>
        {
            Vec::new()
        }
        HostEvent::Sessions {
            host,
            sessions,
            err,
        } => vec![EventEffect::ApplyPollResult {
            host,
            sessions,
            err,
        }],
    }
}

/// Handles a remote host's control client dying. A client that died with "no sessions" /
/// "no server running" / "server exited" leaves a REACHABLE host with no mux server - it
/// renders "(empty)" (and a session can be created there), not an unreachable state. Any
/// other death of a host that had connected keeps its last-known rows. Any other
/// never-connected death is a
/// transport failure and renders the unreachable state. Returns `true` only when it marked the host
/// unreachable.
pub(crate) fn note_host_exited(
    switcher: &mut Switcher,
    state: &mut crate::state::State,
    connected: &mut HashSet<String>,
    host: &str,
    reason: Option<String>,
) -> bool {
    // Clear the connected mark so this host is no longer pinned to "keep last-known
    // rows". A transient drop of a once-connected host keeps its rows (no unreachable
    // state) on THIS exit; but a later reconnect that fails (no sessions / unreachable)
    // must then resolve its real state - otherwise a refresh that set it scanning would
    // spin on "loading…" forever, since a sticky `connected` made every exit a no-op.
    let was_connected = connected.remove(host);
    // The mux said it has nothing to serve (its server ended with its last session, or a
    // reopened channel found no server): the host is empty, whatever it listed before.
    if reason
        .as_deref()
        .is_some_and(crate::model::host_def::reason_is_no_sessions)
    {
        switcher.apply_host_result(host.to_string(), Vec::new(), None, state);
        return false;
    }
    if was_connected {
        return false;
    }
    let msg = reason.unwrap_or_else(|| "connection closed".into());
    switcher.apply_host_result(host.to_string(), Vec::new(), Some(msg), state);
    true
}

/// The hosts whose last answer was an answer: settled, with no failure.
fn answering_hosts(state: &crate::state::State) -> HashSet<String> {
    state
        .groups
        .iter()
        .filter(|g| g.err.is_none() && !state.scanning.contains(&g.host))
        .map(|g| g.host.clone())
        .collect()
}

/// Records each host that was answering before and stopped since as a background event:
/// the history only, because nobody asked for that answer just now. A host that has not
/// answered yet (a launch scan failing its first probe) stopped nothing, so its card alone
/// says so. A re-scan in flight is left to its own summary, which reports the same change
/// as the result of the re-scan.
fn record_lost_hosts(model: &mut AppModel, before: &HashSet<String>) {
    let reported = |host: &str| match &model.rescan {
        Some(RescanInFlight {
            machine: Some(machine),
            ..
        }) => crate::session::machine_of(host) == machine,
        Some(_) => true,
        None => false,
    };
    let lost: Vec<(String, String)> = model
        .state
        .groups
        .iter()
        .filter(|g| before.contains(&g.host) && !model.state.scanning.contains(&g.host))
        .filter(|g| !reported(&g.host))
        .filter_map(|g| {
            let reason = g.err.as_deref()?.lines().next().unwrap_or_default();
            Some((
                model.state.chrome.host_label_when(&g.host, false),
                reason.to_string(),
            ))
        })
        .collect();
    for (label, reason) in lost {
        model.state.notify.record(
            label,
            vec![crate::state::notify::Note::new(
                crate::state::notify::Level::Warning,
                format!("unreachable: {reason}"),
            )],
        );
    }
}

/// Marks the re-scan's roster answer as in: applied, or kept because the config did not
/// parse.
fn settle_rescan_roster(model: &mut AppModel) {
    if let Some(rescan) = model.rescan.as_mut() {
        rescan.roster = false;
    }
    model.switcher.hold_numbers(false, &model.state);
}

/// Makes the re-scan's summary toast once every host it asked and the roster have
/// answered. A one-machine re-scan waits for that machine's hosts alone and reports
/// them alone, under a title naming the machine.
fn settle_rescan(model: &mut AppModel) {
    let Some(rescan) = model.rescan.as_ref() else {
        return;
    };
    let waiting = match &rescan.machine {
        Some(machine) => {
            model
                .state
                .scanning
                .iter()
                .any(|s| crate::session::machine_of(s) == machine)
                || model.state.machine_scanning.contains(machine)
        }
        None => model.state.scanning_any() || rescan.roster,
    };
    if waiting {
        return;
    }
    let Some(RescanInFlight {
        before,
        locked,
        machine,
        ..
    }) = model.rescan.take()
    else {
        return;
    };
    let after = crate::state::notify::ScanSnapshot::of(&model.state, &locked);
    let after = match &machine {
        Some(machine) => after.only_machine(machine),
        None => after,
    };
    // A host names its mux only when it answered, as its card does.
    let state = &model.state;
    let notes = before.summary(&after, |host| {
        let answered = state
            .groups
            .iter()
            .any(|g| g.host == host && g.err.is_none());
        state.chrome.named_mux(host, answered).to_string()
    });
    let title = match &machine {
        Some(machine) => format!("rescan machine {machine}"),
        None => "rescan all machines".to_string(),
    };
    model.state.notify.toast(title, notes);
}

/// The application update transition: one message in, the effects it asks for out. Every
/// message is folded by [`step`]; around it, a message that can carry a host's answer
/// records what stopped answering, and a re-scan whose last answer arrived reports.
pub(crate) fn update(model: &mut AppModel, msg: Msg) -> Vec<Effect> {
    let answers = matches!(msg, Msg::HostEvent { .. } | Msg::ApplyInventory { .. });
    let answering_before = answers.then(|| answering_hosts(&model.state));
    let landing = model.switcher.landing_open();
    let mut effects = step(model, msg);
    if let Some(before) = answering_before {
        record_lost_hosts(model, &before);
    }
    settle_rescan(model);
    show_logout_question(model);
    effects.extend(settle_logout_choice(model));
    if landing {
        effects.extend(settle_landing(model));
    }
    effects
}

/// The first execution ends the landing screen and gives the terminal view the focus,
/// whichever input it came from: a move into the terminal view (Enter, a click on a nav
/// target) closes the landing, and an execution that closed it (a landing link, a jump, a
/// switch) moves the focus there.
fn settle_landing(model: &mut AppModel) -> Vec<Effect> {
    let nav = model.state.focus.view_is_nav();
    if model.switcher.landing_open() {
        if !nav {
            model.switcher.close_landing();
        }
        Vec::new()
    } else if nav {
        update(model, Msg::Focus(crate::model::FocusTarget::Terminal))
    } else {
        Vec::new()
    }
}

/// Executes the item a list popup was told to execute, by an Enter on its hard selection
/// or a click on its soft selection: one shared execution, whichever input asked.
fn execute_list_choice(model: &mut AppModel) -> Vec<Effect> {
    if model.switcher.open_checked_host(&mut model.state) {
        return update(model, Msg::Focus(crate::model::FocusTarget::Terminal));
    }
    if let Some(choice) = model.switcher.take_palette_choice(&mut model.state) {
        return run_palette_choice(model, choice);
    }
    Vec::new()
}

fn run_palette_choice(model: &mut AppModel, choice: crate::state::PaletteChoice) -> Vec<Effect> {
    use crate::model::keys::KeyCommand;
    use crate::state::PaletteChoice;
    match choice {
        PaletteChoice::Login(host) => {
            let opened = model.switcher.open_host(&host, &mut model.state);
            if opened {
                update(model, Msg::Focus(crate::model::FocusTarget::Terminal))
            } else {
                Vec::new()
            }
        }
        PaletteChoice::Command(command) => match command {
            KeyCommand::Filter
            | KeyCommand::NewSession
            | KeyCommand::Rescan
            | KeyCommand::RescanMachine
            | KeyCommand::Logout => {
                let key = match command {
                    KeyCommand::Filter => '/',
                    KeyCommand::NewSession => 'n',
                    KeyCommand::RescanMachine => 'r',
                    KeyCommand::Rescan => 'R',
                    KeyCommand::Logout => 'L',
                    _ => unreachable!(),
                };
                update(
                    model,
                    Msg::Key(KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE)),
                )
            }
            KeyCommand::FocusTerminal => {
                update(model, Msg::Focus(crate::model::FocusTarget::Terminal))
            }
            KeyCommand::FocusNav => update(model, Msg::Focus(crate::model::FocusTarget::Nav)),
            KeyCommand::Check => update(model, Msg::ToggleCheck),
            KeyCommand::Collapse => update(model, Msg::ToggleNavCollapsed),
            KeyCommand::AutoHide => update(model, Msg::Action(Action::ToggleAutoHide)),
            KeyCommand::Position => update(model, Msg::CycleNavPosition),
            KeyCommand::History => update(model, Msg::ToggleHistory),
            KeyCommand::Help => update(model, Msg::ToggleHelp),
            KeyCommand::Quit => update(model, Msg::Action(Action::Quit)),
            _ => Vec::new(),
        },
    }
}

fn step(model: &mut AppModel, msg: Msg) -> Vec<Effect> {
    match msg {
        Msg::Action(action) => {
            if let Action::CreateSession { host, .. } = &action {
                model.switcher.note_create(host);
            }
            let commands = model.state.apply(action);
            commands
                .into_iter()
                .filter_map(|command| command_effect(model, command))
                .collect()
        }
        #[cfg(test)]
        Msg::Commands(commands) => commands
            .into_iter()
            .filter_map(|command| command_effect(model, command))
            .collect(),
        Msg::SyncSelection => {
            sync_selection(model);
            Vec::new()
        }
        Msg::KeysRead => {
            if model.state.chrome.key_read() {
                vec![Effect::PersistFirstKeyHelpSeen]
            } else {
                Vec::new()
            }
        }
        Msg::Key(key) => {
            let before = model.switcher.selected_node();
            let commands = model.switcher.handle_key(key, &mut model.state);
            // A logout confirm scrolls no further than the offset that shows its last fact
            // row in the popup as last painted, so a scroll back up moves the view at once.
            let popup = model.render_plan.popup_rect;
            if let Some(crate::state::Modal::Input(input)) = model.state.modal.as_mut() {
                input.scroll = input.scroll.min(crate::ui::modal::logout_max_scroll(
                    input,
                    popup.width,
                    popup.height.saturating_sub(2),
                ));
            }
            hint_selection_move(model, &before);
            commands
                .into_iter()
                .filter_map(|command| command_effect(model, command))
                .collect()
        }
        Msg::MouseSelect { col, row, execute } => {
            let before = model.switcher.selected_node();
            let hit = model.switcher.mouse_select(&model.render_plan, col, row);
            hint_selection_move(model, &before);
            if hit && execute {
                update(model, Msg::Focus(crate::model::FocusTarget::Terminal))
            } else {
                Vec::new()
            }
        }
        Msg::Hover { col, row } => {
            model.switcher.mouse_hover(&model.render_plan, col, row);
            model.switcher.link_hover_at(&model.render_plan, col, row);
            Vec::new()
        }
        Msg::StepLink(delta) => {
            model.switcher.step_link(delta, &model.state);
            Vec::new()
        }
        Msg::OpenLink(index) => {
            let before = model.switcher.selected_node();
            match index {
                Some(i) => model.switcher.open_link(i, &model.state),
                None => model.switcher.open_selected_link(&model.state),
            };
            hint_selection_move(model, &before);
            Vec::new()
        }
        Msg::MouseScroll { down } => {
            let before = model.switcher.selected_node();
            model.switcher.mouse_scroll(down);
            hint_selection_move(model, &before);
            Vec::new()
        }
        Msg::ToggleHelp => {
            model.switcher.toggle_help(&mut model.state);
            Vec::new()
        }
        Msg::ToggleHistory => {
            model.switcher.toggle_history(&mut model.state);
            Vec::new()
        }
        Msg::ToggleCheck => {
            model.switcher.toggle_check(&mut model.state);
            Vec::new()
        }
        Msg::TogglePalette => {
            model.switcher.toggle_palette(&mut model.state);
            Vec::new()
        }
        Msg::DismissToast(id) => {
            model.state.notify.dismiss(id);
            Vec::new()
        }
        Msg::ReaderBytes { bytes, prefix } => {
            let popup = model.render_plan.popup_rect;
            model.switcher.feed_reader_key(
                &bytes,
                prefix,
                &mut model.mouse_state.nav_armed,
                (
                    popup.width.saturating_sub(2),
                    popup.height.saturating_sub(2),
                ),
                &mut model.state,
            );
            execute_list_choice(model)
        }
        Msg::OpResult {
            result: crate::model::OpResult::MachineKeysFound { machine, result },
            logged_in,
        } => {
            model.state.logged_in = logged_in;
            logout_keys_found(model, machine, result)
        }
        Msg::OpResult {
            result: crate::model::OpResult::MachineKeysRemoved { machine, result },
            logged_in,
        } => {
            model.state.logged_in = logged_in;
            logout_keys_removed(model, machine, result)
        }
        Msg::OpResult {
            result: crate::model::OpResult::SshConfigEntriesFound { machine, result },
            logged_in,
        } => {
            model.state.logged_in = logged_in;
            logout_entries_found(model, machine, result)
        }
        Msg::OpResult {
            result: crate::model::OpResult::SshConfigEntriesRemoved { machine, result },
            logged_in,
        } => {
            model.state.logged_in = logged_in;
            logout_stanza_removed(model, machine, result)
        }
        Msg::OpResult {
            result: crate::model::OpResult::SshFactsRead { machine, facts },
            logged_in,
        } => {
            model.state.logged_in = logged_in;
            model.state.set_ssh_facts(&machine, facts);
            Vec::new()
        }
        Msg::OpResult { result, logged_in } => {
            let mut read_facts = Vec::new();
            if let crate::model::OpResult::Login {
                host,
                attempt,
                outcome,
                ..
            } = &result
            {
                // A saved connection changed ssh config whether or not this login is
                // still the current one, so the facts are read again either way.
                if outcome.saved == Some(Ok(())) {
                    read_facts.push(Effect::ReadSshFacts {
                        machine: crate::session::machine_of(host).to_owned(),
                    });
                }
                model
                    .running_logins
                    .retain(|run| !(run.host == *host && run.attempt == *attempt));
                // The running handle, not the steps, says which login is current: the
                // steps can leave before the result arrives, while only a logout or a
                // newer submission replaces the handle, and so ends the login it held.
                if !model
                    .state
                    .login_run
                    .as_ref()
                    .is_some_and(|run| run.host == *host && run.attempt == *attempt)
                {
                    return read_facts;
                }
                if outcome.connect.is_ok() {
                    model
                        .state
                        .invalid_auth
                        .remove(crate::session::machine_of(host));
                    if let Some(method) = outcome.auth_method {
                        model.state.note_auth(
                            crate::session::machine_of(host).to_owned(),
                            method,
                            None,
                        );
                    }
                }
            }
            model.state.logged_in = logged_in;
            let mut effects: Vec<_> = model
                .switcher
                .apply_op_result(result, &mut model.state)
                .map(|(host, login)| Effect::LoginApplied { host, login })
                .into_iter()
                .collect();
            effects.extend(read_facts);
            effects
        }
        Msg::LoginSettled {
            host,
            credential_held,
            machine_has_hosts,
            probe,
        } => {
            if let Some(progress) = model.state.login_progress.get_mut(&host) {
                progress.arm_probe(probe);
            }
            let machine = crate::session::machine_of(&host);
            if credential_held {
                model.state.logged_in.insert(machine.to_owned());
            } else {
                model.state.logged_in.remove(machine);
            }
            if !machine_has_hosts {
                model
                    .switcher
                    .mark_machine_scanning(machine, &mut model.state);
            }
            if model
                .state
                .login
                .as_ref()
                .is_some_and(|draft| draft.host == host)
            {
                model.state.login = None;
            }
            Vec::new()
        }
        Msg::CredentialInventory { held } => {
            let mut missing: HashSet<String> = model
                .state
                .auth_methods
                .iter()
                .filter(|(machine, method)| {
                    **method == crate::model::AuthMethod::Password && !held.contains(*machine)
                })
                .map(|(machine, _)| machine.clone())
                .collect();
            for (host, method) in &model.state.display_auth_methods {
                let machine = crate::session::machine_of(host);
                if *method == crate::model::AuthMethod::Password && !held.contains(machine) {
                    missing.insert(machine.to_owned());
                }
            }
            let mut effects = Vec::new();
            for machine in missing {
                model.state.forget_auth(&machine);
                model.state.invalid_auth.insert(machine.clone());
                model.state.logged_in.remove(&machine);
                model
                    .state
                    .live_hosts
                    .retain(|host| crate::session::machine_of(host) != machine);
                model
                    .connected
                    .retain(|host| crate::session::machine_of(host) != machine);
                if crate::session::machine_of(&model.state.displayed.host) == machine {
                    model.state.displayed = Default::default();
                    model.state.attach_deadline = None;
                    model.state.attach_pending = false;
                }
                let hosts: Vec<_> = model
                    .state
                    .groups
                    .iter()
                    .filter(|group| crate::session::machine_of(&group.host) == machine)
                    .map(|group| group.host.clone())
                    .collect();
                for host in hosts {
                    model.switcher.apply_host_result(
                        host,
                        Vec::new(),
                        Some("SSH password no longer held; log in again".into()),
                        &mut model.state,
                    );
                }
                if model.state.stands_alone(&machine) {
                    model.switcher.apply_machine_result(
                        &machine,
                        Some("SSH password no longer held; log in again".into()),
                        &mut model.state,
                    );
                }
                effects.push(Effect::Event(EventEffect::DisconnectMachine { machine }));
            }
            effects
        }
        Msg::DisplayAuth { host, method } => {
            if let Some(method) = method {
                if !model
                    .state
                    .invalid_auth
                    .contains(crate::session::machine_of(&host))
                {
                    model.state.display_auth_methods.insert(host, method);
                }
            } else {
                model.state.display_auth_methods.remove(&host);
            }
            Vec::new()
        }
        Msg::ApplyInventory {
            host,
            sessions,
            live,
        } => {
            if !live {
                return Vec::new();
            }
            let renamed =
                model
                    .switcher
                    .apply_host_result(host.clone(), sessions, None, &mut model.state);
            renamed
                .map(|(from, to)| Effect::Event(EventEffect::RenameDisplayed { host, from, to }))
                .into_iter()
                .collect()
        }
        Msg::AddHost { host, scanning } => {
            model.switcher.add_host(host.clone(), &mut model.state);
            if scanning {
                model.switcher.mark_scanning(&host, &mut model.state);
            }
            Vec::new()
        }
        Msg::AddHosts { hosts } => {
            model.switcher.add_hosts(hosts, &mut model.state);
            Vec::new()
        }
        Msg::AddMachine { machine } => {
            model.switcher.add_machine(machine, &mut model.state);
            Vec::new()
        }
        Msg::RemoveMachine { machine } => {
            model.state.machine_scan_deadlines.remove(&machine);
            model.switcher.remove_machine(&machine, &mut model.state);
            Vec::new()
        }
        Msg::ApplyMachineResult { machine, err } => {
            model
                .switcher
                .apply_machine_result(&machine, err, &mut model.state);
            Vec::new()
        }
        Msg::SettleMuxless { machine } => {
            model.switcher.settle_muxless(&machine, &mut model.state);
            Vec::new()
        }
        Msg::RemoveHost {
            host,
            clear_tracking,
        } => {
            model.state.display_auth_methods.remove(&host);
            if clear_tracking {
                model.connected.remove(&host);
                model.detecting.remove(&host);
            }
            model.state.login_progress.remove(&host);
            model.switcher.remove_host(&host, &mut model.state);
            Vec::new()
        }
        Msg::RescanRosterApplied => {
            settle_rescan_roster(model);
            Vec::new()
        }
        Msg::LaunchRosterApplied => {
            model.switcher.hold_numbers(false, &model.state);
            Vec::new()
        }
        Msg::DetectionFinished { host } => {
            model.detecting.remove(&host);
            Vec::new()
        }
        Msg::SetHostReach(reach) => {
            model.state.chrome.set_host_reach(reach);
            Vec::new()
        }
        Msg::SetRosterFacts {
            providers,
            login_defaults,
            ssh_stanzas,
            held_credentials,
            host_reach,
        } => {
            let machines: HashSet<_> = host_reach
                .keys()
                .map(|host| crate::session::machine_of(host).to_owned())
                .collect();
            model
                .state
                .auth_methods
                .retain(|machine, _| machines.contains(machine));
            model
                .state
                .display_auth_methods
                .retain(|host, _| host_reach.contains_key(host));
            model
                .state
                .invalid_auth
                .retain(|machine| machines.contains(machine));
            model.state.chrome.set_roster_providers(providers);
            model
                .state
                .chrome
                .set_login_defaults(login_defaults, ssh_stanzas);
            model
                .state
                .logged_in
                .retain(|machine| held_credentials.contains(machine));
            model.state.chrome.set_host_reach(host_reach);
            Vec::new()
        }
        Msg::HostEvent { event, logged_in } => {
            model.state.logged_in = logged_in;
            let event_effects = host_event_effects(model, event);
            event_effects
                .into_iter()
                .filter_map(|effect| match effect {
                    EventEffect::MarkConnected { host } => {
                        model.state.live_hosts.insert(host.clone());
                        model.connected.insert(host);
                        None
                    }
                    EventEffect::ApplyHostResult {
                        host,
                        sessions,
                        err,
                    } => {
                        model
                            .switcher
                            .apply_host_result(host, sessions, err, &mut model.state);
                        None
                    }
                    EventEffect::ApplyPollResult {
                        host,
                        sessions,
                        err,
                    } => {
                        let failed = err.is_some();
                        let renamed = model.switcher.apply_host_result(
                            host.clone(),
                            sessions.clone(),
                            err,
                            &mut model.state,
                        );
                        if failed {
                            None
                        } else {
                            let mut effects: Vec<EventEffect> = renamed
                                .map(|(from, to)| EventEffect::RenameDisplayed {
                                    host: host.clone(),
                                    from,
                                    to,
                                })
                                .into_iter()
                                .collect();
                            effects.push(EventEffect::SyncPollSessions { host, sessions });
                            Some(Effect::EventBatch(effects))
                        }
                    }
                    EventEffect::NoteHostExited { host, reason } => {
                        note_host_exited(
                            &mut model.switcher,
                            &mut model.state,
                            &mut model.connected,
                            &host,
                            reason,
                        );
                        None
                    }
                    effect => Some(Effect::Event(effect)),
                })
                .collect()
        }
        Msg::Focus(target) => update(model, Msg::Action(Action::Focus(target))),
        Msg::FeedLogin { host, bytes } => model
            .state
            .feed_login(&host, &bytes)
            .and_then(|command| command_effect(model, command))
            .into_iter()
            .collect(),
        Msg::SetMouseNavArmed(armed) => {
            model.mouse_state.nav_armed = armed;
            Vec::new()
        }
        Msg::SetMouseDragging(dragging) => {
            model.mouse_state.dragging_view_border = dragging;
            Vec::new()
        }
        Msg::EndNavDrag { band } => {
            model.mouse_state.dragging_view_border = false;
            if band {
                vec![Effect::PersistNavHeight(model.nav_height)]
            } else {
                vec![Effect::PersistNavWidth(model.nav_width_natural)]
            }
        }
        Msg::SetMouseHovered(hovered) => {
            model.mouse_state.hovered_view_border = hovered;
            Vec::new()
        }
        Msg::SetResizeRepeat(repeat_until) => {
            model.mouse_state.repeat_until = repeat_until;
            Vec::new()
        }
        Msg::EndPopupDrag => {
            model
                .switcher
                .end_popup_drag_in_plan(&model.render_plan, &mut model.state);
            execute_list_choice(model)
        }
        Msg::AbandonPopupDrag => {
            model.switcher.end_popup_drag();
            Vec::new()
        }
        Msg::HoverPopup { col, row } => {
            model
                .switcher
                .hover_popup(&model.render_plan, col, row, &mut model.state);
            Vec::new()
        }
        Msg::DragPopup { col, row } => {
            model.switcher.drag_popup(col, row);
            Vec::new()
        }
        Msg::BeginPopupDrag { col, row } => {
            model
                .switcher
                .begin_popup_drag_in_plan(&model.render_plan, col, row, &model.state);
            Vec::new()
        }
        Msg::ToggleNavCollapsed => {
            model.nav_collapsed = !model.nav_collapsed;
            model.mouse_state.hovered_view_border = false;
            vec![Effect::PersistNavCollapsed(model.nav_collapsed)]
        }
        Msg::SetNavCollapsed(collapsed) => {
            if model.nav_collapsed == collapsed {
                Vec::new()
            } else {
                update(model, Msg::ToggleNavCollapsed)
            }
        }
        Msg::SetNavNaturalWidth(width) => {
            model.nav_width_natural = width;
            Vec::new()
        }
        Msg::SetNavHeight(height) => {
            model.nav_height = height;
            Vec::new()
        }
        Msg::ResizeNav {
            horizontal,
            delta,
            body_rows,
            ui_prefix,
        } => {
            let top = model.render_plan.layout == crate::ui::switcher::ViewLayout::Band;
            let delta = if model.nav_position.forward_arrows_face_terminal() {
                delta
            } else {
                -delta
            };
            match (horizontal, top) {
                (true, false) => {
                    model.nav_width_natural =
                        adjust_nav_width(model.nav_width_natural, delta, &ui_prefix);
                    Vec::new()
                }
                (false, true) => {
                    let base = if model.nav_height == 0 {
                        crate::ui::switcher::default_nav_height(body_rows)
                    } else {
                        model.nav_height
                    };
                    let ceil = body_rows
                        .saturating_sub(2)
                        .clamp(NAV_HEIGHT_MIN, NAV_HEIGHT_MAX);
                    let next =
                        (base as i32 + delta).clamp(NAV_HEIGHT_MIN as i32, ceil as i32) as u16;
                    if next == model.nav_height {
                        Vec::new()
                    } else {
                        model.nav_height = next;
                        vec![Effect::PersistNavHeight(model.nav_height)]
                    }
                }
                _ => Vec::new(),
            }
        }
        Msg::CycleNavPosition => {
            model.nav_position_pinned = crate::ui::switcher::step_nav_position(
                model.nav_position_pinned,
                model.nav_position,
            );
            vec![Effect::PersistNavPosition(model.nav_position_pinned)]
        }
        Msg::CancelRunningLogin => model
            .state
            .login_run
            .as_ref()
            .cloned()
            .map(Effect::CancelLogin)
            .into_iter()
            .collect(),
        Msg::PersistNavSize => vec![
            Effect::PersistNavWidth(model.nav_width_natural),
            Effect::PersistNavHeight(model.nav_height),
        ],
        Msg::SyncFrame {
            spinner_frame,
            animation_ms,
            view_border_hovered,
            prefix_active,
        } => {
            model.state.chrome.set_spinner_frame(spinner_frame);
            model.state.chrome.animation_ms = animation_ms;
            model
                .state
                .chrome
                .set_view_border_hovered(view_border_hovered);
            model.state.chrome.set_armed(prefix_active);
            model.switcher.settle_popup_position(&model.state);
            let modal_kind = model.state.modal_kind();
            model.state.focus.sync_modal(modal_kind);
            let nav_focused = model.state.focus.view_is_nav();
            model.switcher.sync_view_focus(!nav_focused);
            let mut effects = Vec::new();
            if nav_focused && !model.nav_was_focused && model.nav_collapsed {
                model.nav_collapsed = false;
                effects.push(Effect::PersistNavCollapsed(false));
            }
            model.nav_was_focused = nav_focused;
            model.state.chrome.set_auto_hide(model.auto_hide_nav);
            if model.state.recorded_logins != model.saved_logins {
                model.saved_logins = model.state.recorded_logins.clone();
                effects.push(Effect::PersistSshLogins(model.saved_logins.clone()));
            }
            effects
        }
        Msg::ReconcileNav { width, position } => {
            model.nav_position = position;
            model.nav_width = width;
            model.applied_nav_height = model.nav_height;
            model.applied_nav_collapsed = model.nav_collapsed;
            model.state.chrome.set_nav_position(position);
            Vec::new()
        }
        Msg::ConsumeReattach { now } => {
            if model.switcher.take_reattach_kick() && !model.state.selection.is_empty() {
                let selection = model.state.selection.clone();
                model.state.apply(Action::ClearDisplay);
                model.state.apply(Action::RearmAttachNow { now });
                vec![Effect::ReattachDisplay(selection)]
            } else {
                Vec::new()
            }
        }
        Msg::MarkWidthDirty { flush_at } => {
            model.width_dirty = true;
            model.width_flush_at = Some(flush_at);
            Vec::new()
        }
        Msg::FlushWidth { now, force } => {
            if model.width_dirty
                && (force || model.width_flush_at.is_some_and(|deadline| now >= deadline))
            {
                model.width_dirty = false;
                model.width_flush_at = None;
                vec![Effect::PersistNavWidth(model.nav_width_natural)]
            } else {
                Vec::new()
            }
        }
        Msg::SetRenderPlan(plan) => {
            model.render_plan = plan;
            Vec::new()
        }
        Msg::FollowDisplay(address) => {
            model.switcher.select_address(&address);
            Vec::new()
        }
        Msg::Tick { now, spinner } => {
            let mut expired = Vec::new();
            for host in &model.state.scanning {
                let deadline = model
                    .state
                    .scan_deadlines
                    .entry(host.clone())
                    .or_insert(now + crate::provision::env::SCAN_TIMEOUT);
                if now >= *deadline {
                    expired.push(host.clone());
                }
            }
            for host in expired {
                let sessions = model
                    .state
                    .groups
                    .iter()
                    .find(|group| group.host == host)
                    .map(|group| group.sessions.clone())
                    .unwrap_or_default();
                model.switcher.apply_host_result(
                    host,
                    sessions,
                    Some("scan timed out after 10s".into()),
                    &mut model.state,
                );
            }
            let mut expired = Vec::new();
            for machine in &model.state.machine_scanning {
                let deadline = model
                    .state
                    .machine_scan_deadlines
                    .entry(machine.clone())
                    .or_insert(now + crate::provision::env::SCAN_TIMEOUT);
                if now >= *deadline {
                    expired.push(machine.clone());
                }
            }
            for machine in expired {
                model.switcher.apply_machine_result(
                    &machine,
                    Some("scan timed out after 10s".into()),
                    &mut model.state,
                );
            }
            model.state.chrome.expire_selection_hint(now);
            let history_open =
                matches!(model.state.modal, Some(crate::state::Modal::History { .. }));
            model.state.notify.tick(now, history_open);
            model.state.chrome.set_spinner(spinner);
            Vec::new()
        }
        Msg::ConfigObserved { mtime, ui } => {
            model.config_last_mtime = mtime;
            if let Some(ui) = ui {
                let (ui, palette) = *ui;
                model.state.chrome.apply_palette(&ui, &palette);
                model.switcher.set_palette(palette);
                model.nav_default = ui.nav_position();
                model.max_fps = ui.max_fps;
                model.state.notify.set_toasts_enabled(ui.notifications);
                model
                    .switcher
                    .set_renumbering(ui.renumbering, &mut model.state);
                model.state.chrome.braille_animation = ui.braille_animation;
            }
            Vec::new()
        }
        Msg::ConfigError(error) => {
            model.state.notify.toast(
                "config",
                vec![crate::state::notify::Note::new(
                    crate::state::notify::Level::Warning,
                    format!("Config error: {error}"),
                )],
            );
            Vec::new()
        }
        // The release notice answers the launch, so it is a toast like any other result.
        Msg::Notice(line) => {
            model.state.notify.toast(
                "update",
                vec![crate::state::notify::Note::new(
                    crate::state::notify::Level::Info,
                    line,
                )],
            );
            Vec::new()
        }
        Msg::DetectionStarted(host) => {
            model.detecting.insert(host);
            Vec::new()
        }
        Msg::Shutdown => {
            let mut effects = update(
                model,
                Msg::FlushWidth {
                    now: std::time::Instant::now(),
                    force: true,
                },
            );
            if let Some(login) = model.state.login_run.take() {
                effects.push(Effect::CancelLogin(login));
            }
            effects
        }
    }
}

/// Raises the hint about the card the user just moved the selection to, replacing any
/// earlier one. A selection that stayed on `before` raises nothing.
fn hint_selection_move(model: &mut AppModel, before: &Option<crate::model::Node>) {
    if model.state.chrome.first_key_notice {
        return;
    }
    if !model.switcher.selection_moved_from(before) {
        return;
    }
    // The hint names keys for the view that holds the focus once the move is done (the
    // one behind a modal included), so it never offers a key the pane would receive.
    let nav_focused = model.state.focus.view_is_nav();
    match model.switcher.selection_hint(&model.state, nav_focused) {
        Some((keys, fact)) => {
            model
                .state
                .chrome
                .show_selection_hint(keys, fact, std::time::Instant::now());
        }
        None => model.state.chrome.clear_selection_hint(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::{update, AppModel, Effect, Msg};
    use crate::model::EventEffect;

    fn model() -> AppModel {
        AppModel::from_hosts(vec!["local".to_owned()])
    }

    /// A model listing two sessions on `local` and an unreachable `prod`, with the
    /// selection on the first session.
    fn model_with_cards() -> AppModel {
        let mut model = AppModel::from_hosts(vec!["local".to_owned(), "prod".to_owned()]);
        let session = |name: &str, windows: i64| crate::session::Session {
            host: "local".to_owned(),
            name: name.to_owned(),
            windows,
            ..Default::default()
        };
        update(
            &mut model,
            Msg::HostEvent {
                event: crate::link::HostEvent::Sessions {
                    host: "local".to_owned(),
                    sessions: vec![session("build", 3), session("editor", 1)],
                    err: None,
                },
                logged_in: HashSet::new(),
            },
        );
        update(
            &mut model,
            Msg::HostEvent {
                event: crate::link::HostEvent::Sessions {
                    host: "prod".to_owned(),
                    sessions: Vec::new(),
                    err: Some("ssh: connect to host prod port 22: Connection refused".to_owned()),
                },
                logged_in: HashSet::new(),
            },
        );
        model
    }

    fn down() -> Msg {
        Msg::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
    }

    fn hint_text(model: &AppModel) -> String {
        model.state.chrome.hint_bar_text(200, &model.state)
    }

    #[test]
    fn first_interactive_key_introduces_prefix_and_help_once() {
        let mut m = model();
        assert!(!m.state.chrome.first_key_seen);
        assert!(matches!(
            update(&mut m, Msg::KeysRead).as_slice(),
            [Effect::PersistFirstKeyHelpSeen]
        ));
        assert!(m.state.chrome.first_key_seen);
        assert!(hint_text(&m).contains("C-g prefix"));
        assert!(hint_text(&m).contains("C-g ? help"));
        assert!(update(&mut m, Msg::KeysRead).is_empty());
        assert!(!m.state.chrome.first_key_notice);
        assert!(!hint_text(&m).contains("C-g ? help"));
    }

    #[test]
    fn a_selection_move_raises_the_cards_keys_and_a_fact_for_three_seconds() {
        let mut model = model_with_cards();
        assert!(
            model.state.chrome.selection_hint.is_none(),
            "nothing moved yet"
        );
        let start = std::time::Instant::now();
        update(&mut model, down());
        let hint = model.state.chrome.selection_hint.clone().expect("a hint");
        assert_eq!(
            hint_text(&model),
            " Enter focus terminal view · C-g n new session · 1 window",
            "the session's keys, from the key table, and its windows"
        );
        assert!(hint.until >= start + std::time::Duration::from_secs(3));
        assert!(hint.until <= std::time::Instant::now() + std::time::Duration::from_secs(3));
        // Still up just before its three seconds, and gone at them.
        let tick = |now| Msg::Tick {
            now,
            spinner: HashSet::new(),
        };
        update(
            &mut model,
            tick(hint.until - std::time::Duration::from_millis(1)),
        );
        assert!(model.state.chrome.selection_hint.is_some());
        update(&mut model, tick(hint.until));
        assert!(model.state.chrome.selection_hint.is_none());
        assert_eq!(
            hint_text(&model).trim(),
            "C-g",
            "back to the resting prefix"
        );
    }

    #[test]
    fn the_next_move_replaces_the_hint_and_any_key_ends_it() {
        let mut model = model_with_cards();
        model.state.chrome.first_key_seen = true;
        update(&mut model, down());
        assert!(hint_text(&model).contains("1 window"));
        // The next move replaces it with the card it lands on: the unreachable host,
        // with the reason behind its state.
        update(&mut model, down());
        assert_eq!(
            hint_text(&model),
            " Enter focus terminal view · C-g r rescan this machine · unreachable: ssh: connect to host prod port 22: Connection refused"
        );
        // Any key read ends it before the key is applied; a key that moves nothing
        // raises nothing new.
        update(&mut model, Msg::KeysRead);
        assert!(model.state.chrome.selection_hint.is_none());
        update(
            &mut model,
            Msg::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)),
        );
        assert!(model.state.chrome.selection_hint.is_none());
    }

    #[test]
    fn a_move_that_leaves_the_terminal_focused_offers_no_key_for_the_pane() {
        let mut model = model_with_cards();
        update(&mut model, Msg::Focus(crate::model::FocusTarget::Terminal));
        // prefix 3 from the terminal view opens the jump on the unreachable host card,
        // and Enter closes it with the focus back on the terminal view.
        update(
            &mut model,
            Msg::Key(KeyEvent::new(KeyCode::Char('3'), KeyModifiers::NONE)),
        );
        update(
            &mut model,
            Msg::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        );
        assert!(!model.state.focus.view_is_nav());
        let text = hint_text(&model);
        assert!(text.contains("C-g r"), "{text}");
        assert!(
            !text.contains("Enter"),
            "Enter would reach the pane, so it is not offered: {text}"
        );
    }

    #[test]
    fn a_selection_xmux_was_told_to_make_raises_no_hint() {
        let mut model = model_with_cards();
        update(
            &mut model,
            Msg::FollowDisplay(crate::session::Address::new("local", "editor")),
        );
        assert!(
            model.state.chrome.selection_hint.is_none(),
            "only the user's own moves are answered with a hint"
        );
    }

    #[test]
    fn key_rescan_and_ctl_rescan_produce_the_same_effects() {
        let key = update(
            &mut model(),
            Msg::Key(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::NONE)),
        );
        let ctl = update(&mut model(), Msg::Action(crate::model::Action::Rescan));

        assert!(matches!(
            key.as_slice(),
            [Effect::Command(crate::model::Command::Rescan)]
        ));
        assert!(matches!(
            ctl.as_slice(),
            [Effect::Command(crate::model::Command::Rescan)]
        ));
    }

    #[test]
    fn mouse_row_click_goes_through_update() {
        let mut model = model();
        let session = crate::session::Session {
            host: "local".to_owned(),
            name: "work".to_owned(),
            ..Default::default()
        };
        update(
            &mut model,
            Msg::HostEvent {
                event: crate::link::HostEvent::Sessions {
                    host: "local".to_owned(),
                    sessions: vec![session],
                    err: None,
                },
                logged_in: HashSet::new(),
            },
        );
        model.render_plan = model.layout_for_test(ratatui::layout::Rect::new(0, 0, 80, 24));
        let (_, rect) = model
            .render_plan
            .nav_cells
            .last()
            .expect("one session card");
        let (col, row) = (rect.x, rect.y);

        let effects = update(
            &mut model,
            Msg::MouseSelect {
                col,
                row,
                execute: false,
            },
        );

        assert!(effects.is_empty());
        assert!(model.state.selection.session.is_empty());
        update(&mut model, Msg::SyncSelection);
        assert_eq!(model.state.selection.session, "work");
    }

    #[test]
    fn remove_host_tracking_depends_on_the_removal_origin() {
        let mut model = model();
        model.connected.insert("local".into());
        model.detecting.insert("local".into());

        let effects = update(
            &mut model,
            Msg::RemoveHost {
                host: "local".into(),
                clear_tracking: false,
            },
        );

        assert!(effects.is_empty());
        assert!(model.connected.contains("local"));
        assert!(model.detecting.contains("local"));

        let effects = update(
            &mut model,
            Msg::RemoveHost {
                host: "local".into(),
                clear_tracking: true,
            },
        );

        assert!(effects.is_empty());
        assert!(!model.connected.contains("local"));
        assert!(!model.detecting.contains("local"));
    }

    #[test]
    fn clamped_band_resize_does_not_persist_an_unchanged_height() {
        let mut model = model();
        model.render_plan.layout = crate::ui::switcher::ViewLayout::Band;
        model.nav_height = super::NAV_HEIGHT_MIN;

        let effects = update(
            &mut model,
            Msg::ResizeNav {
                horizontal: false,
                delta: -1,
                body_rows: 24,
                ui_prefix: "C-g".into(),
            },
        );

        assert!(effects.is_empty());
        assert_eq!(model.nav_height, super::NAV_HEIGHT_MIN);
    }

    #[test]
    fn host_event_enters_the_same_effect_stream_as_a_command() {
        let mut model = model();
        let host = update(
            &mut model,
            Msg::HostEvent {
                event: crate::link::HostEvent::Changed {
                    host: "local".to_owned(),
                },
                logged_in: HashSet::new(),
            },
        );
        let command = update(&mut model, Msg::Action(crate::model::Action::Quit));

        assert!(matches!(
            host.as_slice(),
            [Effect::Event(crate::model::EventEffect::Refetch { host })] if host == "local"
        ));
        assert!(matches!(
            command.as_slice(),
            [Effect::Command(crate::model::Command::Quit)]
        ));
    }

    #[test]
    fn host_event_preserves_state_actions_before_runtime_followups() {
        let mut model = AppModel::from_hosts(Vec::new());
        let connected = super::host_event_effects(
            &mut model,
            crate::link::HostEvent::Connected {
                host: "jup".into(),
                sessions: Vec::new(),
            },
        );
        assert!(matches!(
            connected.as_slice(),
            [
                crate::model::EventEffect::MarkConnected { host: marked },
                crate::model::EventEffect::CheckSharedConnection { machine },
                crate::model::EventEffect::ApplyInventory { host: applied, .. }
            ] if marked == "jup" && machine == "jup" && applied == "jup"
        ));

        let exited = super::host_event_effects(
            &mut model,
            crate::link::HostEvent::Exited {
                host: "jup".into(),
                reason: Some("connection refused".into()),
                detached: false,
            },
        );
        assert!(matches!(
            exited.as_slice(),
            [
                crate::model::EventEffect::NoteHostExited { host: noted, .. },
                crate::model::EventEffect::ReapHost { host: reaped }
            ] if noted == "jup" && reaped == "jup"
        ));

        model.state.scanning.insert("jup".into());
        let scanned = super::host_event_effects(
            &mut model,
            crate::link::HostEvent::Scanned {
                host: "jup".into(),
                detected: None,
                err: Some("mux not found".into()),
            },
        );
        assert!(matches!(
            scanned.as_slice(),
            [
                crate::model::EventEffect::ApplyHostResult {
                    host: applied,
                    ..
                },
                crate::model::EventEffect::DispatchScanned {
                    host: dispatched,
                    ..
                }
            ] if applied == "jup" && dispatched == "jup"
        ));
    }

    #[test]
    fn cancelling_login_keeps_the_running_marker_until_the_result_arrives() {
        let mut model = model();
        model.state.login_run = Some(crate::link::unlock::RunningLogin::parked("local"));

        let effects = update(&mut model, Msg::CancelRunningLogin);

        assert!(model.state.login_run.is_some());
        assert!(matches!(effects.as_slice(), [Effect::CancelLogin(_)]));
    }

    fn sessions(host: &str, names: &[&str]) -> Vec<crate::session::Session> {
        names
            .iter()
            .map(|name| crate::session::Session {
                host: host.to_owned(),
                name: (*name).to_owned(),
                mux: "tmux".to_owned(),
                windows: 1,
                attached: false,
            })
            .collect()
    }

    fn answer(model: &mut AppModel, host: &str, names: &[&str], err: Option<&str>) {
        let logged_in = model.state.logged_in.clone();
        update(
            model,
            Msg::HostEvent {
                event: crate::link::HostEvent::Sessions {
                    host: host.to_owned(),
                    sessions: sessions(host, names),
                    err: err.map(str::to_owned),
                },
                logged_in,
            },
        );
    }

    fn note_texts(model: &AppModel) -> Vec<String> {
        model.state.notify.toasts[0]
            .notes
            .iter()
            .map(|n| n.text.clone())
            .collect()
    }

    /// Background answers never move the focus or close a popup the user opened, and a
    /// host that is lost moves the selection up to its machine, which its return does not
    /// undo.
    #[test]
    fn background_events_keep_the_focus_and_the_popup_and_a_lost_host_moves_up() {
        let mut m = AppModel::from_hosts(vec!["box".to_owned(), "gpu".to_owned()]);
        answer(&mut m, "box", &["work"], None);
        answer(&mut m, "gpu", &["train"], None);
        m.switcher
            .select_address(&crate::session::Address::new("box", "work"));
        update(&mut m, Msg::Focus(crate::model::FocusTarget::Terminal));
        m.switcher.toggle_help(&mut m.state);
        let focus = m.state.focus.view_is_nav();
        let started = std::time::Instant::now();
        let background = [
            Msg::HostEvent {
                event: crate::link::HostEvent::Sessions {
                    host: "gpu".to_owned(),
                    sessions: sessions("gpu", &["eval", "train"]),
                    err: None,
                },
                logged_in: HashSet::new(),
            },
            Msg::HostEvent {
                event: crate::link::HostEvent::MuxesFound {
                    machine: "gpu".to_owned(),
                    muxes: Ok(vec!["zellij".to_owned()]),
                },
                logged_in: HashSet::new(),
            },
            Msg::Tick {
                now: started,
                spinner: HashSet::new(),
            },
            Msg::Tick {
                now: started + std::time::Duration::from_secs(30),
                spinner: HashSet::new(),
            },
        ];
        for msg in background {
            update(&mut m, msg);
            assert_eq!(m.state.focus.view_is_nav(), focus, "the focus stays");
            assert!(
                matches!(m.state.modal, Some(crate::state::Modal::Help { .. })),
                "the popup stays open"
            );
            assert_eq!(
                m.switcher.selected_node(),
                Some(crate::model::Node::Session(crate::session::Address::new(
                    "box", "work"
                )))
            );
        }
        answer(&mut m, "box", &[], Some("connection refused"));
        assert_eq!(
            m.switcher.selected_node(),
            Some(crate::model::Node::Machine("box".into())),
            "the lost host moves the selection up"
        );
        assert!(matches!(
            m.state.modal,
            Some(crate::state::Modal::Help { .. })
        ));
        answer(&mut m, "box", &["work"], None);
        assert_eq!(
            m.switcher.selected_node(),
            Some(crate::model::Node::Machine("box".into())),
            "the host's return moves nothing down"
        );
    }

    #[test]
    fn a_rescan_reports_one_summary_once_every_host_has_answered() {
        let mut m = AppModel::from_hosts(vec!["a".to_owned(), "b".to_owned()]);
        answer(&mut m, "a", &["x"], None);
        answer(&mut m, "b", &[], None);
        assert!(
            m.state.notify.toasts.is_empty(),
            "the launch scan makes no toast"
        );

        update(&mut m, Msg::Action(crate::model::Action::Rescan));
        update(&mut m, Msg::RescanRosterApplied);
        answer(&mut m, "a", &["x", "y"], None);
        assert!(
            m.state.notify.toasts.is_empty(),
            "no summary while a host is still scanning"
        );
        answer(&mut m, "b", &[], Some("ssh: connect to host b: timed out"));
        assert_eq!(m.state.notify.toasts.len(), 1, "one toast for the re-scan");
        assert_eq!(m.state.notify.toasts[0].title, "rescan all machines");
        assert_eq!(
            note_texts(&m),
            ["1 session started: a/tmux/y", "b unreachable"],
            "the summary names what changed"
        );
        assert!(
            m.state.notify.toasts[0].until.is_none(),
            "a host that stopped answering keeps the summary up"
        );
        assert!(
            !m.state
                .notify
                .history
                .iter()
                .any(|e| e.note.text.starts_with("unreachable:")),
            "the re-scan's own summary reports the lost host, not a background record"
        );

        // A second re-scan that finds the same inventory says nothing changed.
        update(&mut m, Msg::Action(crate::model::Action::Rescan));
        update(&mut m, Msg::RescanRosterApplied);
        answer(&mut m, "a", &["x", "y"], None);
        answer(&mut m, "b", &[], Some("ssh: connect to host b: timed out"));
        assert_eq!(m.state.notify.toasts.len(), 2);
        assert_eq!(
            m.state.notify.toasts[1].notes[0].text,
            "no changes · 2 machines, 2 sessions"
        );
    }

    #[test]
    fn a_rescan_summary_waits_for_the_roster_answer() {
        let mut m = AppModel::from_hosts(vec!["a".to_owned()]);
        answer(&mut m, "a", &["x"], None);
        update(&mut m, Msg::Action(crate::model::Action::Rescan));
        answer(&mut m, "a", &["x"], None);
        assert!(
            m.state.notify.toasts.is_empty(),
            "every host answered, the roster has not"
        );
        update(
            &mut m,
            Msg::AddHost {
                host: "b".to_owned(),
                scanning: false,
            },
        );
        update(&mut m, Msg::RescanRosterApplied);
        assert!(
            m.state.notify.toasts.is_empty(),
            "the host the roster added is still scanning"
        );
        answer(&mut m, "b", &[], None);
        assert_eq!(note_texts(&m), ["1 machine added: b"]);

        // A roster that could not be read leaves the hosts as they are and still lets the
        // re-scan report.
        update(&mut m, Msg::Action(crate::model::Action::Rescan));
        answer(&mut m, "a", &["x"], None);
        answer(&mut m, "b", &[], None);
        assert_eq!(m.state.notify.toasts.len(), 1);
        update(
            &mut m,
            Msg::HostEvent {
                event: crate::link::HostEvent::RosterKept,
                logged_in: HashSet::new(),
            },
        );
        assert_eq!(m.state.notify.toasts.len(), 2);
    }

    #[test]
    fn a_refused_saved_password_is_summarized_as_login_needed() {
        let mut m = AppModel::from_hosts(vec!["a".to_owned()]);
        answer(&mut m, "a", &["x", "y"], None);
        update(&mut m, Msg::Action(crate::model::Action::Rescan));
        update(&mut m, Msg::RescanRosterApplied);
        update(
            &mut m,
            Msg::HostEvent {
                event: crate::link::HostEvent::MachineProbed {
                    machine: "a".to_owned(),
                    err: Some("dev@a: Permission denied (publickey,password).".to_owned()),
                    shell: None,
                    password_supplied: true,
                    credential_rejection_generation: None,
                    credential_held: true,
                    credential_generation: 1,
                    current_credential_generation: 1,
                    rescan: true,
                    probe: 0,
                },
                logged_in: HashSet::new(),
            },
        );
        assert!(m.state.scanning.is_empty(), "the refusal settles the card");
        assert_eq!(
            note_texts(&m),
            ["a login needed"],
            "the sessions did not end; the login was refused"
        );
        assert!(m.state.notify.toasts[0].until.is_none());
    }

    #[test]
    fn a_launch_probe_that_fails_records_nothing() {
        let mut m = AppModel::from_hosts(vec!["a".to_owned()]);
        assert!(m.state.scanning.contains("a"));
        answer(&mut m, "a", &[], Some("ssh: connect to host a: timed out"));
        assert!(
            m.state.notify.history.is_empty(),
            "a host that never answered stopped nothing"
        );
    }

    fn host_event(m: &mut AppModel, event: crate::link::HostEvent) -> Vec<String> {
        update(
            m,
            Msg::HostEvent {
                event,
                logged_in: HashSet::new(),
            },
        )
        .into_iter()
        .flat_map(|effect| match effect {
            Effect::Event(effect) => vec![effect],
            Effect::EventBatch(effects) => effects,
            effect => panic!("a host event emitted an unrelated effect: {effect:?}"),
        })
        .map(|effect| format!("{effect:?}"))
        .collect()
    }

    fn exited(reason: Option<&str>, detached: bool) -> crate::link::HostEvent {
        crate::link::HostEvent::Exited {
            host: "gpu".to_owned(),
            reason: reason.map(str::to_owned),
            detached,
        }
    }

    /// A `gpu` host whose control channel connected listing `names`.
    fn connected_gpu(names: &[&str]) -> AppModel {
        let mut m = AppModel::from_hosts(vec!["gpu".to_owned()]);
        answer(&mut m, "gpu", names, None);
        host_event(
            &mut m,
            crate::link::HostEvent::Connected {
                host: "gpu".to_owned(),
                sessions: sessions("gpu", names),
            },
        );
        m
    }

    fn gpu_card(m: &AppModel) -> (Vec<String>, Option<String>) {
        let g = m.state.groups.iter().find(|g| g.host == "gpu").unwrap();
        (
            g.sessions.iter().map(|s| s.name.clone()).collect(),
            g.err.clone(),
        )
    }

    fn unreachable_records(m: &AppModel) -> usize {
        m.state
            .notify
            .history
            .iter()
            .filter(|e| e.note.text.starts_with("unreachable:"))
            .count()
    }

    const REAP_AND_REOPEN: [&str; 2] =
        ["ReapHost { host: \"gpu\" }", "ReopenHost { host: \"gpu\" }"];

    #[test]
    fn a_detach_of_a_connected_host_reopens_its_channel_and_keeps_its_card() {
        let mut m = connected_gpu(&["keep", "train"]);
        // tmux pushes the destroyed session to the nav before it detaches its client.
        update(
            &mut m,
            Msg::ApplyInventory {
                host: "gpu".to_owned(),
                sessions: sessions("gpu", &["keep"]),
                live: true,
            },
        );

        let effects = host_event(&mut m, exited(None, true));

        assert_eq!(effects, REAP_AND_REOPEN, "one reap, then one reopen");
        assert_eq!(gpu_card(&m), (vec!["keep".to_owned()], None));
        assert_eq!(unreachable_records(&m), 0, "a detach loses no host");
        assert!(m.state.notify.toasts.is_empty());

        // The reopened channel lists sessions, so its own later detach reopens it again.
        host_event(
            &mut m,
            crate::link::HostEvent::Connected {
                host: "gpu".to_owned(),
                sessions: sessions("gpu", &["keep"]),
            },
        );
        assert_eq!(host_event(&mut m, exited(None, true)), REAP_AND_REOPEN);
    }

    #[test]
    fn a_reopen_that_fails_marks_the_host_unreachable_and_opens_nothing_more() {
        let mut m = connected_gpu(&["keep", "train"]);
        assert_eq!(host_event(&mut m, exited(None, true)), REAP_AND_REOPEN);

        // The reopened stream ends before it lists any session.
        let effects = host_event(&mut m, exited(None, false));

        assert_eq!(effects, ["ReapHost { host: \"gpu\" }"], "no second reopen");
        assert!(gpu_card(&m).1.is_some(), "the card reads unreachable");
        assert_eq!(unreachable_records(&m), 1, "the lost host is recorded once");
    }

    #[test]
    fn a_detach_notice_on_a_reopened_stream_that_never_listed_does_not_reopen_again() {
        let mut m = connected_gpu(&["keep"]);
        assert_eq!(host_event(&mut m, exited(None, true)), REAP_AND_REOPEN);

        let effects = host_event(&mut m, exited(None, true));

        assert_eq!(effects, ["ReapHost { host: \"gpu\" }"]);
        assert!(gpu_card(&m).1.is_some());
    }

    #[test]
    fn a_reopen_onto_a_host_with_no_sessions_left_shows_it_empty() {
        // The two ways a reopened channel learns the server is gone: tmux 3.3a's attach
        // starts a server and answers with its own "no sessions" error and a bare notice;
        // a client that cannot start one prints its "no server running" complaint through
        // the tty and the stream ends with no notice.
        for (reason, detached) in [
            ("no sessions", true),
            ("no server running on /tmp/tmux-1000/default", false),
        ] {
            let mut m = connected_gpu(&["last"]);
            assert_eq!(host_event(&mut m, exited(None, true)), REAP_AND_REOPEN);

            let effects = host_event(&mut m, exited(Some(reason), detached));

            assert_eq!(effects, ["ReapHost { host: \"gpu\" }"], "{reason}");
            assert_eq!(
                gpu_card(&m),
                (Vec::new(), None),
                "an empty host, not unreachable: {reason}"
            );
            assert_eq!(unreachable_records(&m), 0, "{reason}");
        }
    }

    #[test]
    fn a_server_that_exits_leaves_a_connected_host_empty_without_a_reopen() {
        let mut m = connected_gpu(&["keep", "last"]);

        let effects = host_event(&mut m, exited(Some("server exited"), false));

        assert_eq!(effects, ["ReapHost { host: \"gpu\" }"], "no reopen");
        assert_eq!(gpu_card(&m), (Vec::new(), None));
        assert_eq!(unreachable_records(&m), 0);
    }

    #[test]
    fn turning_notifications_off_live_takes_the_toasts_down() {
        let mut m = model();
        m.state.notify.toast(
            "gpu-02",
            vec![crate::state::notify::Note::new(
                crate::state::notify::Level::Error,
                "login failed: denied",
            )],
        );
        let ui = crate::provision::config::UiConfig {
            notifications: false,
            ..Default::default()
        };
        update(
            &mut m,
            Msg::ConfigObserved {
                mtime: None,
                ui: Some(Box::new((ui, crate::ui::palette::Palette::default()))),
            },
        );
        assert!(m.state.notify.toasts.is_empty());
        assert_eq!(m.state.notify.history.len(), 1, "the history keeps it");
    }

    #[test]
    fn config_observation_updates_braille_animation() {
        let mut m = model();
        assert!(m.state.chrome.braille_animation);
        let ui = crate::provision::config::UiConfig {
            braille_animation: false,
            ..Default::default()
        };
        update(
            &mut m,
            Msg::ConfigObserved {
                mtime: None,
                ui: Some(Box::new((ui, crate::ui::palette::Palette::default()))),
            },
        );
        assert!(!m.state.chrome.braille_animation);
    }

    #[test]
    fn config_observation_updates_frame_cap() {
        let mut m = model();
        assert_eq!(m.max_fps, 30);
        let ui = crate::provision::config::UiConfig {
            max_fps: 120,
            ..Default::default()
        };
        update(
            &mut m,
            Msg::ConfigObserved {
                mtime: None,
                ui: Some(Box::new((ui, crate::ui::palette::Palette::default()))),
            },
        );
        assert_eq!(m.max_fps, 120);
        update(
            &mut m,
            Msg::ConfigObserved {
                mtime: None,
                ui: None,
            },
        );
        assert_eq!(m.max_fps, 120);
    }

    #[test]
    fn the_tick_repaints_the_open_history_as_its_ages_move() {
        let mut m = model();
        let t0 = std::time::Instant::now();
        m.state.notify.record(
            "a",
            vec![crate::state::notify::Note::new(
                crate::state::notify::Level::Warning,
                "unreachable",
            )],
        );
        let tick = |m: &mut AppModel, ms: u64| {
            update(
                m,
                Msg::Tick {
                    now: t0 + std::time::Duration::from_millis(ms),
                    spinner: HashSet::new(),
                },
            );
            m.state.notify.repaint
        };
        assert!(!tick(&mut m, 0), "a closed history asks for nothing");
        update(&mut m, Msg::ToggleHistory);
        assert!(tick(&mut m, 120));
        assert!(!tick(&mut m, 240));
        assert!(tick(&mut m, 1120), "a second later its ages move");
    }

    #[test]
    fn a_host_that_stops_answering_unasked_is_recorded_without_a_toast() {
        let mut m = AppModel::from_hosts(vec!["a".to_owned()]);
        answer(&mut m, "a", &["x"], None);
        answer(&mut m, "a", &[], Some("connection closed\nmore detail"));
        assert!(
            m.state.notify.toasts.is_empty(),
            "nobody asked, so no toast"
        );
        let entry = m.state.notify.history.back().expect("a history record");
        assert_eq!(entry.title, "a");
        assert_eq!(entry.note.text, "unreachable: connection closed");
        assert_eq!(entry.note.level, crate::state::notify::Level::Warning);
        // The same failure answered again is no new event.
        answer(&mut m, "a", &[], Some("connection closed"));
        assert_eq!(m.state.notify.history.len(), 1);
    }

    #[test]
    fn prefix_m_opens_the_history_takes_the_toasts_down_and_closes_again() {
        let mut m = model();
        m.state.notify.toast(
            "gpu-02",
            vec![crate::state::notify::Note::new(
                crate::state::notify::Level::Error,
                "login failed: denied",
            )],
        );
        update(&mut m, Msg::ToggleHistory);
        assert!(matches!(
            m.state.modal,
            Some(crate::state::Modal::History { scroll: 0 })
        ));
        assert!(
            m.state.notify.toasts.is_empty(),
            "reading the history dismisses"
        );
        assert_eq!(m.state.notify.history.len(), 1, "the history keeps it");
        // The history scrolls no further than its oldest record.
        update(
            &mut m,
            Msg::ReaderBytes {
                bytes: b"j".to_vec(),
                prefix: 0x07,
            },
        );
        update(
            &mut m,
            Msg::ReaderBytes {
                bytes: b"j".to_vec(),
                prefix: 0x07,
            },
        );
        assert!(matches!(
            m.state.modal,
            Some(crate::state::Modal::History { scroll: 0 })
        ));
        update(
            &mut m,
            Msg::ReaderBytes {
                bytes: b"q".to_vec(),
                prefix: 0x07,
            },
        );
        assert!(m.state.modal.is_none(), "q closes it");
        update(&mut m, Msg::ToggleHistory);
        update(&mut m, Msg::ToggleHistory);
        assert!(m.state.modal.is_none(), "prefix m toggles it closed");
    }

    #[test]
    fn a_click_dismisses_one_toast_and_the_tick_expires_a_timed_one() {
        let mut m = model();
        let t0 = std::time::Instant::now();
        m.state.notify.toast_at(
            t0,
            "a",
            vec![crate::state::notify::Note::new(
                crate::state::notify::Level::Error,
                "boom",
            )],
        );
        m.state.notify.toast_at(
            t0,
            "b",
            vec![crate::state::notify::Note::new(
                crate::state::notify::Level::Success,
                "done",
            )],
        );
        let sticky = m.state.notify.toasts[0].id;
        update(
            &mut m,
            Msg::Tick {
                now: t0 + crate::state::notify::TOAST_TTL,
                spinner: HashSet::new(),
            },
        );
        assert_eq!(m.state.notify.toasts.len(), 1, "the timed toast left");
        update(&mut m, Msg::DismissToast(sticky));
        assert!(
            m.state.notify.toasts.is_empty(),
            "the click took the error down"
        );
    }

    #[test]
    fn the_release_notice_is_an_info_toast() {
        let mut m = model();
        update(&mut m, Msg::Notice("xmux 9.9.9 is available".to_owned()));
        let toast = &m.state.notify.toasts[0];
        assert_eq!(toast.notes[0].level, crate::state::notify::Level::Info);
        assert!(toast.until.is_some());
    }

    #[test]
    fn a_session_create_reports_its_result_as_a_toast() {
        let mut m = AppModel::from_hosts(vec!["a".to_owned()]);
        answer(&mut m, "a", &[], None);
        update(
            &mut m,
            Msg::OpResult {
                result: crate::ui::switcher::OpResult::Created {
                    session: sessions("a", &["api"]).remove(0),
                },
                logged_in: HashSet::new(),
            },
        );
        assert_eq!(note_texts(&m), ["a/tmux/api created"]);
        update(
            &mut m,
            Msg::OpResult {
                result: crate::ui::switcher::OpResult::Failed {
                    message: "create failed: boom".to_owned(),
                },
                logged_in: HashSet::new(),
            },
        );
        let failed = &m.state.notify.toasts[1];
        assert_eq!(failed.notes[0].text, "create failed: boom");
        assert!(failed.until.is_none(), "a failure waits to be dismissed");
    }

    fn sync_frame(m: &mut AppModel) -> Vec<Effect> {
        update(
            m,
            Msg::SyncFrame {
                spinner_frame: 0,
                animation_ms: 0,
                view_border_hovered: false,
                prefix_active: false,
            },
        )
    }

    #[test]
    fn a_reported_login_is_recorded_for_a_later_run_once() {
        let mut m = AppModel::from_hosts(vec!["box:tmux".into()]);
        update(
            &mut m,
            Msg::HostEvent {
                event: crate::link::HostEvent::AuthObserved {
                    machine: "box".into(),
                    method: crate::model::AuthMethod::PublicKey,
                    credential_generation: 0,
                    connection: Some("7:100.000000001".into()),
                },
                logged_in: HashSet::new(),
            },
        );
        let recorded = std::collections::HashMap::from([(
            "box".to_string(),
            crate::model::RecordedLogin {
                method: crate::model::AuthMethod::PublicKey,
                connection: Some("7:100.000000001".into()),
            },
        )]);
        assert!(
            matches!(sync_frame(&mut m).as_slice(), [Effect::PersistSshLogins(l)] if *l == recorded)
        );
        assert!(
            sync_frame(&mut m).is_empty(),
            "an unchanged record is not written again"
        );
    }

    fn seen(m: &mut AppModel, identity: &str, at: std::time::Instant) {
        update(
            m,
            Msg::HostEvent {
                event: crate::link::HostEvent::SharedConnectionSeen {
                    machine: "box".into(),
                    identity: identity.into(),
                    at,
                },
                logged_in: HashSet::new(),
            },
        );
    }

    #[test]
    fn a_login_record_takes_the_shared_connection_seen_after_it() {
        let mut m = AppModel::from_hosts(vec!["box:tmux".into()]);
        let start = std::time::Instant::now();
        m.state
            .note_auth("box".into(), crate::model::AuthMethod::PublicKey, None);
        // The re-probe after the login rides the connection the login opened.
        seen(
            &mut m,
            "7:100.000000001",
            start + std::time::Duration::from_millis(2),
        );
        assert_eq!(
            m.state.ssh_login("box", None),
            Some(crate::model::AuthMethod::PublicKey)
        );
        assert!(matches!(
            sync_frame(&mut m).as_slice(),
            [Effect::PersistSshLogins(l)] if l["box"].connection.as_deref() == Some("7:100.000000001")
        ));
        // A reading taken before that one, arriving late, does not replace it.
        seen(
            &mut m,
            "7:50.000000001",
            start + std::time::Duration::from_millis(1),
        );
        assert_eq!(m.state.shared_connections["box"].0, "7:100.000000001");
    }

    #[test]
    fn a_recorded_login_is_not_shown_for_a_newer_shared_connection() {
        // An earlier run recorded the method of the shared connection it opened. That
        // connection closed, and a channel that reports no method opened a new one.
        let mut m = AppModel::from_hosts(vec!["box:tmux".into()]);
        m.state.recorded_logins.insert(
            "box".into(),
            crate::model::RecordedLogin {
                method: crate::model::AuthMethod::Password,
                connection: Some("7:100.000000001".into()),
            },
        );
        seen(&mut m, "7:100.000000001", std::time::Instant::now());
        assert_eq!(
            m.state.ssh_login("box", None),
            Some(crate::model::AuthMethod::Password),
            "the recorded connection is the one the machine rides"
        );
        seen(&mut m, "7:200.000000002", std::time::Instant::now());
        assert_eq!(
            m.state.ssh_login("box", None),
            None,
            "a new shared connection says nothing about how it authenticated"
        );
    }

    fn logged_in_box() -> AppModel {
        let mut m = AppModel::from_hosts(vec!["box:tmux".into()]);
        m.state
            .note_auth("box".into(), crate::model::AuthMethod::Password, None);
        m.state
            .display_auth_methods
            .insert("box:tmux".into(), crate::model::AuthMethod::Password);
        m.state.live_hosts.insert("box:tmux".into());
        m.connected.insert("box:tmux".into());
        m
    }

    /// The found line for `line`, whose key is its `ssh-` word and the word after it.
    fn key_line(line: &str, marked: bool) -> crate::provision::env::MachineKeyLine {
        let mut words = line
            .split_whitespace()
            .skip_while(|w| !w.starts_with("ssh-"));
        crate::provision::env::MachineKeyLine {
            file: crate::provision::env::KeyFile::User,
            kind: words.next().unwrap().into(),
            body: words.next().unwrap().into(),
            marked,
        }
    }

    fn keys_found(result: Result<Vec<crate::provision::env::MachineKeyLine>, String>) -> Msg {
        Msg::OpResult {
            result: crate::ui::switcher::OpResult::MachineKeysFound {
                machine: "box".into(),
                result,
            },
            logged_in: HashSet::new(),
        }
    }

    fn keys_removed(result: Result<(), String>) -> Msg {
        Msg::OpResult {
            result: crate::ui::switcher::OpResult::MachineKeysRemoved {
                machine: "box".into(),
                result,
            },
            logged_in: HashSet::new(),
        }
    }

    fn entries_found(result: Result<Vec<crate::provision::config::RemovedEntry>, String>) -> Msg {
        Msg::OpResult {
            result: crate::ui::switcher::OpResult::SshConfigEntriesFound {
                machine: "box".into(),
                result,
            },
            logged_in: HashSet::new(),
        }
    }

    /// The key search answering `keys`, then the ssh config search answering `entries`:
    /// both come before anything is removed or asked.
    fn find_with(
        m: &mut AppModel,
        keys: Msg,
        entries: Vec<crate::provision::config::RemovedEntry>,
    ) -> Vec<Effect> {
        let effects = update(m, keys);
        assert!(
            matches!(effects.as_slice(), [Effect::FindSshConfigEntries { machine }] if machine == "box"),
            "{effects:?}"
        );
        assert!(m.state.modal.is_none());
        update(m, entries_found(Ok(entries)))
    }

    /// The key search answering `keys` on a machine no ssh config entry xmux did not
    /// write names.
    fn find(m: &mut AppModel, keys: Msg) -> Vec<Effect> {
        find_with(m, keys, Vec::new())
    }

    fn user_entry() -> crate::provision::config::RemovedEntry {
        crate::provision::config::RemovedEntry {
            header: "Host gpu-01 box".into(),
            after: Some("Host gpu-01".into()),
        }
    }

    fn unmarked_of(effects: &[Effect]) -> bool {
        let [Effect::RemoveSshConfigEntries { unmarked, .. }] = effects else {
            panic!("{effects:?}");
        };
        *unmarked
    }

    fn type_remove(m: &mut AppModel) -> Vec<Effect> {
        for c in "remove".chars() {
            assert!(press(m, KeyCode::Char(c)).is_empty());
        }
        press(m, KeyCode::Enter)
    }

    fn stanza_removed(result: Result<Vec<crate::provision::config::RemovedEntry>, String>) -> Msg {
        Msg::OpResult {
            result: crate::ui::switcher::OpResult::SshConfigEntriesRemoved {
                machine: "box".into(),
                result,
            },
            logged_in: HashSet::new(),
        }
    }

    /// The ssh config step every logout ends with: the machine is still there while its
    /// ssh config entries are removed, and `result` is what the removal answers.
    fn stanza_step(
        m: &mut AppModel,
        effects: Vec<Effect>,
        result: Result<Vec<crate::provision::config::RemovedEntry>, String>,
    ) -> Vec<Effect> {
        assert!(
            matches!(effects.as_slice(), [Effect::RemoveSshConfigEntries { machine, .. }] if machine == "box"),
            "{effects:?}"
        );
        assert!(m.logout.is_some());
        assert!(!m.state.invalid_auth.contains("box"));
        update(m, stanza_removed(result))
    }

    /// The ssh config step of a machine no ssh config entry names.
    fn no_stanza(m: &mut AppModel, effects: Vec<Effect>) -> Vec<Effect> {
        stanza_step(m, effects, Ok(Vec::new()))
    }

    fn start_logout(m: &mut AppModel) {
        let effects = update(
            m,
            Msg::Commands(vec![crate::model::Command::Logout("box".into())]),
        );
        assert!(
            matches!(effects.as_slice(), [Effect::FindMachineKeys { machine, .. }] if machine == "box"),
            "{effects:?}"
        );
    }

    fn assert_still_logged_in(m: &AppModel) {
        assert!(m.state.auth_methods.contains_key("box"));
        assert!(m.state.live_hosts.contains("box:tmux"));
        assert!(m.connected.contains("box:tmux"));
        assert!(!m.state.invalid_auth.contains("box"));
    }

    fn assert_logged_out(m: &AppModel) {
        assert!(!m.state.auth_methods.contains_key("box"));
        assert!(m.state.display_auth_methods.is_empty());
        assert!(!m.state.recorded_logins.contains_key("box"));
        assert!(m.state.invalid_auth.contains("box"));
        assert!(m.state.live_hosts.is_empty());
        assert!(m.connected.is_empty());
        assert!(m.logout.is_none());
        assert_eq!(
            m.state.groups[0].err.as_deref(),
            Some(crate::model::LOGGED_OUT)
        );
    }

    fn logout_notes(m: &AppModel) -> Vec<(crate::state::notify::Level, String)> {
        let toast = m.state.notify.toasts.last().expect("the logout reports");
        assert_eq!(toast.title, "logout box");
        toast
            .notes
            .iter()
            .map(|note| (note.level, note.text.clone()))
            .collect()
    }

    fn press(m: &mut AppModel, code: KeyCode) -> Vec<Effect> {
        update(m, Msg::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    /// The key comes off the machine while the connection the login left is still there,
    /// so nothing of the machine's is cleared until the removal answered.
    #[test]
    fn logout_removes_the_keys_xmux_added_before_it_clears_the_machine() {
        let mut m = logged_in_box();
        start_logout(&mut m);
        assert_still_logged_in(&m);
        let found = vec![key_line("ssh-ed25519 AAAAkey me xmux-registered", true)];
        let effects = find(&mut m, keys_found(Ok(found.clone())));
        assert!(
            matches!(effects.as_slice(), [Effect::RemoveMachineKeys { machine, lines }] if machine == "box" && *lines == found),
            "{effects:?}"
        );
        assert!(
            m.state.modal.is_none(),
            "lines xmux added need no second confirmation"
        );
        assert_still_logged_in(&m);
        let effects = update(&mut m, keys_removed(Ok(())));
        let effects = no_stanza(&mut m, effects);
        assert!(
            matches!(effects.as_slice(), [Effect::LogoutMachine { machine, .. }] if machine == "box"),
            "{effects:?}"
        );
        assert_logged_out(&m);
        assert_eq!(
            logout_notes(&m),
            vec![(
                crate::state::notify::Level::Success,
                "this PC's key removed from box".to_string()
            )]
        );
    }

    #[test]
    fn a_machine_holding_no_key_of_this_pc_logs_out_at_once() {
        let mut m = logged_in_box();
        start_logout(&mut m);
        let effects = find(&mut m, keys_found(Ok(Vec::new())));
        let effects = no_stanza(&mut m, effects);
        assert!(matches!(effects.as_slice(), [Effect::LogoutMachine { .. }]));
        assert_logged_out(&m);
    }

    /// A line xmux did not add may be how the user reaches the machine outside xmux, so it
    /// goes only after a second confirmation says so.
    #[test]
    fn a_key_xmux_did_not_add_asks_again_and_confirming_removes_it_too() {
        let mut m = logged_in_box();
        start_logout(&mut m);
        let found = vec![
            key_line("ssh-ed25519 AAAAkey me xmux-registered", true),
            key_line("no-pty ssh-ed25519 AAAAkey me", false),
        ];
        let effects = find(&mut m, keys_found(Ok(found.clone())));
        assert!(effects.is_empty(), "{effects:?}");
        let Some(crate::state::Modal::Input(input)) = m.state.modal.as_ref() else {
            panic!("the second confirmation opens");
        };
        assert!(input.mode == crate::state::InputMode::LogoutKeys);
        assert_eq!(
            input.facts[0].1,
            "1 line of this PC's key not added by xmux"
        );
        assert_eq!(input.facts[3].1, "only the 1 line xmux added go");
        assert_still_logged_in(&m);
        press(&mut m, KeyCode::Enter);
        assert!(m.state.modal.is_some(), "Enter alone does not confirm");
        for c in "remove".chars() {
            assert!(press(&mut m, KeyCode::Char(c)).is_empty());
        }
        let effects = press(&mut m, KeyCode::Enter);
        assert!(
            matches!(effects.as_slice(), [Effect::RemoveMachineKeys { lines, .. }] if *lines == found),
            "{effects:?}"
        );
        assert_still_logged_in(&m);
        let effects = update(&mut m, keys_removed(Ok(())));
        let effects = no_stanza(&mut m, effects);
        assert!(matches!(effects.as_slice(), [Effect::LogoutMachine { .. }]));
        assert_logged_out(&m);
        assert_eq!(
            logout_notes(&m),
            vec![(
                crate::state::notify::Level::Success,
                "this PC's key removed from box".to_string()
            )]
        );
    }

    /// The second confirmation is the logout's question, so it waits behind a popup the
    /// user opened while the keys were searched for rather than taking its place.
    #[test]
    fn the_second_confirmation_waits_for_a_popup_the_user_opened() {
        let mut m = logged_in_box();
        start_logout(&mut m);
        m.switcher.toggle_help(&mut m.state);
        let found = vec![
            key_line("ssh-ed25519 AAAAkey me xmux-registered", true),
            key_line("no-pty ssh-ed25519 AAAAkey me", false),
        ];
        let effects = update(&mut m, keys_found(Ok(found)));
        assert!(matches!(
            effects.as_slice(),
            [Effect::FindSshConfigEntries { .. }]
        ));
        let effects = update(&mut m, entries_found(Ok(Vec::new())));
        assert!(effects.is_empty(), "{effects:?}");
        assert!(
            matches!(m.state.modal, Some(crate::state::Modal::Help { .. })),
            "the help the user opened stays"
        );
        assert_still_logged_in(&m);
        m.switcher.toggle_help(&mut m.state);
        let effects = update(&mut m, Msg::SyncSelection);
        assert!(
            effects.is_empty(),
            "the waiting question is no decline: {effects:?}"
        );
        let Some(crate::state::Modal::Input(input)) = m.state.modal.as_ref() else {
            panic!("the second confirmation opens once the help closes");
        };
        assert!(input.mode == crate::state::InputMode::LogoutKeys);
    }

    #[test]
    fn declining_the_second_confirmation_removes_only_the_keys_xmux_added() {
        let mut m = logged_in_box();
        start_logout(&mut m);
        let marked = key_line("ssh-ed25519 AAAAkey me xmux-registered", true);
        let found = vec![marked.clone(), key_line("ssh-ed25519 AAAAkey me", false)];
        find(&mut m, keys_found(Ok(found)));
        let effects = press(&mut m, KeyCode::Esc);
        assert!(
            matches!(effects.as_slice(), [Effect::RemoveMachineKeys { lines, .. }] if *lines == vec![marked.clone()]),
            "{effects:?}"
        );
        assert_still_logged_in(&m);
        let effects = update(&mut m, keys_removed(Ok(())));
        let effects = no_stanza(&mut m, effects);
        assert!(matches!(effects.as_slice(), [Effect::LogoutMachine { .. }]));
        assert_logged_out(&m);
        assert_eq!(
            logout_notes(&m),
            vec![
                (
                    crate::state::notify::Level::Success,
                    "this PC's key removed from box".to_string()
                ),
                (
                    crate::state::notify::Level::Info,
                    "this PC's key that xmux did not add stays on box".to_string()
                ),
            ]
        );
    }

    /// A confirmation that closes any way but confirming is a decline, so a logout never
    /// waits on a question nobody can see.
    #[test]
    fn a_second_confirmation_another_screen_replaced_keeps_the_key_and_logs_out() {
        let mut m = logged_in_box();
        start_logout(&mut m);
        find(
            &mut m,
            keys_found(Ok(vec![key_line("ssh-ed25519 AAAAkey me", false)])),
        );
        let effects = update(&mut m, Msg::ToggleHelp);
        let effects = no_stanza(&mut m, effects);
        assert!(
            matches!(effects.as_slice(), [Effect::LogoutMachine { .. }]),
            "nothing xmux added is left to remove: {effects:?}"
        );
        assert_logged_out(&m);
        assert_eq!(
            logout_notes(&m),
            vec![(
                crate::state::notify::Level::Info,
                "this PC's key that xmux did not add stays on box".to_string()
            )]
        );
    }

    #[test]
    fn an_unreachable_machine_still_logs_out_and_reports_the_key_was_not_removed() {
        let mut m = logged_in_box();
        start_logout(&mut m);
        let effects = find(
            &mut m,
            keys_found(Err(
                "ssh: connect to host box port 22: Connection timed out".into(),
            )),
        );
        let effects = no_stanza(&mut m, effects);
        assert!(matches!(effects.as_slice(), [Effect::LogoutMachine { .. }]));
        assert_logged_out(&m);
        assert_eq!(
            logout_notes(&m),
            vec![(
                crate::state::notify::Level::Warning,
                "this PC's key was not removed from box: ssh: connect to host box port 22: Connection timed out"
                    .to_string()
            )]
        );
    }

    #[test]
    fn a_failed_removal_still_logs_out_and_reports_the_key_remains() {
        let mut m = logged_in_box();
        start_logout(&mut m);
        find(
            &mut m,
            keys_found(Ok(vec![key_line(
                "ssh-ed25519 AAAAkey xmux-registered",
                true,
            )])),
        );
        let effects = update(&mut m, keys_removed(Err("Permission denied".into())));
        let effects = no_stanza(&mut m, effects);
        assert!(matches!(effects.as_slice(), [Effect::LogoutMachine { .. }]));
        assert_logged_out(&m);
        assert_eq!(
            logout_notes(&m),
            vec![(
                crate::state::notify::Level::Warning,
                "this PC's key remains on box: Permission denied".to_string()
            )]
        );
    }

    /// The ssh config entries naming the machine go once the key steps settled, because they
    /// reach the machine through them, and the toast names each one after the key.
    #[test]
    fn logout_removes_the_ssh_config_entries_after_the_key() {
        let mut m = logged_in_box();
        start_logout(&mut m);
        find(
            &mut m,
            keys_found(Ok(vec![key_line(
                "ssh-ed25519 AAAAkey xmux-registered",
                true,
            )])),
        );
        let effects = update(&mut m, keys_removed(Ok(())));
        assert!(!unmarked_of(&effects), "only xmux's stanza goes unasked");
        let effects = stanza_step(
            &mut m,
            effects,
            Ok(vec![
                crate::provision::config::RemovedEntry {
                    header: "Host box".into(),
                    after: None,
                },
                user_entry(),
            ]),
        );
        assert!(
            matches!(effects.as_slice(), [Effect::LogoutMachine { .. }, Effect::ReadSshFacts { machine }] if machine == "box"),
            "the removed entries change what ssh config says about box: {effects:?}"
        );
        assert_logged_out(&m);
        assert_eq!(
            logout_notes(&m),
            vec![
                (
                    crate::state::notify::Level::Success,
                    "this PC's key removed from box".to_string()
                ),
                (
                    crate::state::notify::Level::Success,
                    "ssh config: removed Host box; removed box from Host gpu-01 box".to_string()
                ),
            ]
        );
    }

    /// An ssh config entry xmux did not write may be how the user reaches the machine
    /// outside xmux, so it changes only after the second confirmation says so.
    #[test]
    fn an_ssh_config_entry_xmux_did_not_write_asks_and_confirming_removes_it() {
        let mut m = logged_in_box();
        start_logout(&mut m);
        let effects = find_with(&mut m, keys_found(Ok(Vec::new())), vec![user_entry()]);
        assert!(effects.is_empty(), "{effects:?}");
        let Some(crate::state::Modal::Input(input)) = m.state.modal.as_ref() else {
            panic!("the second confirmation opens");
        };
        assert!(input.mode == crate::state::InputMode::LogoutKeys);
        assert_eq!(
            input.facts[0],
            (
                "ssh config",
                "Host gpu-01 box becomes Host gpu-01".to_string()
            )
        );
        assert_still_logged_in(&m);
        let effects = type_remove(&mut m);
        assert!(unmarked_of(&effects), "{effects:?}");
        let effects = stanza_step(&mut m, effects, Ok(vec![user_entry()]));
        assert!(
            matches!(effects.as_slice(), [Effect::LogoutMachine { .. }, Effect::ReadSshFacts { machine }] if machine == "box"),
            "{effects:?}"
        );
        assert_logged_out(&m);
        assert_eq!(
            logout_notes(&m),
            vec![
                (
                    crate::state::notify::Level::Info,
                    "box holds no key of this PC".to_string()
                ),
                (
                    crate::state::notify::Level::Success,
                    "ssh config: removed box from Host gpu-01 box".to_string()
                ),
            ]
        );
    }

    #[test]
    fn declining_keeps_the_ssh_config_entries_xmux_did_not_write() {
        let mut m = logged_in_box();
        start_logout(&mut m);
        find_with(&mut m, keys_found(Ok(Vec::new())), vec![user_entry()]);
        let effects = press(&mut m, KeyCode::Esc);
        assert!(!unmarked_of(&effects), "{effects:?}");
        let effects = no_stanza(&mut m, effects);
        assert!(matches!(effects.as_slice(), [Effect::LogoutMachine { .. }]));
        assert_logged_out(&m);
        assert_eq!(
            logout_notes(&m),
            vec![
                (
                    crate::state::notify::Level::Info,
                    "box holds no key of this PC".to_string()
                ),
                (
                    crate::state::notify::Level::Info,
                    "ssh config entries xmux did not add stay: Host gpu-01 box".to_string()
                ),
            ]
        );
    }

    /// A key line and an ssh config entry xmux did not add share one confirmation, and
    /// its answer decides both.
    #[test]
    fn keys_and_entries_xmux_did_not_add_share_one_confirmation() {
        let marked = key_line("ssh-ed25519 AAAAkey me xmux-registered", true);
        let found = vec![marked.clone(), key_line("ssh-ed25519 AAAAkey me", false)];
        for confirm in [true, false] {
            let mut m = logged_in_box();
            start_logout(&mut m);
            let effects = find_with(&mut m, keys_found(Ok(found.clone())), vec![user_entry()]);
            assert!(effects.is_empty(), "{effects:?}");
            let Some(crate::state::Modal::Input(input)) = m.state.modal.as_ref() else {
                panic!("one confirmation opens");
            };
            let labels: Vec<&str> = input.facts.iter().map(|(label, _)| *label).collect();
            assert_eq!(
                labels,
                vec!["key", "file", "ssh config", "remove", "keep", "logout"]
            );
            let effects = if confirm {
                type_remove(&mut m)
            } else {
                press(&mut m, KeyCode::Esc)
            };
            let want = if confirm {
                found.clone()
            } else {
                vec![marked.clone()]
            };
            assert!(
                matches!(effects.as_slice(), [Effect::RemoveMachineKeys { lines, .. }] if *lines == want),
                "{effects:?}"
            );
            assert!(m.state.modal.is_none(), "no second question follows");
            let effects = update(&mut m, keys_removed(Ok(())));
            assert_eq!(unmarked_of(&effects), confirm);
            let removed = if confirm {
                vec![user_entry()]
            } else {
                Vec::new()
            };
            let effects = stanza_step(&mut m, effects, Ok(removed));
            // Only an entry that went changes what ssh config says about the machine.
            let read = matches!(effects.as_slice(), [Effect::LogoutMachine { .. }, Effect::ReadSshFacts { machine }] if machine == "box");
            assert!(
                read == confirm
                    && (confirm || matches!(effects.as_slice(), [Effect::LogoutMachine { .. }])),
                "{effects:?}"
            );
            assert_logged_out(&m);
            let notes: Vec<String> = logout_notes(&m).into_iter().map(|(_, text)| text).collect();
            let want: Vec<&str> = if confirm {
                vec![
                    "this PC's key removed from box",
                    "ssh config: removed box from Host gpu-01 box",
                ]
            } else {
                vec![
                    "this PC's key removed from box",
                    "this PC's key that xmux did not add stays on box",
                    "ssh config entries xmux did not add stay: Host gpu-01 box",
                ]
            };
            assert_eq!(notes, want);
        }
    }

    #[test]
    fn an_ssh_config_that_cannot_be_rewritten_still_logs_out_and_says_why() {
        let mut m = logged_in_box();
        start_logout(&mut m);
        let effects = find(&mut m, keys_found(Ok(Vec::new())));
        let effects = stanza_step(&mut m, effects, Err("Access is denied.".into()));
        assert!(matches!(effects.as_slice(), [Effect::LogoutMachine { .. }]));
        assert_logged_out(&m);
        assert_eq!(
            logout_notes(&m),
            vec![
                (
                    crate::state::notify::Level::Info,
                    "box holds no key of this PC".to_string()
                ),
                (
                    crate::state::notify::Level::Warning,
                    "ssh config entries for box remain: Access is denied.".to_string()
                ),
            ]
        );
    }

    #[test]
    fn a_second_logout_waits_for_the_first() {
        let mut m = logged_in_box();
        start_logout(&mut m);
        let effects = update(
            &mut m,
            Msg::Commands(vec![crate::model::Command::Logout("box".into())]),
        );
        assert!(effects.is_empty(), "{effects:?}");
        assert_eq!(
            m.state.notify.last_report(),
            Some((
                "logout box",
                crate::state::notify::Level::Warning,
                "a logout is still running"
            ))
        );
    }

    #[test]
    fn logout_cancels_its_login_and_rejects_a_late_success() {
        let (mut m, attempt) = submitted_login(&["box"]);
        let effects = update(
            &mut m,
            Msg::Commands(vec![crate::model::Command::Logout("box".into())]),
        );
        assert!(matches!(
            effects.as_slice(),
            [Effect::FindMachineKeys { cancel_login, .. }] if cancel_login.len() == 1
        ));
        assert!(m.state.login_run.is_none());
        assert!(!m.state.login_progress.contains_key("box"));
        let effects = update(
            &mut m,
            login_result("box", attempt, crate::link::unlock::UnlockOutcome::Ok),
        );
        assert!(effects.is_empty());
        let effects = find(&mut m, keys_found(Ok(Vec::new())));
        no_stanza(&mut m, effects);
        assert!(m.state.invalid_auth.contains("box"));
        assert!(!m.state.auth_methods.contains_key("box"));
        assert_eq!(
            m.state.groups[0].err.as_deref(),
            Some(crate::model::LOGGED_OUT)
        );
    }

    /// A login submitted on another machine takes the pane's handle, but a logout still
    /// cancels the login on its own machine, so that login registers no key afterwards.
    #[test]
    fn logout_cancels_its_machines_login_after_another_machine_logged_in() {
        let mut m = AppModel::from_hosts(vec!["a:tmux".into(), "b:tmux".into()]);
        let run = |machine: &str| crate::model::Command::RunLogin {
            host: format!("{machine}:tmux"),
            login: crate::transport::Login::default(),
            password: Default::default(),
            after_login: crate::model::AfterLogin::RegisterKey,
        };
        update(&mut m, Msg::Commands(vec![run("a")]));
        update(&mut m, Msg::Commands(vec![run("b")]));
        assert_eq!(
            m.state.login_run.as_ref().map(|r| r.host.as_str()),
            Some("b:tmux")
        );
        let effects = update(
            &mut m,
            Msg::Commands(vec![crate::model::Command::Logout("a".into())]),
        );
        let [Effect::FindMachineKeys { cancel_login, .. }] = effects.as_slice() else {
            panic!("{effects:?}");
        };
        let cancelled: Vec<_> = cancel_login.iter().map(|r| r.host.as_str()).collect();
        assert_eq!(cancelled, vec!["a:tmux"]);
        assert_eq!(
            m.state.login_run.as_ref().map(|r| r.host.as_str()),
            Some("b:tmux"),
            "the other machine's login keeps running"
        );
        assert_eq!(m.running_logins.len(), 1);
    }

    /// The info view shows the key report of the machine's latest login, so neither a
    /// later login that registers nothing nor a logout keeps an earlier "registered".
    #[test]
    fn a_login_without_registration_and_a_logout_clear_the_key_report() {
        let mut m = logged_in_box();
        m.state.registration_reports.insert(
            "box".into(),
            crate::ui::ops::RegistrationOutcome::Registered,
        );
        update(
            &mut m,
            Msg::Commands(vec![crate::model::Command::RunLogin {
                host: "box:tmux".into(),
                login: crate::transport::Login::default(),
                password: Default::default(),
                after_login: crate::model::AfterLogin::SshConfig,
            }]),
        );
        assert!(m.state.registration_reports.is_empty());

        let mut m = logged_in_box();
        m.state.registration_reports.insert(
            "box".into(),
            crate::ui::ops::RegistrationOutcome::Registered,
        );
        start_logout(&mut m);
        let effects = find(&mut m, keys_found(Ok(Vec::new())));
        no_stanza(&mut m, effects);
        assert!(m.state.registration_reports.is_empty());
    }

    #[test]
    fn a_login_on_a_machine_being_logged_out_is_refused() {
        let mut m = logged_in_box();
        start_logout(&mut m);
        let effects = update(
            &mut m,
            Msg::Commands(vec![crate::model::Command::RunLogin {
                host: "box:tmux".into(),
                login: crate::transport::Login::default(),
                password: Default::default(),
                after_login: crate::model::AfterLogin::RegisterKey,
            }]),
        );
        assert!(effects.is_empty(), "{effects:?}");
        assert_eq!(
            m.state.notify.last_report(),
            Some((
                "box",
                crate::state::notify::Level::Warning,
                "a logout of box is running"
            ))
        );
        assert!(m.running_logins.is_empty());
        let effects = find(&mut m, keys_found(Ok(Vec::new())));
        no_stanza(&mut m, effects);
        let effects = update(
            &mut m,
            Msg::Commands(vec![crate::model::Command::RunLogin {
                host: "box:tmux".into(),
                login: crate::transport::Login::default(),
                password: Default::default(),
                after_login: crate::model::AfterLogin::RegisterKey,
            }]),
        );
        assert!(
            matches!(effects.as_slice(), [Effect::StartLogin { .. }]),
            "a login is taken once the logout ended: {effects:?}"
        );
    }

    #[test]
    fn losing_a_held_password_disconnects_the_machine() {
        let mut m = AppModel::from_hosts(vec!["box:tmux".into()]);
        m.state
            .auth_methods
            .insert("box".into(), crate::model::AuthMethod::Password);
        m.state.live_hosts.insert("box:tmux".into());
        m.connected.insert("box:tmux".into());
        let effects = update(
            &mut m,
            Msg::CredentialInventory {
                held: HashSet::new(),
            },
        );
        assert!(matches!(
            effects.as_slice(),
            [Effect::Event(EventEffect::DisconnectMachine { machine })] if machine == "box"
        ));
        assert!(!m.state.auth_methods.contains_key("box"));
        assert!(m.state.invalid_auth.contains("box"));
        assert!(m.state.live_hosts.is_empty());
        assert!(m.connected.is_empty());
        assert_eq!(
            m.state.groups[0].err.as_deref(),
            Some("SSH password no longer held; log in again")
        );
    }

    #[test]
    fn scanning_card_stops_after_ten_seconds_and_rescan_gets_a_new_budget() {
        let mut m = AppModel::from_hosts(vec!["box".to_owned()]);
        let started = std::time::Instant::now();
        update(
            &mut m,
            Msg::Tick {
                now: started,
                spinner: HashSet::new(),
            },
        );
        assert!(m.state.scanning.contains("box"));
        update(
            &mut m,
            Msg::Tick {
                now: started + std::time::Duration::from_secs(10),
                spinner: HashSet::new(),
            },
        );
        assert!(!m.state.scanning.contains("box"));
        assert_eq!(
            m.state.groups[0].err.as_deref(),
            Some("scan timed out after 10s")
        );
        m.switcher.mark_scanning("box", &mut m.state);
        update(
            &mut m,
            Msg::Tick {
                now: started + std::time::Duration::from_secs(11),
                spinner: HashSet::new(),
            },
        );
        assert!(m.state.scanning.contains("box"));
    }

    /// A model with `hosts` blocked by a refused probe, then a login submitted on the
    /// first of them through the pane's own keys. Returns the submission's attempt.
    fn submitted_login(hosts: &[&str]) -> (AppModel, u64) {
        let mut m = AppModel::from_hosts(hosts.iter().map(|s| (*s).to_owned()).collect());
        for host in hosts {
            update(
                &mut m,
                Msg::HostEvent {
                    event: crate::link::HostEvent::Sessions {
                        host: (*host).to_owned(),
                        sessions: Vec::new(),
                        err: Some("alice@box: Permission denied (publickey,password).".to_owned()),
                    },
                    logged_in: HashSet::new(),
                },
            );
        }
        // Enter walks every stop to the button and submits there.
        let effects = update(
            &mut m,
            Msg::FeedLogin {
                host: hosts[0].to_owned(),
                bytes: b"\r\ralice\r\r\r\r\r\r".to_vec(),
            },
        );
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::StartLogin { .. })),
            "the button submits"
        );
        let attempt = m.state.login_attempts;
        (m, attempt)
    }

    fn step(m: &AppModel, host: &str, step: crate::model::LoginStep) -> crate::model::StepState {
        m.state.login_progress[host]
            .state_of(step)
            .expect("the step is listed")
    }

    fn login_event(host: &str, attempt: u64, event: crate::model::LoginEvent) -> Msg {
        Msg::OpResult {
            result: crate::ui::switcher::OpResult::LoginProgress {
                host: host.to_owned(),
                attempt,
                event,
            },
            logged_in: HashSet::new(),
        }
    }

    fn login_result(host: &str, attempt: u64, connect: crate::link::unlock::UnlockOutcome) -> Msg {
        Msg::OpResult {
            result: crate::ui::switcher::OpResult::Login {
                host: host.to_owned(),
                login: crate::transport::Login::default(),
                attempt,
                outcome: crate::ui::ops::LoginOutcome {
                    auth_method: None,
                    connect,
                    output: String::new(),
                    saved: None,
                    registration: crate::ui::ops::RegistrationOutcome::NotRequested,
                },
            },
            logged_in: HashSet::new(),
        }
    }

    /// A login that saved its connection changed ssh config, so the machine's ssh facts
    /// are read again, whether or not the login is still the current one. A login that
    /// saved nothing leaves them.
    #[test]
    fn a_login_that_saved_its_connection_reads_the_machines_ssh_facts_again() {
        for (saved, current) in [
            (Some(Ok(())), true),
            (Some(Ok(())), false),
            (Some(Err("denied".to_owned())), true),
            (None, true),
        ] {
            let (mut m, attempt) = submitted_login(&["box"]);
            let mut msg = login_result(
                "box",
                if current { attempt } else { attempt + 1 },
                crate::link::unlock::UnlockOutcome::Ok,
            );
            let Msg::OpResult {
                result: crate::ui::switcher::OpResult::Login { outcome, .. },
                ..
            } = &mut msg
            else {
                unreachable!()
            };
            outcome.saved = saved.clone();
            let effects = update(&mut m, msg);
            let read = effects
                .iter()
                .filter(
                    |effect| matches!(effect, Effect::ReadSshFacts { machine } if machine == "box"),
                )
                .count();
            assert_eq!(
                read,
                usize::from(saved == Some(Ok(()))),
                "{saved:?} {current}: {effects:?}"
            );
        }
    }

    /// Re-read ssh facts reach what the login pane and the machine screen render.
    #[test]
    fn re_read_ssh_facts_replace_the_machines_login_values_and_stanza() {
        let mut m = AppModel::from_hosts(vec!["box".to_owned()]);
        let text = "Host box
    User dev
";
        let facts = crate::provision::env::SshFacts {
            defaults: crate::provision::config::login_defaults("box", None, None, text),
            stanza: crate::provision::config::host_stanza(text, "box"),
        };
        let effects = update(
            &mut m,
            Msg::OpResult {
                result: crate::ui::switcher::OpResult::SshFactsRead {
                    machine: "box".into(),
                    facts: facts.clone(),
                },
                logged_in: HashSet::new(),
            },
        );
        assert!(effects.is_empty(), "{effects:?}");
        assert_eq!(m.state.chrome.login_defaults("box"), facts.defaults);
        assert_eq!(m.state.chrome.ssh_stanzas["box"], facts.stanza);
    }

    fn probed(machine: &str, probe: u64, err: Option<&str>) -> Msg {
        Msg::HostEvent {
            event: crate::link::HostEvent::MachineProbed {
                machine: machine.to_owned(),
                err: err.map(str::to_owned),
                shell: None,
                password_supplied: false,
                credential_rejection_generation: None,
                credential_held: false,
                credential_generation: 0,
                current_credential_generation: 0,
                rescan: false,
                probe,
            },
            logged_in: HashSet::new(),
        }
    }

    /// A working login whose re-probe got `probe` and found the machine answering.
    fn logged_in_awaiting_mux(hosts: &[&str], probe: u64) -> AppModel {
        let (mut m, attempt) = submitted_login(hosts);
        let host = hosts[0];
        update(
            &mut m,
            login_result(host, attempt, crate::link::unlock::UnlockOutcome::Ok),
        );
        update(
            &mut m,
            Msg::LoginSettled {
                host: host.to_owned(),
                credential_held: false,
                machine_has_hosts: true,
                probe,
            },
        );
        update(
            &mut m,
            probed(crate::session::machine_of(host), probe, None),
        );
        m
    }

    #[test]
    fn login_steps_advance_from_the_events_the_login_reports() {
        use crate::model::{LoginEvent, LoginStep, StepState};
        let (mut m, attempt) = submitted_login(&["pwbox"]);
        assert_eq!(step(&m, "pwbox", LoginStep::Connect), StepState::Running);
        assert_eq!(
            step(&m, "pwbox", LoginStep::Authenticate),
            StepState::Pending
        );

        update(
            &mut m,
            login_event("pwbox", attempt, LoginEvent::PasswordAsked),
        );
        assert_eq!(step(&m, "pwbox", LoginStep::Connect), StepState::Done);
        assert_eq!(
            step(&m, "pwbox", LoginStep::Authenticate),
            StepState::Running
        );

        let effects = update(
            &mut m,
            login_result("pwbox", attempt, crate::link::unlock::UnlockOutcome::Ok),
        );
        assert!(effects
            .iter()
            .any(|e| matches!(e, Effect::LoginApplied { .. })));
        assert_eq!(step(&m, "pwbox", LoginStep::Authenticate), StepState::Done);
        assert_eq!(step(&m, "pwbox", LoginStep::FindMux), StepState::Running);

        update(
            &mut m,
            Msg::LoginSettled {
                host: "pwbox".to_owned(),
                credential_held: false,
                machine_has_hosts: true,
                probe: 9,
            },
        );
        // A probe already in flight, and the host answer it leads to, came before the
        // login's own probe: neither settles the search.
        update(&mut m, probed("pwbox", 0, None));
        answer(&mut m, "pwbox", &["work"], None);
        assert_eq!(step(&m, "pwbox", LoginStep::FindMux), StepState::Running);

        update(&mut m, probed("pwbox", 9, None));
        answer(&mut m, "pwbox", &["work"], None);
        assert!(
            !m.state.login_progress.contains_key("pwbox"),
            "a mux answered, so the steps leave with the pane"
        );
    }

    #[test]
    fn detection_that_finds_no_mux_settles_the_search() {
        use crate::model::{LoginStep, StepState};
        // The host was blocked at launch, so it is undetected and no longer scanning:
        // detection's answer is the only one it gets.
        let mut m = logged_in_awaiting_mux(&["pwbox"], 3);
        m.state.scanning.clear();
        update(
            &mut m,
            Msg::HostEvent {
                event: crate::link::HostEvent::Scanned {
                    host: "pwbox".to_owned(),
                    detected: None,
                    err: None,
                },
                logged_in: HashSet::new(),
            },
        );
        assert_eq!(step(&m, "pwbox", LoginStep::FindMux), StepState::Failed);
        assert!(
            !m.state.login_progress["pwbox"].running(),
            "nothing keeps the frame redrawing"
        );

        let mut m = logged_in_awaiting_mux(&["pwbox"], 3);
        m.state.scanning.clear();
        update(
            &mut m,
            Msg::HostEvent {
                event: crate::link::HostEvent::Scanned {
                    host: "pwbox".to_owned(),
                    detected: None,
                    err: Some("tmux: command not found".to_owned()),
                },
                logged_in: HashSet::new(),
            },
        );
        assert_eq!(step(&m, "pwbox", LoginStep::FindMux), StepState::Failed);
        assert_eq!(
            m.state.login_progress["pwbox"].steps[2].note.as_deref(),
            Some("tmux: command not found")
        );
    }

    #[test]
    fn discovery_that_finds_no_mux_settles_the_search_and_the_card_takes_its_steps() {
        use crate::model::{LoginStep, StepState};
        let mut m = logged_in_awaiting_mux(&["pwbox"], 3);
        update(
            &mut m,
            Msg::HostEvent {
                event: crate::link::HostEvent::MuxesFound {
                    machine: "pwbox".to_owned(),
                    muxes: Ok(Vec::new()),
                },
                logged_in: HashSet::new(),
            },
        );
        assert_eq!(step(&m, "pwbox", LoginStep::FindMux), StepState::Failed);
        update(
            &mut m,
            Msg::RemoveHost {
                host: "pwbox".to_owned(),
                clear_tracking: false,
            },
        );
        assert!(m.state.login_progress.is_empty());
    }

    #[test]
    fn steps_belong_to_one_submission_on_one_host() {
        use crate::link::unlock::{FailureKind, UnlockOutcome};
        use crate::model::{LoginEvent, LoginStep, StepState};
        let (mut m, first) = submitted_login(&["box:tmux", "box:zellij"]);
        assert!(
            !m.state.login_progress.contains_key("box:zellij"),
            "another card on the machine has no steps of this login"
        );
        // A second submission replaces the first; the first's late reports change
        // nothing, and its result leaves the newer running handle in place.
        update(
            &mut m,
            Msg::FeedLogin {
                host: "box:tmux".to_owned(),
                bytes: b"\r".to_vec(),
            },
        );
        let second = m.state.login_attempts;
        assert_ne!(first, second);
        update(
            &mut m,
            login_event("box:tmux", first, LoginEvent::PasswordAsked),
        );
        assert_eq!(step(&m, "box:tmux", LoginStep::Connect), StepState::Running);
        update(
            &mut m,
            login_result(
                "box:tmux",
                first,
                UnlockOutcome::Failed {
                    kind: FailureKind::Cancelled,
                    reason: "cancelled".into(),
                },
            ),
        );
        assert_eq!(step(&m, "box:tmux", LoginStep::Connect), StepState::Running);
        assert_eq!(
            m.state.login_run.as_ref().map(|run| run.attempt),
            Some(second)
        );
    }

    #[test]
    fn settled_steps_leave_once_the_machine_is_looked_at_again() {
        use crate::link::unlock::{FailureKind, UnlockOutcome};
        let (mut m, attempt) = submitted_login(&["pwbox"]);
        update(
            &mut m,
            login_result(
                "pwbox",
                attempt,
                UnlockOutcome::Failed {
                    kind: FailureKind::WrongPassword,
                    reason: "the password was refused".into(),
                },
            ),
        );
        assert!(m.state.login_progress.contains_key("pwbox"));
        // A probe the login did not start is a newer look at the machine.
        update(
            &mut m,
            probed(
                "pwbox",
                0,
                Some("alice@box: Permission denied (publickey)."),
            ),
        );
        assert!(!m.state.login_progress.contains_key("pwbox"));
    }

    #[test]
    fn a_result_after_its_steps_left_still_ends_the_login() {
        use crate::link::unlock::{FailureKind, UnlockOutcome};
        use crate::model::LoginEvent;
        let refused = || UnlockOutcome::Failed {
            kind: FailureKind::WrongPassword,
            reason: "the password was refused".into(),
        };
        let (mut m, attempt) = submitted_login(&["pwbox"]);
        update(
            &mut m,
            login_event("pwbox", attempt, LoginEvent::Verdict(refused())),
        );
        // A newer look at the machine arrives before the result and takes the settled
        // steps with it.
        update(&mut m, probed("pwbox", 0, None));
        assert!(!m.state.login_progress.contains_key("pwbox"));
        update(&mut m, login_result("pwbox", attempt, refused()));
        assert!(m.state.login_run.is_none(), "the pane offers a login again");
        assert!(m.state.login_reports.contains_key("pwbox"));

        let (mut m, attempt) = submitted_login(&["pwbox"]);
        update(
            &mut m,
            Msg::RemoveHost {
                host: "pwbox".to_owned(),
                clear_tracking: false,
            },
        );
        update(&mut m, login_result("pwbox", attempt, refused()));
        assert!(m.state.login_run.is_none());
        assert!(m.state.login_reports.contains_key("pwbox"));
    }

    fn lower_r() -> Msg {
        Msg::Key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE))
    }

    #[test]
    fn a_rescan_summary_names_the_listed_mux_before_the_reach_resolves() {
        // No reach yet, so the host names no mux, but its listing does: a started or
        // ended session is named by the path its card shows.
        let mut m = AppModel::from_hosts(vec!["a".to_owned()]);
        answer(&mut m, "a", &["x"], None);
        update(&mut m, lower_r());
        answer(&mut m, "a", &["w"], None);
        assert_eq!(
            note_texts(&m),
            ["1 session started: a/tmux/w", "1 session ended: a/tmux/x"]
        );
    }

    #[test]
    fn prefix_r_rescans_the_selected_machine_and_reports_it_alone() {
        let mut m = AppModel::from_hosts(vec!["a".to_owned(), "b".to_owned()]);
        answer(&mut m, "a", &["x"], None);
        answer(&mut m, "b", &["y"], None);
        assert_eq!(m.switcher.current_host().as_deref(), Some("a"));

        let effects = update(&mut m, lower_r());
        assert!(
            matches!(
                effects.as_slice(),
                [Effect::Command(crate::model::Command::RescanMachine(machine))] if machine == "a"
            ),
            "{effects:?}"
        );
        assert_eq!(
            m.state.scanning,
            HashSet::from(["a".to_owned()]),
            "only the selected machine is asked"
        );
        // Another machine changing meanwhile is not this re-scan's to report.
        answer(&mut m, "b", &["y", "z"], None);
        assert!(m.state.notify.toasts.is_empty());
        answer(&mut m, "a", &["w", "x"], None);
        assert_eq!(m.state.notify.toasts.len(), 1);
        assert_eq!(m.state.notify.toasts[0].title, "rescan machine a");
        assert_eq!(note_texts(&m), ["1 session started: a/tmux/w"]);

        // Nothing changed is said for that machine alone.
        update(&mut m, lower_r());
        answer(&mut m, "a", &["w", "x"], None);
        assert_eq!(
            m.state.notify.toasts[1].notes[0].text,
            "no changes · 1 machine, 2 sessions"
        );
    }

    #[test]
    fn a_second_rescan_waits_for_the_first_and_a_full_one_takes_over() {
        let mut m = AppModel::from_hosts(vec!["a".to_owned(), "b".to_owned()]);
        answer(&mut m, "a", &["x"], None);
        answer(&mut m, "b", &["y"], None);
        update(&mut m, lower_r());
        m.state.chrome.show_selection_hint(
            Vec::new(),
            "1 window".into(),
            std::time::Instant::now(),
        );
        let effects = update(
            &mut m,
            Msg::Commands(vec![crate::model::Command::RescanMachine("b".to_owned())]),
        );
        assert!(effects.is_empty(), "{effects:?}");
        assert_eq!(
            m.state.chrome.hint_bar_text(200, &m.state).trim(),
            "1 window",
            "the refusal leaves the hint bar on its advice"
        );
        assert_eq!(
            m.state.notify.last_report(),
            Some((
                "rescan machine b",
                crate::state::notify::Level::Warning,
                "a re-scan is still running"
            ))
        );
        assert!(!m.state.scanning.contains("b"));

        update(&mut m, Msg::Action(crate::model::Action::Rescan));
        assert_eq!(m.take_rescan_skip_machine(), Some("a".to_owned()));
        assert_eq!(m.take_rescan_skip_machine(), None);
        update(&mut m, Msg::RescanRosterApplied);
        answer(&mut m, "a", &["x"], None);
        answer(&mut m, "b", &["y"], None);
        let titles: Vec<&str> = m
            .state
            .notify
            .toasts
            .iter()
            .map(|t| t.title.as_str())
            .collect();
        assert_eq!(
            titles,
            ["rescan machine b", "rescan all machines"],
            "the refusal, then the full summary"
        );
    }

    #[test]
    fn enter_in_the_check_table_on_a_blocked_host_focuses_its_login_pane() {
        let mut m = AppModel::from_hosts(vec!["a".to_owned(), "lock".to_owned()]);
        answer(&mut m, "a", &["x"], None);
        answer(
            &mut m,
            "lock",
            &[],
            Some("alice@lock: Permission denied (publickey,password)."),
        );
        assert!(m.state.focus.view_is_nav());
        update(&mut m, Msg::ToggleCheck);
        assert!(matches!(
            m.state.modal,
            Some(crate::state::Modal::Check { .. })
        ));
        update(
            &mut m,
            Msg::ReaderBytes {
                bytes: b"\r".to_vec(),
                prefix: 0x07,
            },
        );
        assert!(m.state.modal.is_none());
        assert!(
            !m.state.focus.view_is_nav(),
            "the login pane takes the keys"
        );
        assert_eq!(m.switcher.current_host().as_deref(), Some("lock"));
    }

    #[test]
    fn palette_runs_a_named_command_and_ignores_unmatched_enter() {
        let mut m = AppModel::from_hosts(vec!["a".to_owned()]);
        update(&mut m, Msg::TogglePalette);
        let no_match = update(
            &mut m,
            Msg::ReaderBytes {
                bytes: b"unknown\r".to_vec(),
                prefix: 0x07,
            },
        );
        assert!(no_match.is_empty());
        assert!(matches!(
            m.state.modal,
            Some(crate::state::Modal::Palette { .. })
        ));
        let effects = update(
            &mut m,
            Msg::ReaderBytes {
                bytes: b"\x15quit xmux\r".to_vec(),
                prefix: 0x07,
            },
        );
        assert!(matches!(
            effects.as_slice(),
            [Effect::Command(crate::model::Command::Quit)]
        ));
        assert!(m.state.modal.is_none());
    }

    #[test]
    fn palette_login_opens_an_unreachable_host() {
        let mut m = AppModel::from_hosts(vec!["a".to_owned(), "dead".to_owned()]);
        answer(&mut m, "a", &["x"], None);
        answer(&mut m, "dead", &[], Some("connection refused"));
        update(&mut m, Msg::TogglePalette);
        let effects = update(
            &mut m,
            Msg::ReaderBytes {
                bytes: b"log in to dead\r".to_vec(),
                prefix: 0x07,
            },
        );
        assert!(m.state.filter.is_empty());
        assert_eq!(m.switcher.current_host().as_deref(), Some("dead"));
        assert!(m.switcher.current_machine_blocked());
        assert!(!m.state.focus.view_is_nav());
        assert!(effects.is_empty());
        answer(&mut m, "dead", &[], None);
        assert!(!m.switcher.current_machine_blocked());
    }

    /// Lays the model out at 140x30 with the nav shown, as a frame would, so a mouse
    /// message is hit-tested against the popup it painted.
    fn lay_out(m: &mut AppModel) {
        m.render_plan = m.switcher.layout(
            ratatui::layout::Rect::new(0, 0, 140, 30),
            crate::ui::switcher::NavSize::visible(crate::ui::switcher::NAV_WIDTH),
            &m.state,
            &m.render_plan,
        );
    }

    /// A press and a release on one cell: a click.
    fn click(m: &mut AppModel, col: u16, row: u16) -> Vec<Effect> {
        let effects = update(m, Msg::BeginPopupDrag { col, row });
        assert!(effects.is_empty());
        update(m, Msg::EndPopupDrag)
    }

    fn palette_selection(m: &AppModel) -> (usize, Option<usize>) {
        match &m.state.modal {
            Some(crate::state::Modal::Palette {
                selected, hover, ..
            }) => (*selected, *hover),
            _ => panic!("the palette is open"),
        }
    }

    #[test]
    fn a_click_on_a_palette_entry_runs_it_as_enter_does() {
        let mut m = AppModel::from_hosts(vec!["a".to_owned()]);
        update(&mut m, Msg::TogglePalette);
        update(
            &mut m,
            Msg::ReaderBytes {
                bytes: b"quit xmux".to_vec(),
                prefix: 0x07,
            },
        );
        lay_out(&mut m);
        let r = m.render_plan.popup_rect;
        // The query field and its rule are the first inner rows and the one match is next.
        let effects = click(&mut m, r.x + 3, r.y + 1 + crate::ui::modal::PALETTE_LEAD);
        assert!(matches!(
            effects.as_slice(),
            [Effect::Command(crate::model::Command::Quit)]
        ));
        assert!(m.state.modal.is_none());
    }

    /// The screen `m` paints at 40x12 as text, its plan kept for the keys that follow.
    fn paint_small(m: &mut AppModel) -> String {
        let area = ratatui::layout::Rect::new(0, 0, 40, 12);
        let nav = crate::ui::switcher::NavSize::hidden(crate::ui::switcher::NAV_WIDTH);
        m.render_plan = m.switcher.layout(area, nav, &m.state, &m.render_plan);
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(40, 12)).unwrap();
        term.draw(|f| m.switcher.render_test(f, None, true, nav, &m.state))
            .unwrap();
        let buf = term.backend().buffer();
        (0..12)
            .map(|y| (0..40).map(|x| buf[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn both_logout_confirms_scroll_every_fact_into_a_40_by_12_window() {
        for mode in [
            crate::state::InputMode::Logout,
            crate::state::InputMode::LogoutKeys,
        ] {
            let mut m = AppModel::from_hosts(vec!["box".to_owned()]);
            let mut input = crate::state::Input::new(mode, String::new(), Some("box".into()));
            input.facts = vec![
                ("machine", "box-a-machine-with-a-rather-long-name".into()),
                ("SSH login", "password".into()),
                ("password", "held password is cleared".into()),
                (
                    "key",
                    "removed from box; asks first if xmux did not add it".into(),
                ),
                (
                    "ssh config",
                    "removes the entry xmux saved; asks first for others naming it".into(),
                ),
                ("connections", "closes box connections".into()),
            ];
            m.state.modal = Some(crate::state::Modal::Input(Box::new(input)));
            let first = paint_small(&mut m);
            assert!(
                first.contains(" of "),
                "the border counts the rows: {first}"
            );
            let mut seen = first.clone();
            for _ in 0..12 {
                update(
                    &mut m,
                    Msg::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
                );
                let screen = paint_small(&mut m);
                assert!(screen.contains("type "), "the field stays: {screen}");
                seen.push_str(&screen);
            }
            let squeezed: String = seen.replace('│', " ").split_whitespace().collect();
            for word in ["rather-long-name", "held", "xmux", "closesboxconnections"] {
                assert!(squeezed.contains(word), "{word} is reachable: {seen}");
            }
            // The scroll stops at the last row, so one step back up moves the view.
            let bottom = paint_small(&mut m);
            update(
                &mut m,
                Msg::Key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)),
            );
            assert_ne!(paint_small(&mut m), bottom);
            update(
                &mut m,
                Msg::Key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE)),
            );
            assert_eq!(paint_small(&mut m), first);
            // Typing still reaches the field, and Esc still cancels.
            for c in "logout".chars() {
                update(
                    &mut m,
                    Msg::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)),
                );
            }
            match &m.state.modal {
                Some(crate::state::Modal::Input(input)) => assert_eq!(input.buffer, "logout"),
                _ => panic!("the confirm stays open"),
            }
            update(
                &mut m,
                Msg::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            );
            assert!(m.state.modal.is_none());
        }
    }

    #[test]
    fn a_click_on_the_query_field_runs_nothing() {
        let mut m = AppModel::from_hosts(vec!["a".to_owned()]);
        update(&mut m, Msg::TogglePalette);
        lay_out(&mut m);
        let r = m.render_plan.popup_rect;
        assert!(click(&mut m, r.x + 3, r.y + 1).is_empty());
        assert_eq!(palette_selection(&m), (0, None));
    }

    #[test]
    fn a_click_on_a_host_to_check_opens_it_as_enter_does() {
        let mut m = AppModel::from_hosts(vec!["a".to_owned(), "lock".to_owned()]);
        answer(&mut m, "a", &["x"], None);
        answer(
            &mut m,
            "lock",
            &[],
            Some("alice@lock: Permission denied (publickey,password)."),
        );
        update(&mut m, Msg::ToggleCheck);
        lay_out(&mut m);
        let r = m.render_plan.popup_rect;
        // The cause title is the first inner row and its one host the second.
        click(&mut m, r.x + 3, r.y + 2);
        assert!(m.state.modal.is_none());
        assert!(
            !m.state.focus.view_is_nav(),
            "the login pane takes the keys"
        );
        assert_eq!(m.switcher.current_host().as_deref(), Some("lock"));
    }

    #[test]
    fn hovering_a_palette_entry_marks_it_without_moving_the_selection() {
        let mut m = AppModel::from_hosts(vec!["a".to_owned()]);
        update(&mut m, Msg::TogglePalette);
        lay_out(&mut m);
        let r = m.render_plan.popup_rect;
        update(
            &mut m,
            Msg::HoverPopup {
                col: r.x + 3,
                row: r.y + 3 + crate::ui::modal::PALETTE_LEAD,
            },
        );
        assert_eq!(palette_selection(&m), (0, Some(2)), "the third entry");
        update(
            &mut m,
            Msg::HoverPopup {
                col: r.x + 3,
                row: r.y + 1,
            },
        );
        assert_eq!(
            palette_selection(&m),
            (0, None),
            "the query field is no entry"
        );
        update(
            &mut m,
            Msg::HoverPopup {
                col: r.x + 3,
                row: r.y + 3 + crate::ui::modal::PALETTE_LEAD,
            },
        );
        update(
            &mut m,
            Msg::ReaderBytes {
                bytes: b"[B".to_vec(),
                prefix: 0x07,
            },
        );
        assert_eq!(
            palette_selection(&m),
            (1, None),
            "the arrow moves the hard selection and ends the soft one"
        );
    }

    #[test]
    fn a_press_dragged_off_a_palette_entry_moves_the_popup_and_runs_nothing() {
        let mut m = AppModel::from_hosts(vec!["a".to_owned()]);
        update(&mut m, Msg::TogglePalette);
        update(
            &mut m,
            Msg::ReaderBytes {
                bytes: b"quit xmux".to_vec(),
                prefix: 0x07,
            },
        );
        lay_out(&mut m);
        let before = m.render_plan.popup_rect;
        let (col, row) = (before.x + 3, before.y + 2);
        update(&mut m, Msg::BeginPopupDrag { col, row });
        update(&mut m, Msg::DragPopup { col: col - 5, row });
        assert!(update(&mut m, Msg::EndPopupDrag).is_empty());
        assert_eq!(palette_selection(&m), (0, None), "nothing ran");
        lay_out(&mut m);
        assert_eq!(m.render_plan.popup_rect.x, before.x - 5, "the popup moved");
    }

    /// `model_with_cards` as it stands at launch: the landing screen up, the selection on
    /// the first session, nothing executed.
    fn landed() -> AppModel {
        let mut m = model_with_cards();
        m.switcher.open_landing();
        m
    }

    fn terminal_focused(m: &AppModel) -> bool {
        !m.state.focus.view_is_nav()
    }

    #[test]
    fn the_landing_selection_attaches_nothing_until_it_is_executed() {
        let mut m = landed();
        update(&mut m, down());
        update(&mut m, Msg::SyncSelection);
        assert!(
            m.state.selection.session.is_empty(),
            "a move on the landing selects no session to attach"
        );
        assert!(m.switcher.landing_open());

        update(&mut m, Msg::Focus(crate::model::FocusTarget::Terminal));
        assert!(!m.switcher.landing_open(), "Enter executes the selection");
        update(&mut m, Msg::SyncSelection);
        assert_eq!(m.state.selection.session, "editor");

        update(&mut m, Msg::Focus(crate::model::FocusTarget::Nav));
        assert!(!m.switcher.landing_open(), "the landing never returns");
    }

    #[test]
    fn a_landing_link_executes_and_focuses_the_terminal_view() {
        let mut m = landed();
        let i = m
            .switcher
            .landing_links()
            .iter()
            .position(|l| {
                l.node
                    == crate::model::Node::Session(crate::session::Address::new("local", "editor"))
            })
            .unwrap();
        update(&mut m, Msg::OpenLink(Some(i)));
        assert!(!m.switcher.landing_open());
        assert!(terminal_focused(&m));
        update(&mut m, Msg::SyncSelection);
        assert_eq!(m.state.selection.session, "editor");
    }

    #[test]
    fn a_switch_executes_the_landing_and_focuses_the_terminal_view() {
        let mut m = landed();
        update(
            &mut m,
            Msg::Action(crate::model::Action::Switch(crate::session::Address::new(
                "local", "build",
            ))),
        );
        assert!(!m.switcher.landing_open());
        assert!(terminal_focused(&m));
        update(&mut m, Msg::SyncSelection);
        assert_eq!(m.state.selection.session, "build");
    }

    #[test]
    fn a_landed_jump_focuses_the_terminal_view() {
        let mut m = landed();
        update(
            &mut m,
            Msg::Key(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE)),
        );
        assert!(m.switcher.landing_open(), "a jump being typed only selects");
        update(
            &mut m,
            Msg::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        );
        assert!(!m.switcher.landing_open());
        assert!(terminal_focused(&m));
    }
}
