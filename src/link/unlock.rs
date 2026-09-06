//! The login conversation: one ssh xmux has on the user's behalf, with nothing on screen.
//!
//! ssh asks for two things xmux cannot decide in advance, the host key and the password,
//! and it asks for them on a terminal and nowhere else. So the login runs on a PTY, and
//! that PTY is the MEANS rather than a screen: the pane collected the answers before the
//! login started, so the conversation is xmux's to have, and the user waits for a verdict
//! instead of a prompt.
//!
//! The verdict is the child's exit code. A wrong password only means ssh asks again, so
//! nothing here calls a login failed from what it read - except when ssh asks something
//! this module has no answer for. Nobody is watching the PTY, so such a prompt would
//! stand until the idle budget ran out; it ends the login instead, and what ssh asked for
//! is what the app says.
//!
//! The prompt logic is a pure state machine ([`Answerer`]) tested without a PTY; the
//! conversation runs on its own thread ([`start_login`]) because every part of it -
//! opening the PTY, spawning ssh, reading it - waits on something the runtime thread
//! must never wait on.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How often the conversation wakes while ssh is silent. It bounds how long a cancel
/// waits, and nothing else: the idle budget is counted from its own deadline.
const POLL: Duration = Duration::from_millis(100);

/// The verdict of one login.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnlockOutcome {
    /// The master is established (the child exited 0); every later channel reuses it.
    Ok,
    /// The server refused the credentials.
    AuthFailed,
    /// Neither ssh nor the user said anything for the whole idle budget.
    Timeout,
    /// The user ended it.
    Cancelled,
    /// The machine has no reusable master (local/WSL/Windows).
    Unavailable,
    /// A spawn/io/exit failure that is neither auth nor a timeout.
    Failed(String),
}

/// The pure prompt-answer state machine for one login. Fed the ssh child's output, it
/// says what to type from the pane's values, and says when ssh has asked for something
/// the pane's values cannot answer.
///
/// A password prompt it cannot answer is the end of the login: the pane's password is one
/// answer, so a second prompt means the first was wrong, and an empty pane password means
/// there was never one to give. Either way no answer will ever arrive, and saying so at
/// once is the difference between a verdict and a wait.
pub(crate) struct Answerer {
    secret: String,
    /// Whether the password was already typed.
    replied: bool,
    /// Whether the host-key question was already answered.
    accepted: bool,
    auth_failed: bool,
    /// What ssh asked for that this machine has no answer to, once that has happened.
    stalled: Option<UnlockOutcome>,
}

impl Answerer {
    pub(crate) fn new(secret: String) -> Self {
        Self {
            secret,
            replied: false,
            accepted: false,
            auth_failed: false,
            stalled: None,
        }
    }

    /// Feeds one chunk of the ssh child's output and returns what to type, if anything.
    pub(crate) fn feed(&mut self, chunk: &str) -> Option<Vec<u8>> {
        if chunk.contains("Permission denied") {
            self.auth_failed = true;
        }
        // A prompt is what ssh is WAITING on, so it is the last thing on the stream with
        // no newline after it. Matching the whole chunk would read a login banner that
        // mentions a password as a question to answer, and answer a session that is
        // already open.
        let asking = chunk.rsplit('\n').next().unwrap_or("");
        // The host-key question precedes the password and is its own one-shot: answering
        // it does not spend the password, which ssh asks for next.
        if !self.accepted && asking.contains("yes/no/[fingerprint]") {
            self.accepted = true;
            return Some(b"yes\n".to_vec());
        }
        if asking.contains("assword:") {
            if self.secret.is_empty() {
                self.stalled = Some(UnlockOutcome::Failed(
                    "the server asked for a password".into(),
                ));
            } else if self.replied {
                // The one answer the pane had was already given and ssh asked again.
                self.stalled = Some(UnlockOutcome::AuthFailed);
            } else {
                self.replied = true;
                return Some(format!("{}\n", self.secret).into_bytes());
            }
        }
        None
    }

    /// The verdict for a login that cannot go on, once ssh has asked for something the
    /// pane's values do not answer. `None` while the conversation can still get somewhere.
    pub(crate) fn stalled(&self) -> Option<UnlockOutcome> {
        self.stalled.clone()
    }

    /// The verdict for a child that exited with `code`. Zero is the master; anything else
    /// is a failure, named auth when the output said so.
    pub(crate) fn verdict(&self, code: Option<u32>) -> UnlockOutcome {
        match code {
            Some(0) => UnlockOutcome::Ok,
            Some(c) if self.auth_failed => {
                let _ = c;
                UnlockOutcome::AuthFailed
            }
            Some(c) => UnlockOutcome::Failed(format!("ssh exit {c}")),
            None if self.auth_failed => UnlockOutcome::AuthFailed,
            None => UnlockOutcome::Failed("ssh did not report an exit".into()),
        }
    }
}

