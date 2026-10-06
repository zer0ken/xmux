//! The transport axis: how a mux argv reaches the server, SEPARATE from which mux
//! runs there (that is `Mux`). A `Transport` owns argv assembly and the ssh
//! wrapping only - it never decides a server model. Each machine implementation lives in
//! its own file behind the `Transport` trait - `Local` (`local.rs`), `Ssh`
//! (`ssh.rs`), `Wsl` (`wsl.rs`) - mirroring how each mux implementation lives behind `Mux`. Shared shell
//! helpers (`quote`/`remote_command`) is in `vocab.rs`, the peer of
//! `mux/vocab.rs`. A new implementation is a new file implementing `Transport` plus a
//! factory here; the trait and its callers name no concrete implementation.

pub mod auth;
pub(crate) mod auth_log;
pub mod diagnostic;
pub mod local;
pub mod ssh;
pub mod vocab;
pub mod wsl;

pub use local::Local;
pub use ssh::{Login, Ssh};
pub use wsl::Wsl;

#[derive(Clone)]
pub struct CommandSpec {
    argv: Vec<String>,
    env: Vec<(String, String)>,
    auth: Option<auth::CommandAuth>,
    detach_tty: bool,
    host_key_command: Option<String>,
    credential_generation: u64,
    auth_unavailable: Option<String>,
    password_only_retry: Option<Box<CommandSpec>>,
    auth_trace_allowed: bool,
    observe_auth: bool,
}

impl CommandSpec {
    pub fn new(program: impl Into<String>, args: Vec<String>) -> Self {
        let mut argv = vec![program.into()];
        argv.extend(args);
        Self {
            argv,
            env: Vec::new(),
            auth: None,
            detach_tty: false,
            host_key_command: None,
            credential_generation: 0,
            auth_unavailable: None,
            password_only_retry: None,
            auth_trace_allowed: true,
            observe_auth: false,
        }
    }

    pub fn from_argv(argv: Vec<String>) -> Self {
        Self {
            argv,
            env: Vec::new(),
            auth: None,
            detach_tty: false,
            host_key_command: None,
            credential_generation: 0,
            auth_unavailable: None,
            password_only_retry: None,
            auth_trace_allowed: true,
            observe_auth: false,
        }
    }

    pub fn argv(&self) -> &[String] {
        &self.argv
    }

    pub fn program(&self) -> &str {
        self.argv.first().map(String::as_str).unwrap_or("")
    }

    pub fn args(&self) -> &[String] {
        self.argv.get(1..).unwrap_or_default()
    }

    pub fn env(&self) -> &[(String, String)] {
        &self.env
    }

    pub fn map_argv(mut self, f: impl FnOnce(Vec<String>) -> Vec<String>) -> Self {
        self.argv = f(self.argv);
        self
    }

    pub fn with_auth(mut self, access: auth::AskpassAccess, set_display: bool) -> Self {
        self.credential_generation = access.generation();
        let auth = auth::command_auth(access, set_display);
        self.env = auth.environment().to_vec();
        self.auth = Some(auth);
        self
    }

    pub fn with_credential_generation(mut self, generation: u64) -> Self {
        self.credential_generation = generation;
        self
    }

    pub fn credential_generation(&self) -> u64 {
        self.credential_generation
    }

    pub fn with_auth_unavailable(mut self, reason: Option<String>) -> Self {
        self.auth_unavailable = reason;
        self
    }

    pub fn auth_unavailable(&self) -> Option<&str> {
        self.auth_unavailable.as_deref()
    }

    pub fn with_auth_trace_allowed(mut self, allowed: bool) -> Self {
        self.auth_trace_allowed = allowed;
        self
    }

    pub fn auth_trace_allowed(&self) -> bool {
        self.auth_trace_allowed
    }

    pub fn with_auth_observation(mut self) -> Self {
        if let Some(retry) = self.password_only_retry.take() {
            self.password_only_retry = Some(Box::new(retry.with_auth_observation()));
        }
        self.observe_auth = true;
        self
    }

    pub fn observe_auth(&self) -> bool {
        self.observe_auth
    }

    pub fn detach_tty(mut self) -> Self {
        self.detach_tty = true;
        self
    }

    pub(crate) fn with_host_key_command(mut self, command: String) -> Self {
        if let Some(retry) = self.password_only_retry.take() {
            self.password_only_retry = Some(Box::new(retry.with_host_key_command(command.clone())));
        }
        self.host_key_command = Some(command);
        self
    }

