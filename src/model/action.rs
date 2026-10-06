//! The unidirectional-flow core: [`Action`] (intent) and [`Command`] (effect).
//!
//! Semantic inputs resolve to an `Action`. The state reducer folds that domain
//! intent into domain state and commands. The application update transition owns
//! the reducer call, applies state-only commands, suppresses no-op commands, and
//! emits runtime-facing effects for attachment, persistence, operations, and quit.
//!
//! `Action` is the domain action set, distinct from `display::dispatch::Action` (the
//! app's raw-byte input set, which projects INTO this via `as_action`).
//! The display/navigation intents (Switch/Focus/Rescan/NavWidth/ToggleAutoHide/Quit),
//! the selection/attach-debounce intents (`Select`/`Tick`), and the one async
//! session-lifecycle intent (`CreateSession`) all live here. A lifecycle intent folds
//! into a [`Command::RunOp`] carrying the [`MuxOp`] descriptor the run loop runs
//! off-loop against the live mux.
//!
//! xmux aggregates and switches; it does not edit what a mux already edits. So the
//! action set carries no rename/kill/window intents - those belong to the mux itself.
//! The one creating intent that survives is `CreateSession`, because a host with no
//! sessions has nothing to switch TO until one exists.

use crate::model::Selection;
use crate::session::{Address, Session};
use std::time::Instant;

/// A domain intent. The single input the [`State::apply`](crate::state::State::apply)
/// mutation site accepts. Resolved from a keypress, a ctl command, or the loop-top
/// selection derive.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// Move the display target to this `host/session` pair (the ctl `switch` verb -
    /// its only producer). Selects the addressed SESSION. Moves the selection; the
    /// attach commits on a later `Tick` once the selection settles.
    Switch(Address),
    /// Move focus between the nav view and the terminal view.
    Focus(FocusTarget),
    /// Flip the view focus (Nav ⇄ Terminal) - produced only by a left click on the
    /// unfocused view. (Prefix-key focus moves resolve to a DIRECTED `Focus` instead.)
    /// During a modal it flips the carried `prior` so the modal stays open and restores
    /// onto the flipped view.
    FocusToggle,
    /// Re-enumerate every host (the `R` re-scan).
    Rescan,
    /// Adjust the nav width by a signed delta.
    NavWidth(i32),
    /// Toggle auto-hide-nav mode.
    ToggleAutoHide,
    /// Quit the app.
    Quit,
    /// The settled selection target. Updates `state.selection` and arms the attach
    /// debounce; emits NO attach `Command` - the trailing `Tick` fires the attach
    /// once the selection stops moving.
    Select(Selection),
    /// The loop cadence beat, carrying the clock and the runtime attach facts as
    /// DATA (never read inside `apply`). (Re)arms the attach deadline while a select
    /// is pending so rapid navigation coalesces into one trailing attach, arms it for a
    /// display sitting away from the selection, and fires [`Command::Attach`] when the
    /// deadline has elapsed and the gate holds.
    Tick {
        /// The current instant (injected, not read inside `apply`).
        now: Instant,
        /// Whether an attach for the selected session's key is already in flight.
        in_flight: bool,
        /// Whether the display client sits on a session the SELECTION does not name and
        /// is the side that has to move. A CONDITION, re-derived every beat from where
        /// the client actually is, so nothing about the move is remembered anywhere: the
        /// beat that stops seeing it stops asking for it. Which of the two regions moves
        /// is the app's decision (the selection follows the client while the user drives
        /// the mux); this carries only the case that ends in an attach.
        display_astray: bool,
    },
    /// Advance the display truth (`state.displayed`) to this selection - the
    /// confirmation of a synchronous in-place switch or an attachment whose paint gate
    /// opened. The loop makes the confirmation DECISION (a live grid exists, no
    /// reattach in flight)
    /// and folds the resulting truth here so `apply` owns the mutation.
    ConfirmDisplay(Selection),
    /// Blank the display truth - the `r` reattach-kick tears the current display
    /// down, so nothing is confirmed until the fresh attach lands.
    ClearDisplay,
    /// Arm the attach deadline at `now` itself (already elapsed) so the trailing
    /// `Tick` re-attaches immediately - the `r` reattach-kick, which re-attaches the
    /// current display with no debounce.
    RearmAttachNow { now: Instant },
    /// Create a new session named `name` (empty = auto-named) on `host`. The one
    /// mutating intent xmux keeps: a reachable host with no sessions offers nothing to
    /// switch to, so starting the first one is part of switching, not mux editing.
    CreateSession { host: String, name: String },
}

