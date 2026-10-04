use std::collections::{HashMap, HashSet};

use ratatui::crossterm::event::KeyEvent;

use crate::app::input::MouseState;
use crate::model::{Action, Command, EventEffect, Selection};
use crate::ui::switcher::{NavPosition, NavSize, RenderPlan, Switcher};

pub(crate) const NAV_WIDTH_MAX: u16 = 100;

/// The narrowest expanded side nav: a card's indent, a two-digit number with the cells
/// around it, and eight cells of name. Never narrower than the collapsed nav, so a wide
/// configured prefix raises it. A seam dragged narrower than this collapses the nav.
pub(crate) fn nav_width_min(ui_prefix: &str) -> u16 {
    const CARD_FLOOR: u16 = 14;
    CARD_FLOOR.max(crate::ui::switcher::collapsed_nav_width(ui_prefix) + 1)
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
}

impl AppModel {
    #[cfg(test)]
    pub(crate) fn from_sources(sources: Vec<String>) -> Self {
        let mut state = crate::state::State::from_sources(sources);
        let switcher = Switcher::from_sources(&mut state);
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
    Key(KeyEvent),
    MouseSelect {
        col: u16,
        row: u16,
    },
    MouseScroll {
        down: bool,
    },
    ToggleHelp,
    HelpBytes(Vec<u8>),
    OpResult {
        result: crate::ui::switcher::OpResult,
        logged_in: HashSet<String>,
    },
    LoginSettled {
        source: String,
        credential_held: bool,
        machine_has_sources: bool,
    },
    ApplyInventory {
        source: String,
        sessions: Vec<crate::session::Session>,
        live: bool,
    },
    ApplySourceResult {
        source: String,
        sessions: Vec<crate::session::Session>,
        err: Option<String>,
    },
    AddSource {
        source: String,
        scanning: bool,
    },
    RemoveSource {
        source: String,
        clear_tracking: bool,
    },
    DetectionFinished {
        source: String,
    },
    SetSourceReach(HashMap<String, crate::state::SourceReach>),
    SetRosterFacts {
        providers: HashMap<String, String>,
        login_defaults: HashMap<String, crate::provision::env::LoginDefaults>,
        ssh_stanzas: HashMap<String, String>,
        held_credentials: HashSet<String>,
        source_reach: HashMap<String, crate::state::SourceReach>,
    },
    HostEvent {
        event: crate::link::HostEvent,
        logged_in: HashSet<String>,
    },
    Focus(crate::model::FocusTarget),
    FeedLogin {
        source: String,
        bytes: Vec<u8>,
    },
    SetMouseNavArmed(bool),
    SetMouseDragging(bool),
    EndNavDrag {
        band: bool,
    },
    SetMouseHovered(bool),
    SetResizeRepeat(Option<std::time::Instant>),
    EndPopupDrag,
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
    Notice(String),
    DetectionStarted(String),
    Shutdown,
}

pub(crate) enum Effect {
    Command(Command),
    Event(EventEffect),
    EventBatch(Vec<EventEffect>),
    LoginApplied {
        source: String,
        login: crate::transport::Login,
    },
    StartLogin {
        source: String,
        login: crate::transport::Login,
        password: crate::model::SecretInput,
        remember: crate::model::Remember,
        pubkey: bool,
        cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    },
    PersistNavWidth(u16),
    PersistNavHeight(u16),
    PersistNavCollapsed(bool),
    PersistNavPosition(Option<NavPosition>),
    ReattachDisplay(Selection),
    CancelLogin(crate::link::unlock::RunningLogin),
}

impl std::fmt::Debug for Effect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Command(command) => f.debug_tuple("Command").field(command).finish(),
            Self::Event(effect) => f.debug_tuple("Event").field(effect).finish(),
            Self::EventBatch(effects) => f.debug_tuple("EventBatch").field(effects).finish(),
            Self::LoginApplied { source, login } => f
                .debug_struct("LoginApplied")
                .field("source", source)
                .field("login", login)
                .finish(),
            Self::StartLogin {
                source,
                login,
                remember,
                pubkey,
                ..
            } => f
                .debug_struct("StartLogin")
                .field("source", source)
                .field("login", login)
                .field("password", &"[redacted]")
                .field("remember", remember)
                .field("pubkey", pubkey)
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
            Self::ReattachDisplay(selection) => {
                f.debug_tuple("ReattachDisplay").field(selection).finish()
            }
            Self::CancelLogin(_) => f.write_str("CancelLogin"),
        }
    }
}