    /// Attaches the same command authenticating with the held password alone, run once
    /// when the machine closes the connection after accepting a key.
    pub(crate) fn with_password_only_retry(mut self, retry: CommandSpec) -> Self {
        self.password_only_retry = Some(Box::new(retry));
        self
    }

    /// The command to run instead after this one failed, when it held a password that
    /// askpass never handed over and the machine dropped the connection before a session
    /// started without refusing authentication: the machine accepted a key and could not
    /// open a session for it. The caller runs it once, inside the time budget the first
    /// run started with, and calls [`CommandSpec::password_only_worked`] when it succeeds.
    pub fn password_only_retry(&self, exit_code: i32, diagnostic: &str) -> Option<&CommandSpec> {
        let retry = self.password_only_retry.as_deref()?;
        let auth = self.auth.as_ref()?;
        if exit_code != 255
            || auth.supplied()
            || !crate::transport::diagnostic::closed_before_session(diagnostic)
        {
            return None;
        }
        Some(retry)
    }

    /// Marks the held password so every later command skips key authentication. Called
    /// only after the password-only copy succeeded, so a drop that had another cause
    /// leaves key authentication in use.
    pub fn password_only_worked(&self) {
        if let Some(auth) = &self.auth {
            auth.mark_key_opens_no_session();
        }
    }

    pub(crate) fn host_key_command(&self) -> Option<&str> {
        self.host_key_command.as_deref()
    }

    pub fn should_detach_tty(&self) -> bool {
        self.detach_tty
    }

    /// Whether askpass handed the held password to this command or to the password-only
    /// copy that ran in its place.
    pub fn password_was_supplied(&self) -> bool {
        self.auth.as_ref().is_some_and(auth::CommandAuth::supplied)
            || self
                .password_only_retry
                .as_deref()
                .is_some_and(CommandSpec::password_was_supplied)
    }

    pub fn credential_rejection_generation(&self) -> Option<u64> {
        self.auth
            .as_ref()
            .and_then(auth::CommandAuth::rejection_generation)
            .or_else(|| {
                self.password_only_retry
                    .as_deref()
                    .and_then(CommandSpec::credential_rejection_generation)
            })
    }

    pub fn refused_auth_prompt(&self) -> Option<String> {
        self.auth
            .as_ref()
            .and_then(auth::CommandAuth::refused_prompt)
            .or_else(|| {
                self.password_only_retry
                    .as_deref()
                    .and_then(CommandSpec::refused_auth_prompt)
            })
    }

    /// Keeps this command's one-shot askpass token valid for a spawned child.
    pub(crate) fn auth_guard(&self) -> Option<auth::CommandAuth> {
        self.auth.clone()
    }

    pub fn has_credential(&self) -> bool {
        self.auth.is_some()
    }

    /// Removes the held password only when ssh exited 255, askpass handed this command
    /// the password, and ssh wrote its own authentication refusal line. Any other failure
    /// keeps it.
    pub fn forget_refused_password(&self, exit_code: i32, diagnostic: &str) -> bool {
        if exit_code == 255
            && crate::transport::diagnostic::contains_auth_refusal(diagnostic)
            && self.password_was_supplied()
        {
            if let Some(auth) = &self.auth {
                auth.forget_active();
            }
            true
        } else {
            false
        }
    }

    pub fn promote_credential(&self) -> bool {
        self.auth.as_ref().is_some_and(auth::CommandAuth::promote)
    }

    /// Promotes the login's pending credential for the machine only when askpass handed
    /// it to ssh; a login that never asked for the password discards it. `false` when a
    /// newer login replaced this one.
    pub fn finish_successful_login(&self) -> bool {
        let Some(auth) = &self.auth else {
            return true;
        };
        if auth.supplied() {
            auth.promote()
        } else {
            auth.discard();
            true
        }
    }

    pub fn discard_credential(&self) {
        if let Some(auth) = &self.auth {
            auth.discard();
        }
    }
}

