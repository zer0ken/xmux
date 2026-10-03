use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::Duration;

use interprocess::local_socket::tokio::{Listener, Stream};
use interprocess::local_socket::traits::tokio::{Listener as _, Stream as _};
use interprocess::local_socket::ListenerOptions;
#[cfg(unix)]
use interprocess::local_socket::{GenericFilePath, ToFsName};
#[cfg(windows)]
use interprocess::local_socket::{GenericNamespaced, ToNsName};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

const ASKPASS_MODE: &str = "XMUX_ASKPASS_MODE";
const ASKPASS_ENDPOINT: &str = "XMUX_ASKPASS_ENDPOINT";
const ASKPASS_TOKEN: &str = "XMUX_ASKPASS_TOKEN";
const MAX_REQUEST: u64 = 16 * 1024;
const MAX_SECRET: usize = 16 * 1024;
const IPC_TIMEOUT: Duration = Duration::from_secs(3);
const BROKER_RETRY_MIN: Duration = Duration::from_millis(50);
const BROKER_RETRY_MAX: Duration = Duration::from_secs(1);
const MAX_BROKER_SESSIONS: usize = 16;

#[derive(Clone)]
struct ExpectedPrompt {
    user: Option<String>,
    hosts: Vec<String>,
}

impl ExpectedPrompt {
    fn new(user: Option<String>, hosts: impl IntoIterator<Item = String>) -> Self {
        Self {
            user: user.filter(|value| !value.is_empty()),
            hosts: hosts
                .into_iter()
                .filter(|value| !value.is_empty())
                .map(|value| value.to_ascii_lowercase())
                .collect(),
        }
    }

    fn accepts(&self, prompt: &str, secret_prompt: bool) -> bool {
        if !secret_prompt {
            return false;
        }
        let Some((account, prompt_text)) = prompt_account(prompt.trim()) else {
            return false;
        };
        let Some((user, host)) = account.rsplit_once('@') else {
            return false;
        };
        let words: Vec<_> = prompt_text
            .split(|ch: char| !ch.is_alphanumeric())
            .filter(|word| !word.is_empty())
            .map(str::to_ascii_lowercase)
            .collect();
        if words.iter().any(|word| {
            matches!(
                word.as_str(),
                "otp" | "passcode" | "passphrase" | "verification"
            )
        }) || words.windows(2).any(|pair| pair == ["one", "time"])
        {
            return false;
        }
        self.user
            .as_deref()
            .is_some_and(|expected| expected == user)
            && self
                .hosts
                .iter()
                .any(|expected| expected == &host.to_ascii_lowercase())
    }
}

fn prompt_account(prompt: &str) -> Option<(&str, &str)> {
    if let Some(rest) = prompt.strip_prefix('(') {
        let end = rest.find(')')?;
        return Some((&rest[..end], rest[end + 1..].trim()));
    }
    if let Some(rest) = prompt.strip_prefix("Password for ") {
        return Some((rest.trim_end_matches(':').trim(), "Password"));
    }
    let end = prompt.find("'s ")?;
    Some((&prompt[..end], prompt[end + 3..].trim()))
}

struct Credential {
    machine: String,
    login: super::Login,
    password: Mutex<Option<Secret>>,
    revoked: tokio::sync::watch::Sender<bool>,
    token: String,
    expected: ExpectedPrompt,
    askpass: PathBuf,
    generation: u64,
}

#[derive(Clone)]
struct Secret(String);

impl Secret {
    fn as_str(&self) -> &str {
        &self.0
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        zero_string(&mut self.0);
    }
}

pub(crate) fn zero_string(value: &mut String) {
    // Write through the allocation pointer so spare capacity is covered without
    // pretending its uninitialized bytes are live String contents.
    let capacity = value.capacity();
    let pointer = value.as_mut_ptr();
    for index in 0..capacity {
        unsafe { std::ptr::write_volatile(pointer.add(index), 0) };
    }
    std::sync::atomic::compiler_fence(Ordering::SeqCst);
    value.clear();
}

struct Attempt {
    credential: Weak<Credential>,
    claimed: AtomicBool,
    supplied: AtomicBool,
    rejection_generation: AtomicU64,
    refused_prompt: Mutex<Option<String>>,
    #[cfg(test)]
    pause_before_write: AtomicBool,
    #[cfg(test)]
    authorized: tokio::sync::Notify,
    #[cfg(test)]
    resume: tokio::sync::Notify,
}

struct Broker {
    task: tokio::task::JoinHandle<()>,
    path: PathBuf,
}

impl Broker {
    fn stop(self) {
        self.task.abort();
        let _ = std::fs::remove_file(self.path);
    }
}

struct Inner {
    root: Option<PathBuf>,
    helper: PathBuf,
    force_askpass: AtomicBool,
    active: RwLock<HashMap<String, Arc<Credential>>>,
    pending: RwLock<HashMap<String, Arc<Credential>>>,
    attempts: Arc<RwLock<HashMap<String, Weak<Attempt>>>>,
    next_attempt: AtomicU64,
    next_generation: AtomicU64,
    generations: RwLock<HashMap<String, u64>>,
    broker: Mutex<Option<Broker>>,
    profiles: RwLock<HashMap<String, SshProfile>>,
    broker_failure: Arc<RwLock<Option<String>>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SshProfile {
    pub login: super::Login,
    pub host_names: Vec<String>,
    pub strict_host_key_checking: Option<String>,
    pub proxied: bool,
}

pub fn local_user() -> Option<String> {
    std::env::var("USER")
        .ok()
        .or_else(|| std::env::var("USERNAME").ok())
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(broker) = self.broker.get_mut().expect("broker lock").take() {
            broker.stop();
        }
    }
}

#[derive(Clone)]
pub struct Credentials {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let count = self.inner.active.read().expect("credential lock").len();
        f.debug_struct("Credentials")
            .field("held_machine_count", &count)
            .finish()
    }
}

impl Default for Credentials {
    fn default() -> Self {
        Self::with_parts(None, PathBuf::new(), true)
    }
}

impl Credentials {
    pub fn new(root: PathBuf) -> Self {
        Self::new_with_helper(root, std::env::current_exe().unwrap_or_default(), true)
    }

    pub fn new_with_helper(root: PathBuf, helper: PathBuf, force_askpass: bool) -> Self {
        Self::with_parts(Some(root), helper, force_askpass)
    }