/// A side effect for the run loop to carry out. `apply` returns these; the loop is
/// the sole dispatcher. Keeping effects out of `apply` is what makes `State::apply`
/// the single domain-mutation site.
#[derive(Clone, PartialEq)]
pub enum Command {
    /// Move the switcher selection to this session's row.
    SelectAddress(Address),
    /// Re-enumerate every host (the `R` re-scan), via the switcher.
    Rescan,
    /// Re-scan one machine alone (the `r` re-scan): its reachability probe, then every
    /// host it serves.
    RescanMachine(String),
    /// Take this machine's public key off the machine, then discard xmux's held credential
    /// and close the machine's connections.
    Logout(String),
    /// The logout's second confirmation answered yes: the key lines and the ssh config
    /// entries xmux did not add go with the ones it did.
    RemoveUnmarked(String),
    /// Adjust the natural nav width by this signed delta and schedule the debounced
    /// persist.
    AdjustNavWidth(i32),
    /// Toggle auto-hide-nav mode and persist it.
    ToggleAutoHide,
    /// Persist this session as the user's last-selected.
    PersistLastSession(Address),
    /// Attach (or switch to) the selected session - the settled-selection effect.
    Attach(Selection),
    /// Exit the app run loop.
    Quit,
    /// Run a slow (network) mux action off the event loop. The run loop spawns
    /// [`run_op`](crate::ui::switcher::run_op) on a detached task and folds its
    /// `OpResult` back through the existing op channel, so an ssh round-trip never
    /// freezes rendering.
    RunOp(MuxOp),
    /// Run the off-loop ssh login for a blocked host with the pane's submitted values.
    /// An empty `password` keeps the command non-interactive.
    RunLogin {
        host: String,
        login: crate::transport::Login,
        password: crate::model::SecretInput,
        after_login: crate::model::AfterLogin,
    },
}

impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SelectAddress(address) => f.debug_tuple("SelectAddress").field(address).finish(),
            Self::Rescan => f.write_str("Rescan"),
            Self::RescanMachine(machine) => f.debug_tuple("RescanMachine").field(machine).finish(),
            Self::Logout(machine) => f.debug_tuple("Logout").field(machine).finish(),
            Self::RemoveUnmarked(machine) => {
                f.debug_tuple("RemoveUnmarked").field(machine).finish()
            }
            Self::AdjustNavWidth(delta) => f.debug_tuple("AdjustNavWidth").field(delta).finish(),
            Self::ToggleAutoHide => f.write_str("ToggleAutoHide"),
            Self::PersistLastSession(address) => {
                f.debug_tuple("PersistLastSession").field(address).finish()
            }
            Self::Attach(selection) => f.debug_tuple("Attach").field(selection).finish(),
            Self::Quit => f.write_str("Quit"),
            Self::RunOp(op) => f.debug_tuple("RunOp").field(op).finish(),
            Self::RunLogin {
                host,
                login,
                after_login,
                ..
            } => f
                .debug_struct("RunLogin")
                .field("host", host)
                .field("login", login)
                .field("password", &"[redacted]")
                .field("after_login", after_login)
                .finish(),
        }
    }
}

/// A slow (network) mux action - the descriptor [`Command::RunOp`] carries and
/// [`run_op`](crate::ui::switcher::run_op) executes against the live mux. Built by
/// `State::apply` from a session-lifecycle [`Action`]; pure data, no I/O.
#[derive(Clone, Debug, PartialEq)]
pub enum MuxOp {
    Create { host: String, name: String },
}

