//! One bounded, cancellable login attempt and its user-facing diagnosis.

use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_millis(50);
const REASON_LIMIT: usize = 4096;
pub const LOGIN_IDLE: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    WrongPassword,
    AuthenticationRefused,
    Unreachable,
    HostKeyMismatch,
    /// ssh could not verify the host key without asking, which the submitted login's
    /// accept-new policy prevents unless the user's own ssh setup overrides it.
    HostKeyUnverified,
    ServerClosedAfterAuthentication,
    Timeout,
    Cancelled,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnlockOutcome {
    Ok,
    Failed { kind: FailureKind, reason: String },
    Unavailable,
}

impl UnlockOutcome {
    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Ok)
    }

    pub fn refused_password(&self) -> bool {
        matches!(
            self,
            Self::Failed {
                kind: FailureKind::WrongPassword,
                ..
            }
        )
    }

    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Failed { reason, .. } => Some(reason),
            Self::Unavailable => Some("login is unavailable for this machine"),
            Self::Ok => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conversation {
    pub outcome: UnlockOutcome,
    pub output: String,
    pub shell: Option<crate::transport::vocab::RemoteShell>,
    pub password_supplied: bool,
}

#[derive(Clone)]
pub struct RunningLogin {
    pub source: String,
    /// The submission this handle runs, so a result from a replaced one is told apart.
    pub attempt: u64,
    cancel: Arc<AtomicBool>,
}

impl RunningLogin {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }

    pub(crate) fn pending(source: String, attempt: u64) -> (Self, Arc<AtomicBool>) {
        let cancel = Arc::new(AtomicBool::new(false));
        (
            Self {
                source,
                attempt,
                cancel: cancel.clone(),
            },
            cancel,
        )
    }

    #[cfg(test)]
    pub(crate) fn parked(source: &str) -> Self {
        Self {
            source: source.to_string(),
            attempt: 0,
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }
}

pub fn start_login(
    source: String,
    command: crate::transport::CommandSpec,
    timeout: Duration,
) -> (RunningLogin, tokio::sync::oneshot::Receiver<Conversation>) {
    let (handle, cancel) = RunningLogin::pending(source.clone(), 0);
    let done_rx = start_login_with_cancel(source, command, timeout, cancel, Box::new(|| {}));
    (handle, done_rx)
}

/// Runs the login off the calling thread. `password_asked` runs once, the moment askpass
/// hands the held password to ssh: a server asks only after it accepted the connection,
/// so that moment is the one boundary between connecting and authenticating ssh shows.
pub(crate) fn start_login_with_cancel(
    source: String,
    command: crate::transport::CommandSpec,
    timeout: Duration,
    cancel: Arc<AtomicBool>,
    password_asked: Box<dyn FnOnce() + Send>,
) -> tokio::sync::oneshot::Receiver<Conversation> {
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let _ = done_tx.send(run(source, command, timeout, cancel, password_asked));
    });
    done_rx
}

