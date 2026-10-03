//! The off-loop operation boundary: the slow (network) mux actions a keypress
//! requests (the [`MuxOp`] carried by [`Command::RunOp`](crate::model::Command)),
//! their outcomes (`OpResult`), the `Ops` trait the app implements over the live
//! mux, and `run_op` which executes one `MuxOp` against `Ops` in a detached task.
//! Pure over `Ops` - no switcher state - so it never touches the event loop.

use crate::model::MuxOp;
use crate::session::{Address, Session};

/// The side-effecting actions the switcher delegates to the host program. The
/// event loop also drives the streaming probes through it: [`Ops::sources`] seeds
/// the host skeletons, then [`Ops::list_sessions`] (one per source) feeds the tree
/// incrementally.
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
    /// The command that validates the pane's values against `source`, or `None` where the
    /// machine has no remote login to run. It resolves the effective ssh configuration,
    /// installs a pending credential visible only to that command, and starts no login.
    async fn login_command(
        &self,
        source: &str,
        login: &crate::transport::Login,
        password: String,
    ) -> anyhow::Result<Option<crate::transport::CommandSpec>>;

    /// The pane's remaining choices, applied once a login has worked. A step that could
    /// not do what it said returns an outcome or note for the completion message.
    ///
    /// It is called only after a connection that worked: neither is worth doing over one
    /// that did not. Registering a key is its own ssh, answered the way the login was,
    /// because a side that cannot multiplex keeps no authenticated connection to carry it.
    async fn login_follow_ups(
        &self,
        source: &str,
        login: &crate::transport::Login,
        write_config: bool,
        register: Option<KeyRegistration>,
    ) -> (RegistrationOutcome, Vec<String>);
}

/// The key registration a login asked for. It is handed over only once the login worked,
/// because it authenticates through the machine's held credential.
pub struct KeyRegistration {
    /// The shell family the login's own command read, or `None` when its answer was not in
    /// what ssh wrote. The registration is a command for one family, so without it there
    /// is nothing to send.
    pub shell: Option<crate::transport::vocab::RemoteShell>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistrationOutcome {
    NotRequested,
    Registered,
    Skipped(String),
    Failed(String),
}

/// What one login run did. The connection is the verdict the app branches on; the
/// notes are what the checkboxes could NOT do, so a step that failed says so instead of
/// passing silently. Empty notes mean every step that ran worked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginOutcome {
    pub connect: crate::link::unlock::UnlockOutcome,
    pub registration: RegistrationOutcome,
    pub notes: Vec<String>,
}

/// The outcome of a [`MuxOp`]. [`State::fold_op_result`] folds it into the
/// inventory (the single owner of `groups`) and returns
/// an [`OpFollow`] telling the switcher how to rebuild its rows.
///
/// [`State::fold_op_result`]: crate::state::State::fold_op_result
#[derive(Debug, Clone)]
pub enum OpResult {
    Created {
        session: Session,
    },
    Failed {
        message: String,
    },
    /// The login worker's verdict. Not an inventory mutation: the app reacts to it
    /// (re-probe the machine on success, a flash on failure), never a fold into the
    /// tree. `source` names the host that was logged in to, so success re-probes only
    /// its machine rather than the whole roster. `login` is what the connection was made
    /// WITH, so a success can record the non-secret values on the machine.
    Login {
        source: String,
        login: crate::transport::Login,
        outcome: LoginOutcome,
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
    Reselect(Address),
    /// No inventory change - flash this message (a failed op).
    Flash(String),
    /// The login verdict: re-probe that `source`'s machine on success (only it could
    /// have changed reach state), flash the failure reason otherwise. `login` rides along
    /// so a success can be recorded on the machine before that re-probe goes out.
    LoginResult {
        source: String,
        login: crate::transport::Login,
        outcome: LoginOutcome,
    },
}

/// Runs a [`MuxOp`] against the live mux and returns its [`OpResult`]. Pure over
/// `ops` (no switcher state), so it runs in a detached task off the event loop.
pub async fn run_op(op: &MuxOp, ops: &dyn Ops) -> OpResult {
    match op {
        MuxOp::Create { source, name } => match ops.new_session(source, name).await {
            Ok(session) => OpResult::Created { session },
            Err(e) => OpResult::Failed {
                message: format!("create failed: {e}"),
            },
        },
    }
}

/// Finishes a login the app already ran: takes the connection's verdict, runs the two
/// checkboxes over the master it left (and only if it left one), and returns the
/// [`OpResult`] the switcher folds. Pure over `ops` (no switcher state), so it runs in a
/// detached task off the event loop like [`run_op`].
pub async fn run_login_follow_ups(
    source: &str,
    login: &crate::transport::Login,
    connect: crate::link::unlock::UnlockOutcome,
    write_config: bool,
    register: Option<KeyRegistration>,
    ops: &dyn Ops,
) -> OpResult {
    let (registration, notes) = if connect.is_ok() {
        ops.login_follow_ups(source, login, write_config, register)
            .await
    } else {
        (RegistrationOutcome::NotRequested, Vec::new())
    };
    OpResult::Login {
        source: source.to_string(),
        login: login.clone(),
        outcome: LoginOutcome {
            connect,
            registration,
            notes,
        },
    }
}
