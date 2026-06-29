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
//! The display/navigation intents (Switch/Focus/Rescan/TreeWidth/ToggleAutoHide/Quit)
//! plus the selection/attach-debounce intents (`Select`/`Tick`) live here; the async
//! session-lifecycle path (`PendingOp`) is not folded in yet.

use crate::cockpit::Selection;
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