    fn with_parts(root: Option<PathBuf>, helper: PathBuf, force_askpass: bool) -> Self {
        Self {
            inner: Arc::new(Inner {
                root,
                helper,
                force_askpass: AtomicBool::new(force_askpass),
                active: RwLock::new(HashMap::new()),
                pending: RwLock::new(HashMap::new()),
                attempts: Arc::new(RwLock::new(HashMap::new())),
                next_attempt: AtomicU64::new(0),
                next_generation: AtomicU64::new(0),
                generations: RwLock::new(HashMap::new()),
                broker: Mutex::new(None),
                profiles: RwLock::new(HashMap::new()),
                broker_failure: Arc::new(RwLock::new(None)),
            }),
        }
    }

    pub fn set_force_askpass(&self, supported: bool) {
        self.inner.force_askpass.store(supported, Ordering::Release);
    }

    pub fn set_profiles(&self, profiles: HashMap<String, SshProfile>) {
        *self.inner.profiles.write().expect("profile lock") = profiles;
    }

    pub fn set_profile(&self, machine: &str, profile: SshProfile) {
        self.inner
            .profiles
            .write()
            .expect("profile lock")
            .insert(machine.to_string(), profile);
    }

    /// Forgets the effective configuration recorded for `machine`, so a later command
    /// cannot apply a policy the user's configuration may no longer hold.
    pub fn forget_profile(&self, machine: &str) {
        self.inner
            .profiles
            .write()
            .expect("profile lock")
            .remove(machine);
    }

    pub fn profile(&self, machine: &str) -> Option<SshProfile> {
        self.inner
            .profiles
            .read()
            .expect("profile lock")
            .get(machine)
            .cloned()
    }

    pub fn force_askpass_supported(&self) -> bool {
        self.inner.force_askpass.load(Ordering::Acquire)
    }

    pub fn unavailable_reason(&self, machine: &str, pending: bool) -> Option<String> {
        let held = if pending {
            self.inner
                .pending
                .read()
                .expect("credential lock")
                .contains_key(machine)
        } else {
            self.inner
                .active
                .read()
                .expect("credential lock")
                .contains_key(machine)
        };
        held.then(|| {
            self.inner
                .broker_failure
                .read()
                .expect("broker failure lock")
                .clone()
        })
        .flatten()
    }

    /// Starts one unvalidated login. The credential is visible only to the login
    /// command until that exact token is promoted after a successful exit.
    pub fn begin(
        &self,
        machine: &str,
        login: super::Login,
        password: String,
    ) -> io::Result<Option<AskpassAccess>> {
        self.remove(machine);
        if password.is_empty() {
            return Ok(None);
        }
        let password = Secret(password);
        let profile = self.profile(machine).unwrap_or_default();
        if profile.proxied {
            return Err(io::Error::other(
                "password login through ProxyJump or ProxyCommand is unavailable because the proxy inherits askpass; use key authentication for this host",
            ));
        }
        let endpoint = self.ensure_broker()?;
        let mut hosts = profile.host_names;
        hosts.extend([
            machine.to_string(),
            login.address.clone().unwrap_or_default(),
        ]);
        let target_user = login
            .user
            .clone()
            .or(profile.login.user)
            .or_else(local_user);
        let (revoked, _) = tokio::sync::watch::channel(false);
        let credential = Arc::new(Credential {
            machine: machine.to_string(),
            expected: ExpectedPrompt::new(target_user, hosts),
            login,
            password: Mutex::new(Some(password)),
            revoked,
            token: random_token()?,
            askpass: self.inner.helper.clone(),
            generation: self.bump_generation(machine),
        });
        self.inner
            .pending
            .write()
            .expect("credential lock")
            .insert(machine.to_string(), credential.clone());
        Ok(Some(AskpassAccess {
            endpoint,
            credential,
            inner: self.inner.clone(),
        }))
    }

    pub fn access(&self, machine: &str) -> Option<AskpassAccess> {
        if self
            .inner
            .broker_failure
            .read()
            .expect("broker failure lock")
            .is_some()
        {
            return None;
        }
        let endpoint = self.endpoint()?;
        let credential = self
            .inner
            .active
            .read()
            .expect("credential lock")
            .get(machine)
            .cloned()?;
        Some(AskpassAccess {
            endpoint,
            credential,
            inner: self.inner.clone(),
        })
    }

    pub fn pending_access(&self, machine: &str) -> Option<AskpassAccess> {
        if self
            .inner
            .broker_failure
            .read()
            .expect("broker failure lock")
            .is_some()
        {
            return None;
        }
        let endpoint = self.endpoint()?;
        let credential = self
            .inner
            .pending
            .read()
            .expect("credential lock")
            .get(machine)
            .cloned()?;
        Some(AskpassAccess {
            endpoint,
            credential,
            inner: self.inner.clone(),
        })
    }

    pub fn remove(&self, machine: &str) {
        let pending = self
            .inner
            .pending
            .write()
            .expect("credential lock")
            .remove(machine);
        let active = self
            .inner
            .active
            .write()
            .expect("credential lock")
            .remove(machine);
        for credential in active.into_iter().chain(pending) {
            credential.revoke();
        }
        self.bump_generation(machine);
    }

    pub fn generation(&self, machine: &str) -> u64 {
        self.inner
            .generations
            .read()
            .expect("generation lock")
            .get(machine)
            .copied()
            .unwrap_or_default()
    }

    fn bump_generation(&self, machine: &str) -> u64 {
        bump_generation(&self.inner, machine)
    }

    pub fn contains(&self, machine: &str) -> bool {
        self.inner
            .broker_failure
            .read()
            .expect("broker lock")
            .is_none()
            && self
                .inner
                .active
                .read()
                .expect("credential lock")
                .contains_key(machine)
    }

    pub fn machines(&self) -> HashSet<String> {
        if self
            .inner
            .broker_failure
            .read()
            .expect("broker lock")
            .is_some()
        {
            return HashSet::new();
        }
        self.inner
            .active
            .read()
            .expect("credential lock")
            .keys()
            .cloned()
            .collect()
    }

    pub fn retain_machines(&self, machines: &HashSet<String>) {
        let active: Vec<String> = self
            .inner
            .active
            .read()
            .expect("credential lock")
            .keys()
            .cloned()
            .collect();
        let pending: Vec<String> = self
            .inner
            .pending
            .read()
            .expect("credential lock")
            .keys()
            .cloned()
            .collect();
        for machine in active.into_iter().chain(pending) {
            if !machines.contains(&machine) {
                self.remove(&machine);
            }
        }
    }