/// An ordered host-event effect emitted by the application update transition.
/// The sequence preserves state and runtime ordering without letting backend or
/// domain layers import the application or UI layers. Effects that require host
/// clients, the attach registry, or the display worker run through the app's unified
/// effect executor in the order update emits them.
/// Not `Clone`/`Eq`: `DispatchScanned` carries a `Box<dyn Mux>`; tests match
/// structurally.
pub enum EventEffect {
    /// Record that a host's metadata client has connected before applying its inventory.
    MarkConnected { host: String },
    /// Apply one host result to the navigation model and runtime state.
    ApplyHostResult {
        host: String,
        sessions: Vec<Session>,
        err: Option<String>,
    },
    /// Apply a poll result, then reconcile any rename and live display sessions when
    /// the enumeration succeeded.
    ApplyPollResult {
        host: String,
        sessions: Vec<Session>,
        err: Option<String>,
    },
    /// Fold a metadata client exit into connection tracking and the navigation model.
    NoteHostExited {
        host: String,
        reason: Option<String>,
    },
    /// `Connected`/`Inventory`: fold the carried `sessions` into `host`'s
    /// `model::Host.inventory` (the single owner), apply them to the nav,
    /// and sync the host's display terminal(s). The reader
    /// carries the parsed sessions on the event, so the loop folds + applies here.
    ApplyInventory {
        host: String,
        sessions: Vec<Session>,
    },
    /// `Changed`: the server's session/window STRUCTURE changed - refetch `host`'s
    /// inventory (re-run list-sessions).
    Refetch { host: String },
    /// `Connected`: read which shared SSH connection `machine` rides now that a channel of
    /// it opened, so a recorded login is shown only for the connection it describes.
    CheckSharedConnection { machine: String },
    /// `MuxesFound`: add a host for every mux in `muxes` that `machine` does not
    /// already serve, and settle the card of a machine that serves none yet. The loop
    /// owns it because it needs the host registry (to know what
    /// the machine already serves, and to insert the new hosts) and the manager (to kick
    /// each new host's first scan).
    AddDiscoveredHosts {
        machine: String,
        muxes: Result<Vec<String>, String>,
    },
    /// `RosterResolved`: reconcile the freshly resolved roster against the host
    /// registry and the nav, which must agree about which machines exist, then scan what
    /// was added and tear down what was dropped. The loop owns it because both live
    /// behind it.
    ApplyRoster {
        roster: Box<crate::provision::env::Roster>,
        startup: Option<StartupFacts>,
        /// Whether a re-scan asked for this roster, whose summary waits until it is
        /// applied.
        rescan: bool,
    },
    /// `Exited`: reap `host`'s metadata client after [`Self::NoteHostExited`] has folded
    /// the tree and connected-set state change.
    ReapHost { host: String },
    /// An authentication method stopped being available: close the machine's metadata
    /// and display clients before another explicit connection attempt.
    DisconnectMachine { machine: String },
    /// `Exited` as a detach of a connected host: open `host`'s metadata channel once
    /// more after [`Self::ReapHost`] has removed the detached one.
    ReopenHost { host: String },
    /// `ClientDetached`: reap xmux's own display attach on `host` IFF the detaching
    /// `client` tty matches the host's recorded display tty. The loop owns the
    /// registry + the recover-from-detach rearm, so the match + reap run there.
    ReapDisplayAttach { host: String, client: String },
    /// `ClientSessionChanged`: some client's session changed. IFF `client` matches the
    /// host's recorded display tty, xmux's OWN display PTY was moved to `session` by the
    /// mux itself (e.g. the user's `prefix`+`s`); the loop syncs the display belief (so no
    /// spurious switch-client fires) and follows the nav selection to that session. The tty
    /// match needs `Host.display_tty` (behind the loop's reach), so it runs in the loop.
    FollowDisplaySession {
        host: String,
        client: String,
        session: String,
    },
    /// `Scanned`: a detection probe resolved - (re)identify `host`'s mux with
    /// `detected`, then dispatch the now-detected host onto its metadata channel.
    /// `err` rides along when `detected` is `None`: the reason detection failed, so
    /// the loop settles the undetected card with it.
    DispatchScanned {
        host: String,
        detected: Option<Box<dyn crate::mux::Mux>>,
        err: Option<String>,
    },
    /// A re-enumeration renamed `from` to `to` on `host`: carry the host's display record
    /// across, so the display reads as still on the session it is on.
    RenameDisplayed {
        host: String,
        from: String,
        to: String,
    },
    /// `Connected`/`Inventory` after the navigation model and any display rename have
    /// been applied: sync `host`'s display terminal(s).
    SyncInventorySessions {
        host: String,
        sessions: Vec<Session>,
    },
    /// `Sessions` (poll host, no enumeration error): drop any stale attach whose
    /// registry `.port` vanished, then sync `host`'s display terminal(s).
    /// Emitted by the app after [`Self::ApplyPollResult`] has applied the enumerated
    /// sessions to the navigation model.
    SyncPollSessions {
        host: String,
        sessions: Vec<Session>,
    },
    /// `DisplayTty`: record `host`'s display-client tty (probed over the -CC connection
    /// by `list-clients`) on the Host, behind the loop's reach. With the tty known, a
    /// session switch is an in-place `switch-client -c <tty>`. `None` clears a stale tty.
    RecordDisplayTty { host: String, tty: Option<String> },
    /// `MachineProbed` (connected): resolve every host `machine` serves onto its
    /// metadata channel and, when the machine left its mux list to xmux, ask which
    /// muxes it serves. The loop owns it because it needs the host registry (the
    /// machine's hosts), the manager (the channels), and the shared probe gate. On a
    /// re-scan a live channel re-enumerates; at launch it is ensured.
    ///
    /// `shell` is the family the probe read, recorded on every host the machine
    /// serves BEFORE any channel opens, so the first command composed for the machine
    /// is already composed for its shell.
    MachineConnected {
        machine: String,
        shell: Option<crate::transport::vocab::RemoteShell>,
        rescan: bool,
    },
}