/// A login in progress: which host it is for, and the way to end it. It carries no
/// screen, because the conversation is xmux's to have: ssh's two questions are answered
/// from what the pane collected, and a question xmux does not know is one nobody here can
/// answer either.
///
/// The handle is what the pane reads to say a login is under way, so the user is never
/// looking at a form that appears to have done nothing.
pub struct RunningLogin {
    /// The blocked source this login is for. The pane belongs to one host, so a login
    /// running for another is not this pane's.
    pub source: String,
    cancel: Arc<AtomicBool>,
}

impl RunningLogin {
    /// Ends the conversation. The thread kills the child on its next wake, so the verdict
    /// still arrives through the same channel as any other ending.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }

    /// A handle with no conversation behind it, for the callers that only ask WHETHER a
    /// login is running.
    #[cfg(test)]
    pub(crate) fn parked(source: &str) -> Self {
        Self {
            source: source.to_string(),
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }
}

/// Starts the login and returns at once: the handle that says it is running, and the
/// channel the verdict arrives on. Everything that waits - the PTY open, the ssh spawn,
/// the reading - happens on the thread this starts, so the runtime thread stays free to
/// draw the frames that say a login is under way.
///
/// `idle` bounds a conversation that is going nowhere: it is counted from the last thing
/// ssh said, so a server taking its time does not end the login, while one that went quiet
/// on a question xmux cannot answer does.
pub fn start_login(
    source: String,
    argv: Vec<String>,
    remote: Box<dyn FnOnce() -> String + Send>,
    password: String,
    idle: Duration,
) -> (RunningLogin, tokio::sync::oneshot::Receiver<UnlockOutcome>) {
    let cancel = Arc::new(AtomicBool::new(false));
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    let handle = RunningLogin {
        source,
        cancel: cancel.clone(),
    };
    std::thread::spawn(move || {
        let _ = done_tx.send(converse(argv, remote, password, idle, cancel));
    });
    (handle, done_rx)
}

/// The conversation itself, on its own thread: spawn ssh on a PTY, type the pane's
/// answers at the prompts that want them, and report what the child's exit says - or, for
/// a prompt the pane cannot answer, what ssh asked for.
fn converse(
    mut argv: Vec<String>,
    remote: Box<dyn FnOnce() -> String + Send>,
    password: String,
    idle: Duration,
    cancel: Arc<AtomicBool>,
) -> UnlockOutcome {
    // Composing it can spawn (a machine with no key pair is given one), which is why it
    // happens here and not where the login was asked for.
    argv.push(remote());
    let env_clear = crate::mux::vocab::mux_env_keys_to_clear(std::env::vars().map(|(k, _)| k));
    // The PTY is the MEANS, not a screen: ssh reads a password from a terminal and from
    // nowhere else, so one is opened to answer it and nothing renders it.
    let (mut console, tap) = match crate::display::console::spawn_console(&argv, &env_clear) {
        Ok(v) => v,
        Err(e) => return UnlockOutcome::Failed(e.to_string()),
    };

    let mut answerer = Answerer::new(password);
    let mut deadline = Instant::now() + idle;
    // An ending that KILLS the child still waits for it, so a login the user walked away
    // from leaves no process behind for the rest of the run.
    let ended = loop {
        if cancel.load(Ordering::Acquire) {
            console.kill();
            break Some(UnlockOutcome::Cancelled);
        }
        match tap.recv_timeout(POLL) {
            Ok(chunk) => {
                deadline = Instant::now() + idle;
                let text = String::from_utf8_lossy(&chunk);
                if let Some(reply) = answerer.feed(&text) {
                    console.input(reply);
                }
                // ssh asked for what nobody here can give. Waiting out the idle budget
                // would report a timeout for a login whose real answer is already known.
                if let Some(stall) = answerer.stalled() {
                    console.kill();
                    break Some(stall);
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if Instant::now() >= deadline {
                    console.kill();
                    break Some(UnlockOutcome::Timeout);
                }
            }
            // The master hit EOF: ssh is done and its code is the verdict.
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break None,
        }
    };
    let code = console.wait();
    ended.unwrap_or_else(|| answerer.verdict(code))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_answerer_accepts_the_host_key_then_types_the_pane_password() {
        let mut a = Answerer::new("hunter2".into());
        assert_eq!(
            a.feed(
                "The authenticity of host 'x' can't be established.\n\
                 Are you sure you want to continue connecting (yes/no/[fingerprint])? ",
            ),
            Some(b"yes\n".to_vec())
        );
        assert_eq!(a.feed("alice@x's password: "), Some(b"hunter2\n".to_vec()));
    }

    #[test]
    fn each_pane_value_is_typed_at_most_once() {
        let mut a = Answerer::new("hunter2".into());
        assert_eq!(a.feed("alice@x's password: "), Some(b"hunter2\n".to_vec()));
        assert_eq!(
            a.feed("alice@x's password: "),
            None,
            "the pane had one password and it is spent"
        );
        let mut a = Answerer::new("hunter2".into());
        assert_eq!(
            a.feed("continue connecting (yes/no/[fingerprint])? "),
            Some(b"yes\n".to_vec())
        );
        assert_eq!(a.feed("continue connecting (yes/no/[fingerprint])? "), None);
    }

    /// The pane's password is optional, and an empty one is not an answer: typing a bare
    /// newline would spend ssh's attempt on nothing. A server that wants one is telling
    /// the user what the pane is missing, so the login ends on that word.
    #[test]
    fn an_empty_pane_password_ends_the_login_on_what_the_server_wants() {
        let mut a = Answerer::new(String::new());
        assert_eq!(a.feed("alice@x's password: "), None);
        assert_eq!(
            a.stalled(),
            Some(UnlockOutcome::Failed(
                "the server asked for a password".into()
            ))
        );
    }

    /// A banner is not a question. A line about passwords that ssh has already finished
    /// writing is part of a session that is open, and answering it would type the pane's
    /// secret into a shell.
    #[test]
    fn a_banner_that_mentions_a_password_is_not_a_prompt() {
        let mut a = Answerer::new("hunter2".into());
        assert_eq!(
            a.feed("Your password: expires in 3 days. Run passwd.\r\n"),
            None
        );
        assert_eq!(a.stalled(), None);
        assert_eq!(
            a.feed("u@h's password: "),
            Some(b"hunter2\n".to_vec()),
            "the real prompt is still answered"
        );
    }

    /// A prompt xmux does not recognise draws no answer, and is not called a failure
    /// either: what a two-factor code or a key passphrase means for this login is not
    /// something this machine can read out of the words.
    #[test]
    fn an_unrecognised_prompt_draws_no_answer() {
        let mut a = Answerer::new("hunter2".into());
        assert_eq!(
            a.feed("Enter passphrase for key '/home/u/.ssh/id_ed25519': "),
            None
        );
        assert_eq!(a.feed("Verification code: "), None);
        assert_eq!(a.feed("암호: "), None);
        assert_eq!(a.stalled(), None, "ssh may still get somewhere on its own");
    }

    /// ssh asking a second time means the pane's password was wrong. Nobody is watching
    /// the PTY to type a better one, so the login ends on the answer that is already
    /// known rather than on the idle budget.
    #[test]
    fn a_second_password_prompt_ends_the_login_as_an_auth_failure() {
        let mut a = Answerer::new("hunter2".into());
        assert_eq!(a.feed("alice@x's password: "), Some(b"hunter2\n".to_vec()));
        assert_eq!(a.stalled(), None, "the first prompt was answered");
        assert_eq!(
            a.feed("Permission denied, please try again.\nalice@x's password: "),
            None
        );
        assert_eq!(a.stalled(), Some(UnlockOutcome::AuthFailed));
    }

    #[test]
    fn the_exit_code_is_the_verdict() {
        let a = Answerer::new("hunter2".into());
        assert_eq!(a.verdict(Some(0)), UnlockOutcome::Ok);
        assert_eq!(
            a.verdict(Some(255)),
            UnlockOutcome::Failed("ssh exit 255".into())
        );
    }

    /// The auth-failure text only names the failure of a child that already exited
    /// nonzero; it never stands in for the exit itself.
    #[test]
    fn a_refused_login_is_named_auth_failure() {
        let mut a = Answerer::new("hunter2".into());
        let _ = a.feed("alice@x's password: ");
        let _ = a.feed("Permission denied (publickey,password).");
        assert_eq!(a.verdict(Some(255)), UnlockOutcome::AuthFailed);
        assert_eq!(a.verdict(Some(0)), UnlockOutcome::Ok, "0 is still success");
    }

    /// The conversation drives a real PTY: it types what the pane carried at the prompt
    /// that wants it, and returns the child's own code.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_login_answers_a_password_prompt_and_reports_the_exit() {
        // Stands in for ssh: asks the way ssh asks, accepts one password, exits by it.
        let argv = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "printf \"u@h's password: \"; read -r p; test \"$p\" = hunter2".to_string(),
        ];
        let (login, done) = start_login(
            "prod".into(),
            argv,
            Box::new(|| "true".to_string()),
            "hunter2".into(),
            Duration::from_secs(10),
        );
        assert_eq!(login.source, "prod");
        assert_eq!(
            done.await.expect("the verdict arrives"),
            UnlockOutcome::Ok,
            "the pane's password was typed at the prompt"
        );
    }

