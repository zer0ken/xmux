//! The unidirectional-flow core: [`Action`] (intent) and [`Command`] (effect).
//!
//! Every input surface — keys, the `xmux ctl` socket, the loop-top selection
//! derive — resolves to an `Action`. `State::apply(Action) -> Vec<Command>` is the
//! single site that mutates domain state, and it returns the side effects to run as
//! `Command`s. The app run loop dispatches each `Command` (switcher selection move,
//! attach, prefs persist, quit) — `apply` itself touches only `State`, so the
//! intent → state-change → effect flow is one direction with one mutation point.
//!
//! `Action` is the DOMAIN vocabulary, distinct from `display::dispatch::Action` (the
//! app's raw-byte input vocabulary, which projects INTO this via `as_action`).
//! The display/navigation intents (Switch/Focus/Rescan/TreeWidth/ToggleAutoHide/Quit),
//! the selection/attach-debounce intents (`Select`/`Tick`), and the async
//! session-lifecycle intents (`CreateSession`/`NewWindow`/`SplitWindow`/`RenameSession`/
//! `KillSession`/`KillWindow`/`RenameWindow`) all live here. A lifecycle intent folds
//! into a [`Command::RunOp`] carrying the [`MuxOp`] descriptor the run loop runs
//! off-loop against the live mux.

use crate::app::runtime::Selection;
use crate::session::Session;
use std::time::Instant;

/// A domain intent. The single input the [`State::apply`](crate::state::State::apply)
/// mutation site accepts. Resolved from a keypress, a ctl command, or the loop-top
/// selection derive.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// Move the display target to this `source/session[:window]` address (ctl
    /// `switch`, or a context-menu pick). Moves the selection; the attach commits on a
    /// later `Tick` once the selection settles.
    Switch { address: String },
    /// Move focus between the tree view and the terminal view.
    Focus(FocusTarget),
    /// Re-enumerate every host (the `r` re-scan).
    Rescan,
    /// Adjust the tree width by a signed delta.
    TreeWidth(i32),
    /// Toggle auto-hide-tree mode.
    ToggleAutoHide,
    /// Quit the app.
    Quit,
    /// The settled selection target. Updates `state.selection` and arms the attach
    /// debounce; emits NO attach `Command` — the trailing `Tick` fires the attach
    /// once the selection stops moving.
    Select(Selection),
    /// The loop cadence beat, carrying the clock and the runtime attach facts as
    /// DATA (never read inside `apply`). (Re)arms the attach deadline while a select
    /// is pending so rapid navigation coalesces into one trailing attach, and fires
    /// [`Command::Attach`] when the deadline has elapsed and the gate holds.
    Tick {
        /// The current instant (injected, not read inside `apply`).
        now: Instant,
        /// Whether the selected session's display PTY is currently live.
        key_live: bool,
        /// Whether an attach for the selected session's key is already in flight.
        in_flight: bool,
    },
    /// Create a new session named `name` (empty = mux auto-name) on `source`.
    CreateSession { source: String, name: String },
    /// Create a new window named `name` (empty = mux auto-name) in `session` on `source`.
    NewWindow {
        source: String,
        session: String,
        name: String,
    },
    /// Split window `target` (`session:window`) of `session` on `source` into a new pane.
    SplitWindow {
        source: String,
        target: String,
        session: String,
        vertical: bool,
    },
    /// Rename `sess` to `new_name`.
    RenameSession { sess: Session, new_name: String },
    /// Kill `sess`.
    KillSession { sess: Session },
    /// Kill window `target` (`session:window`) of `session` on `source`.
    KillWindow {
        source: String,
        session: String,
        target: String,
    },
    /// Rename window `target` (`session:window`) of `session` on `source` to `new_name`.
    RenameWindow {
        source: String,
        session: String,
        target: String,
        new_name: String,
    },
}

/// A side effect for the run loop to carry out. `apply` returns these; the loop is
/// the sole dispatcher. Keeping effects out of `apply` is what makes `State::apply`
/// the single domain-mutation site.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// Move the switcher selection to this `source/session[:window]` address.
    SelectAddress(String),
    /// Re-enumerate every host (the `r` re-scan), via the switcher.
    Rescan,
    /// Adjust the natural tree width by this signed delta and schedule the debounced
    /// persist.
    AdjustTreeWidth(i32),
    /// Toggle auto-hide-tree mode and persist it.
    ToggleAutoHide,
    /// Persist this session address as the user's last-selected.
    PersistLastSession(String),
    /// Attach (or switch to) the selected session — the settled-selection effect.
    Attach(Selection),
    /// Exit the app run loop.
    Quit,
    /// Run a slow (network) mux action off the event loop. The run loop spawns
    /// [`run_op`](crate::ui::switcher::run_op) on a detached task and folds its
    /// `OpResult` back through the existing op channel, so an ssh round-trip never
    /// freezes rendering.
    RunOp(MuxOp),
}

