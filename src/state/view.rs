//! Runtime-owned values shared with the navigation and operation UI.

use crate::model::{Group, LoginOutcome};
use crate::session::{Address, Session};

/// A fully-populated snapshot of the reachable environment.
#[derive(Clone, Default)]
pub struct Scan {
    pub groups: Vec<Group>,
}

/// What a navigation row references. A session card attaches to that session (the mux
/// lands on its active window), a source's host-state card selects the source, and a host
/// card selects the host, so its screen shows. A section title is not a card: it carries
/// no number and stays out of the card step, and its host half and source half each
/// select their own level.
#[derive(Clone)]
pub(crate) enum RowRef {
    /// A host/mux SECTION TITLE: the header row a group of sibling session cards hangs
    /// under. It carries `{host}/{mux}` and is never numbered; its host half selects the
    /// host and its mux half the source. The numbers below it are the sessions'. `n` on
    /// one of those sessions creates a sibling in the same section.
    Section { source: String },
    /// A session card: the session name on a single detail line. Every session card
    /// carries its session name; the focused window it used to name is gone from the
    /// card, and the `{host}/{mux}` it used to carry now lives on the section title
    /// above it.
    Session { sess: Session },
    /// A source with no session to show (scanning / unreachable / blocked / list
    /// failed / empty), sunk below the sections. `scanning` is the in-flight state: the
    /// card's unresolved level shows a spinner instead of a settled mux. `blocked`
    /// refines `unreachable`: the failure is one a login answers.
    Host {
        source: String,
        unreachable: bool,
        blocked: bool,
        list_failed: bool,
        scanning: bool,
    },
    /// A host none of whose sources connected (unreachable, or logged out and waiting on
    /// a login): one card for the whole machine in place of a card per source. `source`
    /// is the source the login pane and the probes address for it, its first in card
    /// order. `blocked` says a login can answer the failure.
    Machine {
        machine: String,
        source: String,
        blocked: bool,
    },
}

/// What the switcher must do after [`State::fold_op_result`] applies an op's
/// inventory mutation: rebuild the rows and, per the op, move the cursor to the
/// new session (a create) or, on failure, report a message with no inventory
/// change. The mutation is State's; the row rebuild and cursor restore are the
/// switcher's.
///
/// [`State::fold_op_result`]: crate::state::State::fold_op_result
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpFollow {
    /// Rebuild, then move the cursor to this new session's row (a create).
    Reselect(Address),
    /// No inventory change. Report this message for a failed operation.
    Failed(String),
    /// The login verdict: re-probe that `source`'s machine on success (only it could
    /// have changed reach state), report the failure reason otherwise. `login` rides along
    /// so a success can be recorded on the machine before that re-probe goes out.
    LoginResult {
        source: String,
        login: crate::transport::Login,
        outcome: LoginOutcome,
    },
    /// The state took the result in whole; the rows and the cursor stay as they are.
    Nothing,
}