    pub fn shutdown(&self) {
        if let Some(broker) = self.inner.broker.lock().expect("broker lock").take() {
            broker.stop();
        }
    }

    fn endpoint(&self) -> Option<PathBuf> {
        self.inner
            .broker
            .lock()
            .expect("broker lock")
            .as_ref()
            .map(|broker| broker.path.clone())
    }

    fn ensure_broker(&self) -> io::Result<PathBuf> {
        if let Some(reason) = self
            .inner
            .broker_failure
            .read()
            .expect("broker failure lock")
            .as_ref()
        {
            return Err(io::Error::other(format!(
                "credential broker is unavailable: {reason}"
            )));
        }
        let mut broker = self.inner.broker.lock().expect("broker lock");
        if let Some(broker) = broker.as_ref() {
            return Ok(broker.path.clone());
        }
        let root = self
            .inner
            .root
            .as_ref()
            .ok_or_else(|| io::Error::other("credential broker has no runtime directory"))?;
        std::fs::create_dir_all(root)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700))?;
        }
        sweep_stale_sockets(root);
        let path = root.join(format!(
            "askpass-{}-{}.sock",
            std::process::id(),
            &random_token()?[..16]
        ));
        let _ = std::fs::remove_file(&path);
        let listener = match create_listener(&path) {
            Ok(listener) => Some(listener),
            Err(error) => {
                *self
                    .inner
                    .broker_failure
                    .write()
                    .expect("broker failure lock") = Some(error.to_string());
                None
            }
        };
        let attempts = self.inner.attempts.clone();
        let failure = self.inner.broker_failure.clone();
        let serve_path = path.clone();
        let task = tokio::spawn(serve(listener, serve_path, attempts, failure));
        *broker = Some(Broker {
            task,
            path: path.clone(),
        });
        match self
            .inner
            .broker_failure
            .read()
            .expect("broker failure lock")
            .clone()
        {
            Some(reason) => Err(io::Error::other(format!(
                "credential broker is unavailable: {reason}"
            ))),
            None => Ok(path),
        }
    }
}

fn create_listener(path: &Path) -> io::Result<Listener> {
    #[cfg(unix)]
    let _ = std::fs::remove_file(path);
    let options = ListenerOptions::new().name(endpoint_name(path)?);
    #[cfg(windows)]
    let options = {
        use interprocess::os::windows::local_socket::ListenerOptionsExt as _;
        use interprocess::os::windows::security_descriptor::SecurityDescriptor;
        let sddl = widestring::U16CString::from_str(windows_pipe_sddl()?)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        options.security_descriptor(SecurityDescriptor::deserialize(&sddl)?)
    };
    let listener = options.create_tokio()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(listener)
}

#[cfg(windows)]
fn windows_pipe_sddl() -> io::Result<String> {
    use windows_sys::Win32::Foundation::{CloseHandle, LocalFree};
    use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows_sys::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    unsafe {
        let mut token = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(io::Error::last_os_error());
        }
        let result = (|| {
            let mut needed = 0;
            GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed);
            if needed == 0 {
                return Err(io::Error::last_os_error());
            }
            let words = usize::try_from(needed)
                .ok()
                .and_then(|bytes| bytes.checked_add(std::mem::size_of::<usize>() - 1))
                .map(|bytes| bytes / std::mem::size_of::<usize>())
                .ok_or_else(|| io::Error::other("token user size overflow"))?;
            let mut buffer = vec![0usize; words];
            if GetTokenInformation(
                token,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                needed,
                &mut needed,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            let user = &*buffer.as_ptr().cast::<TOKEN_USER>();
            let mut sid_text = std::ptr::null_mut();
            if ConvertSidToStringSidW(user.User.Sid, &mut sid_text) == 0 {
                return Err(io::Error::last_os_error());
            }
            let length = (0..).take_while(|index| *sid_text.add(*index) != 0).count();
            let sid = String::from_utf16_lossy(std::slice::from_raw_parts(sid_text, length));
            let _ = LocalFree(sid_text.cast());
            Ok(format!("O:{sid}D:P(A;;GA;;;{sid})(A;;GA;;;SY)"))
        })();
        let _ = CloseHandle(token);
        result
    }
}

#[derive(Clone)]
pub struct AskpassAccess {
    endpoint: PathBuf,
    credential: Arc<Credential>,
    inner: Arc<Inner>,
}

impl AskpassAccess {
    pub fn login(&self) -> &super::Login {
        &self.credential.login
    }

    pub fn generation(&self) -> u64 {
        self.credential.generation
    }

    pub fn promote(&self) -> bool {
        let mut pending = self.inner.pending.write().expect("credential lock");
        if !pending
            .get(&self.credential.machine)
            .is_some_and(|current| current.token == self.credential.token)
        {
            return false;
        }
        if self
            .credential
            .password
            .lock()
            .expect("secret lock")
            .is_none()
        {
            return false;
        }
        pending.remove(&self.credential.machine);
        self.inner
            .active
            .write()
            .expect("credential lock")
            .insert(self.credential.machine.clone(), self.credential.clone());
        true
    }

    pub fn discard(&self) {
        let removed_pending = remove_matching(
            &self.inner.pending,
            &self.credential.machine,
            &self.credential.token,
        );
        let removed_active = remove_matching(
            &self.inner.active,
            &self.credential.machine,
            &self.credential.token,
        );
        if removed_pending || removed_active {
            bump_generation(&self.inner, &self.credential.machine);
        }
    }

