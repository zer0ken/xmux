//! The operation port and the values exchanged across it.

use crate::session::Session;

/// The side-effecting actions delegated to the resolved runtime environment.
#[async_trait::async_trait]
pub trait Ops: Send + Sync {
    /// The resolved host ids in display order, without probing.
    fn hosts(&self) -> Vec<String>;
    /// Probes one host's sessions. An empty success is a reachable host with no
    /// sessions; an error is an unreachable host.
    async fn list_sessions(&self, host: &str) -> anyhow::Result<Vec<Session>>;
    async fn new_session(&self, host: &str, name: &str) -> anyhow::Result<Session>;
    /// Builds the command that validates the supplied login, or returns `None` when the
    /// host has no remote login.
    async fn login_command(
        &self,
        host: &str,
        login: &crate::transport::Login,
        password: String,
    ) -> anyhow::Result<Option<crate::transport::CommandSpec>>;
    /// Records the values of a successful login in ssh config, or says why it could not.
    fn write_login_stanza(&self, host: &str, login: &crate::transport::Login)
        -> Result<(), String>;
    /// Registers this machine's public key on the machine a successful login reached.
    async fn register_login_key(
        &self,
        host: &str,
        login: &crate::transport::Login,
        register: KeyRegistration,
    ) -> RegistrationOutcome;
}

/// The key registration requested by a login.
pub struct KeyRegistration {
    /// The shell family reported by the login command, which reads it because a locked
    /// machine's family is unknown before its login.
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
        host: String,
        login: crate::transport::Login,
        /// The submission this result answers.
        attempt: u64,
        outcome: LoginOutcome,
    },
    /// A step boundary a running login reported before its verdict.
    LoginProgress {
        host: String,
        attempt: u64,
        event: crate::model::LoginEvent,
    },
    /// The lines of a machine's key files that hold this machine's public keys, which a
    /// logout looks for before it closes anything.
    MachineKeysFound {
        machine: String,
        result: Result<Vec<crate::provision::env::MachineKeyLine>, String>,
    },
    /// What removing the key lines a logout chose did.
    MachineKeysRemoved {
        machine: String,
        result: Result<(), String>,
    },
    /// The ssh config entries naming a machine being logged out that xmux did not write,
    /// which the logout asks about before it changes them.
    SshConfigEntriesFound {
        machine: String,
        result: Result<Vec<crate::provision::config::RemovedEntry>, String>,
    },
    /// What removing a logged-out machine from ssh config did: the entries that named it,
    /// or why they stay.
    SshConfigEntriesRemoved {
        machine: String,
        result: Result<Vec<crate::provision::config::RemovedEntry>, String>,
    },
}
