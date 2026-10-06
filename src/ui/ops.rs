//! The off-loop operation runners for deferred mux actions and login follow-ups.
//! The operation port and its exchanged values live in the domain model; this
//! module decides the UI-facing result messages.
//!
//! A switcher key that COMMITS a slow action resolves it through the state's apply into a
//! deferred-operation command it RETURNS up; the run loop spawns the runner here and folds
//! the outcome back through the operation channel, so the switcher holds no
//! pending-operation queue of its own.

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

/// Finishes a login the app already ran: takes the ssh child's conversation, runs the
/// pane's after-login choice after a connection that worked, and returns the [`LoginOutcome`]
/// the switcher folds. Each follow-up reports through `progress` as it settles, so the pane's
/// steps advance with the work rather than all at once. A failed recording does not stop
/// the key registration or the mux search. Pure over `ops` (no switcher
/// state), so it runs in a detached task off the event loop like [`run_op`].
pub async fn run_login_follow_ups(
    source: &str,
    login: &crate::transport::Login,
    conversation: crate::link::unlock::Conversation,
    write_config: bool,
    register_key: bool,
    ops: &dyn Ops,
    progress: &(dyn Fn(crate::model::LoginEvent) + Send + Sync),
) -> LoginOutcome {
    let connect = conversation.outcome;
    let mut saved = None;
    let mut registration = RegistrationOutcome::NotRequested;
    if connect.is_ok() {
        if write_config {
            let result = ops.write_login_stanza(source, login);
            progress(crate::model::LoginEvent::Saved(result.clone()));
            saved = Some(result);
        }
        if register_key {
            let register = KeyRegistration {
                shell: conversation.shell,
            };
            registration = ops.register_login_key(source, login, register).await;
        }
    }
    LoginOutcome {
        connect,
        auth_method: conversation.auth_method,
        output: conversation.output,
        saved,
        registration,
    }
}
