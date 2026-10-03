//! The off-loop operation runners for deferred mux actions and login follow-ups.
//! The operation port and its exchanged values live in the domain model; this
//! module decides the UI-facing result messages.

use crate::model::MuxOp;
use crate::session::Address;

pub use crate::model::{KeyRegistration, LoginOutcome, OpResult, Ops, RegistrationOutcome};

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