impl std::fmt::Debug for CommandSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommandSpec")
            .field("argv", &self.argv)
            .field(
                "env_keys",
                &self.env.iter().map(|(key, _)| key).collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl PartialEq for CommandSpec {
    fn eq(&self, other: &Self) -> bool {
        self.argv == other.argv && self.env == other.env
    }
}

impl Eq for CommandSpec {}

impl<T: AsRef<str>> PartialEq<Vec<T>> for CommandSpec {
    fn eq(&self, other: &Vec<T>) -> bool {
        self.argv.len() == other.len()
            && self
                .argv
                .iter()
                .zip(other)
                .all(|(left, right)| left == right.as_ref())
    }
}

impl std::ops::Deref for CommandSpec {
    type Target = [String];

    fn deref(&self) -> &Self::Target {
        &self.argv
    }
}

/// The machine boundary: turns a full mux argv (`argv[0]` = the mux binary) into a
/// runnable `(command, args)`, and wraps interactive/control/raw execution for the
/// machine it targets. Implementors are the machine implementations (`Local`, `Ssh`, `Wsl`); no
/// caller branches on which one - it addresses a machine through this trait.
pub trait Transport: Send + Sync {
    /// `"local"`, the ssh alias, or `wsl.<distro>` - the stable host id and `Hosts` map
    /// key.
    fn host_id(&self) -> &str;

    /// True for a remote (ssh) machine. Used only to SHAPE ssh options - not to decide a
    /// server MODEL (that is `ServerModel`) nor the two capability predicates below.
    fn is_remote(&self) -> bool {
        false
    }

    /// True when a display attach on this machine runs THROUGH a machine shell (so an attach
    /// can prepend a `tty >file` record snippet, and a `SwitchPlan::Shell` can run). A
    /// machine that spawns the mux binary directly is `false` (the default). NOT derived
    /// from `is_remote`: a local-but-shell implementation (WSL) sets this `true` while staying
    /// non-remote.
    fn runs_through_shell(&self) -> bool {
        false
    }

    /// True when THIS box's local mux registry (`~/.psmux`) is the authority for this
    /// host's sessions - enabling the registry-merge enumeration and the local
    /// `list-clients` tty probe. `false` (the default) for a machine whose sessions live
    /// on the far side. NOT derived from `is_remote`.
    fn local_registry_scope(&self) -> bool {
        false
    }

    /// True when running one more command on this machine opens no new connection to it.
    /// The local box and a WSL distribution are reached by a local process, and an ssh
    /// machine is reached over one authenticated master when this side shares it across
    /// runs. A POLL host refreshes on a cadence only over such a path, because there a
    /// repeat costs the machine nothing it is not already honouring; anywhere else every
    /// repeat is a fresh login. `false` (the default) is the side that must not repeat.
    fn reuses_connection(&self) -> bool {
        false
    }

    /// A control command for this transport's shared connection, when it owns one.
    fn close_shared_connection_argv(&self) -> Option<CommandSpec> {
        None
    }

    /// A local command that prints this transport's effective configuration, its shared
    /// connection's socket path included, when it shares a connection at all.
    fn shared_connection_config_argv(&self) -> Option<CommandSpec> {
        None
    }

    /// Which shell family answers this machine's remote commands. `Posix` (the default)
    /// for every machine whose shell is known to be POSIX and for one not yet asked; a
    /// remote learns its own answer from the reachability probe. NOT derived from the
    /// three predicates above: an ssh remote is remote and shell-based whatever family
    /// its shell belongs to.
    fn remote_shell(&self) -> vocab::RemoteShell {
        vocab::RemoteShell::Posix
    }

    /// Records the family the probe read. A no-op on a machine whose shell cannot
    /// differ (the local box and a WSL distribution are POSIX by construction).
    fn set_remote_shell(&mut self, _shell: vocab::RemoteShell) {}

    /// Records the connection values a successful login established, so every later
    /// command reaches the machine the way that login did. A no-op on a machine with
    /// nothing to authenticate.
    ///
    /// Without this the values would live only in the login's own argv: the machine
    /// would be reached as whoever runs xmux the moment the login's connection is gone,
    /// which is a different account and a refusal.
    fn set_login(&mut self, _login: ssh::Login) {}

    /// Hands this transport the process-memory credential store. An ssh transport
    /// consults it each time a command is composed, so every spawn path and every host
    /// on one machine receives current authentication: submitted connection values are
    /// the machine's, not one host's. A host found later and a transport rebuilt from
    /// the roster receive the same store before use, so neither can lose the machine
    /// credential.
    fn set_credentials(&mut self, _credentials: auth::Credentials) {}

    fn has_credential(&self) -> bool {
        false
    }

    fn credential_generation(&self) -> u64 {
        0
    }

    fn probe_diagnostic(&self, diagnostic: String) -> String {
        diagnostic
    }

    /// Turns a full mux argv (`argv[0]` = the mux binary) into the (command, args)
    /// to spawn.
    fn exec_argv(&self, tty: bool, mux_argv: &[String]) -> CommandSpec;

    /// Lowers a mux attach argv into the interactive terminal-handover (cmd, args).
    /// This is the SOLE owner of the `exec`/ssh-tty machinery.
    fn interactive_attach_argv(&self, mux_attach_argv: &[String]) -> CommandSpec;

    /// The standalone CLI handover. Its terminal may be used by ssh for interactive
    /// authentication because no TUI or background channel owns it.
    fn cli_attach_argv(&self, mux_attach_argv: &[String]) -> CommandSpec {
        self.interactive_attach_argv(mux_attach_argv)
    }

    /// The argv for a `-CC` control-mode child given the mux's control argv.
    fn control_argv(&self, mux_control_argv: &[String]) -> CommandSpec;

    /// True when the machine's `-CC` control child must run on a pty the spawner
    /// allocates for it. The remote path already forces one (`ssh -tt`) and WSL
    /// wraps its child in `script`; a machine that spawns the mux binary directly
    /// on a Unix box (local tmux) has no such flag and the `-CC` client dies on
    /// pipe stdio, so the spawner must give it a pty itself. A machine arranges the
    /// terminal on the HOST side and never rewrites a mux flag to work around a pipe:
    /// which control payload runs is the mux's word, not the transport's.
    fn control_needs_pty(&self) -> bool {
        false
    }

    /// Joins a raw remote shell command behind the machine's execution wrapper.
    /// `None` when the machine issues no remote shell command (a local machine).
    fn raw_shell_argv(&self, _remote_cmd: &str) -> Option<CommandSpec> {
        None
    }

    /// The explicit login validation. Only this path may use a pending credential and
    /// accept a previously unseen host key.
    fn login_argv(&self, remote_cmd: &str) -> Option<CommandSpec> {
        self.raw_shell_argv(remote_cmd)
    }

    /// A connection of its own that may authenticate with a key and nothing else, running
    /// `remote_cmd` directly. `None` when the machine is not reached by logging in.
    fn key_only_argv(&self, _remote_cmd: &str) -> Option<CommandSpec> {
        None
    }

    /// Clones into a fresh box - a spawned poll task needs an owned transport, and a
    /// trait object cannot derive `Clone`.
    fn clone_box(&self) -> Box<dyn Transport>;

    /// The same machine, reached exactly as this transport reaches it (the recorded
    /// login and shell family included), answering as the host `id`. A host found on
    /// a machine after it connected is built from this, so its first command already
    /// knows what the machine's probe and login established.
    fn clone_as(&self, id: &str) -> Box<dyn Transport>;

    /// The construction data that reaches this machine as this host. Rebuilding a
    /// transport from it with [`MachineKind::transport`] reaches the same machine the same
    /// way, without what was recorded on this one since it was built (a login, a shell
    /// family, the credential store).
    fn machine_kind(&self) -> MachineKind;
}

impl Clone for Box<dyn Transport> {
    fn clone(&self) -> Self {
        self.clone_box()
    }
}

/// A boxed transport is itself a `Transport`, delegating to the inner value. This lets
/// a stored `Box<dyn Transport>` be passed where `&dyn Transport` is expected (via
/// `&boxed`) without an explicit reborrow at every call site.
impl Transport for Box<dyn Transport> {
    fn host_id(&self) -> &str {
        (**self).host_id()
    }
    fn is_remote(&self) -> bool {
        (**self).is_remote()
    }
    fn runs_through_shell(&self) -> bool {
        (**self).runs_through_shell()
    }
    fn local_registry_scope(&self) -> bool {
        (**self).local_registry_scope()
    }
    fn reuses_connection(&self) -> bool {
        (**self).reuses_connection()
    }
    fn remote_shell(&self) -> vocab::RemoteShell {
        (**self).remote_shell()
    }
    fn set_remote_shell(&mut self, shell: vocab::RemoteShell) {
        (**self).set_remote_shell(shell)
    }
    fn set_login(&mut self, login: ssh::Login) {
        (**self).set_login(login)
    }
    fn set_credentials(&mut self, credentials: auth::Credentials) {
        (**self).set_credentials(credentials)
    }
    fn has_credential(&self) -> bool {
        (**self).has_credential()
    }
    fn credential_generation(&self) -> u64 {
        (**self).credential_generation()
    }
    fn probe_diagnostic(&self, diagnostic: String) -> String {
        (**self).probe_diagnostic(diagnostic)
    }
    fn exec_argv(&self, tty: bool, mux_argv: &[String]) -> CommandSpec {
        (**self).exec_argv(tty, mux_argv)
    }
    fn interactive_attach_argv(&self, mux_attach_argv: &[String]) -> CommandSpec {
        (**self).interactive_attach_argv(mux_attach_argv)
    }
    fn cli_attach_argv(&self, mux_attach_argv: &[String]) -> CommandSpec {
        (**self).cli_attach_argv(mux_attach_argv)
    }
    fn control_argv(&self, mux_control_argv: &[String]) -> CommandSpec {
        (**self).control_argv(mux_control_argv)
    }
    fn control_needs_pty(&self) -> bool {
        (**self).control_needs_pty()
    }
    fn raw_shell_argv(&self, remote_cmd: &str) -> Option<CommandSpec> {
        (**self).raw_shell_argv(remote_cmd)
    }
    fn login_argv(&self, remote_cmd: &str) -> Option<CommandSpec> {
        (**self).login_argv(remote_cmd)
    }
    fn key_only_argv(&self, remote_cmd: &str) -> Option<CommandSpec> {
        (**self).key_only_argv(remote_cmd)
    }
    fn clone_box(&self) -> Box<dyn Transport> {
        (**self).clone_box()
    }
    fn clone_as(&self, id: &str) -> Box<dyn Transport> {
        (**self).clone_as(id)
    }
    fn machine_kind(&self) -> MachineKind {
        (**self).machine_kind()
    }
}

/// The concrete, runnable shape of a display-client switch - what the driver hands to
/// `run_lowered`. Lives on the TRANSPORT side (it is the execution shape), not in the
/// mux's intent set. The mux never names these variants.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoweredSwitch {
    /// A local mux argv (`argv[0]` = binary) - run non-interactively.
    Local(CommandSpec),
    /// A full ssh argv carrying a guarded raw remote `switch-client` snippet, run via
    /// the same path `run_raw` uses.
    RawSsh(CommandSpec),
}

