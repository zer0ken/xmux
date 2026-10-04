//! Login input values shared by domain commands and runtime state.

/// What the login pane does with the values once the connection works. The two are one
/// choice, not two switches: a draft either leaves nothing behind or writes a stanza.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Remember {
    #[default]
    Nothing,
    SshConfig,
}

/// One step a login performs, in the order it performs them. Connecting and
/// authenticating are one ssh child; recording the values and registering the key run
/// after it; finding the mux is the re-probe a working login starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginStep {
    Connect,
    Authenticate,
    Save,
    RegisterKey,
    FindMux,
}

/// Where one step stands. A step the login never reached because an earlier one failed is
/// skipped, as is a key registration that declined to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepState {
    Pending,
    Running,
    Done,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepRow {
    pub step: LoginStep,
    pub state: StepState,
    /// Why the step failed or was skipped, when the step itself said so.
    pub note: Option<String>,
}

/// What a running login reports before its verdict, each at the moment it happens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginEvent {
    /// ssh asked xmux for the password. Only a server that accepted the connection asks,
    /// so the connection is up and authentication is under way.
    PasswordAsked,
    /// The ssh child ended with this verdict.
    Verdict(crate::link::unlock::UnlockOutcome),
    /// The values were written to ssh config, or the reason they were not.
    Saved(Result<(), String>),
}

/// What a machine answered when asked for its muxes after a working login.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MuxAnswer {
    /// A mux answered with its sessions.
    Found,
    /// The machine answered, and no mux xmux drives answered on it.
    NoMux,
    /// Asking failed, for this reason.
    Failed(String),
}

/// The steps of one login attempt and where each stands, advanced only by what that
/// attempt reports. ssh reports no boundary between connecting and authenticating unless
/// it asks for a password, so a key login holds the connect step until its verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginProgress {
    /// Which submission these steps belong to. An event carrying another attempt is from
    /// a login this one replaced, and changes nothing here.
    pub attempt: u64,
    pub steps: Vec<StepRow>,
    /// `address:port` as submitted, or `None` when ssh resolves the address itself.
    pub target: Option<String>,
    pub user: Option<String>,
    pub password: bool,
    /// The machine probe a working login started. Only that probe's answer, and the mux
    /// answers that follow it, settle the mux search: an answer to a probe already in
    /// flight says nothing about what the login changed.
    pub probe: Option<u64>,
    /// Whether that probe found the machine answering, so a mux answer now settles the
    /// mux search.
    pub machine_answered: bool,
}

impl LoginProgress {
    /// The steps a submitted login will run: the two connection steps, the follow-ups the
    /// pane selected, and the mux search. Connecting starts at once.
    pub fn start(
        attempt: u64,
        login: &crate::transport::Login,
        password: bool,
        save: bool,
        register: bool,
    ) -> Self {
        let row = |step, state| StepRow {
            step,
            state,
            note: None,
        };
        let mut steps = vec![
            row(LoginStep::Connect, StepState::Running),
            row(LoginStep::Authenticate, StepState::Pending),
        ];
        if save {
            steps.push(row(LoginStep::Save, StepState::Pending));
        }
        if register {
            steps.push(row(LoginStep::RegisterKey, StepState::Pending));
        }
        steps.push(row(LoginStep::FindMux, StepState::Pending));
        let target = login.address.as_ref().map(|address| match login.port {
            Some(port) => format!("{address}:{port}"),
            None => address.clone(),
        });
        Self {
            attempt,
            steps,
            target,
            user: login.user.clone(),
            password,
            probe: None,
            machine_answered: false,
        }
    }

    pub fn state_of(&self, step: LoginStep) -> Option<StepState> {
        self.steps.iter().find(|r| r.step == step).map(|r| r.state)
    }

    /// True while a step is under way, which is what keeps the spinner turning.
    pub fn running(&self) -> bool {
        self.steps.iter().any(|r| r.state == StepState::Running)
    }

    /// True once every step finished and none failed or was skipped.
    pub fn succeeded(&self) -> bool {
        self.steps.iter().all(|r| r.state == StepState::Done)
    }

