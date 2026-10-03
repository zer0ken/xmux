//! The operation port and the values exchanged across it.

use crate::session::Session;

/// The side-effecting actions delegated to the resolved runtime environment.
#[async_trait::async_trait]
pub trait Ops: Send + Sync {
    /// The resolved source aliases in display order, without probing.
    fn sources(&self) -> Vec<String>;
    /// Probes one source's sessions. An empty success is a reachable source with no
    /// sessions; an error is an unreachable source.
    async fn list_sessions(&self, source: &str) -> anyhow::Result<Vec<Session>>;
    async fn new_session(&self, source: &str, name: &str) -> anyhow::Result<Session>;
    /// Builds the command that validates the supplied login, or returns `None` when the
    /// source has no remote login.
    async fn login_command(
        &self,
        source: &str,
        login: &crate::transport::Login,
        password: String,
    ) -> anyhow::Result<Option<crate::transport::CommandSpec>>;
    /// Applies the non-connection choices after a successful login.
    async fn login_follow_ups(
        &self,
        source: &str,
        login: &crate::transport::Login,
        write_config: bool,
        register: Option<KeyRegistration>,
    ) -> (RegistrationOutcome, Vec<String>);
}

/// The key registration requested by a login.
pub struct KeyRegistration {
    /// The shell family reported by the login command.
    pub shell: Option<crate::transport::vocab::RemoteShell>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistrationOutcome {
    NotRequested,
    Registered,
    Skipped(String),
    Failed(String),
}

/// What one login run did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginOutcome {
    pub connect: crate::link::unlock::UnlockOutcome,
    pub registration: RegistrationOutcome,
    pub notes: Vec<String>,
}

/// The result of a deferred mux operation.
#[derive(Debug, Clone)]
pub enum OpResult {
    Created {
        session: Session,
    },
    Failed {
        message: String,
    },
    Login {
        source: String,
        login: crate::transport::Login,
        outcome: LoginOutcome,
    },
}