    fn command(&self, set_display: bool) -> CommandAuth {
        let sequence = self.inner.next_attempt.fetch_add(1, Ordering::Relaxed);
        let token = format!("{}-{sequence:016x}", self.credential.token);
        let attempt = Arc::new(Attempt {
            credential: Arc::downgrade(&self.credential),
            claimed: AtomicBool::new(false),
            supplied: AtomicBool::new(false),
            rejection_generation: AtomicU64::new(0),
            refused_prompt: Mutex::new(None),
            #[cfg(test)]
            pause_before_write: AtomicBool::new(false),
            #[cfg(test)]
            authorized: tokio::sync::Notify::new(),
            #[cfg(test)]
            resume: tokio::sync::Notify::new(),
        });
        self.inner
            .attempts
            .write()
            .expect("attempt lock")
            .insert(token.clone(), Arc::downgrade(&attempt));
        let mut env = vec![
            (
                "SSH_ASKPASS".into(),
                self.credential.askpass.to_string_lossy().into_owned(),
            ),
            ("SSH_ASKPASS_REQUIRE".into(), "force".into()),
            (ASKPASS_MODE.into(), "1".into()),
            (
                ASKPASS_ENDPOINT.into(),
                self.endpoint.to_string_lossy().into_owned(),
            ),
            (ASKPASS_TOKEN.into(), token.clone()),
        ];
        if set_display && std::env::var_os("DISPLAY").is_none() {
            env.push(("DISPLAY".into(), "xmux-askpass".into()));
        }
        CommandAuth {
            env,
            token,
            attempt,
            inner: self.inner.clone(),
            machine: self.credential.machine.clone(),
            credential_token: self.credential.token.clone(),
            generation: self.credential.generation,
        }
    }
}

fn remove_matching(
    entries: &RwLock<HashMap<String, Arc<Credential>>>,
    machine: &str,
    token: &str,
) -> bool {
    let mut entries = entries.write().expect("credential lock");
    if entries
        .get(machine)
        .is_some_and(|current| current.token == token)
    {
        if let Some(credential) = entries.remove(machine) {
            credential.revoke();
        }
        true
    } else {
        false
    }
}

fn bump_generation(inner: &Inner, machine: &str) -> u64 {
    let generation = inner.next_generation.fetch_add(1, Ordering::Relaxed) + 1;
    inner
        .generations
        .write()
        .expect("generation lock")
        .insert(machine.to_string(), generation);
    generation
}

impl Credential {
    fn revoke(&self) {
        self.revoked.send_replace(true);
        self.password.lock().expect("secret lock").take();
    }
}

#[derive(Clone)]
pub struct CommandAuth {
    env: Vec<(String, String)>,
    token: String,
    attempt: Arc<Attempt>,
    inner: Arc<Inner>,
    machine: String,
    credential_token: String,
    generation: u64,
}

impl CommandAuth {
    pub fn environment(&self) -> &[(String, String)] {
        &self.env
    }

    pub fn supplied(&self) -> bool {
        self.attempt.supplied.load(Ordering::Acquire)
    }

    pub fn rejection_generation(&self) -> Option<u64> {
        match self.attempt.rejection_generation.load(Ordering::Acquire) {
            0 => None,
            generation => Some(generation),
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn refused_prompt(&self) -> Option<String> {
        self.attempt
            .refused_prompt
            .lock()
            .expect("prompt lock")
            .clone()
    }

    #[cfg(test)]
    fn pause_delivery(&self) {
        self.attempt
            .pause_before_write
            .store(true, Ordering::Release);
    }

    pub fn promote(&self) -> bool {
        let mut pending = self.inner.pending.write().expect("credential lock");
        let Some(credential) = pending.get(&self.machine) else {
            return false;
        };
        if credential.token != self.credential_token
            || credential.password.lock().expect("secret lock").is_none()
        {
            return false;
        }
        let credential = pending
            .remove(&self.machine)
            .expect("checked pending entry");
        self.inner
            .active
            .write()
            .expect("credential lock")
            .insert(self.machine.clone(), credential);
        true
    }

    pub fn discard(&self) {
        let removed_pending =
            remove_matching(&self.inner.pending, &self.machine, &self.credential_token);
        let removed_active =
            remove_matching(&self.inner.active, &self.machine, &self.credential_token);
        if removed_pending || removed_active {
            bump_generation(&self.inner, &self.machine);
        }
    }

    pub fn forget_active(&self) -> bool {
        if remove_matching(&self.inner.active, &self.machine, &self.credential_token) {
            let generation = bump_generation(&self.inner, &self.machine);
            self.attempt
                .rejection_generation
                .store(generation, Ordering::Release);
            true
        } else {
            false
        }
    }
}

impl Drop for CommandAuth {
    fn drop(&mut self) {
        if Arc::strong_count(&self.attempt) == 1 {
            self.inner
                .attempts
                .write()
                .expect("attempt lock")
                .remove(&self.token);
        }
    }
}

pub fn command_auth(access: AskpassAccess, set_display: bool) -> CommandAuth {
    access.command(set_display)
}

#[derive(Serialize, Deserialize)]
struct Request {
    token: String,
    prompt: String,
    secret_prompt: bool,
}

async fn serve(
    mut listener: Option<Listener>,
    path: PathBuf,
    attempts: Arc<RwLock<HashMap<String, Weak<Attempt>>>>,
    failure: Arc<RwLock<Option<String>>>,
) {
    let sessions = Arc::new(tokio::sync::Semaphore::new(MAX_BROKER_SESSIONS));
    if listener.is_some() {
        *failure.write().expect("broker failure lock") = None;
    }
    let mut retry = BROKER_RETRY_MIN;
    loop {
        if listener.is_none() {
            tokio::time::sleep(retry).await;
            match create_listener(&path) {
                Ok(recreated) => {
                    listener = Some(recreated);
                    *failure.write().expect("broker failure lock") = None;
                    retry = BROKER_RETRY_MIN;
                }
                Err(error) => {
                    tracing::warn!(%error, "credential broker endpoint recreation failed");
                    *failure.write().expect("broker failure lock") = Some(error.to_string());
                    retry = retry.saturating_mul(2).min(BROKER_RETRY_MAX);
                }
            }
            continue;
        }
        match listener.as_ref().expect("listener checked").accept().await {
            Ok(stream) => {
                let Ok(permit) = sessions.clone().try_acquire_owned() else {
                    tracing::warn!("credential broker session limit reached");
                    drop(stream);
                    continue;
                };
                let attempts = attempts.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    let _ = tokio::time::timeout(IPC_TIMEOUT, serve_one(stream, attempts)).await;
                });
            }
            Err(error) => {
                tracing::warn!(%error, "credential broker accept failed; recreating endpoint");
                *failure.write().expect("broker failure lock") = Some(error.to_string());
                listener.take();
            }
        }
    }
}

