//! The unidirectional-flow core: [`Action`] (intent) and [`Command`] (effect).
//!
//! Every input surface — keys, the `xmux ctl` socket, the loop-top selection
//! derive — resolves to an `Action`. `State::apply(Action) -> Vec<Command>` is the
//! single site that mutates domain state, and it returns the side effects to run as
//! `Command`s. The cockpit run loop dispatches each `Command` (switcher cursor move,
//! attach, prefs persist, quit) — `apply` itself touches only `State`, so the
//! intent → state-change → effect flow is one direction with one mutation point.
//!
//! `Action` is the DOMAIN vocabulary, distinct from `proxy::dispatch::Action` (the
//! cockpit's raw-byte input vocabulary, which projects INTO this via `as_action`).
//! The display/navigation intents (Switch/Focus/Rescan/TreeWidth/ToggleAutoHide/Quit),
//! the selection/attach-debounce intents (`Select`/`Tick`), and the async
//! session-lifecycle intents (`CreateSession`/`NewWindow`/`SplitWindow`/`RenameSession`/
//! `KillSession`/`KillWindow`/`RenameWindow`) all live here. A lifecycle intent folds
//! into a [`Command::RunOp`] carrying the [`MuxOp`] descriptor the run loop runs
//! off-loop against the live mux.

use crate::cockpit::Selection;
use crate::session::Session;
use std::time::Instant;

/// A domain intent. The single input the [`State::apply`](crate::state::State::apply)
/// mutation site accepts. Resolved from a keypress, a ctl command, or the loop-top
/// selection derive.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// Move the display target to this `source/session[:window]` address (ctl
    /// `switch`, or a context-menu pick). Moves the cursor; the attach commits on a
    /// later `Tick` once the selection settles.
    Switch { address: String },
    /// Move focus between the tree sidebar and the terminal pane.
    Focus(FocusTarget),
    /// Re-enumerate every host (the `r` re-scan).
    Rescan,
    /// Adjust the tree width by a signed delta.
    TreeWidth(i32),
    /// Toggle auto-hide-tree mode.
    ToggleAutoHide,
    /// Quit the cockpit.
    Quit,
    /// The settled cursor target. Updates `state.selection` and arms the attach
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
    /// Move the switcher cursor to this `source/session[:window]` address.
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
    /// Exit the cockpit run loop.
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

/// Which pane [`Action::Focus`] targets. The ctl `focus` verb and the keyboard
/// focus toggles both resolve to this.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusTarget {
    Tree,
    Terminal,
}

impl FocusTarget {
    /// Parses the ctl `focus` argument. `mux` is accepted as a render-side alias
    /// for `terminal` (the mux pane IS the terminal pane in the cockpit's vocab).
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
