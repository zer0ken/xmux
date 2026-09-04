//! The unidirectional-flow core: [`Action`] (intent) and [`Command`] (effect).
//!
//! Every input surface - keys, the `xmux ctl` socket, the loop-top selection
//! derive - resolves to an `Action`. `State::apply(Action) -> Vec<Command>` is the
//! single site that mutates domain state, and it returns the side effects to run as
//! `Command`s. The app run loop dispatches each `Command` (switcher selection move,
//! attach, prefs persist, quit) - `apply` itself touches only `State`, so the
//! intent → state-change → effect flow is one direction with one mutation point.
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
    /// Move the display target to this `source/session` pair (the ctl `switch` verb -
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
    /// Re-enumerate every host (the `r` re-scan).
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
        /// Whether the selected session's display PTY is currently live.
        key_live: bool,
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
    /// confirmation of a synchronous in-place switch or a `DisplayReady`. The loop
    /// makes the confirmation DECISION (a live grid exists, no reattach in flight)
    /// and folds the resulting truth here so `apply` owns the mutation.
    ConfirmDisplay(Selection),
    /// Blank the display truth - the `r` reattach-kick tears the current display
    /// down, so nothing is confirmed until the fresh attach lands.
    ClearDisplay,
    /// Re-arm the attach debounce one interval out from `now` - the recovery rearm
    /// (a matched-client detach-reap, or the viewed session's PTY exiting). Carries
    /// the same debounce arithmetic `apply(Tick)` owns, so the two arming paths
    /// cannot drift. `now` is injected (apply never reads the clock itself).
    RearmAttach { now: Instant },
    /// Arm the attach deadline at `now` itself (already elapsed) so the trailing
    /// `Tick` re-attaches immediately - the `r` reattach-kick, which re-attaches the
    /// current display with no debounce.
    RearmAttachNow { now: Instant },
    /// Create a new session named `name` (empty = auto-named) on `source`. The one
    /// mutating intent xmux keeps: a reachable host with no sessions offers nothing to
    /// switch to, so starting the first one is part of switching, not mux editing.
    CreateSession { source: String, name: String },
}

/// A side effect for the run loop to carry out. `apply` returns these; the loop is
/// the sole dispatcher. Keeping effects out of `apply` is what makes `State::apply`
/// the single domain-mutation site.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// Move the switcher selection to this session's row.
    SelectAddress(Address),
    /// Re-enumerate every host (the `r` re-scan), via the switcher.
    Rescan,
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
    /// Run the off-loop ssh unlock for a locked host with the submitted id+password.
    RunUnlock {
        source: String,
        user: String,
        password: String,
    },
}

/// A slow (network) mux action - the descriptor [`Command::RunOp`] carries and
/// [`run_op`](crate::ui::switcher::run_op) executes against the live mux. Built by
/// `State::apply` from a session-lifecycle [`Action`]; pure data, no I/O.
#[derive(Clone, Debug, PartialEq)]
pub enum MuxOp {
    Create { source: String, name: String },
}

