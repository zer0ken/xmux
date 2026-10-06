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
    /// Records the values of a successful login in ssh config, or says why it could not.
    fn write_login_stanza(
        &self,
        source: &str,
        login: &crate::transport::Login,
    ) -> Result<(), String>;
    /// Registers this machine's public key on the host a successful login reached.
    async fn register_login_key(
        &self,
        source: &str,
        login: &crate::transport::Login,
        register: KeyRegistration,
    ) -> RegistrationOutcome;
}

/// The key registration requested by a login.
pub struct KeyRegistration {
    /// The shell family reported by the login command, which reads it because a locked
    /// host's family is unknown before its login.
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
    pub auth_method: Option<crate::model::AuthMethod>,
    /// ssh's own sanitized text from the login command, empty when no ssh ran.
    pub output: String,
    /// The ssh config recording, `None` when the pane did not ask for it.
    pub saved: Option<Result<(), String>>,
    pub registration: RegistrationOutcome,
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
        /// The submission this result answers.
        attempt: u64,
        outcome: LoginOutcome,
    },
    /// A step boundary a running login reported before its verdict.
    LoginProgress {
        source: String,
        attempt: u64,
        event: crate::model::LoginEvent,
    },
    /// The lines of a machine's key files that hold this machine's public keys, which a
    /// logout looks for before it closes anything.
    HostKeysFound {
        machine: String,
        result: Result<Vec<crate::provision::env::HostKeyLine>, String>,
    },
    /// What removing the key lines a logout chose did.
    HostKeysRemoved {
        machine: String,
        result: Result<(), String>,
    },
}
