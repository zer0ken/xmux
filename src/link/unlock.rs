//! The login conversation: one ssh on a PTY whose screen the USER watches.
//!
//! ssh asks for two things xmux cannot decide in advance, the host key and the
//! credentials, and it asks for them on a terminal. So the login runs on a PTY, xmux
//! draws that PTY, and the keys the user presses reach it. What the pane already
//! collected is typed for them: the host-key question is answered once, and the password
//! is written once if the pane carried one. Everything else is theirs to answer, which is
//! what makes two-factor codes, key passphrases, and prompts in any language a
//! conversation rather than a failure.
//!
//! Nothing here decides that a login failed from what it read. A wrong password only
//! means ssh will ask again, and the person watching can answer better than xmux can.
//! The verdict is the child's exit code, and until the child exits the conversation is
//! still open.
//!
//! The prompt logic is a pure state machine ([`Answerer`]) tested without a PTY; the
//! conversation runs on its own thread ([`start_login`]) because every part of it -
//! opening the PTY, spawning ssh, reading it - waits on something the runtime thread
//! must never wait on.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::display::attachment::PtyCmd;
use crate::display::grid::Grid;

/// How often the conversation wakes while ssh is silent. It bounds how long a cancel
/// waits, and nothing else: the idle budget is counted from its own deadline.
const POLL: Duration = Duration::from_millis(100);

/// The id a login's redraw request carries. The attach registry hands out ids counting
/// up from zero, so this one belongs to no attachment and can never be mistaken for one
/// to reap; the app reads only the fact that something was drawn.
const WAKE_ID: u64 = u64::MAX;

/// What the [`Answerer`] tells the conversation to type next. An empty return means it
/// has nothing to say, which is the normal state once the pane's values are spent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PromptWrite {
    /// The ssh host-key question: type `yes`.
    HostKey,
    /// The password prompt: type the pane's secret.
    Password,
}

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
/// says what to type from the pane's values and, once those are spent, says nothing -
/// the user is watching the same screen and answers the rest.
///
/// It also NOTES an auth failure it recognises. That note only sharpens the word the app
/// shows for a child that exited nonzero; it never ends the conversation, because ssh
/// re-prompts after a wrong password and the user can get it right.
pub(crate) struct Answerer {
    secret: String,
    /// Whether the password was already typed. The pane's value is one answer, not a
    /// standing offer: a second prompt means the first was wrong, and the user answers
    /// that one.
    replied: bool,
    /// Whether the host-key question was already answered.
    accepted: bool,
    auth_failed: bool,
}

impl Answerer {
    pub(crate) fn new(secret: String) -> Self {
        Self {
            secret,
            replied: false,
            accepted: false,
            auth_failed: false,
        }
    }

    /// Feeds one chunk of the ssh child's output and returns what to type.
    pub(crate) fn feed(&mut self, chunk: &str) -> Vec<PromptWrite> {
        if chunk.contains("Permission denied") {
            self.auth_failed = true;
        }
        // The host-key question precedes the password and is its own one-shot: answering
        // it does not spend the password, which ssh asks for next.
        if !self.accepted && chunk.contains("yes/no/[fingerprint]") {
            self.accepted = true;
            return vec![PromptWrite::HostKey];
        }
        // An empty pane password is not an answer: the user types it, so ssh's prompt
        // must be left standing rather than answered with a blank line.
        if !self.replied && !self.secret.is_empty() && chunk.contains("assword:") {
            self.replied = true;
            return vec![PromptWrite::Password];
        }
        Vec::new()
    }