/// Which machine kind a host reaches its mux over, carrying that kind's own
/// construction data. The SINGLE representation of transport kind: config/`Hosts::build`
/// picks a variant, and the `MachineKind` query methods ([`transport`](Self::transport),
/// [`local_socket`](Self::local_socket)) are the only code that matches on the kind. A new
/// kind is a variant here plus one arm in each of those methods - no code OUTSIDE
/// `MachineKind` matches on the kind.
#[derive(Clone, Debug)]
pub enum MachineKind {
    /// The local machine, optionally targeting a non-default mux socket (`-S`). `id` is
    /// the host id it answers as (empty ⇒ the bare `local`).
    Local {
        #[allow(missing_docs)]
        id: String,
        socket: Option<String>,
    },
    /// A remote over ssh: the host `id` it answers as (empty ⇒ `alias`), the
    /// destination `alias`, its ControlMaster socket `control_path`, and the LOCAL
    /// platform `os` (gates ControlMaster).
    Ssh {
        id: String,
        alias: String,
        control_path: String,
        os: String,
    },
    /// A WSL distribution on this machine: the host `id` it answers as (empty means the
    /// bare machine name `wsl.<distro>`) and the `distro` name `wsl.exe -d` takes.
    Wsl { id: String, distro: String },
}

/// The [`MachineKind`] for `machine`, answering as the host `id`.
///
/// The SINGLE place a machine's construction data is assembled, so a host added LATER
/// (an async mux discovery result) reaches its machine exactly as one built at launch
/// does, and the `Host` the loop drives and the `HostDef` the off-loop ops use cannot
/// disagree about how to get there. The ControlMaster socket is per MACHINE, not per
/// host: several muxes on one machine share the one multiplexed connection.
pub fn kind_for(
    machine: &str,
    id: String,
    os: &str,
    xmux_dir: &std::path::Path,
    local_socket: Option<String>,
) -> MachineKind {
    if machine == crate::session::LOCAL_MACHINE {
        MachineKind::Local {
            id,
            socket: local_socket,
        }
    } else if let Some(distro) = crate::session::wsl_distro_of(machine) {
        // The kind is read back OUT of the machine name, so a host added later (an
        // async mux-discovery answer carries a bare machine name and nothing else) reaches
        // its distribution the same way one built at launch does.
        MachineKind::Wsl {
            id,
            distro: distro.to_string(),
        }
    } else {
        MachineKind::Ssh {
            id,
            alias: machine.to_string(),
            // OpenSSH expands %C to a fixed-width hash of the connection tuple. The
            // short name leaves room for the random suffix on its temporary Unix socket.
            control_path: xmux_dir.join("cm-%C").to_string_lossy().into_owned(),
            os: os.to_string(),
        }
    }
}