async fn serve_one(
    stream: Stream,
    attempts: Arc<RwLock<HashMap<String, Weak<Attempt>>>>,
) -> io::Result<()> {
    let mut stream = BufReader::new(stream);
    let mut line = String::new();
    let mut bounded = (&mut stream).take(MAX_REQUEST);
    bounded.read_line(&mut line).await?;
    if bounded.limit() == 0 && !line.ends_with('\n') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "askpass request is too large",
        ));
    }
    let request: Request = serde_json::from_str(line.trim_end())?;
    let attempt = attempts
        .read()
        .expect("attempt lock")
        .get(&request.token)
        .and_then(Weak::upgrade);
    let mut answer = attempt.and_then(|attempt| {
        let credential = attempt.credential.upgrade()?;
        if !credential
            .expected
            .accepts(&request.prompt, request.secret_prompt)
        {
            *attempt.refused_prompt.lock().expect("prompt lock") = Some(request.prompt);
            return None;
        }
        if attempt
            .claimed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return None;
        }
        let revoked = credential.revoked.subscribe();
        if *revoked.borrow() {
            return None;
        }
        let password = credential.password.lock().expect("secret lock").clone()?;
        Some(InFlightAnswer {
            attempt,
            password,
            revoked,
        })
    });
    let result = if let Some(answer) = answer.as_mut() {
        #[cfg(test)]
        if answer.attempt.pause_before_write.load(Ordering::Acquire) {
            answer.attempt.authorized.notify_one();
            answer.attempt.resume.notified().await;
        }
        tokio::select! {
            biased;
            changed = answer.revoked.changed() => {
                if changed.is_ok() && *answer.revoked.borrow() {
                    return Ok(());
                }
                write_secret(stream.get_mut(), Some(answer.password.as_str())).await
            }
            result = write_secret(stream.get_mut(), Some(answer.password.as_str())) => result,
        }
    } else {
        write_secret(stream.get_mut(), None).await
    };
    if result.is_ok() {
        if let Some(answer) = &answer {
            answer.attempt.supplied.store(true, Ordering::Release);
        }
    }
    result
}

struct InFlightAnswer {
    attempt: Arc<Attempt>,
    password: Secret,
    revoked: tokio::sync::watch::Receiver<bool>,
}

async fn write_secret(stream: &mut Stream, secret: Option<&str>) -> io::Result<()> {
    let bytes = secret.unwrap_or_default().as_bytes();
    stream
        .write_all(format!("{}\n", bytes.len()).as_bytes())
        .await?;
    stream.write_all(bytes).await?;
    stream.flush().await
}

#[cfg(test)]
pub async fn request_password(
    endpoint: &Path,
    token: &str,
    prompt: &str,
) -> io::Result<Option<String>> {
    request_password_inner(endpoint, token, prompt, true)
        .await
        .map(|secret| secret.map(|secret| secret.as_str().to_string()))
}

async fn request_password_inner(
    endpoint: &Path,
    token: &str,
    prompt: &str,
    secret_prompt: bool,
) -> io::Result<Option<Secret>> {
    tokio::time::timeout(
        IPC_TIMEOUT,
        request_password_unbounded(endpoint, token, prompt, secret_prompt),
    )
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "askpass broker timed out"))?
}

async fn request_password_unbounded(
    endpoint: &Path,
    token: &str,
    prompt: &str,
    secret_prompt: bool,
) -> io::Result<Option<Secret>> {
    let stream = Stream::connect(endpoint_name(endpoint)?).await?;
    let mut stream = BufReader::new(stream);
    let request = serde_json::to_string(&Request {
        token: token.to_string(),
        prompt: prompt.to_string(),
        secret_prompt,
    })?;
    stream.get_mut().write_all(request.as_bytes()).await?;
    stream.get_mut().write_all(b"\n").await?;
    stream.get_mut().flush().await?;
    let mut length = String::new();
    stream.read_line(&mut length).await?;
    let length: usize = length
        .trim()
        .parse()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid askpass response"))?;
    if length > MAX_SECRET {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "askpass response is too large",
        ));
    }
    if length == 0 {
        return Ok(None);
    }
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes).await?;
    String::from_utf8(bytes)
        .map(|value| Some(Secret(value)))
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

pub async fn run_helper_from_env() -> Option<i32> {
    let mode = std::env::var_os(ASKPASS_MODE)?;
    if mode != "1" {
        return Some(1);
    }
    Some(run_helper().await.unwrap_or(1))
}

async fn run_helper() -> Option<i32> {
    let endpoint = PathBuf::from(std::env::var_os(ASKPASS_ENDPOINT)?);
    let token = std::env::var(ASKPASS_TOKEN).ok()?;
    let prompt = std::env::args().nth(1).unwrap_or_default();
    let secret_prompt = std::env::var_os("SSH_ASKPASS_PROMPT").is_none();
    match request_password_inner(&endpoint, &token, &prompt, secret_prompt).await {
        Ok(Some(password)) => {
            use std::io::Write as _;
            let mut stdout = std::io::stdout().lock();
            if stdout.write_all(password.as_str().as_bytes()).is_ok()
                && stdout.write_all(b"\n").is_ok()
                && stdout.flush().is_ok()
            {
                Some(0)
            } else {
                Some(1)
            }
        }
        Ok(None) => Some(1),
        Err(error) => {
            eprintln!("xmux credential broker unavailable: {error}");
            Some(1)
        }
    }
}

pub async fn detect_force_askpass() -> bool {
    let mut command = tokio::process::Command::new("ssh");
    command.arg("-V").kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(3), command.output()).await;
    let Ok(Ok(output)) = output else {
        return false;
    };
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    openssh_supports_force(&text)
}

fn openssh_supports_force(version: &str) -> bool {
    let Some(version) = version.split("OpenSSH_").nth(1) else {
        return false;
    };
    let Some(start) = version.find(|ch: char| ch.is_ascii_digit()) else {
        return false;
    };
    let mut numbers = version[start..].split(|ch: char| !ch.is_ascii_digit());
    let major = numbers.next().and_then(|value| value.parse::<u32>().ok());
    let minor = numbers.next().and_then(|value| value.parse::<u32>().ok());
    matches!((major, minor), (Some(major), Some(minor)) if (major, minor) >= (8, 4))
}

fn random_token() -> io::Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|error| io::Error::other(format!("random token failed: {error}")))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(test)]
pub(crate) fn request_test_token() -> String {
    random_token().expect("test token")
}