fn run(
    source: String,
    command: crate::transport::CommandSpec,
    timeout: Duration,
    cancel: Arc<AtomicBool>,
    password_asked: Box<dyn FnOnce() + Send>,
) -> Conversation {
    let mut password_asked = Some(password_asked);
    let mut process = std::process::Command::new(command.program());
    process
        .args(command.args())
        .envs(command.env().iter().cloned())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(unix)]
    if command.should_detach_tty() {
        use std::os::unix::process::CommandExt as _;
        unsafe {
            process.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    let mut child = match process.spawn() {
        Ok(child) => child,
        Err(error) => {
            command.discard_credential();
            return failed(FailureKind::Other, error.to_string(), false);
        }
    };
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let out = std::thread::spawn(move || read_bounded(stdout));
    let err = std::thread::spawn(move || read_bounded(stderr));
    let started = Instant::now();
    let ended = loop {
        if cancel.load(Ordering::Acquire) {
            let _ = child.kill();
            break Err(FailureKind::Cancelled);
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            break Err(FailureKind::Timeout);
        }
        if command.password_was_supplied() {
            if let Some(asked) = password_asked.take() {
                asked();
            }
        }
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => std::thread::sleep(POLL),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = out.join();
                let _ = err.join();
                command.discard_credential();
                return failed(
                    FailureKind::Other,
                    error.to_string(),
                    command.password_was_supplied(),
                );
            }
        }
    };
    let _ = child.wait();
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    let raw = format!(
        "{}\n{}",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr)
    );
    let shell = crate::transport::vocab::RemoteShell::from_marked_probe(&raw);
    let mut output = sanitize_output(&raw);
    if let Some(reason) = command.auth_unavailable() {
        let unavailable = sanitize_output(&format!("xmux credential broker unavailable: {reason}"));
        if !output.is_empty() {
            output.insert(0, '\n');
        }
        output.insert_str(0, &unavailable);
    }
    if let Some(prompt) = command.refused_auth_prompt() {
        let refusal = sanitize_output(&format!("xmux askpass refused prompt: {prompt}"));
        if !refusal.is_empty() && !output.contains(&refusal) {
            if !output.is_empty() {
                output.push('\n');
            }
            output.push_str(&refusal);
        }
    }
    if let Ok(status) = &ended {
        if let Some(retry) = command.password_only_retry(status.code().unwrap_or(-1), &output) {
            tracing::info!(source = %source, "key opened no session; retrying with the password alone");
            let retry = retry.clone();
            let remaining = timeout.saturating_sub(started.elapsed());
            let asked = password_asked.unwrap_or_else(|| Box::new(|| {}));
            return run(source, retry, remaining, cancel, asked);
        }
    }
    let password_supplied = command.password_was_supplied();
    let outcome = match ended {
        Err(FailureKind::Cancelled) => UnlockOutcome::Failed {
            kind: FailureKind::Cancelled,
            reason: "cancelled".into(),
        },
        Err(FailureKind::Timeout) => UnlockOutcome::Failed {
            kind: FailureKind::Timeout,
            reason: format!(
                "timed out\nssh did not finish within {}s",
                timeout.as_secs()
            ),
        },
        Err(_) => unreachable!(),
        Ok(status) if status.success() => {
            if !command.finish_successful_login() {
                UnlockOutcome::Failed {
                    kind: FailureKind::Cancelled,
                    reason: "login was replaced by a newer attempt".into(),
                }
            } else {
                UnlockOutcome::Ok
            }
        }
        Ok(status) => {
            command.forget_refused_password(status.code().unwrap_or(-1), &output);
            command.discard_credential();
            classify_failure_with_host_key(&output, password_supplied, command.host_key_command())
        }
    };
    if matches!(
        outcome,
        UnlockOutcome::Failed {
            kind: FailureKind::Timeout | FailureKind::Cancelled,
            ..
        }
    ) {
        command.discard_credential();
    }
    if !outcome.is_ok() {
        tracing::warn!(source = %source, outcome = ?outcome, "login_failed");
    }
    Conversation {
        outcome,
        output,
        shell,
        password_supplied,
    }
}

fn failed(kind: FailureKind, reason: String, password_supplied: bool) -> Conversation {
    Conversation {
        outcome: UnlockOutcome::Failed { kind, reason },
        output: String::new(),
        shell: None,
        password_supplied,
    }
}

fn read_bounded(mut reader: impl Read) -> Vec<u8> {
    let mut all = Vec::new();
    let _ = reader.read_to_end(&mut all);
    if all.len() > REASON_LIMIT * 4 {
        all.drain(..all.len() - REASON_LIMIT * 4);
    }
    all
}

#[cfg(test)]
pub(crate) fn classify_failure(output: &str, password_supplied: bool) -> UnlockOutcome {
    classify_failure_with_host_key(output, password_supplied, None)
}

/// Categorizes a probe's raw ssh text the way a login failure is categorized.
/// `password_supplied` says whether the probe handed ssh the held password, which is
/// what makes a refusal a refused password rather than a refused authentication.
pub fn classify_probe(output: &str, password_supplied: bool) -> UnlockOutcome {
    classify_failure_with_host_key(output, password_supplied, None)
}