fn command_effect(model: &mut AppModel, command: Command) -> Option<Effect> {
    match command {
        Command::SelectAddress(address) => {
            model.switcher.select_address(&address, &model.state);
            None
        }
        Command::Rescan => {
            model.switcher.request_rescan(&mut model.state);
            let armed = model.switcher.take_rescan_kick();
            debug_assert!(armed);
            Some(Effect::Command(Command::Rescan))
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
            source,
            login,
            password,
            remember,
            pubkey,
        } => {
            let machine = crate::session::machine_of(&source);
            model.state.logged_in.remove(machine);
            model.state.login_reports.remove(machine);
            let (running, cancel) = crate::link::unlock::RunningLogin::pending(source.clone());
            model.state.login_run = Some(running);
            Some(Effect::StartLogin {
                source,
                login,
                password,
                remember,
                pubkey,
                cancel,
            })
        }
        command => Some(Effect::Command(command)),
    }
}

fn sync_selection(model: &mut AppModel) {
    let target = model.switcher.terminal_view_target();
    let selection = Selection {
        source: target.source,
        session: target.target,
    };
    if selection != model.state.selection {
        model.state.apply(Action::Select(selection));
    }
}

fn host_event_effects(model: &mut AppModel, event: crate::link::HostEvent) -> Vec<EventEffect> {
    use crate::link::HostEvent;

    match event {
        HostEvent::Connected { host, sessions } | HostEvent::Inventory { host, sessions } => vec![
            EventEffect::MarkConnected { host: host.clone() },
            EventEffect::ApplyInventory { host, sessions },
        ],
        HostEvent::Changed { host } => vec![EventEffect::Refetch { host }],
        HostEvent::Exited { host, reason } => vec![
            EventEffect::NoteHostExited {
                host: host.clone(),
                reason,
            },
            EventEffect::ReapHost { host },
        ],
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
        HostEvent::MuxesFound { machine, muxes } => {
            vec![EventEffect::AddDiscoveredSources { machine, muxes }]
        }
        HostEvent::RosterResolved { roster } => vec![EventEffect::ApplyRoster {
            roster,
            startup: None,
        }],
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
        }],
        HostEvent::Scanned {
            source,
            detected,
            err,
        } => {
            if detected.is_none() && model.state.scanning.contains(&source) {
                let reason = err.clone().unwrap_or_else(|| "mux not detected".to_owned());
                return vec![
                    EventEffect::ApplySourceResult {
                        source: source.clone(),
                        sessions: Vec::new(),
                        err: Some(reason),
                    },
                    EventEffect::DispatchScanned {
                        source,
                        detected,
                        err,
                    },
                ];
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
            password_supplied,
            credential_rejection_generation,
            credential_held,
            credential_generation,
            current_credential_generation,
            rescan,
        } => {
            let result_generation =
                credential_rejection_generation.unwrap_or(credential_generation);
            if result_generation != current_credential_generation {
                return Vec::new();
            }
            match err {
                Some(reason) => {
                    if credential_held
                        && password_supplied
                        && crate::transport::diagnostic::contains_auth_refusal(&reason)
                    {
                        model
                            .state
                            .scanning
                            .retain(|source| crate::session::machine_of(source) != machine);
                        return Vec::new();
                    }
                    model
                        .state
                        .groups
                        .iter()
                        .filter(|group| crate::session::machine_of(&group.source) == machine)
                        .map(|group| EventEffect::ApplySourceResult {
                            source: group.source.clone(),
                            sessions: Vec::new(),
                            err: Some(reason.clone()),
                        })
                        .collect()
                }
                None => {
                    model.state.login_reports.remove(&machine);
                    vec![EventEffect::MachineConnected {
                        machine,
                        shell,
                        rescan,
                    }]
                }
            }
        }
        HostEvent::Sessions {
            source,
            sessions,
            err,
        } => vec![EventEffect::ApplyPollResult {
            source,
            sessions,
            err,
        }],
    }
}