    /// The bytes to type for one prompt.
    pub(crate) fn bytes_for(&self, write: &PromptWrite) -> Vec<u8> {
        match write {
            PromptWrite::HostKey => b"yes\n".to_vec(),
            PromptWrite::Password => format!("{}\n", self.secret).into_bytes(),
        }
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

/// A login the user is watching: the grid the pane draws, the keys it forwards, and the
/// way to end it. Held by the app for as long as the conversation runs.
pub struct RunningLogin {
    /// The blocked source this login is for. The pane belongs to one host, so a login
    /// running for another is not this pane's.
    pub source: String,
    /// The screen ssh is drawing. Present from the first frame, empty until the child
    /// writes, so the view never has nothing to show.
    pub grid: Arc<Mutex<Grid>>,
    /// Set once the PTY is open. Before that there is no child to type at, and the few
    /// keystrokes that could land in that window are dropped rather than queued for a
    /// prompt that has not been asked yet.
    input: Arc<Mutex<Option<Sender<PtyCmd>>>>,
    cancel: Arc<AtomicBool>,
}

impl RunningLogin {
    /// Types `bytes` at the child.
    pub fn input(&self, bytes: Vec<u8>) {
        if let Ok(slot) = self.input.lock() {
            if let Some(tx) = slot.as_ref() {
                let _ = tx.send(PtyCmd::Input(bytes));
            }
        }
    }

    /// Resizes the PTY and the grid together, so ssh draws for the pane it is shown in.
    pub fn resize(&self, cols: u16, rows: u16) {
        if cols == 0 || rows == 0 {
            return;
        }
        if let Ok(slot) = self.input.lock() {
            if let Some(tx) = slot.as_ref() {
                let _ = tx.send(PtyCmd::Resize { cols, rows });
            }
        }
        if let Ok(mut g) = self.grid.lock() {
            g.resize(rows, cols);
        }
    }

    /// Ends the conversation. The thread kills the child on its next wake, so the verdict
    /// still arrives through the same channel as any other ending.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }

    /// A handle with no conversation behind it, for the callers that only ask WHETHER a
    /// login is running. The view decides what to draw from that alone, so testing that
    /// decision needs no ssh, no PTY, and no platform.
    #[cfg(test)]
    pub(crate) fn parked(source: &str) -> Self {
        Self {
            source: source.to_string(),
            grid: Arc::new(Mutex::new(Grid::new(24, 80))),
            input: Arc::new(Mutex::new(None)),
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }
}

/// Starts the login and returns at once: the handle the app renders and types into, and
/// the channel the verdict arrives on. Everything that waits - the PTY open, the ssh
/// spawn, the reading - happens on the thread this starts, so the runtime thread is free
/// for the frames that make the conversation watchable.
///
/// `idle` bounds a conversation nobody is having: it is counted from the last thing ssh
/// said, so a prompt the user is still reading does not end the login, but an ssh that
/// went quiet and a user who walked away do.
///
/// `wake` is how the screen keeps up with the conversation: every chunk ssh writes is
/// announced on the app's own PTY event channel, so the frame that shows a prompt is
/// drawn when the prompt arrives rather than on the next animation beat. It carries no
/// attachment id that could be reaped - [`WAKE_ID`] is outside what the registry hands
/// out - because a login PTY is nobody's attachment.
#[allow(clippy::too_many_arguments)]
pub fn start_login(
    source: String,
    argv: Vec<String>,
    password: String,
    cols: u16,
    rows: u16,
    idle: Duration,
    wake: tokio::sync::mpsc::UnboundedSender<crate::display::attachment::PtyEvent>,
) -> (RunningLogin, tokio::sync::oneshot::Receiver<UnlockOutcome>) {
    let grid = Arc::new(Mutex::new(Grid::new(rows.max(1), cols.max(1))));
    let input: Arc<Mutex<Option<Sender<PtyCmd>>>> = Arc::new(Mutex::new(None));
    let cancel = Arc::new(AtomicBool::new(false));
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();

    let handle = RunningLogin {
        source,
        grid: grid.clone(),
        input: input.clone(),
        cancel: cancel.clone(),
    };
    std::thread::spawn(move || {
        let outcome = converse(argv, password, cols, rows, idle, grid, input, cancel, wake);
        let _ = done_tx.send(outcome);
    });
    (handle, done_rx)
}

/// The conversation itself, on its own thread: spawn ssh on a PTY, type the pane's
/// answers at the prompts that want them, and report what the child's exit says.
#[allow(clippy::too_many_arguments)]
fn converse(
    argv: Vec<String>,
    password: String,
    cols: u16,
    rows: u16,
    idle: Duration,
    grid: Arc<Mutex<Grid>>,
    input: Arc<Mutex<Option<Sender<PtyCmd>>>>,
    cancel: Arc<AtomicBool>,
    wake: tokio::sync::mpsc::UnboundedSender<crate::display::attachment::PtyEvent>,
) -> UnlockOutcome {
    let env_clear = crate::mux::vocab::mux_env_keys_to_clear(std::env::vars().map(|(k, _)| k));
    let (mut console, tap) = match crate::display::console::spawn_console_into(
        &argv,
        cols.max(1),
        rows.max(1),
        &env_clear,
        grid,
    ) {
        Ok(v) => v,
        Err(e) => return UnlockOutcome::Failed(e.to_string()),
    };
    if let Ok(mut slot) = input.lock() {
        *slot = Some(console.input_sender());
    }

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
                for write in answerer.feed(&text) {
                    console.input(answerer.bytes_for(&write));
                }
                // The screen changed: ask for a frame now rather than on the next beat.
                let _ = wake.send(crate::display::attachment::PtyEvent::Output { id: WAKE_ID });
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
        let writes = a.feed(
            "The authenticity of host 'x' can't be established.\n\
             Are you sure you want to continue connecting (yes/no/[fingerprint])? ",
        );
        assert_eq!(writes, vec![PromptWrite::HostKey]);
        assert_eq!(a.bytes_for(&PromptWrite::HostKey), b"yes\n");
        let writes = a.feed("alice@x's password: ");
        assert_eq!(writes, vec![PromptWrite::Password]);
        assert_eq!(a.bytes_for(&PromptWrite::Password), b"hunter2\n");
    }

    #[test]
    fn each_pane_value_is_typed_at_most_once() {
        let mut a = Answerer::new("hunter2".into());
        assert_eq!(a.feed("alice@x's password: "), vec![PromptWrite::Password]);
        assert_eq!(
            a.feed("alice@x's password: "),
            Vec::new(),
            "a second prompt is the user's to answer"
        );
        let mut a = Answerer::new("hunter2".into());
        assert_eq!(
            a.feed("continue connecting (yes/no/[fingerprint])? "),
            vec![PromptWrite::HostKey]
        );
        assert_eq!(
            a.feed("continue connecting (yes/no/[fingerprint])? "),
            Vec::new()
        );
    }

    /// The pane's password is optional, and an empty one is not an answer: typing a bare
    /// newline would spend ssh's attempt on nothing. The prompt is left standing for the
    /// person looking at it.
    #[test]
    fn an_empty_pane_password_leaves_the_prompt_to_the_user() {
        let mut a = Answerer::new(String::new());
        assert_eq!(a.feed("alice@x's password: "), Vec::new());
    }

    /// A prompt xmux does not recognise draws no answer at all, which is what hands
    /// two-factor codes and key passphrases to the user instead of failing on them.
    #[test]
    fn an_unrecognised_prompt_draws_no_answer() {
        let mut a = Answerer::new("hunter2".into());
        assert_eq!(
            a.feed("Enter passphrase for key '/home/u/.ssh/id_ed25519': "),
            Vec::new()
        );
        assert_eq!(a.feed("Verification code: "), Vec::new());
        assert_eq!(a.feed("암호: "), Vec::new());
    }

    /// A wrong password is not the end: ssh asks again and the user answers. Nothing here
    /// may decide the login failed while the child is still running.
    #[test]
    fn a_wrong_password_does_not_end_the_conversation() {
        let mut a = Answerer::new("hunter2".into());
        let _ = a.feed("alice@x's password: ");
        assert_eq!(
            a.feed("Permission denied, please try again.\nalice@x's password: "),
            Vec::new(),
            "the retry prompt is the user's"
        );
        assert_eq!(
            a.verdict(Some(0)),
            UnlockOutcome::Ok,
            "a user who then got it right logged in"
        );
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

    /// The conversation drives a real PTY: the login handle renders a grid, types what
    /// the pane carried, and returns the child's own code.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_login_answers_a_password_prompt_and_reports_the_exit() {
        // Stands in for ssh: asks the way ssh asks, accepts one password, exits by it.
        let argv = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "printf \"u@h's password: \"; read -r p; test \"$p\" = hunter2".to_string(),
        ];
        let (wake, _wake_rx) = tokio::sync::mpsc::unbounded_channel();
        let (login, done) = start_login(
            "prod".into(),
            argv,
            "hunter2".into(),
            40,
            6,
            Duration::from_secs(10),
            wake,
        );
        assert_eq!(login.source, "prod");
        assert_eq!(
            done.await.expect("the verdict arrives"),
            UnlockOutcome::Ok,
            "the pane's password was typed at the prompt"
        );
    }

    /// A prompt the pane has no answer for waits for the user, and what they type is what
    /// decides it.
    #[cfg(unix)]
    #[tokio::test]
    async fn what_the_user_types_reaches_the_login() {
        let argv = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "printf 'Verification code: '; read -r c; test \"$c\" = 123456".to_string(),
        ];
        let (wake, _wake_rx) = tokio::sync::mpsc::unbounded_channel();
        let (login, done) = start_login(
            "prod".into(),
            argv,
            String::new(),
            40,
            6,
            Duration::from_secs(10),
            wake,
        );
        // The user reads the prompt and answers it.
        tokio::time::sleep(Duration::from_millis(300)).await;
        login.input(b"123456\n".to_vec());
        assert_eq!(done.await.expect("the verdict arrives"), UnlockOutcome::Ok);
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
        let (wake, _wake_rx) = tokio::sync::mpsc::unbounded_channel();
        let (login, done) = start_login(
            "prod".into(),
            argv,
            String::new(),
            40,
            6,
            Duration::from_secs(30),
            wake,
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
        let (wake, _wake_rx) = tokio::sync::mpsc::unbounded_channel();
        let (_login, done) = start_login(
            "prod".into(),
            argv,
            String::new(),
            40,
            6,
            Duration::from_millis(300),
            wake,
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
        let (wake, _wake_rx) = tokio::sync::mpsc::unbounded_channel();
        let (_running, done) = start_login(
            "live".into(),
            argv,
            String::new(),
            80,
            24,
            Duration::from_secs(30),
            wake,
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