/// A slow (network) mux action — the descriptor [`Command::RunOp`] carries and
/// [`run_op`](crate::ui::switcher::run_op) executes against the live mux. Built by
/// `State::apply` from a session-lifecycle [`Action`]; pure data, no I/O.
#[derive(Clone, Debug, PartialEq)]
pub enum MuxOp {
    Create {
        source: String,
        name: String,
    },
    NewWindow {
        source: String,
        session: String,
        name: String,
    },
    SplitWindow {
        source: String,
        target: String,
        session: String,
        vertical: bool,
    },
    Rename {
        sess: Session,
        new_name: String,
    },
    Kill {
        sess: Session,
    },
    KillWindow {
        source: String,
        session: String,
        target: String,
    },
    RenameWindow {
        source: String,
        session: String,
        target: String,
        new_name: String,
    },
}

/// A mux follow-up a [`HostEvent`](crate::host::HostEvent) requires after
/// [`State::apply_event`](crate::state::State::apply_event) has folded the event's
/// self-contained state mutation. `apply_event` owns the domain-state changes (tree
/// rebuild, marker move, unreachable mark); these effects carry the mux I/O the
/// state layer must not perform itself (the AGENTS rule: no IO/registry mutation in
/// `state`). The app run loop is the sole executor — it holds the host clients,
/// the attach registry, and the display worker the effects act on.
///
/// The events whose payload is self-contained (`Focus`/`Panes`) produce NO effect —
/// `apply_event` mutates the tree directly and returns an empty `Vec`. The events
/// that need a mux handle (the single-owner inventory fold into `model::Host`, a
/// control-mode probe, the registry, the detection box) return the matching effect
/// for the loop to run.
/// Not `Clone`/`Eq` — `DispatchScanned` carries a `Box<dyn Mux>`; tests match
/// structurally.
pub enum EventEffect {
    /// `Connected`/`Inventory`: fold the carried `sessions` into `host`'s
    /// `model::Host.inventory` (the single owner), apply them to the tree, request
    /// each session's panes, and sync the host's display terminal(s). The reader
    /// carries the parsed sessions on the event, so the loop folds + applies here.
    ApplyInventory {
        host: String,
        sessions: Vec<Session>,
    },
    /// `Changed`: the server's session/window STRUCTURE changed — refetch `host`'s
    /// inventory (re-run list-sessions + re-list panes).
    Refetch { host: String },
    /// `ActiveWindowChanged`: a session's active window switched — probe `session_ref`
    /// (the tmux SESSION id from the notification payload) over `host`'s control
    /// connection (no refetch). Targets THAT SPECIFIC session, not a displayed guess.
    ProbeActiveWindow { host: String, session_ref: String },
    /// `Exited`: reap `host`'s metadata client. (`apply_event` has already folded the
    /// tree/connected-set state change; this is the mux teardown.)
    ReapHost { host: String },
    /// `ClientDetached`: reap xmux's own display attach on `host` IFF the detaching
    /// `client` tty matches the host's recorded display tty. The loop owns the
    /// registry + the recover-from-detach rearm, so the match + reap run there.
    ReapDisplayAttach { host: String, client: String },
    /// `Scanned`: a detection probe resolved — (re)identify `source`'s mux with
    /// `detected`, then dispatch the now-detected host onto its metadata channel.
    DispatchScanned {
        source: String,
        detected: Option<Box<dyn crate::mux::Mux>>,
    },
    /// `Sessions` (poll host, no enumeration error): drop any stale attach whose
    /// registry `.port` vanished, then sync `source`'s display terminal(s).
    /// (`apply_event` has already applied the enumerated sessions to the tree.)
    SyncPollSessions {
        source: String,
        sessions: Vec<Session>,
    },
    /// `DisplayTty`: record `host`'s display-client tty (probed over the -CC connection
    /// by `list-clients`) on the Host, behind the loop's reach. With the tty known, a
    /// session switch is an in-place `switch-client -c <tty>`. `None` clears a stale tty.
    RecordDisplayTty { host: String, tty: Option<String> },
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
            EventEffect::Refetch { host } => f.debug_struct("Refetch").field("host", host).finish(),
            EventEffect::ProbeActiveWindow { host, session_ref } => f
                .debug_struct("ProbeActiveWindow")
                .field("host", host)
                .field("session_ref", session_ref)
                .finish(),
            EventEffect::ReapHost { host } => {
                f.debug_struct("ReapHost").field("host", host).finish()
            }
            EventEffect::ReapDisplayAttach { host, client } => f
                .debug_struct("ReapDisplayAttach")
                .field("host", host)
                .field("client", client)
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
        }
    }
}

/// Which view [`Action::Focus`] targets. The ctl `focus` verb and the keyboard
/// focus toggles both resolve to this.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusTarget {
    Tree,
    Terminal,
}

impl FocusTarget {
    /// Parses the ctl `focus` argument. `mux` is accepted as a render-side alias
    /// for `terminal` (the terminal view shows the selected session's mux).
    #[allow(clippy::should_implement_trait)] // intentionally not FromStr: returns Option, not Result
    pub fn from_str(s: &str) -> Option<FocusTarget> {
        match s.trim() {
            "tree" => Some(FocusTarget::Tree),
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
        assert_eq!(FocusTarget::from_str("tree"), Some(FocusTarget::Tree));
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
            FocusTarget::from_str(" tree "),
            Some(FocusTarget::Tree),
            "trims"
        );
        assert_eq!(FocusTarget::from_str("sideways"), None);
    }
}