    /// ssh asking twice ends the login there and then. Nobody is watching the PTY, so a
    /// login left to the idle budget would report a timeout minutes after the answer was
    /// known: the child is killed and the verdict is the refusal.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_refused_password_ends_the_login_before_the_idle_budget() {
        // Stands in for an ssh that refuses and asks again, then waits far past the test.
        let argv = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "printf \"u@h's password: \"; read -r p; \
             printf '\\nPermission denied, please try again.\\n'; \
             printf \"u@h's password: \"; sleep 60"
                .to_string(),
        ];
        let started = Instant::now();
        let (_login, done) = start_login(
            "prod".into(),
            argv,
            Box::new(|| "true".to_string()),
            "wrong".into(),
            Duration::from_secs(60),
        );
        assert_eq!(
            done.await.expect("the verdict arrives"),
            UnlockOutcome::AuthFailed
        );
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "the verdict did not wait out the idle budget"
        );
    }

    /// A server asking for a password the pane does not carry ends the login on what it
    /// asked for, which is the one thing the user has to know to fill the pane in.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_password_the_pane_does_not_carry_ends_the_login_on_what_the_server_asked() {
        let argv = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "printf \"u@h's password: \"; sleep 60".to_string(),
        ];
        let (_login, done) = start_login(
            "prod".into(),
            argv,
            Box::new(|| "true".to_string()),
            String::new(),
            Duration::from_secs(60),
        );
        assert_eq!(
            done.await.expect("the verdict arrives"),
            UnlockOutcome::Failed("the server asked for a password".into())
        );
    }

    /// Cancelling ends a conversation that is going nowhere, and says so.
    #[cfg(unix)]
    #[tokio::test]
    async fn cancelling_ends_the_login() {
        let argv = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "sleep 30".to_string(),
        ];
        let (login, done) = start_login(
            "prod".into(),
            argv,
            Box::new(|| "true".to_string()),
            String::new(),
            Duration::from_secs(30),
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
        login.cancel();
        assert_eq!(
            done.await.expect("the verdict arrives"),
            UnlockOutcome::Cancelled
        );
    }

    /// An ssh that says nothing for the whole budget ends on its own, so a login nobody
    /// is having cannot hold a child open for the rest of the run.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_silent_login_ends_on_the_idle_budget() {
        let argv = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "sleep 30".to_string(),
        ];
        let (_login, done) = start_login(
            "prod".into(),
            argv,
            Box::new(|| "true".to_string()),
            String::new(),
            Duration::from_millis(300),
        );
        assert_eq!(
            done.await.expect("the verdict arrives"),
            UnlockOutcome::Timeout
        );
    }

    /// The live gate: the real login path against a real sshd, reaching a host by an
    /// ADDRESS its own name does not resolve to - the shape the pane submits when a
    /// machine is offered under a label this box cannot look up. Skipped (not just
    /// ignored) when the env is absent, so a routine `cargo test` never depends on a live
    /// host.
    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "live gate: set XMUX_LIVE_ADDRESS to a reachable sshd absent from known_hosts"]
    async fn live_login_reaches_a_host_by_the_submitted_address() {
        let Ok(address) = std::env::var("XMUX_LIVE_ADDRESS") else {
            return;
        };
        let cp = "/tmp/xmux-live-login.sock".to_string();
        let _ = std::fs::remove_file(&cp);
        // A name that resolves to nothing, reached by the address the pane supplies.
        let transport = crate::transport::ssh_as(
            "xmux-live-nonexistent".into(),
            "xmux-live-nonexistent".into(),
            cp.clone(),
            "linux".into(),
        );
        let login = crate::transport::Login {
            address: Some(address),
            port: Some(22),
            user: std::env::var("USER").ok(),
        };
        let argv = crate::transport::Transport::login_argv(&*transport, &login)
            .expect("a remote host has a login argv");
        let (_running, done) = start_login(
            "live".into(),
            argv,
            Box::new(|| "true".to_string()),
            String::new(),
            Duration::from_secs(30),
        );
        assert_eq!(
            done.await.expect("the verdict arrives"),
            UnlockOutcome::Ok,
            "the submitted address reaches the host the name does not"
        );
        // The master it left behind is what every later channel rides.
        let (name, args) =
            crate::transport::Transport::exec_argv(&*transport, false, &["true".to_string()]);
        let status = std::process::Command::new(&name)
            .args(&args)
            .status()
            .expect("ssh runs");
        assert!(status.success(), "a later channel reuses the master");
    }
}
