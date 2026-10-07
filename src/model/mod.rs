//! The host model: `Host` (its `transport` is a `transport::Transport`, its `mux` a
//! `Box<dyn Mux>`), the mux's server model (`ServerModel`), and the plan/value types
//! they exchange. The mux layer is transport-blind: it supplies mux argv and the
//! `transport::Transport` decides how to run it. The two axes themselves live in
//! `crate::transport` (TRANSPORT) and `crate::mux` (MUX).

pub mod action;
pub mod death;
pub mod host;
pub mod host_def;
pub mod hosts;
pub mod inventory;
pub mod keys;
pub mod login;
pub mod nav;
pub mod operation;
pub mod plan;
pub mod selection;
pub mod server_model;
pub mod view;

pub use action::{Action, Command, EventEffect, FocusTarget, MuxOp, StartupFacts};
pub use death::{
    display_tty_marker_prefix, matches_display_tty, parse_display_tty_marker, psmux_port_path,
    psmux_session_is_live,
};
pub(crate) use host::PendingInstall;
pub use host::{Host, HostDisplay, Liveness, ReadyOutcome, EARLY_END};
pub use hosts::{host_for, Hosts, RosterDelta};
pub use inventory::{add_session, sort_by_name, FailureKind, Group, Machine, LOGGED_OUT};
pub(crate) use login::SECRET_INPUT_CAPACITY;
pub use login::{
    AfterLogin, AuthMethod, LoginEvent, LoginFailure, LoginField, LoginProgress, LoginStep,
    MuxAnswer, RecordedLogin, SecretInput, StepRow, StepState,
};
pub use nav::{step_nav_position, NavPosition, NavSize, ViewLayout};
pub use operation::{KeyRegistration, LoginOutcome, OpResult, Ops, RegistrationOutcome};
pub use plan::{DeathSignal, DisplayTty, EventSource};
pub use selection::{Node, Selection};
pub use server_model::ServerModel;
pub use view::{
    choose_machine_screen, choose_view_screen, screen_actions, ConfirmedDisplay, ScreenAction,
    ViewScreen,
};