/// Handles a remote host's control client dying. A host that had connected keeps its
/// last-known rows. A never-connected host that died with "no sessions" / "no server
/// running" is REACHABLE but has no mux server - it renders "(empty)" (and a session
/// can be created there), not an unreachable state. Any other never-connected death is a
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
    // flash) on THIS exit; but a later reconnect that fails (no sessions / unreachable)
    // must then resolve its real state - otherwise a refresh that set it scanning would
    // spin on "loading…" forever, since a sticky `connected` made every exit a no-op.
    if connected.remove(host) {
        return false;
    }
    if reason
        .as_deref()
        .is_some_and(crate::model::source::reason_is_no_sessions)
    {
        switcher.apply_source_result(host.to_string(), Vec::new(), None, state);
        return false;
    }
    let msg = reason.unwrap_or_else(|| "connection closed".into());
    switcher.apply_source_result(host.to_string(), Vec::new(), Some(msg), state);
    true
}

pub(crate) fn update(model: &mut AppModel, msg: Msg) -> Vec<Effect> {
    match msg {
        Msg::Action(action) => {
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
        Msg::Key(key) => {
            let commands = model.switcher.handle_key(key, &mut model.state);
            commands
                .into_iter()
                .filter_map(|command| command_effect(model, command))
                .collect()
        }
        Msg::MouseSelect { col, row } => {
            model
                .switcher
                .mouse_select(&model.render_plan, col, row, &model.state);
            Vec::new()
        }
        Msg::MouseScroll { down } => {
            model.switcher.mouse_scroll(down, &model.state);
            Vec::new()
        }
        Msg::ToggleHelp => {
            model.switcher.toggle_help(&mut model.state);
            Vec::new()
        }
        Msg::HelpBytes(bytes) => {
            model.switcher.feed_help_key(&bytes, &mut model.state);
            Vec::new()
        }
        Msg::OpResult { result, logged_in } => {
            model.state.logged_in = logged_in;
            model
                .switcher
                .apply_op_result(result, &mut model.state)
                .map(|(source, login)| Effect::LoginApplied { source, login })
                .into_iter()
                .collect()
        }
        Msg::LoginSettled {
            source,
            credential_held,
            machine_has_sources,
        } => {
            let machine = crate::session::machine_of(&source);
            if credential_held {
                model.state.logged_in.insert(machine.to_owned());
            } else {
                model.state.logged_in.remove(machine);
            }
            if !machine_has_sources {
                model.switcher.mark_scanning(machine, &mut model.state);
            }
            if model
                .state
                .login
                .as_ref()
                .is_some_and(|draft| draft.source == source)
            {
                model.state.login = None;
            }
            Vec::new()
        }
        Msg::ApplyInventory {
            source,
            sessions,
            live,
        } => {
            if !live {
                return Vec::new();
            }
            let renamed = model.switcher.apply_source_result(
                source.clone(),
                sessions,
                None,
                &mut model.state,
            );
            renamed
                .map(|(from, to)| Effect::Event(EventEffect::RenameDisplayed { source, from, to }))
                .into_iter()
                .collect()
        }
        Msg::ApplySourceResult {
            source,
            sessions,
            err,
        } => {
            model
                .switcher
                .apply_source_result(source, sessions, err, &mut model.state);
            Vec::new()
        }
        Msg::AddSource { source, scanning } => {
            model.switcher.add_source(source.clone(), &mut model.state);
            if scanning {
                model.switcher.mark_scanning(&source, &mut model.state);
            }
            Vec::new()
        }
        Msg::RemoveSource {
            source,
            clear_tracking,
        } => {
            if clear_tracking {
                model.connected.remove(&source);
                model.detecting.remove(&source);
            }
            model.switcher.remove_source(&source, &mut model.state);
            Vec::new()
        }
        Msg::DetectionFinished { source } => {
            model.detecting.remove(&source);
            Vec::new()
        }
        Msg::SetSourceReach(reach) => {
            model.state.chrome.set_source_reach(reach);
            Vec::new()
        }
        Msg::SetRosterFacts {
            providers,
            login_defaults,
            ssh_stanzas,
            held_credentials,
            source_reach,
        } => {
            model.state.chrome.set_roster_providers(providers);
            model
                .state
                .chrome
                .set_login_defaults(login_defaults, ssh_stanzas);
            model
                .state
                .logged_in
                .retain(|machine| held_credentials.contains(machine));
            model.state.chrome.set_source_reach(source_reach);
            Vec::new()
        }
        Msg::HostEvent { event, logged_in } => {
            model.state.logged_in = logged_in;
            let event_effects = host_event_effects(model, event);
            event_effects
                .into_iter()
                .filter_map(|effect| match effect {
                    EventEffect::MarkConnected { host } => {
                        model.connected.insert(host);
                        None
                    }
                    EventEffect::ApplySourceResult {
                        source,
                        sessions,
                        err,
                    } => {
                        model
                            .switcher
                            .apply_source_result(source, sessions, err, &mut model.state);
                        None
                    }
                    EventEffect::ApplyPollResult {
                        source,
                        sessions,
                        err,
                    } => {
                        let failed = err.is_some();
                        let renamed = model.switcher.apply_source_result(
                            source.clone(),
                            sessions.clone(),
                            err,
                            &mut model.state,
                        );
                        if failed {
                            None
                        } else {
                            let mut effects: Vec<EventEffect> = renamed
                                .map(|(from, to)| EventEffect::RenameDisplayed {
                                    source: source.clone(),
                                    from,
                                    to,
                                })
                                .into_iter()
                                .collect();
                            effects.push(EventEffect::SyncPollSessions { source, sessions });
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
        Msg::FeedLogin { source, bytes } => model
            .state
            .feed_login(&source, &bytes)
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
            model.switcher.end_popup_drag();
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
            view_border_hovered,
            prefix_active,
        } => {
            model.state.chrome.set_spinner_frame(spinner_frame);
            model
                .state
                .chrome
                .set_view_border_hovered(view_border_hovered);
            model.state.chrome.set_armed(prefix_active);
            model.switcher.sync_prefix(prefix_active);
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
            model.switcher.select_address(&address, &model.state);
            Vec::new()
        }
        Msg::Tick { now, spinner } => {
            model.state.chrome.expire_flash(now);
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
            }
            Vec::new()
        }
        Msg::Notice(line) => {
            model.state.notice(line);
            Vec::new()
        }
        Msg::DetectionStarted(source) => {
            model.detecting.insert(source);
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

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::{update, AppModel, Effect, Msg};

    fn model() -> AppModel {
        AppModel::from_sources(vec!["local".to_owned()])
    }

    #[test]
    fn key_rescan_and_ctl_rescan_produce_the_same_effects() {
        let key = update(
            &mut model(),
            Msg::Key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE)),
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
            source: "local".to_owned(),
            name: "work".to_owned(),
            ..Default::default()
        };
        update(
            &mut model,
            Msg::HostEvent {
                event: crate::link::HostEvent::Sessions {
                    source: "local".to_owned(),
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
            .first()
            .expect("one session card");
        let (col, row) = (rect.x, rect.y);

        let effects = update(&mut model, Msg::MouseSelect { col, row });

        assert!(effects.is_empty());
        assert!(model.state.selection.session.is_empty());
        update(&mut model, Msg::SyncSelection);
        assert_eq!(model.state.selection.session, "work");
    }

    #[test]
    fn remove_source_tracking_depends_on_the_removal_origin() {
        let mut model = model();
        model.connected.insert("local".into());
        model.detecting.insert("local".into());

        let effects = update(
            &mut model,
            Msg::RemoveSource {
                source: "local".into(),
                clear_tracking: false,
            },
        );

        assert!(effects.is_empty());
        assert!(model.connected.contains("local"));
        assert!(model.detecting.contains("local"));

        let effects = update(
            &mut model,
            Msg::RemoveSource {
                source: "local".into(),
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
        let mut model = AppModel::from_sources(Vec::new());
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
                crate::model::EventEffect::ApplyInventory { host: applied, .. }
            ] if marked == "jup" && applied == "jup"
        ));

        let exited = super::host_event_effects(
            &mut model,
            crate::link::HostEvent::Exited {
                host: "jup".into(),
                reason: Some("connection refused".into()),
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
                source: "jup".into(),
                detected: None,
                err: Some("mux not found".into()),
            },
        );
        assert!(matches!(
            scanned.as_slice(),
            [
                crate::model::EventEffect::ApplySourceResult {
                    source: applied,
                    ..
                },
                crate::model::EventEffect::DispatchScanned {
                    source: dispatched,
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
}