fn classify_failure_with_host_key(
    output: &str,
    password_supplied: bool,
    host_key_command: Option<&str>,
) -> UnlockOutcome {
    let lower = output.to_ascii_lowercase();
    let broker_unavailable = lower.contains("xmux credential broker unavailable");
    let kind = if broker_unavailable {
        FailureKind::Other
    } else if crate::transport::diagnostic::host_key_changed(output) {
        FailureKind::HostKeyMismatch
    } else if crate::transport::diagnostic::host_key_unknown(output) {
        FailureKind::HostKeyUnverified
    } else if password_supplied && crate::transport::diagnostic::contains_auth_refusal(output) {
        FailureKind::WrongPassword
    } else if crate::transport::diagnostic::contains_auth_refusal(output) {
        FailureKind::AuthenticationRefused
    } else if password_supplied
        && (lower.contains("connection closed")
            || lower.contains("connection reset")
            || lower.contains("connection was closed"))
    {
        FailureKind::ServerClosedAfterAuthentication
    } else if lower.contains("could not resolve hostname")
        || lower.contains("connection refused")
        || lower.contains("no route to host")
        || lower.contains("network is unreachable")
        || lower.contains("connection timed out")
    {
        FailureKind::Unreachable
    } else {
        FailureKind::Other
    };
    let summary = match kind {
        FailureKind::WrongPassword => "the password was refused",
        FailureKind::AuthenticationRefused => "authentication was refused",
        FailureKind::ServerClosedAfterAuthentication => {
            "the server accepted the password but closed the session before it started"
        }
        FailureKind::HostKeyMismatch => "the host key changed",
        FailureKind::HostKeyUnverified => "the host key could not be verified",
        FailureKind::Unreachable if lower.contains("could not resolve hostname") => {
            "the host name could not be resolved"
        }
        FailureKind::Unreachable => "the host could not be reached",
        FailureKind::Timeout => "timed out",
        FailureKind::Cancelled => "cancelled",
        FailureKind::Other if broker_unavailable => "xmux could not provide the held password",
        FailureKind::Other if lower.contains("xmux askpass refused prompt") => {
            "xmux refused an unexpected authentication prompt"
        }
        FailureKind::Other => "ssh failed",
    };
    let summary = if kind == FailureKind::HostKeyUnverified {
        host_key_command
            .map(|command| format!("the host key must be added first\nrun: {command}"))
            .unwrap_or_else(|| summary.to_string())
    } else {
        summary.to_string()
    };
    let reason = if output.trim().is_empty() {
        summary
    } else {
        format!("{summary}\n{output}")
    };
    UnlockOutcome::Failed { kind, reason }
}

pub(crate) fn sanitize_output(input: &str) -> String {
    crate::transport::diagnostic::sanitize(input)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conpty_controls_prompts_and_shell_probe_are_removed() {
        let sample = "\u{1b}[6n\u{1b}[?9001h...dev@127.0.0.1's password: \r\nxmux-shell:bash\r\n";
        assert_eq!(sanitize_output(sample), "");
    }

    #[test]
    fn meaningful_diagnostics_are_preserved_and_categorized() {
        let output = sanitize_output(
            "\u{1b}[31mdev@host: Permission denied (publickey,password,keyboard-interactive).\u{1b}[0m\r\n",
        );
        assert_eq!(
            classify_failure(&output, true),
            UnlockOutcome::Failed {
                kind: FailureKind::WrongPassword,
                reason: "the password was refused\ndev@host: Permission denied (publickey,password,keyboard-interactive).".into(),
            }
        );
        assert!(matches!(
            classify_failure("Connection closed by 127.0.0.1 port 22", true),
            UnlockOutcome::Failed {
                kind: FailureKind::ServerClosedAfterAuthentication,
                ..
            }
        ));
        assert!(matches!(
            classify_failure(
                "WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!\nHost key verification failed.",
                false
            ),
            UnlockOutcome::Failed {
                kind: FailureKind::HostKeyMismatch,
                ..
            }
        ));
        assert!(matches!(
            classify_failure("Host key verification failed.", false),
            UnlockOutcome::Failed {
                kind: FailureKind::HostKeyUnverified,
                ..
            }
        ));
    }

    #[test]
    fn strict_host_key_failure_tells_the_user_how_to_add_it() {
        let outcome = classify_failure_with_host_key(
            "Host key verification failed.",
            false,
            Some("ssh -o User=dev -- box"),
        );
        assert_eq!(
            outcome.reason(),
            Some(
                "the host key must be added first\nrun: ssh -o User=dev -- box\nHost key verification failed."
            )
        );
    }

    #[test]
    fn an_askpass_refusal_names_the_prompt() {
        let output = sanitize_output("xmux askpass refused prompt: Password for dev@bastion:\n");
        assert_eq!(
            output,
            "xmux askpass refused prompt: Password for dev@bastion:"
        );
    }

    #[test]
    fn key_only_authentication_refusal_is_explained() {
        assert_eq!(
            classify_failure("dev@host: Permission denied (publickey).", false).reason(),
            Some("authentication was refused\ndev@host: Permission denied (publickey).")
        );
    }
}