impl MachineKind {
    /// The one site that maps a machine kind to a concrete [`Transport`] (Decision A).
    /// A new kind = a variant above + one arm here (and in the sibling `local_socket`);
    /// no code outside `MachineKind` matches on the kind.
    /// How this machine is ADDRESSED, in words, with the wait that bounds reaching it.
    ///
    /// Shown, never parsed: the unreachable screen states it, so a machine that failed says
    /// what it was asked over. It lives here because this is where a machine implementation's
    /// construction data already lives - the alternative is a caller that matches on the
    /// implementation to describe it, and every such caller drifts from `transport`.
    pub fn addressed_as(&self) -> String {
        match self {
            MachineKind::Local { .. } => "this box, no connection to make".to_string(),
            MachineKind::Ssh { alias, .. } => {
                format!("ssh to {alias}, given {}s to connect", ssh::CONNECT_TIMEOUT)
            }
            MachineKind::Wsl { distro, .. } => format!("wsl distribution {distro}"),
        }
    }

    /// The socket / multiplexed-connection path this machine addresses its mux through,
    /// or empty when it addresses one without a path.
    pub fn socket_path(&self) -> String {
        match self {
            MachineKind::Local { socket, .. } => socket.clone().unwrap_or_default(),
            MachineKind::Ssh { control_path, .. } => control_path.clone(),
            MachineKind::Wsl { .. } => String::new(),
        }
    }