    fn open(&self, step: LoginStep) -> bool {
        matches!(
            self.state_of(step),
            Some(StepState::Pending | StepState::Running)
        )
    }

    /// Settles a step that has not settled yet. A settled step keeps its state, so an
    /// event repeating what an earlier one said changes nothing.
    fn settle(&mut self, step: LoginStep, state: StepState, note: Option<String>) {
        if let Some(row) = self.steps.iter_mut().find(|r| r.step == step) {
            if matches!(row.state, StepState::Pending | StepState::Running) {
                row.state = state;
                row.note = note;
            }
        }
    }

    /// Starts the next pending step once nothing is running.
    fn advance(&mut self) {
        if self.running() {
            return;
        }
        if let Some(row) = self
            .steps
            .iter_mut()
            .find(|r| r.state == StepState::Pending)
        {
            row.state = StepState::Running;
        }
    }

    fn skip_rest(&mut self) {
        for row in &mut self.steps {
            if matches!(row.state, StepState::Pending | StepState::Running) {
                row.state = StepState::Skipped;
            }
        }
    }

    pub fn apply(&mut self, event: &LoginEvent) {
        match event {
            LoginEvent::PasswordAsked => {
                if self.state_of(LoginStep::Connect) == Some(StepState::Running) {
                    self.settle(LoginStep::Connect, StepState::Done, None);
                    self.settle(LoginStep::Authenticate, StepState::Running, None);
                }
            }
            LoginEvent::Verdict(outcome) => self.verdict(outcome),
            LoginEvent::Saved(result) => {
                match result {
                    Ok(()) => self.settle(LoginStep::Save, StepState::Done, None),
                    Err(reason) => {
                        self.settle(LoginStep::Save, StepState::Failed, Some(reason.clone()))
                    }
                }
                self.advance();
            }
        }
    }

    /// Settles the two connection steps from the ssh child's verdict. A failure category
    /// names the step it belongs to; a failure with no step of its own (a timeout, a
    /// cancellation, anything else) fails the step that was running.
    fn verdict(&mut self, outcome: &crate::link::unlock::UnlockOutcome) {
        use crate::link::unlock::{FailureKind, UnlockOutcome};
        if !self.open(LoginStep::Connect) && !self.open(LoginStep::Authenticate) {
            return;
        }
        let failed = match outcome {
            UnlockOutcome::Ok => {
                self.settle(LoginStep::Connect, StepState::Done, None);
                self.settle(LoginStep::Authenticate, StepState::Done, None);
                self.advance();
                return;
            }
            UnlockOutcome::Unavailable => LoginStep::Connect,
            UnlockOutcome::Failed { kind, .. } => match kind {
                FailureKind::Unreachable
                | FailureKind::HostKeyMismatch
                | FailureKind::HostKeyUnverified => LoginStep::Connect,
                FailureKind::WrongPassword
                | FailureKind::AuthenticationRefused
                | FailureKind::ServerClosedAfterAuthentication => LoginStep::Authenticate,
                FailureKind::Timeout | FailureKind::Cancelled | FailureKind::Other => {
                    if self.state_of(LoginStep::Authenticate) == Some(StepState::Running) {
                        LoginStep::Authenticate
                    } else {
                        LoginStep::Connect
                    }
                }
            },
        };
        if failed == LoginStep::Authenticate {
            self.settle(LoginStep::Connect, StepState::Done, None);
        }
        self.settle(failed, StepState::Failed, None);
        self.skip_rest();
    }

    /// Settles everything the finished login answered, then starts the mux search a
    /// working login leads to.
    pub fn finish(&mut self, outcome: &crate::model::LoginOutcome) {
        use crate::model::RegistrationOutcome;
        self.verdict(&outcome.connect);
        if !outcome.connect.is_ok() {
            return;
        }
        if let Some(saved) = &outcome.saved {
            self.apply(&LoginEvent::Saved(saved.clone()));
        }
        match &outcome.registration {
            RegistrationOutcome::NotRequested => {}
            RegistrationOutcome::Registered => {
                self.settle(LoginStep::RegisterKey, StepState::Done, None)
            }
            RegistrationOutcome::Skipped(reason) => self.settle(
                LoginStep::RegisterKey,
                StepState::Skipped,
                Some(reason.clone()),
            ),
            RegistrationOutcome::Failed(reason) => self.settle(
                LoginStep::RegisterKey,
                StepState::Failed,
                Some(reason.clone()),
            ),
        }
        self.advance();
    }