/// Launch facts that resolve off the runtime loop because learning them may spawn a
/// subprocess. Present only on the first roster resolution.
pub struct StartupFacts {
    pub own_session: Option<Address>,
    pub force_askpass: bool,
}

// Hand-written: `Box<dyn Mux>` is not `Debug`, so `DispatchScanned` cannot derive
// it. Print the variant + its string fields (the detection box as a presence flag)
// so test assertion messages can format `{effects:?}`.
impl std::fmt::Debug for EventEffect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EventEffect::MarkConnected { host } => {
                f.debug_struct("MarkConnected").field("host", host).finish()
            }
            EventEffect::ApplyHostResult {
                host,
                sessions,
                err,
            } => f
                .debug_struct("ApplyHostResult")
                .field("host", host)
                .field("sessions", sessions)
                .field("err", err)
                .finish(),
            EventEffect::ApplyPollResult {
                host,
                sessions,
                err,
            } => f
                .debug_struct("ApplyPollResult")
                .field("host", host)
                .field("sessions", sessions)
                .field("err", err)
                .finish(),
            EventEffect::NoteHostExited { host, reason } => f
                .debug_struct("NoteHostExited")
                .field("host", host)
                .field("reason", reason)
                .finish(),
            EventEffect::ApplyInventory { host, sessions } => f
                .debug_struct("ApplyInventory")
                .field("host", host)
                .field("sessions", sessions)
                .finish(),
            EventEffect::AddDiscoveredHosts { machine, muxes } => f
                .debug_struct("AddDiscoveredHosts")
                .field("machine", machine)
                .field("muxes", muxes)
                .finish(),
            EventEffect::Refetch { host } => f.debug_struct("Refetch").field("host", host).finish(),
            EventEffect::CheckSharedConnection { machine } => f
                .debug_struct("CheckSharedConnection")
                .field("machine", machine)
                .finish(),
            EventEffect::ApplyRoster {
                roster,
                startup,
                rescan,
            } => f
                .debug_struct("ApplyRoster")
                .field("ssh_aliases", &roster.ssh_aliases.len())
                .field("startup", &startup.is_some())
                .field("rescan", rescan)
                .finish(),
            EventEffect::ReapHost { host } => {
                f.debug_struct("ReapHost").field("host", host).finish()
            }
            EventEffect::DisconnectMachine { machine } => f
                .debug_struct("DisconnectMachine")
                .field("machine", machine)
                .finish(),
            EventEffect::ReopenHost { host } => {
                f.debug_struct("ReopenHost").field("host", host).finish()
            }
            EventEffect::ReapDisplayAttach { host, client } => f
                .debug_struct("ReapDisplayAttach")
                .field("host", host)
                .field("client", client)
                .finish(),
            EventEffect::FollowDisplaySession {
                host,
                client,
                session,
            } => f
                .debug_struct("FollowDisplaySession")
                .field("host", host)
                .field("client", client)
                .field("session", session)
                .finish(),
            EventEffect::DispatchScanned {
                host,
                detected,
                err,
            } => f
                .debug_struct("DispatchScanned")
                .field("host", host)
                .field("detected_some", &detected.is_some())
                .field("err", err)
                .finish(),
            EventEffect::RenameDisplayed { host, from, to } => f
                .debug_struct("RenameDisplayed")
                .field("host", host)
                .field("from", from)
                .field("to", to)
                .finish(),
            EventEffect::SyncInventorySessions { host, sessions } => f
                .debug_struct("SyncInventorySessions")
                .field("host", host)
                .field("sessions", sessions)
                .finish(),
            EventEffect::SyncPollSessions { host, sessions } => f
                .debug_struct("SyncPollSessions")
                .field("host", host)
                .field("sessions", sessions)
                .finish(),
            EventEffect::RecordDisplayTty { host, tty } => f
                .debug_struct("RecordDisplayTty")
                .field("host", host)
                .field("tty", tty)
                .finish(),
            EventEffect::MachineConnected {
                machine,
                shell,
                rescan,
            } => f
                .debug_struct("MachineConnected")
                .field("machine", machine)
                .field("shell", shell)
                .field("rescan", rescan)
                .finish(),
        }
    }
}

