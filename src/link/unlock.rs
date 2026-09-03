//! The password unlock worker: answers a `ControlMaster=yes` ssh's prompts over a
//! pty and establishes the one authenticated master every later channel reuses.
//! The prompt logic is a pure state machine ([`Answerer`]) tested without a pty;
//! the thin pty wrapper ([`unlock_host`]) spawns, reads, writes, and reaps.

use crate::transport::Transport;

/// What the [`Answerer`] tells the pty loop to write next, or that it is done. An
/// empty return means nothing yet (keep reading).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PromptWrite {
    /// The ssh host-key prompt: write `yes\n`.
    HostKey,
    /// The password/passphrase prompt: write the secret and a newline.
    Password,
    /// The unlock is decided; the loop returns this outcome.
    Done(UnlockOutcome),
}

/// The verdict of one unlock attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnlockOutcome {
    /// The master is established (the child exited 0); every later channel reuses it.
    Ok,
    /// The server refused the credentials (`Permission denied`).
    AuthFailed,
    /// Nothing answered within the budget.
    Timeout,
    /// The machine has no reusable master (local/WSL/Windows).
    Unavailable,
    /// A spawn/io/exit failure that is neither auth nor a timeout.
    Failed(String),
}

/// The pure prompt-answer state machine for one unlock. Fed the ssh child's output
/// chunks, it returns what to write next and, finally, the outcome - so the prompt
/// logic is unit-tested without a pty.
pub(crate) struct Answerer {
    secret: String,
    /// Whether the password (or host key, which precedes it) was already answered.
    /// One unlock answers the sequence at most once, so a repeated prompt after the
    /// first reply is a wrong password, not a second credential.
    replied: bool,
    done: Option<UnlockOutcome>,
}

impl Answerer {
    pub(crate) fn new(secret: String) -> Self {
        Self {
            secret,
            replied: false,
            done: None,
        }
    }

    /// Feeds one chunk of the ssh child's output and returns what to write. A wrong
    /// password re-prompts right after "please try again", and the final auth
    /// failure carries ssh's canonical `Permission denied (` signature - either
    /// decides AuthFailed immediately, with no 3-attempt loop.
    pub(crate) fn feed(&mut self, chunk: &str) -> Vec<PromptWrite> {
        if self.done.is_some() {
            return Vec::new();
        }
        if chunk.contains("Permission denied, please try again.")
            || chunk.contains("Permission denied (publickey")
        {
            self.done = Some(UnlockOutcome::AuthFailed);
            return vec![PromptWrite::Done(UnlockOutcome::AuthFailed)];
        }
        if !self.replied {
            // The host-key question precedes the password and is one-shot: answering
            // it does NOT consume the password reply (ssh then asks for the password).
            if chunk.contains("yes/no/[fingerprint]") {
                return vec![PromptWrite::HostKey];
            }
            if chunk.contains("assword:") {
                self.replied = true;
                return vec![PromptWrite::Password];
            }
        }
        Vec::new()
    }

    /// The child exited: its code is the definitive verdict (0 = the master is up).
    /// Any auth-failure text the output carried was already decided by [`feed`].
    pub(crate) fn confirm_exit(&mut self, code: u32) {
        if self.done.is_none() {
            self.done = Some(if code == 0 {
                UnlockOutcome::Ok
            } else {
                UnlockOutcome::Failed(format!("ssh exit {code}"))
            });
        }
    }

    /// The bytes to write for a password prompt: the secret plus a newline.
    pub(crate) fn secret_with_newline(&self) -> Vec<u8> {
        format!("{}\n", self.secret).into_bytes()
    }

    pub(crate) fn outcome(&self) -> Option<UnlockOutcome> {
        self.done.clone()
    }
}