    pub fn transport(self) -> Box<dyn Transport> {
        match self {
            MachineKind::Local { id, socket } if id.is_empty() => local(socket),
            MachineKind::Local { id, socket } => local_as(id, socket),
            MachineKind::Ssh {
                id,
                alias,
                control_path,
                os,
            } if id.is_empty() || id == alias => ssh(alias, control_path, os),
            MachineKind::Ssh {
                id,
                alias,
                control_path,
                os,
            } => ssh_as(id, alias, control_path, os),
            MachineKind::Wsl { id, distro } if id.is_empty() => wsl(distro),
            MachineKind::Wsl { id, distro } => wsl_as(id, distro),
        }
    }

    /// The local mux server socket (`-S`) this machine targets - `Some` only for a local
    /// machine on a non-default socket, `None` for any other kind or the default socket.
    /// Like [`transport`](Self::transport), the match on the kind lives HERE on the type, so
    /// a new implementation is compiler-forced to state its socket in one place.
    pub fn local_socket(&self) -> Option<String> {
        match self {
            MachineKind::Local { socket, .. } => socket.clone(),
            // A WSL distribution is a machine of its own: the `$TMUX` socket this machine is
            // running inside is a Windows-side path that names nothing in the distro.
            MachineKind::Ssh { .. } | MachineKind::Wsl { .. } => None,
        }
    }
}

/// A local machine transport targeting an optional non-default mux socket, answering
/// as the bare `local` host - this machine serving one mux.
pub fn local(socket: Option<String>) -> Box<dyn Transport> {
    Box::new(Local {
        socket,
        ..Local::default()
    })
}

/// A local machine transport answering as the host `id`. Used when this machine serves
/// SEVERAL muxes and each needs its own key.
pub fn local_as(id: String, socket: Option<String>) -> Box<dyn Transport> {
    Box::new(Local { id, socket })
}

/// A remote (ssh) machine transport answering as the host `alias` - that machine
/// serving one mux.
pub fn ssh(alias: String, control_path: String, os: String) -> Box<dyn Transport> {
    Box::new(Ssh {
        id: alias.clone(),
        alias,
        control_path,
        os,
        login: Login::default(),
        credentials: auth::Credentials::default(),
        shell: vocab::RemoteShell::default(),
    })
}

/// A remote (ssh) machine transport answering as the host `id` while still reaching
/// the machine at `alias`. Used when a machine serves SEVERAL muxes.
pub fn ssh_as(id: String, alias: String, control_path: String, os: String) -> Box<dyn Transport> {
    Box::new(Ssh {
        id,
        alias,
        control_path,
        os,
        login: Login::default(),
        credentials: auth::Credentials::default(),
        shell: vocab::RemoteShell::default(),
    })
}

/// A WSL machine transport for `distro`, answering as the bare machine name
/// `wsl.<distro>` - that distribution serving one mux.
pub fn wsl(distro: String) -> Box<dyn Transport> {
    Box::new(Wsl {
        id: crate::session::WSL_PREFIX.to_string() + &distro,
        distro,
    })
}

