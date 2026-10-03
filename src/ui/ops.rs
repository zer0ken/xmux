//! The off-loop operation runners for deferred mux actions and login follow-ups.
//! The operation port and its exchanged values live in the domain model; this
//! module decides the UI-facing result messages.

use crate::model::MuxOp;
pub use crate::model::{KeyRegistration, LoginOutcome, OpResult, Ops, RegistrationOutcome};
pub use crate::state::OpFollow;

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
