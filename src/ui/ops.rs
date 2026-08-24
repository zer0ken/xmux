//! The off-loop operation boundary: the slow (network) mux actions a keypress
//! requests (the [`MuxOp`] carried by [`Command::RunOp`](crate::model::Command)),
//! their outcomes (`OpResult`), the `Ops` trait the app implements over the live
//! mux, and `run_op` which executes one `MuxOp` against `Ops` in a detached task.
//! Pure over `Ops` - no switcher state - so it never touches the event loop.

use crate::model::MuxOp;
use crate::session::{Session, WindowPanes};

/// The side-effecting actions the switcher delegates to the host program. The
/// event loop also drives the streaming probes through it: [`Ops::sources`] seeds
/// the host skeletons, then [`Ops::list_sessions`] (one per source) and
/// [`Ops::panes`] (one per session) feed the tree incrementally.
///
/// Only ONE method mutates the mux ([`Ops::new_session`]); the rest read. xmux
/// aggregates and switches, so renaming, killing, and window/pane editing stay
/// with the mux that already owns them.
///
/// This is deliberately one trait, not split into read/mutate halves: the
/// `Switcher` is its sole consumer and uses every method, so an ISP split would
/// add test boilerplate without decoupling any independent caller. Split it only
/// when a second consumer needs just one half.
#[async_trait::async_trait]
pub trait Ops: Send + Sync {
    /// The resolved source aliases in display order - synchronous, no probing -
    /// so the UI can paint host skeletons before any probe runs.
    fn sources(&self) -> Vec<String>;
    /// Probes one source's sessions. `Ok` (possibly empty) ⇒ reachable; `Err` ⇒
    /// unreachable (the message is shown as the host's failure reason).
    async fn list_sessions(&self, source: &str) -> anyhow::Result<Vec<Session>>;
    async fn new_session(&self, source: &str, name: &str) -> anyhow::Result<Session>;
    async fn panes(&self, s: &Session) -> anyhow::Result<Vec<WindowPanes>>;
}

/// The outcome of a [`MuxOp`]. [`State::fold_op_result`] folds it into the
/// inventory (the single owner of `groups`/`panes`/`panes_loaded`) and returns
/// an [`OpFollow`] telling the switcher how to rebuild its rows.
///
/// [`State::fold_op_result`]: crate::state::State::fold_op_result
#[derive(Debug, Clone)]
pub enum OpResult {
    Created {
        session: Session,
        panes: Vec<WindowPanes>,
    },
    Failed {
        message: String,
    },
}

/// What the switcher must do after [`State::fold_op_result`] applies an op's
/// inventory mutation: rebuild the rows and, per the op, move the cursor to the
/// new session (a create) or, on failure, flash a message with no inventory
/// change. The mutation is State's; the row rebuild + cursor restore is the
/// switcher's.
///
/// [`State::fold_op_result`]: crate::state::State::fold_op_result
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpFollow {
    /// Rebuild, then move the cursor to this new session's row (a create).
    Reselect(String),
    /// No inventory change - flash this message (a failed op).
    Flash(String),
}

/// Runs a [`MuxOp`] against the live mux and returns its [`OpResult`]. Pure over
/// `ops` (no switcher state), so it runs in a detached task off the event loop.
pub async fn run_op(op: &MuxOp, ops: &dyn Ops) -> OpResult {
    match op {
        MuxOp::Create { source, name } => match ops.new_session(source, name).await {
            Ok(session) => {
                let panes = ops.panes(&session).await.unwrap_or_default();
                OpResult::Created { session, panes }
            }
            Err(e) => OpResult::Failed {
                message: format!("create failed: {e}"),
            },
        },
    }
}