/// A WSL machine transport answering as the host `id` while still reaching the same
/// `distro`. Used when a distribution serves SEVERAL muxes.
pub fn wsl_as(id: String, distro: String) -> Box<dyn Transport> {
    Box::new(Wsl { id, distro })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_qualified_transport_keeps_reaching_the_same_machine() {
        // Two muxes on one machine are two HOSTS at one DESTINATION: the id
        // distinguishes them, the ssh argv must not change.
        let one = ssh("prod".into(), String::new(), "linux".into());
        let two = ssh_as(
            "prod:zellij".into(),
            "prod".into(),
            String::new(),
            "linux".into(),
        );
        assert_eq!(one.host_id(), "prod");
        assert_eq!(two.host_id(), "prod:zellij");
        let one = one.exec_argv(false, &["tmux".to_string(), "ls".to_string()]);
        let two = two.exec_argv(false, &["tmux".to_string(), "ls".to_string()]);
        assert_eq!(one, two, "same destination, same argv");
    }

    #[test]
    fn a_qualified_local_transport_is_still_this_box() {
        let l = local_as("local:zellij".into(), None);
        assert_eq!(l.host_id(), "local:zellij");
        assert!(crate::session::is_local_host(l.host_id()));
        assert!(l.local_registry_scope(), "still this box's registry scope");
    }

    #[test]
    fn local_factory_is_local_and_issues_no_raw_ssh() {
        let t = local(None);
        assert_eq!(t.host_id(), "local");
        assert!(!t.is_remote());
        assert!(
            t.raw_shell_argv("anything").is_none(),
            "a local machine issues no remote shell command"
        );
    }

    #[test]
    fn ssh_factory_is_remote_with_alias_id() {
        let t = ssh("prod".into(), String::new(), "linux".into());
        assert_eq!(t.host_id(), "prod");
        assert!(t.is_remote());
    }

    #[test]
    fn boxed_transport_clones_via_clone_box() {
        let t = ssh("prod".into(), String::new(), "linux".into());
        let c = t.clone();
        assert_eq!(c.host_id(), "prod");
        assert!(c.is_remote());
    }

    #[test]
    fn machine_kind_selects_the_implementation_at_one_site() {
        // `MachineKind::transport` is the single site that maps a machine kind to a
        // concrete Transport (Decision A: a new kind = a variant + one match arm).
        let local = MachineKind::Local {
            id: String::new(),
            socket: Some("/tmp/s".into()),
        }
        .transport();
        assert_eq!(local.host_id(), "local");
        assert!(!local.is_remote());
        let args = local.exec_argv(false, &["tmux".to_string(), "ls".to_string()]);
        assert!(
            args.windows(2)
                .any(|w| w == ["-S".to_string(), "/tmp/s".to_string()]),
            "the local socket threads into the transport as -S: {args:?}"
        );

        let ssh = MachineKind::Ssh {
            id: String::new(),
            alias: "prod".into(),
            control_path: String::new(),
            os: "linux".into(),
        }
        .transport();
        assert_eq!(ssh.host_id(), "prod");
        assert!(ssh.is_remote());
    }

    #[test]
    fn ssh_control_path_leaves_room_for_opensshs_temporary_socket() {
        let machine = "m".repeat(48);
        let kind = kind_for(
            &machine,
            machine.clone(),
            "android",
            std::path::Path::new("/data/data/com.termux/files/home/.xmux"),
            None,
        );
        let MachineKind::Ssh { control_path, .. } = kind else {
            panic!("a remote machine uses ssh");
        };
        let expanded = control_path.replace("%C", &"0".repeat(40));
        let temporary = format!("{expanded}.{}", "0".repeat(16));
        assert!(
            temporary.len() < 108,
            "OpenSSH must be able to bind its temporary Unix socket: {temporary}"
        );
    }

    #[test]
    fn a_wsl_machine_name_selects_the_wsl_kind() {
        // `kind_for` is the single assembly site, and the WSL kind is chosen by the
        // machine NAME - nothing else is threaded in to say which kind this is.
        let kind = kind_for(
            "wsl.Ubuntu-24.04",
            String::new(),
            "windows",
            std::path::Path::new("/x"),
            Some("/tmp/tmux-1000/work".into()),
        );
        assert!(matches!(kind, MachineKind::Wsl { .. }));
        assert_eq!(
            kind.local_socket(),
            None,
            "this box's $TMUX socket names nothing inside the distribution"
        );
        let t = kind.transport();
        assert_eq!(t.host_id(), "wsl.Ubuntu-24.04");
        assert!(!t.is_remote());
        let args = t.exec_argv(false, &["tmux".to_string(), "ls".to_string()]);
        assert_eq!(args.program(), "wsl.exe");
        assert!(
            args.windows(2)
                .any(|w| w == ["-d".to_string(), "Ubuntu-24.04".to_string()]),
            "the distribution threads into the transport as -d: {args:?}"
        );
    }

    #[test]
    fn a_qualified_wsl_transport_keeps_reaching_the_same_distribution() {
        // Two muxes in one distribution are two HOSTS at one destination, exactly as
        // for ssh: the id tells them apart and the wsl.exe argv must not change.
        let one = kind_for(
            "wsl.Ubuntu",
            String::new(),
            "windows",
            std::path::Path::new("/x"),
            None,
        )
        .transport();
        let two = kind_for(
            "wsl.Ubuntu",
            "wsl.Ubuntu:zellij".to_string(),
            "windows",
            std::path::Path::new("/x"),
            None,
        )
        .transport();
        assert_eq!(one.host_id(), "wsl.Ubuntu");
        assert_eq!(two.host_id(), "wsl.Ubuntu:zellij");
        assert_eq!(
            one.exec_argv(false, &["tmux".to_string(), "ls".to_string()]),
            two.exec_argv(false, &["tmux".to_string(), "ls".to_string()]),
            "same destination, same argv"
        );
    }

    #[test]
    fn local_socket_is_some_only_for_a_local_nondefault_socket() {
        assert_eq!(
            MachineKind::Local {
                id: String::new(),
                socket: Some("/tmp/s".into())
            }
            .local_socket(),
            Some("/tmp/s".into())
        );
        assert_eq!(
            MachineKind::Local {
                id: String::new(),
                socket: None
            }
            .local_socket(),
            None
        );
        assert_eq!(
            MachineKind::Ssh {
                id: String::new(),
                alias: "prod".into(),
                control_path: String::new(),
                os: "linux".into(),
            }
            .local_socket(),
            None,
            "a remote machine has no local socket"
        );
    }

    #[test]
    fn capability_predicates_split_shell_from_registry_scope() {
        // The two capability predicates split the meanings `is_remote` conflated: local
        // psmux is the authority for THIS box's registry (registry scope) yet attaches
        // without a shell; ssh attaches THROUGH a shell yet has no local-registry
        // authority here. Neither is derived from `is_remote`, which is what lets WSL take
        // the third combination: local, so no ssh option is shaped, yet shell-based, and
        // holding a registry of its own inside the distribution.
        let local = local(None);
        assert!(local.local_registry_scope());
        assert!(!local.runs_through_shell());
        let ssh = ssh("prod".into(), String::new(), "linux".into());
        assert!(ssh.runs_through_shell());
        assert!(!ssh.local_registry_scope());
        let wsl = wsl("Ubuntu".into());
        assert!(!wsl.is_remote());
        assert!(wsl.runs_through_shell());
        assert!(!wsl.local_registry_scope());
    }

    #[test]
    fn only_a_path_already_open_reuses_its_connection() {
        // A repeat over the local box or a WSL distribution is a local process, and an
        // ssh machine repeats over its master only where this side multiplexes. A Windows
        // side, or an ssh machine with no control socket, would log in again each time.
        assert!(local(None).reuses_connection());
        assert!(wsl("Ubuntu".into()).reuses_connection());
        assert!(ssh("prod".into(), "/tmp/cm.sock".into(), "linux".into()).reuses_connection());
        assert!(!ssh("prod".into(), "/tmp/cm.sock".into(), "windows".into()).reuses_connection());
        assert!(!ssh("prod".into(), String::new(), "linux".into()).reuses_connection());
    }
}