/// Runs the unlock over a pty: spawn the `ControlMaster=yes` ssh on a pty (it needs
/// a controlling tty to prompt), answer its prompts, and return the outcome. On
/// success the ControlMaster socket holds an authenticated connection that every
/// `BatchMode=yes` probe, metadata channel, and display attach reuses. Bounded by
/// `timeout`; the blocking master read runs on a thread so a stall cannot hold the
/// tokio task this runs in.
#[cfg(unix)]
pub(crate) async fn unlock_host(
    transport: &dyn Transport,
    user: &str,
    password: &str,
    timeout: std::time::Duration,
) -> UnlockOutcome {
    let Some(argv) = transport.unlock_argv(user) else {
        return UnlockOutcome::Unavailable;
    };
    let spawned = match super::client::spawn_pty_child(&argv, &[], 80, 24) {
        Ok(s) => s,
        Err(e) => return UnlockOutcome::Failed(e.to_string()),
    };
    let mut child = spawned.child;
    let mut reader = spawned.stdout;
    let mut writer = spawned.stdin;
    let mut answerer = Answerer::new(password.to_string());

    // A thread drains the pty master into a channel; the loop below is the only
    // prompt logic, and nothing it does can block on the child.
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    loop {
        match rx.recv_timeout(timeout) {
            Ok(chunk) => {
                let text = String::from_utf8_lossy(&chunk);
                for action in answerer.feed(&text) {
                    match action {
                        PromptWrite::HostKey => {
                            let _ = writer.write_all(b"yes\n");
                        }
                        PromptWrite::Password => {
                            let _ = writer.write_all(&answerer.secret_with_newline());
                        }
                        PromptWrite::Done(o) => return o,
                    }
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                let _ = child.kill();
                return UnlockOutcome::Timeout;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
        // The child exited: its exit code is the definitive auth verdict.
        if let Some(exit) = child.try_wait().ok().flatten() {
            answerer.confirm_exit(exit.exit_code());
            return answerer.outcome().unwrap_or(UnlockOutcome::Failed(format!(
                "ssh exit {}",
                exit.exit_code()
            )));
        }
    }
    answerer
        .outcome()
        .unwrap_or(UnlockOutcome::Failed("unlock closed".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answerer_sends_yes_then_the_password_and_detects_wrong_password() {
        let mut a = Answerer::new("hunter2".into());
        let writes = a.feed(
            "The authenticity of host 'x' can't be established.\n\
             Are you sure you want to continue connecting (yes/no/[fingerprint])? ",
        );
        assert_eq!(writes, vec![PromptWrite::HostKey]);
        let writes = a.feed("alice@x's password: ");
        assert_eq!(writes, vec![PromptWrite::Password]);
        // Wrong password: the re-prompt after one try decides AuthFailed fast.
        let writes = a.feed("Permission denied, please try again.\nalice@x's password: ");
        assert_eq!(writes, vec![PromptWrite::Done(UnlockOutcome::AuthFailed)]);
    }

    #[test]
    fn answerer_ignores_a_plain_password_prompt_before_the_first_reply() {
        // The same "assword:" pattern that answers a prompt must not fire twice:
        // after the password is written, a normal idle chunk draws no write.
        let mut a = Answerer::new("hunter2".into());
        assert_eq!(a.feed("alice@x's password: "), vec![PromptWrite::Password]);
        assert_eq!(a.feed(""), Vec::new(), "no further write after answering");
    }

    #[test]
    fn answerer_reports_success_on_a_zero_exit() {
        let mut a = Answerer::new("hunter2".into());
        let _ = a.feed("alice@x's password: ");
        a.confirm_exit(0);
        assert_eq!(a.outcome(), Some(UnlockOutcome::Ok));
    }

    #[test]
    fn answerer_reports_the_exit_failure_when_no_auth_text_decided_it() {
        let mut a = Answerer::new("hunter2".into());
        let _ = a.feed("alice@x's password: ");
        a.confirm_exit(255);
        assert_eq!(
            a.outcome(),
            Some(UnlockOutcome::Failed("ssh exit 255".into()))
        );
    }

    #[test]
    fn answerer_detects_the_final_auth_failure_signature() {
        let mut a = Answerer::new("hunter2".into());
        let _ = a.feed("alice@x's password: ");
        let writes =
            a.feed("Permission denied (publickey,password).");
        assert_eq!(writes, vec![PromptWrite::Done(UnlockOutcome::AuthFailed)]);
    }
}