    /// Records the machine probe the working login started, while its mux search runs.
    pub fn arm_probe(&mut self, probe: u64) {
        if self.state_of(LoginStep::FindMux) == Some(StepState::Running) {
            self.probe = Some(probe);
        }
    }

    /// Takes a machine probe's answer. Returns `false` when the probe is not the one
    /// this login started, which leaves every step as it was.
    pub fn probe_answered(&mut self, probe: u64, err: Option<&str>) -> bool {
        if self.probe != Some(probe) {
            return false;
        }
        match err {
            Some(reason) => self.settle(
                LoginStep::FindMux,
                StepState::Failed,
                Some(first_line(reason)),
            ),
            None => self.machine_answered = true,
        }
        true
    }

    /// True while the mux search waits for a mux answer it may take.
    pub fn awaiting_mux(&self) -> bool {
        self.machine_answered && self.state_of(LoginStep::FindMux) == Some(StepState::Running)
    }

    /// Settles the mux search from the first mux answer after the login's own probe.
    pub fn found_mux(&mut self, answer: &MuxAnswer) {
        if !self.awaiting_mux() {
            return;
        }
        let (state, note) = match answer {
            MuxAnswer::Found => (StepState::Done, None),
            MuxAnswer::NoMux => (StepState::Failed, Some("no mux answered".to_string())),
            MuxAnswer::Failed(reason) => (StepState::Failed, Some(first_line(reason))),
        };
        self.settle(LoginStep::FindMux, state, note);
    }
}

/// The first non-empty line of a reason, which names the failure; the whole text stays
/// on the host screen.
fn first_line(reason: &str) -> String {
    reason
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// A login pane input field a failure can be traced to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginField {
    Address,
    Port,
    Username,
    Password,
}

/// A failure as the login pane states it: the verdict in plain words, then ssh's own
/// text, kept apart so the verdict reads first and ssh's words stay whole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginFailure {
    pub kind: Option<crate::link::unlock::FailureKind>,
    pub verdict: String,
    pub raw: String,
}

impl LoginFailure {
    /// The failure a finished login reported, or `None` when it worked.
    pub fn of_login(outcome: &crate::model::LoginOutcome) -> Option<Self> {
        use crate::link::unlock::UnlockOutcome;
        match &outcome.connect {
            UnlockOutcome::Ok => None,
            UnlockOutcome::Unavailable => Some(Self {
                kind: None,
                verdict: outcome.connect.reason().unwrap_or_default().to_string(),
                raw: String::new(),
            }),
            UnlockOutcome::Failed { kind, reason } => {
                Some(Self::split(Some(*kind), reason, &outcome.output))
            }
        }
    }

    /// The failure a probe reported, categorized the way a login failure is. The probe's
    /// error is xmux's summary over ssh's text and the exit line xmux appends, so it is
    /// parted first: the category is read from ssh's text alone, a probe that handed ssh
    /// the held password keeps its refusal a refused password, and only ssh's text is
    /// shown as ssh's.
    pub fn of_probe(err: &str) -> Self {
        use crate::link::unlock::UnlockOutcome;
        let explained = crate::transport::diagnostic::split_explained(err.trim());
        let raw = crate::model::source::without_exit_line(explained.detail).trim();
        match crate::link::unlock::classify_probe(raw, explained.password_supplied) {
            UnlockOutcome::Failed { kind, reason } => {
                let mut failure = Self::split(Some(kind), &reason, raw);
                if let Some(summary) = explained.summary {
                    failure.verdict = summary.to_string();
                }
                failure
            }
            other => Self {
                kind: None,
                verdict: other.reason().unwrap_or_default().to_string(),
                raw: raw.to_string(),
            },
        }
    }