#[cfg(test)]
mod describe_tests {
    use super::*;

    #[test]
    fn each_kind_says_how_it_is_addressed_and_over_what_path() {
        // Shown on the unreachable screen: what a failed machine was asked over. The ssh
        // wait is the SAME constant the option carries, so the words and the command
        // cannot disagree.
        let ssh = MachineKind::Ssh {
            id: String::new(),
            alias: "prod".into(),
            control_path: "/tmp/cm.sock".into(),
            os: "linux".into(),
        };
        assert_eq!(
            ssh.addressed_as(),
            format!("ssh to prod, given {}s to connect", ssh::CONNECT_TIMEOUT)
        );
        assert_eq!(ssh.socket_path(), "/tmp/cm.sock");

        let local = MachineKind::Local {
            id: String::new(),
            socket: Some("/tmp/psmux.sock".into()),
        };
        assert!(local.addressed_as().contains("this box"));
        assert_eq!(local.socket_path(), "/tmp/psmux.sock");

        // A machine addressed without a path states none, and the screen then carries no
        // such row rather than an empty one.
        let wsl = MachineKind::Wsl {
            id: String::new(),
            distro: "Ubuntu".into(),
        };
        assert!(wsl.addressed_as().contains("Ubuntu"));
        assert_eq!(wsl.socket_path(), "");
    }
}
