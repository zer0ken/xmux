use std::collections::{HashMap, HashSet};

use ratatui::crossterm::event::KeyEvent;

use crate::app::input::MouseState;
use crate::model::{Action, Command, EventEffect, Selection};
#[cfg(test)]
use crate::ui::switcher::NavSize;
use crate::ui::switcher::{NavPosition, RenderPlan, Switcher};

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

    #[cfg(test)]
    pub(crate) fn state(&self) -> &crate::state::State {
        &self.state
    }

    #[cfg(test)]
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
        }
    }
}

impl PartialEq for Effect {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Command(left), Self::Command(right)) => left == right,
            (Self::Event(_), Self::Event(_)) => false,
            (Self::EventBatch(_), Self::EventBatch(_)) => false,
            (Self::LoginApplied { .. }, Self::LoginApplied { .. }) => false,
            (Self::StartLogin { .. }, Self::StartLogin { .. }) => false,
            _ => false,
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
            let min = super::runtime::nav_width_min(&model.state.chrome.ui_prefix) as i32;
            let next = (model.nav_width_natural as i32 + delta)
                .clamp(min, super::runtime::NAV_WIDTH_MAX as i32) as u16;
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
        HostEvent::RosterResolved { roster } => vec![EventEffect::ApplyRoster { roster }],
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

pub(crate) fn update(model: &mut AppModel, msg: Msg) -> Vec<Effect> {
    match msg {
        Msg::Action(action) => {
            let commands = model.state.apply(action);
            commands
                .into_iter()
                .filter_map(|command| command_effect(model, command))
                .collect()
        }
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
            sync_selection(model);
            Vec::new()
        }
        Msg::MouseScroll { down } => {
            model.switcher.mouse_scroll(down, &model.state);
            sync_selection(model);
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
            sync_selection(model);
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
            sync_selection(model);
            Vec::new()
        }
        Msg::AddSource { source, scanning } => {
            model.switcher.add_source(source.clone(), &mut model.state);
            if scanning {
                model.switcher.mark_scanning(&source, &mut model.state);
            }
            Vec::new()
        }
        Msg::RemoveSource { source } => {
            model.connected.remove(&source);
            model.detecting.remove(&source);
            model.switcher.remove_source(&source, &mut model.state);
            sync_selection(model);
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
                        sync_selection(model);
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
                        sync_selection(model);
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
                        super::runtime::note_host_exited(
                            &mut model.switcher,
                            &mut model.state,
                            &mut model.connected,
                            &host,
                            reason,
                        );
                        sync_selection(model);
                        None
                    }
                    effect @ EventEffect::Refetch { .. } => Some(Effect::Event(effect)),
                    effect => Some(Effect::Event(effect)),
                })
                .collect()
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

        assert_eq!(key, ctl);
        assert_eq!(key, vec![Effect::Command(crate::model::Command::Rescan)]);
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
        assert_eq!(model.state().selection.session, "work");
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
        assert_eq!(command, vec![Effect::Command(crate::model::Command::Quit)]);
    }
}