/// A mux follow-up a [`HostEvent`](crate::link::HostEvent) requires after
/// [`State::apply_event`](crate::state::State::apply_event) has folded the event's
/// self-contained state mutation. `apply_event` owns the domain-state changes (tree
/// rebuild, marker move, unreachable mark); these effects carry the mux I/O the
/// state layer must not perform itself (the AGENTS rule: no IO/registry mutation in
/// `state`). The app run loop is the sole executor - it holds the host clients,
/// the attach registry, and the display worker the effects act on.
///
/// The events whose payload is self-contained (`Focus`/`Panes`) produce NO effect -
/// `apply_event` mutates the nav directly and returns an empty `Vec`. The events
/// that need a mux handle (the single-owner inventory fold into `model::Host`, a
/// control-mode probe, the registry, the detection box) return the matching effect
/// for the loop to run.
/// Not `Clone`/`Eq` - `DispatchScanned` carries a `Box<dyn Mux>`; tests match
/// structurally.
pub enum EventEffect {
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
    /// `MuxesFound`: add a source for every mux in `muxes` that `machine` does not
    /// already serve. The loop owns it because it needs the host registry (to know what
    /// the machine already serves, and to insert the new hosts) and the manager (to kick
    /// each new source's first scan).
    AddDiscoveredSources { machine: String, muxes: Vec<String> },
    /// `RosterResolved`: reconcile the freshly resolved roster against the three
    /// registries that must agree about which machines exist (the host registry, the
    /// source list the off-loop ops resolve against, and the nav), then scan what was
    /// added and tear down what was dropped. The loop owns it because every one of those
    /// lives behind it.
    ApplyRoster {
        roster: Box<crate::provision::env::Roster>,
    },
    /// `Exited`: reap `host`'s metadata client. (`apply_event` has already folded the
    /// tree/connected-set state change; this is the mux teardown.)
    ReapHost { host: String },
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
    /// `Scanned`: a detection probe resolved - (re)identify `source`'s mux with
    /// `detected`, then dispatch the now-detected host onto its metadata channel.
    DispatchScanned {
        source: String,
        detected: Option<Box<dyn crate::mux::Mux>>,
    },
    /// `Sessions` (poll host, no enumeration error): drop any stale attach whose
    /// registry `.port` vanished, then sync `source`'s display terminal(s).
    /// (`apply_event` has already applied the enumerated sessions to the nav.)
    SyncPollSessions {
        source: String,
        sessions: Vec<Session>,
    },
    /// `DisplayTty`: record `host`'s display-client tty (probed over the -CC connection
    /// by `list-clients`) on the Host, behind the loop's reach. With the tty known, a
    /// session switch is an in-place `switch-client -c <tty>`. `None` clears a stale tty.
    RecordDisplayTty { host: String, tty: Option<String> },
    /// `MachineProbed` (connected): resolve every source `machine` serves onto its
    /// metadata channel and, when the machine left its mux list to xmux, ask which
    /// muxes it serves. The loop owns it because it needs the host registry (the
    /// machine's sources), the manager (the channels), and the shared probe gate. On a
    /// re-scan a live channel re-enumerates; at launch it is ensured.
    MachineConnected { machine: String, rescan: bool },
}

// Hand-written: `Box<dyn Mux>` is not `Debug`, so `DispatchScanned` cannot derive
// it. Print the variant + its string fields (the detection box as a presence flag)
// so test assertion messages can format `{effects:?}`.
impl std::fmt::Debug for EventEffect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EventEffect::ApplyInventory { host, sessions } => f
                .debug_struct("ApplyInventory")
                .field("host", host)
                .field("sessions", sessions)
                .finish(),
            EventEffect::AddDiscoveredSources { machine, muxes } => f
                .debug_struct("AddDiscoveredSources")
                .field("machine", machine)
                .field("muxes", muxes)
                .finish(),
            EventEffect::Refetch { host } => f.debug_struct("Refetch").field("host", host).finish(),
            EventEffect::ApplyRoster { roster } => f
                .debug_struct("ApplyRoster")
                .field("sources", &roster.sources.len())
                .finish(),
            EventEffect::ReapHost { host } => {
                f.debug_struct("ReapHost").field("host", host).finish()
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
            EventEffect::DispatchScanned { source, detected } => f
                .debug_struct("DispatchScanned")
                .field("source", source)
                .field("detected_some", &detected.is_some())
                .finish(),
            EventEffect::SyncPollSessions { source, sessions } => f
                .debug_struct("SyncPollSessions")
                .field("source", source)
                .field("sessions", sessions)
                .finish(),
            EventEffect::RecordDisplayTty { host, tty } => f
                .debug_struct("RecordDisplayTty")
                .field("host", host)
                .field("tty", tty)
                .finish(),
            EventEffect::MachineConnected { machine, rescan } => f
                .debug_struct("MachineConnected")
                .field("machine", machine)
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
}