/// Which view [`Action::Focus`] targets. The ctl `focus` verb and the keyboard
/// focus toggles both resolve to this.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusTarget {
    Nav,
    Terminal,
}

impl FocusTarget {
    /// Parses the ctl `focus` argument. `mux` is accepted as a render-side alias
    /// for `terminal` (the terminal view shows the selected session's mux).
    #[allow(clippy::should_implement_trait)] // intentionally not FromStr: returns Option, not Result
    pub fn from_str(s: &str) -> Option<FocusTarget> {
        match s.trim() {
            "nav" => Some(FocusTarget::Nav),
            "terminal" | "mux" => Some(FocusTarget::Terminal),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_target_parses_aliases() {
        assert_eq!(FocusTarget::from_str("nav"), Some(FocusTarget::Nav));
        assert_eq!(
            FocusTarget::from_str("terminal"),
            Some(FocusTarget::Terminal)
        );
        assert_eq!(
            FocusTarget::from_str("mux"),
            Some(FocusTarget::Terminal),
            "mux is an alias for terminal"
        );
        assert_eq!(
            FocusTarget::from_str(" nav "),
            Some(FocusTarget::Nav),
            "trims"
        );
        assert_eq!(FocusTarget::from_str("sideways"), None);
    }

    #[test]
    fn login_command_debug_redacts_the_password() {
        let command = Command::RunLogin {
            host: "pwbox/tmux".into(),
            login: crate::transport::Login {
                address: Some("pwbox".into()),
                port: Some(22),
                user: Some("dev".into()),
            },
            password: "do-not-print-this".into(),
            after_login: crate::model::AfterLogin::Nothing,
        };

        let shown = format!("{command:?}");
        assert!(!shown.contains("do-not-print-this"));
        assert!(shown.contains("[redacted]"));
    }
}