fn endpoint_name(path: &Path) -> io::Result<interprocess::local_socket::Name<'static>> {
    #[cfg(unix)]
    {
        path.to_owned()
            .into_os_string()
            .to_fs_name::<GenericFilePath>()
    }
    #[cfg(windows)]
    {
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "invalid askpass endpoint")
            })?;
        format!("xmux-{stem}").to_ns_name::<GenericNamespaced>()
    }
}

#[cfg(unix)]
fn sweep_stale_sockets(root: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(pid) = name
            .strip_prefix("askpass-")
            .and_then(|rest| rest.split('-').next())
            .and_then(|pid| pid.parse::<libc::pid_t>().ok())
        else {
            continue;
        };
        let alive = unsafe { libc::kill(pid, 0) == 0 }
            || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
        if !alive {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(not(unix))]
fn sweep_stale_sockets(_root: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn expected() -> ExpectedPrompt {
        ExpectedPrompt::new(
            Some("dev".into()),
            ["pwbox".into(), "canonical.example".into()],
        )
    }

    fn login() -> crate::transport::Login {
        crate::transport::Login {
            user: Some("dev".into()),
            ..Default::default()
        }
    }

    #[test]
    fn askpass_accepts_password_and_keyboard_interactive_for_the_user() {
        let expected = expected();
        assert!(expected.accepts("dev@pwbox's password: ", true));
        assert!(expected.accepts("Password for dev@canonical.example:", true));
        assert!(expected.accepts("(dev@pwbox) Password: ", true));
        assert!(expected.accepts("(dev@pwbox) 암호: ", true));
        assert!(!expected.accepts("Password:", true));
        assert!(!expected.accepts("dev@bastion's password: ", true));
        assert!(!expected.accepts("root@pwbox's password: ", true));
        assert!(!expected.accepts("(root@pwbox) Password: ", true));
    }

    #[test]
    fn askpass_matches_the_target_account_and_host_exactly() {
        let expected = ExpectedPrompt::new(
            Some("dev".into()),
            ["app".into(), "10.0.0.1".into(), "target.example".into()],
        );
        for prompt in [
            "dev@10.0.0.1's password: ",
            "(dev@app) Password: ",
            "Password for dev@target.example:",
        ] {
            assert!(expected.accepts(prompt, true), "rejected {prompt:?}");
        }
        for prompt in [
            "dev@10.0.0.12's password: ",
            "dev@app-bastion's password: ",
            "developer@app's password: ",
        ] {
            assert!(!expected.accepts(prompt, true), "accepted {prompt:?}");
        }
    }

    #[test]
    fn otp_is_matched_as_a_prompt_word_not_inside_an_account() {
        let expected = ExpectedPrompt::new(Some("dev".into()), ["depotprod".into()]);
        assert!(expected.accepts("dev@depotprod's password: ", true));
        assert!(!expected.accepts("(dev@depotprod) OTP: ", true));
        assert!(!expected.accepts("(dev@depotprod) Passcode: ", true));
    }

    #[test]
    fn askpass_refuses_host_keys_passphrases_and_one_time_codes() {
        let expected = expected();
        assert!(!expected.accepts(
            "Are you sure you want to continue connecting (yes/no/[fingerprint])?",
            false
        ));
        for prompt in [
            "Enter passphrase for key 'dev@pwbox':",
            "dev@pwbox verification code:",
            "dev@pwbox one-time password:",
            "dev@pwbox OTP:",
        ] {
            assert!(
                !expected.accepts(prompt, true),
                "unexpectedly accepted {prompt:?}"
            );
        }
    }

    #[test]
    fn an_unspecified_target_user_refuses_every_account() {
        let expected =
            ExpectedPrompt::new(None, ["canonical.example".into(), "host-key-alias".into()]);
        assert!(!expected.accepts("resolved@canonical.example's password:", true));
        assert!(!expected.accepts("(resolved@host-key-alias) Password:", true));
    }

    #[test]
    fn force_requirement_starts_at_openssh_8_4() {
        assert!(!openssh_supports_force(
            "OpenSSH_for_Windows_8.1p1, LibreSSL"
        ));
        assert!(!openssh_supports_force("OpenSSH_8.3p1 Ubuntu"));
        assert!(openssh_supports_force("OpenSSH_8.4p1"));
        assert!(openssh_supports_force("OpenSSH_10.0p2"));
        assert!(openssh_supports_force(
            "OpenSSH_for_Windows_10.0p2, LibreSSL"
        ));
        assert!(!openssh_supports_force("unknown"));
    }

    #[tokio::test]
    async fn pending_secret_is_command_scoped_and_promoted_only_on_success() {
        let root = std::env::temp_dir().join(format!(
            "xmux-auth-test-{}-{}",
            std::process::id(),
            random_token().unwrap()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let credentials = Credentials::new(root.clone());
        credentials.set_profiles(HashMap::from([(
            "pwbox".into(),
            SshProfile {
                host_names: vec!["canonical.example".into()],
                ..Default::default()
            },
        )]));
        let access = credentials
            .begin(
                "pwbox",
                crate::transport::Login {
                    address: Some("127.0.0.1".into()),
                    port: Some(2222),
                    user: Some("dev".into()),
                },
                "correct horse battery staple".into(),
            )
            .unwrap()
            .unwrap();
        assert!(!credentials.contains("pwbox"));

        let command = access.command(false);
        let token = command
            .environment()
            .iter()
            .find(|(key, _)| key == ASKPASS_TOKEN)
            .map(|(_, value)| value.as_str())
            .unwrap();
        assert_eq!(
            request_password(
                access.endpoint.as_path(),
                token,
                "(dev@canonical.example) Password:",
            )
            .await
            .unwrap(),
            Some("correct horse battery staple".into())
        );
        assert!(command.supplied());
        assert!(command.promote());
        assert!(credentials.contains("pwbox"));

        credentials.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn one_commands_answer_does_not_mark_another_command_supplied() {
        let root = std::env::temp_dir().join(format!(
            "xmux-auth-attempt-test-{}-{}",
            std::process::id(),
            random_token().unwrap()
        ));
        let credentials = Credentials::new(root.clone());
        let access = credentials
            .begin("pwbox", login(), "secret".into())
            .unwrap()
            .unwrap();
        let first = access.command(false);
        let second = access.command(false);
        let first_token = first
            .environment()
            .iter()
            .find(|(key, _)| key == ASKPASS_TOKEN)
            .unwrap()
            .1
            .clone();
        request_password(
            access.endpoint.as_path(),
            &first_token,
            "dev@pwbox's password:",
        )
        .await
        .unwrap();
        assert!(first.supplied());
        assert!(!second.supplied());
        credentials.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_late_attempt_cannot_remove_a_newer_credential() {
        let root = std::env::temp_dir().join(format!(
            "xmux-auth-replace-test-{}-{}",
            std::process::id(),
            random_token().unwrap()
        ));
        let credentials = Credentials::new(root.clone());
        let first = credentials
            .begin("pwbox", login(), "old".into())
            .unwrap()
            .unwrap();
        assert!(first.promote());
        let old_command = first.command(false);
        let second = credentials
            .begin("pwbox", login(), "new".into())
            .unwrap()
            .unwrap();
        assert!(second.promote());
        old_command.forget_active();
        assert!(credentials.contains("pwbox"));
        credentials.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_spawned_child_guard_keeps_its_token_valid_until_reap() {
        struct StubChild(Option<CommandAuth>);
        fn spawn_stub(command: &crate::transport::CommandSpec) -> StubChild {
            StubChild(command.auth_guard())
        }

        let root = std::env::temp_dir().join(format!(
            "xmux-auth-child-lifetime-{}-{}",
            std::process::id(),
            random_token().unwrap()
        ));
        let credentials = Credentials::new(root.clone());
        let access = credentials
            .begin("pwbox", login(), "secret".into())
            .unwrap()
            .unwrap();
        let command =
            crate::transport::CommandSpec::new("stub", Vec::new()).with_auth(access.clone(), false);
        let endpoint = access.endpoint.clone();
        let token = command
            .env()
            .iter()
            .find(|(key, _)| key == ASKPASS_TOKEN)
            .unwrap()
            .1
            .clone();
        let child = spawn_stub(&command);
        drop(command);
        tokio::task::yield_now().await;
        assert_eq!(
            request_password(&endpoint, &token, "dev@pwbox's password:")
                .await
                .unwrap()
                .as_deref(),
            Some("secret")
        );
        drop(child.0);
        assert!(request_password(&endpoint, &token, "dev@pwbox's password:")
            .await
            .unwrap()
            .is_none());
        credentials.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_token_answers_at_most_once() {
        let root = std::env::temp_dir().join(format!(
            "xmux-auth-once-{}-{}",
            std::process::id(),
            random_token().unwrap()
        ));
        let credentials = Credentials::new(root.clone());
        let access = credentials
            .begin("pwbox", login(), "secret".into())
            .unwrap()
            .unwrap();
        let command = access.command(false);
        let token = command
            .environment()
            .iter()
            .find(|(key, _)| key == ASKPASS_TOKEN)
            .unwrap()
            .1
            .clone();
        assert!(
            request_password(&access.endpoint, &token, "dev@pwbox's password:")
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            request_password(&access.endpoint, &token, "dev@pwbox's password:")
                .await
                .unwrap()
                .is_none()
        );
        assert!(command.refused_prompt().is_none());
        credentials.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_proxy_prompt_does_not_consume_the_targets_answer() {
        let root = std::env::temp_dir().join(format!(
            "xmux-auth-proxy-{}-{}",
            std::process::id(),
            random_token().unwrap()
        ));
        let credentials = Credentials::new(root.clone());
        let access = credentials
            .begin(
                "app",
                crate::transport::Login {
                    user: Some("dev".into()),
                    ..Default::default()
                },
                "secret".into(),
            )
            .unwrap()
            .unwrap();
        let command = access.command(false);
        let token = command
            .environment()
            .iter()
            .find(|(key, _)| key == ASKPASS_TOKEN)
            .unwrap()
            .1
            .clone();
        assert!(
            request_password(&access.endpoint, &token, "dev@app-bastion's password: ")
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            request_password(&access.endpoint, &token, "dev@app's password: ")
                .await
                .unwrap()
                .as_deref(),
            Some("secret")
        );
        credentials.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn removing_a_credential_revokes_outstanding_command_tokens() {
        let root = std::env::temp_dir().join(format!(
            "xmux-auth-revoke-{}-{}",
            std::process::id(),
            random_token().unwrap()
        ));
        let credentials = Credentials::new(root.clone());
        let access = credentials
            .begin("pwbox", login(), "secret".into())
            .unwrap()
            .unwrap();
        assert!(access.promote());
        let command = access.command(false);
        let token = command
            .environment()
            .iter()
            .find(|(key, _)| key == ASKPASS_TOKEN)
            .unwrap()
            .1
            .clone();
        let generation = credentials.generation("pwbox");
        credentials.remove("pwbox");
        assert!(credentials.generation("pwbox") > generation);
        assert!(
            request_password(&access.endpoint, &token, "dev@pwbox's password: ")
                .await
                .unwrap()
                .is_none()
        );
        credentials.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn removal_cancels_an_authorized_answer_before_delivery() {
        let root = std::env::temp_dir().join(format!(
            "xmux-auth-in-flight-revoke-{}-{}",
            std::process::id(),
            random_token().unwrap()
        ));
        let credentials = Credentials::new(root.clone());
        let access = credentials
            .begin("pwbox", login(), "secret".into())
            .unwrap()
            .unwrap();
        assert!(access.promote());
        let command = credentials.access("pwbox").unwrap().command(false);
        command.pause_delivery();
        let endpoint = access.endpoint.clone();
        let token = command
            .environment()
            .iter()
            .find(|(key, _)| key == ASKPASS_TOKEN)
            .unwrap()
            .1
            .clone();
        let request = tokio::spawn(async move {
            request_password(&endpoint, &token, "dev@pwbox's password:").await
        });
        command.attempt.authorized.notified().await;
        credentials.remove("pwbox");
        command.attempt.resume.notify_one();
        assert!(!matches!(request.await.unwrap(), Ok(Some(_))));
        assert!(!command.supplied());
        credentials.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_refused_prompt_is_recorded_for_the_login_reason() {
        let root = std::env::temp_dir().join(format!(
            "xmux-auth-refused-prompt-{}-{}",
            std::process::id(),
            random_token().unwrap()
        ));
        let credentials = Credentials::new(root.clone());
        let access = credentials
            .begin("pwbox", login(), "secret".into())
            .unwrap()
            .unwrap();
        let command = access.command(false);
        let token = command
            .environment()
            .iter()
            .find(|(key, _)| key == ASKPASS_TOKEN)
            .unwrap()
            .1
            .clone();
        assert!(
            request_password(&access.endpoint, &token, "dev@bastion's password:")
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            command.refused_prompt().as_deref(),
            Some("dev@bastion's password:")
        );
        credentials.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_key_authenticated_login_discards_its_unused_password() {
        let root = std::env::temp_dir().join(format!(
            "xmux-auth-key-login-{}-{}",
            std::process::id(),
            random_token().unwrap()
        ));
        let credentials = Credentials::new(root.clone());
        let access = credentials
            .begin("pwbox", login(), "unused".into())
            .unwrap()
            .unwrap();
        let command =
            crate::transport::CommandSpec::new("stub", Vec::new()).with_auth(access, false);
        assert!(command.finish_successful_login());
        assert!(!credentials.contains("pwbox"));
        assert!(credentials.pending_access("pwbox").is_none());
        credentials.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_fatal_broker_failure_marks_an_active_credential_unusable() {
        let root = std::env::temp_dir().join(format!(
            "xmux-auth-fatal-{}-{}",
            std::process::id(),
            random_token().unwrap()
        ));
        let credentials = Credentials::new(root.clone());
        let access = credentials
            .begin("pwbox", login(), "secret".into())
            .unwrap()
            .unwrap();
        assert!(access.promote());
        assert!(credentials.contains("pwbox"));
        *credentials
            .inner
            .broker_failure
            .write()
            .expect("broker failure lock") = Some("endpoint failed".into());
        assert!(!credentials.contains("pwbox"));
        assert!(credentials.machines().is_empty());
        assert!(credentials.access("pwbox").is_none());
        assert!(credentials.pending_access("pwbox").is_none());
        credentials.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_recreated_listener_restores_credential_access() {
        let root = std::env::temp_dir().join(format!(
            "xmux-auth-recreate-{}-{}",
            std::process::id(),
            random_token().unwrap()
        ));
        let credentials = Credentials::new(root.clone());
        let access = credentials
            .begin("pwbox", login(), "secret".into())
            .unwrap()
            .unwrap();
        assert!(access.promote());
        let old = credentials
            .inner
            .broker
            .lock()
            .expect("broker lock")
            .take()
            .unwrap();
        let path = old.path.clone();
        old.task.abort();
        let _ = old.task.await;
        let _ = std::fs::remove_file(&path);
        *credentials
            .inner
            .broker_failure
            .write()
            .expect("broker failure lock") = Some("broken pipe".into());
        assert!(credentials.access("pwbox").is_none());

        let task = tokio::spawn(serve(
            None,
            path.clone(),
            credentials.inner.attempts.clone(),
            credentials.inner.broker_failure.clone(),
        ));
        *credentials.inner.broker.lock().expect("broker lock") = Some(Broker { task, path });
        tokio::time::timeout(Duration::from_secs(1), async {
            while credentials.access("pwbox").is_none() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("recreated listener becomes ready");
        credentials.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn helper_request_times_out_when_the_broker_withholds_a_response() {
        let root = std::env::temp_dir().join(format!(
            "xmux-auth-timeout-{}-{}",
            std::process::id(),
            random_token().unwrap()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("stalled.sock");
        let listener = create_listener(&path).unwrap();
        let stalled = tokio::spawn(async move {
            let _stream = listener.accept().await.unwrap();
            tokio::time::sleep(IPC_TIMEOUT + Duration::from_secs(1)).await;
        });
        let error =
            match request_password_inner(&path, "token", "dev@host's password: ", true).await {
                Err(error) => error,
                Ok(_) => panic!("stalled broker answered"),
            };
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        stalled.abort();
        let _ = stalled.await;
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn windows_pipe_descriptor_allows_only_owner_and_system() {
        let descriptor = windows_pipe_sddl().unwrap();
        // Local, domain, and Entra accounts carry different SID authorities, so the
        // owner is checked by identity: the one user ACE grants exactly the owner.
        let owner = descriptor
            .strip_prefix("O:")
            .and_then(|rest| rest.split_once("D:P"))
            .map(|(owner, _)| owner)
            .expect("owner then protected DACL");
        assert!(owner.starts_with("S-1-"), "{descriptor}");
        assert!(
            descriptor.ends_with(&format!("D:P(A;;GA;;;{owner})(A;;GA;;;SY)")),
            "{descriptor}"
        );
        assert!(!descriptor.contains(";;;OW"), "{descriptor}");
        let sddl = widestring::U16CString::from_str(&descriptor).unwrap();
        interprocess::os::windows::security_descriptor::SecurityDescriptor::deserialize(&sddl)
            .expect("valid protected owner and SYSTEM descriptor");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unix_broker_directory_and_socket_are_private() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = std::env::temp_dir().join(format!(
            "xmux-auth-mode-{}-{}",
            std::process::id(),
            random_token().unwrap()
        ));
        let credentials = Credentials::new(root.clone());
        let access = credentials
            .begin("pwbox", login(), "secret".into())
            .unwrap()
            .unwrap();
        assert_eq!(
            std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&access.endpoint)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        credentials.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn empty_password_creates_no_credential() {
        let credentials = Credentials::new(std::env::temp_dir());
        assert!(credentials
            .begin("pwbox", crate::transport::Login::default(), String::new())
            .unwrap()
            .is_none());
        assert!(!credentials.contains("pwbox"));
    }

    #[test]
    fn proxied_target_never_receives_a_shared_askpass_credential() {
        let credentials = Credentials::new(std::env::temp_dir());
        credentials.set_profiles(HashMap::from([(
            "pwbox".into(),
            SshProfile {
                proxied: true,
                ..Default::default()
            },
        )]));
        let error = match credentials.begin("pwbox", login(), "secret".into()) {
            Err(error) => error,
            Ok(_) => panic!("proxied password was accepted"),
        };
        assert!(error.to_string().contains("proxy inherits askpass"));
        assert!(credentials.pending_access("pwbox").is_none());
    }

    #[test]
    fn secret_storage_is_zeroed_before_release() {
        let mut value = String::with_capacity(64);
        value.extend(std::iter::repeat_n('x', 64));
        value.truncate("sensitive".len());
        zero_string(&mut value);
        let allocation = unsafe { std::slice::from_raw_parts(value.as_ptr(), value.capacity()) };
        assert!(allocation.iter().all(|byte| *byte == 0));
    }
}
