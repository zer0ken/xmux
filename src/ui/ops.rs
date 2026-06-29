//! The off-loop operation boundary: the slow (network) mux actions a keypress
//! requests (the [`MuxOp`] carried by [`Command::RunOp`](crate::model::Command)),
//! their outcomes (`OpResult`), the `Ops` trait the cockpit implements over the live
//! mux, and `run_op` which executes one `MuxOp` against `Ops` in a detached task.
//! Pure over `Ops` — no switcher state — so it never touches the event loop.

use crate::model::MuxOp;
use crate::session::{Session, WindowPanes};

/// The side-effecting actions the switcher delegates to the host program. The
/// event loop also drives the streaming probes through it: [`Ops::sources`] seeds
/// the host skeletons, then [`Ops::list_sessions`] (one per source) and
/// [`Ops::panes`] (one per session) feed the tree incrementally.
///
/// This is deliberately one trait, not split into read/mutate halves: the
/// `Switcher` is its sole consumer and uses every method, so an ISP split would
/// add test boilerplate without decoupling any independent caller. Split it only
/// when a second consumer needs just one half.
#[async_trait::async_trait]
pub trait Ops: Send + Sync {
    /// The resolved source aliases in display order — synchronous, no probing —
    /// so the UI can paint host skeletons before any probe runs.
    fn sources(&self) -> Vec<String>;
    /// Probes one source's sessions. `Ok` (possibly empty) ⇒ reachable; `Err` ⇒
    /// unreachable (the message is shown as the host's failure reason).
    async fn list_sessions(&self, source: &str) -> anyhow::Result<Vec<Session>>;
    async fn new_session(&self, source: &str, name: &str) -> anyhow::Result<Session>;
    /// Creates a new window in `session` on `source` (the `n` action on a session row).
    async fn new_window(&self, source: &str, session: &str, name: &str) -> anyhow::Result<()>;
    /// Splits `target` (`session:window`) into a new pane (the `n` action on a window row).
    async fn split_window(&self, source: &str, target: &str, vertical: bool) -> anyhow::Result<()>;
    async fn kill(&self, s: &Session) -> anyhow::Result<()>;
    async fn rename(&self, s: &Session, new_name: &str) -> anyhow::Result<()>;
    async fn panes(&self, s: &Session) -> anyhow::Result<Vec<WindowPanes>>;
    async fn kill_window(&self, source: &str, target: &str) -> anyhow::Result<()>;
    async fn rename_window(&self, source: &str, target: &str, new_name: &str)
        -> anyhow::Result<()>;
}

/// The outcome of a [`MuxOp`], applied back into the switcher state by
/// [`Switcher::apply_op_result`].
#[derive(Debug, Clone)]
pub enum OpResult {
    Created {
        session: Session,
        panes: Vec<WindowPanes>,
    },
    Renamed {
        source: String,
        old_name: String,
        new_name: String,
    },
    Killed {
        address: String,
    },
    /// A session's windows/panes were re-fetched after a new-window/split so the
    /// tree shows the change.
    PanesRefreshed {
        address: String,
        panes: Vec<WindowPanes>,
    },
    Failed {
        message: String,
    },
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
        MuxOp::NewWindow {
            source,
            session,
            name,
        } => match ops.new_window(source, session, name).await {
            Ok(()) => refreshed_panes(ops, source, session).await,
            Err(e) => OpResult::Failed {
                message: format!("new window failed: {e}"),
            },
        },
        MuxOp::SplitWindow {
            source,
            target,
            session,
            vertical,
        } => match ops.split_window(source, target, *vertical).await {
            Ok(()) => refreshed_panes(ops, source, session).await,
            Err(e) => OpResult::Failed {
                message: format!("split failed: {e}"),
            },
        },
        MuxOp::Rename { sess, new_name } => match ops.rename(sess, new_name).await {
            Ok(()) => OpResult::Renamed {
                source: sess.source.clone(),
                old_name: sess.name.clone(),
                new_name: new_name.clone(),
            },
            Err(e) => OpResult::Failed {
                message: format!("rename failed: {e}"),
            },
        },
        MuxOp::Kill { sess } => match ops.kill(sess).await {
            Ok(()) => OpResult::Killed {
                address: sess.address(),
            },
            Err(e) => OpResult::Failed {
                message: format!("kill failed: {e}"),
            },
        },
        MuxOp::KillWindow {
            source,
            session,
            target,
        } => match ops.kill_window(source, target).await {
            Ok(()) => refreshed_panes(ops, source, session).await,
            Err(e) => OpResult::Failed {
                message: format!("kill window failed: {e}"),
            },
        },
        MuxOp::RenameWindow {
            source,
            session,
            target,
            new_name,
        } => match ops.rename_window(source, target, new_name).await {
            Ok(()) => refreshed_panes(ops, source, session).await,
            Err(e) => OpResult::Failed {
                message: format!("rename window failed: {e}"),
            },
        },
    }
}

/// Re-fetches a session's windows/panes after a structural change (new window /
/// split) so the tree reflects it. A failed fetch still resolves (empty) rather
/// than erroring the whole op.
async fn refreshed_panes(ops: &dyn Ops, source: &str, session: &str) -> OpResult {
    let sess = Session {
        source: source.to_string(),
        name: session.to_string(),
        ..Default::default()
    };
    let panes = ops.panes(&sess).await.unwrap_or_default();
    OpResult::PanesRefreshed {
        address: sess.address(),
        panes,
    }
}
