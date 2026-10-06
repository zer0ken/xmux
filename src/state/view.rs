//! Runtime-owned values shared with the navigation and operation UI.

use crate::model::{Group, LoginOutcome};
use crate::session::{Address, Session};

/// A fully-populated snapshot of the reachable environment.
#[derive(Clone, Default)]
pub struct Scan {
    pub groups: Vec<Group>,
}

/// What a navigation row references. A session card attaches to that session (the mux
/// lands on its active window), a host's host-state card selects the host, and a machine
/// card selects the machine, so its screen shows. A section title is not a card: it carries
/// no number and stays out of the card step, and its machine half and host half each
/// select their own level.
#[derive(Clone)]
pub(crate) enum RowRef {
    /// A machine/mux SECTION TITLE: the header row a group of sibling session cards hangs
    /// under. It carries `{machine}/{mux}` and is never numbered; its machine half selects
    /// the machine and its mux half the host. The numbers below it are the sessions'. `n` on
    /// one of those sessions creates a sibling in the same section.
    Section { host: String },
    /// A session card: the session name on a single detail line. Every session card
    /// carries its session name; the focused window it used to name is gone from the
    /// card, and the `{machine}/{mux}` it used to carry now lives on the section title
    /// above it.
    Session { sess: Session },
    /// A host with no session to show (scanning / unreachable / blocked / list
    /// failed / empty), sunk below the sections. `scanning` is the in-flight state: the
    /// card's unresolved level shows a spinner instead of a settled mux. `blocked`
    /// refines `unreachable`: the failure is one a login answers. `logged_out` refines
    /// `blocked`: the user logged out of the machine.
    Host {
        host: String,
        unreachable: bool,
        blocked: bool,
        logged_out: bool,
        list_failed: bool,
        scanning: bool,
    },
    /// A machine's own card: one for a machine none of whose hosts connected
    /// (unreachable, or logged out and waiting on a login), in place of a card per host,
    /// and one for a machine no host of which is known yet. `host` is the address the
    /// login pane and the probes use for it: its first host in card order, or the
    /// machine's own name while it has none. `blocked` says a login can answer the
    /// failure, `logged_out` that the user logged out of the machine, and `scanning` that
    /// the machine's answer is still on its way.
    Machine {
        machine: String,
        host: String,
        blocked: bool,
        logged_out: bool,
        scanning: bool,
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
    /// The login verdict: re-probe that `host`'s machine on success (only it could
    /// have changed reach state), report the failure reason otherwise. `login` rides along
    /// so a success can be recorded on the machine before that re-probe goes out.
    LoginResult {
        host: String,
        login: crate::transport::Login,
        outcome: LoginOutcome,
    },
    /// The state took the result in whole; the rows and the cursor stay as they are.
    Nothing,
}