    /// A categorized reason is the verdict with ssh's text on the lines after it.
    fn split(kind: Option<crate::link::unlock::FailureKind>, reason: &str, raw: &str) -> Self {
        let verdict = if raw.trim().is_empty() {
            reason
        } else {
            reason
                .strip_suffix(raw)
                .map(|v| v.strip_suffix('\n').unwrap_or(v))
                .unwrap_or(reason)
        };
        Self {
            kind,
            verdict: verdict.trim_end().to_string(),
            raw: raw.trim().to_string(),
        }
    }

    /// The input fields this failure concerns. A failure no field of the pane could fix
    /// names none.
    pub fn fields(&self) -> Vec<LoginField> {
        use crate::link::unlock::FailureKind;
        let lower = self.raw.to_ascii_lowercase();
        match self.kind {
            Some(FailureKind::WrongPassword) => vec![LoginField::Password],
            Some(FailureKind::AuthenticationRefused) => {
                vec![LoginField::Username, LoginField::Password]
            }
            Some(FailureKind::Unreachable) if lower.contains("could not resolve hostname") => {
                vec![LoginField::Address]
            }
            Some(FailureKind::Unreachable) if lower.contains("connection refused") => {
                vec![LoginField::Port]
            }
            Some(FailureKind::Unreachable) => vec![LoginField::Address, LoginField::Port],
            Some(FailureKind::HostKeyMismatch | FailureKind::HostKeyUnverified) => {
                vec![LoginField::Address]
            }
            _ => Vec::new(),
        }
    }
}

pub(crate) const SECRET_INPUT_CAPACITY: usize = 16 * 1024;

#[derive(PartialEq, Eq)]
pub struct SecretInput(String);

impl Default for SecretInput {
    fn default() -> Self {
        Self(String::with_capacity(SECRET_INPUT_CAPACITY))
    }
}

impl Clone for SecretInput {
    fn clone(&self) -> Self {
        let mut value = String::with_capacity(SECRET_INPUT_CAPACITY);
        value.push_str(&self.0);
        Self(value)
    }
}

impl SecretInput {
    pub(crate) fn take_plain(&mut self) -> String {
        std::mem::take(&mut self.0)
    }
}

impl From<String> for SecretInput {
    fn from(mut value: String) -> Self {
        let mut secret = Self::default();
        for ch in value.chars() {
            if secret.0.len() + ch.len_utf8() > SECRET_INPUT_CAPACITY {
                break;
            }
            secret.0.push(ch);
        }
        crate::transport::auth::zero_string(&mut value);
        secret
    }
}

impl From<&str> for SecretInput {
    fn from(value: &str) -> Self {
        let mut secret = Self::default();
        for ch in value.chars() {
            if secret.0.len() + ch.len_utf8() > SECRET_INPUT_CAPACITY {
                break;
            }
            secret.0.push(ch);
        }
        secret
    }
}

impl PartialEq<&str> for SecretInput {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl std::ops::Deref for SecretInput {
    type Target = String;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for SecretInput {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl std::fmt::Debug for SecretInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[redacted]")
    }
}

impl Drop for SecretInput {
    fn drop(&mut self) {
        crate::transport::auth::zero_string(&mut self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link::unlock::{FailureKind, UnlockOutcome};
    use crate::model::{LoginOutcome, RegistrationOutcome};

    fn states(progress: &LoginProgress) -> Vec<(LoginStep, StepState)> {
        progress.steps.iter().map(|r| (r.step, r.state)).collect()
    }

    fn login() -> crate::transport::Login {
        crate::transport::Login {
            address: Some("10.0.4.12".into()),
            port: Some(2222),
            user: Some("alice".into()),
        }
    }

    fn ok_outcome(registration: RegistrationOutcome) -> LoginOutcome {
        LoginOutcome {
            connect: UnlockOutcome::Ok,
            output: String::new(),
            saved: Some(Ok(())),
            registration,
        }
    }

    #[test]
    fn each_reported_event_advances_exactly_the_step_it_names() {
        use LoginStep::*;
        use StepState::*;
        let mut p = LoginProgress::start(1, &login(), true, true, true);
        assert_eq!(p.target.as_deref(), Some("10.0.4.12:2222"));
        assert_eq!(
            states(&p),
            [
                (Connect, Running),
                (Authenticate, Pending),
                (Save, Pending),
                (RegisterKey, Pending),
                (FindMux, Pending),
            ]
        );
        p.apply(&LoginEvent::PasswordAsked);
        assert_eq!(states(&p)[..2], [(Connect, Done), (Authenticate, Running)]);
        p.apply(&LoginEvent::Verdict(UnlockOutcome::Ok));
        assert_eq!(states(&p)[1..3], [(Authenticate, Done), (Save, Running)]);
        p.apply(&LoginEvent::Saved(Ok(())));
        assert_eq!(states(&p)[2..4], [(Save, Done), (RegisterKey, Running)]);
        // The finished login repeats the verdict and the recording; neither moves a
        // settled step, and the registration it carries starts the mux search.
        p.finish(&ok_outcome(RegistrationOutcome::Registered));
        assert_eq!(states(&p)[3..], [(RegisterKey, Done), (FindMux, Running)]);
        assert!(p.running());
        p.arm_probe(7);
        assert!(p.probe_answered(7, None));
        p.found_mux(&MuxAnswer::Found);
        assert!(p.succeeded());
        assert!(!p.running());
    }

    #[test]
    fn a_key_login_settles_both_connection_steps_at_its_verdict() {
        use LoginStep::*;
        use StepState::*;
        let mut p =
            LoginProgress::start(1, &crate::transport::Login::default(), false, false, false);
        assert_eq!(p.target, None);
        p.apply(&LoginEvent::Verdict(UnlockOutcome::Ok));
        assert_eq!(
            states(&p),
            [(Connect, Done), (Authenticate, Done), (FindMux, Running)]
        );
    }

    #[test]
    fn a_failure_fails_its_own_step_and_skips_the_rest() {
        use LoginStep::*;
        use StepState::*;
        let failed = |kind| UnlockOutcome::Failed {
            kind,
            reason: String::new(),
        };
        let mut p = LoginProgress::start(1, &login(), true, false, true);
        p.apply(&LoginEvent::Verdict(failed(FailureKind::Unreachable)));
        assert_eq!(
            states(&p),
            [
                (Connect, Failed),
                (Authenticate, Skipped),
                (RegisterKey, Skipped),
                (FindMux, Skipped),
            ]
        );

        let mut p = LoginProgress::start(1, &login(), true, false, false);
        p.apply(&LoginEvent::Verdict(failed(FailureKind::WrongPassword)));
        assert_eq!(
            states(&p),
            [(Connect, Done), (Authenticate, Failed), (FindMux, Skipped)]
        );

        // A timeout has no step of its own: it fails whichever step was running.
        let mut p = LoginProgress::start(1, &login(), true, false, false);
        p.apply(&LoginEvent::PasswordAsked);
        p.apply(&LoginEvent::Verdict(failed(FailureKind::Timeout)));
        assert_eq!(states(&p)[1], (Authenticate, Failed));
        let mut p = LoginProgress::start(1, &login(), false, false, false);
        p.apply(&LoginEvent::Verdict(failed(FailureKind::Timeout)));
        assert_eq!(states(&p)[0], (Connect, Failed));
    }

    #[test]
    fn follow_ups_settle_with_their_own_outcome() {
        use LoginStep::*;
        use StepState::*;
        let mut p = LoginProgress::start(1, &login(), false, true, true);
        p.apply(&LoginEvent::Verdict(UnlockOutcome::Ok));
        p.apply(&LoginEvent::Saved(Err("permission denied".into())));
        p.finish(&LoginOutcome {
            saved: Some(Err("permission denied".into())),
            ..ok_outcome(RegistrationOutcome::Skipped("no shell".into()))
        });
        assert_eq!(states(&p)[2], (Save, Failed));
        assert_eq!(p.steps[2].note.as_deref(), Some("permission denied"));
        assert_eq!(states(&p)[3], (RegisterKey, Skipped));
        assert_eq!(p.steps[3].note.as_deref(), Some("no shell"));
        assert_eq!(states(&p)[4], (FindMux, Running));
        p.arm_probe(7);
        p.probe_answered(7, None);
        p.found_mux(&MuxAnswer::NoMux);
        assert_eq!(states(&p)[4], (FindMux, Failed));
        assert_eq!(p.steps[4].note.as_deref(), Some("no mux answered"));
        assert!(!p.succeeded());
    }

    #[test]
    fn a_probe_failure_is_read_from_ssh_text_and_keeps_a_refused_held_password() {
        // The probe error is xmux's summary over ssh's text and xmux's exit line.
        let err = crate::transport::diagnostic::explain(
            "alice@box: Permission denied (publickey,password).\ncommand exited with status 255",
            true,
        );
        let failure = LoginFailure::of_probe(&err);
        assert_eq!(failure.kind, Some(FailureKind::WrongPassword));
        assert_eq!(failure.verdict, "the password was refused");
        assert_eq!(
            failure.raw, "alice@box: Permission denied (publickey,password).",
            "only ssh's own text reads as ssh's"
        );
        assert_eq!(failure.fields(), [LoginField::Password]);

        let err = crate::transport::diagnostic::explain(
            "alice@box: Permission denied (publickey).\ncommand exited with status 255",
            false,
        );
        let failure = LoginFailure::of_probe(&err);
        assert_eq!(failure.kind, Some(FailureKind::AuthenticationRefused));
        assert_eq!(failure.raw, "alice@box: Permission denied (publickey).");
    }

    #[test]
    fn only_the_logins_own_probe_and_what_follows_it_settle_the_mux_search() {
        use LoginStep::*;
        use StepState::*;
        let mut p = LoginProgress::start(1, &login(), false, false, false);
        p.apply(&LoginEvent::Verdict(UnlockOutcome::Ok));
        assert_eq!(p.state_of(FindMux), Some(Running));
        // An answer before the login armed its probe, and an answer to another probe,
        // say nothing about what the login changed.
        p.found_mux(&MuxAnswer::Found);
        assert_eq!(p.state_of(FindMux), Some(Running));
        p.arm_probe(4);
        assert!(!p.probe_answered(3, None));
        p.found_mux(&MuxAnswer::Found);
        assert_eq!(p.state_of(FindMux), Some(Running));
        // The login's own probe failing settles the search with its reason.
        assert!(p.probe_answered(4, Some("the host could not be reached\nssh: connect")));
        assert_eq!(p.state_of(FindMux), Some(Failed));
        assert_eq!(
            p.steps[2].note.as_deref(),
            Some("the host could not be reached")
        );
    }

    #[test]
    fn a_login_failure_parts_its_verdict_from_ssh_text_and_names_its_field() {
        let raw =
            "Warning: Permanently added 'box'.\nalice@box: Permission denied (publickey,password).";
        let failure = LoginFailure::of_login(&LoginOutcome {
            connect: UnlockOutcome::Failed {
                kind: FailureKind::WrongPassword,
                reason: format!("the password was refused\n{raw}"),
            },
            output: raw.into(),
            saved: None,
            registration: RegistrationOutcome::NotRequested,
        })
        .unwrap();
        assert_eq!(failure.verdict, "the password was refused");
        assert_eq!(failure.raw, raw);
        assert_eq!(failure.fields(), [LoginField::Password]);

        let probe = LoginFailure::of_probe("ssh: Could not resolve hostname gpu-02: no such host");
        assert_eq!(probe.verdict, "the host name could not be resolved");
        assert_eq!(probe.fields(), [LoginField::Address]);
        let probe =
            LoginFailure::of_probe("ssh: connect to host box port 2222: Connection refused");
        assert_eq!(probe.fields(), [LoginField::Port]);
        let probe = LoginFailure::of_probe("alice@box: Permission denied (publickey).");
        assert_eq!(probe.verdict, "authentication was refused");
        assert_eq!(probe.fields(), [LoginField::Username, LoginField::Password]);
        assert_eq!(
            LoginFailure::of_login(&ok_outcome(RegistrationOutcome::NotRequested)),
            None
        );
    }

    #[test]
    fn secret_input_uses_one_bounded_allocation() {
        let secret = SecretInput::from("x".repeat(SECRET_INPUT_CAPACITY + 1));
        assert_eq!(secret.len(), SECRET_INPUT_CAPACITY);
        assert_eq!(secret.0.capacity(), SECRET_INPUT_CAPACITY);
    }
}
