//! zellij: one server per session, no control mode, every query addressed to a
//! single session over zellij's own CLI.
//!
//! zellij shares no argv with tmux, so this impl overrides every command plan rather
//! than inheriting the tmux-compatible defaults. What it keeps from the shared model
//! is the SHAPE: a per-session server model (as psmux has), a poll event source (no
//! `-CC` stream exists), and death by attachment EOF.

use super::*;

pub mod display;
mod parse;

pub use display::ZellijDriver;

/// The zellij poll cadence. zellij pushes no change events, so the session list is
/// discovered by re-enumeration; one sweep costs one `list-sessions` plus one
/// `list-tabs` per session, and every one of them is a separate process (over ssh, a
/// separate connection), so the cadence is slower than psmux's local registry read.
const ZELLIJ_POLL_MS: u64 = 3000;

/// zellij: one server per session, enumerated from `list-sessions`, polled for change,
/// each session displayed through its own attachment.
pub struct Zellij {
    pub bin: String,
}

impl Zellij {
    /// The argv addressing one zellij `action` at `session` from OUTSIDE it. zellij's
    /// actions default to the session the caller is inside; `--session` names the
    /// target instead, which is how xmux (never inside one) reaches any of them.
    fn action(&self, session: &str, verb: &[&str]) -> Vec<String> {
        let mut v = vec![
            self.bin.clone(),
            "--session".to_string(),
            session.to_string(),
            "action".to_string(),
        ];
        v.extend(verb.iter().map(|s| s.to_string()));
        v
    }
}

#[async_trait]
impl Mux for Zellij {
    /// zellij has no server-socket flag and refuses an unexpected one before it reads
    /// the verb, so a socket must never reach it.
    fn takes_server_socket(&self) -> bool {
        false
    }

    /// zellij's detached create requires the name it is given and prints nothing back,
    /// so any stdout under the create is noise, never a name. (A name-less `attach -b`
    /// does auto-name, but only on a host with zero sessions, silently does nothing on
    /// a host with several, and never prints the name it picked - unusable as a create.)
    fn assigns_new_session_name(&self) -> bool {
        false
    }

    fn kind(&self) -> &str {
        "zellij"
    }

    fn bin(&self) -> &str {
        &self.bin
    }

    /// zellij names itself in its `help` banner (`Usage: zellij [OPTIONS]`), the same
    /// positive signal psmux gives. One probe.
    fn identity_probes(&self) -> Vec<Vec<String>> {
        vec![vec![self.bin.clone(), "help".to_string()]]
    }

    fn classify_identity(&self, outputs: &[Option<String>]) -> Option<&'static str> {
        named_mux(outputs.first()?.as_deref()?)
    }

    fn server_model(&self) -> ServerModel {
        ServerModel::PerSession
    }

    fn driver(&self) -> Box<dyn crate::driver::MuxDriver> {
        Box::new(ZellijDriver)
    }

    fn clone_box(&self) -> Box<dyn Mux> {
        Box::new(Self {
            bin: self.bin.clone(),
        })
    }

    /// `-n` (no formatting) is the machine-readable listing: `-s` prints bare names but
    /// drops the marker that separates a live session from a resurrectable record, and
    /// the default output wraps every field in colour escapes. zellij takes none of the
    /// tmux listing's format flags, so it names its own listing rather than inheriting
    /// the shared one.
    fn list_sessions_plan(&self) -> Vec<String> {
        vec![
            self.bin.clone(),
            "list-sessions".to_string(),
            "-n".to_string(),
        ]
    }

    async fn enumerate(
        &self,
        transport: &dyn Transport,
        runner: &dyn Runner,
    ) -> Result<Vec<Session>, RunError> {
        let argv = self.list_sessions_plan();
        let (name, args) = transport.exec_argv(false, &argv);
        match runner.run(&name, &args).await {
            Ok(out) => Ok(parse::parse_sessions(
                transport.host_id(),
                &String::from_utf8_lossy(&out),
                now_secs(),
            )),
            Err(e) if crate::mux::is_no_sessions(&e) => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    fn attach_plan(&self, session: &str) -> Vec<String> {
        // Plain `attach`, never `attach -c`: xmux displays sessions it enumerated and
        // must not create one as a side effect of showing it. A session that died
        // between the scan and the attach fails the attach, which is the EOF the death
        // signal is waiting for.
        vec![self.bin.clone(), "attach".to_string(), session.to_string()]
    }

    /// zellij moves its client between sessions INSIDE the client process:
    /// `switch-session` detaches the client that runs it from one session's server and
    /// the same process attaches to another, rewriting this variable each time it lands.
    /// The client sets it on its first attach too, so the value always names the session
    /// the client is on right now while its argv keeps naming the session it was started
    /// on. Because the variable belongs to the PROCESS, it names xmux's own display
    /// client and no other zellij client of the user's.
    ///
    /// It is the only source of truth there is. No server sees the move, so the poll cannot ask
    /// for it, and the session listing cannot answer it either: the listing's
    /// current-session marker names the session the LISTING COMMAND ITSELF ran inside,
    /// and xmux polls from outside every session, so that marker is never present.
    fn display_session_env(&self) -> Option<&str> {
        Some("ZELLIJ_SESSION_NAME")
    }

    fn control_argv(&self) -> Option<Vec<String>> {
        // zellij has no control-mode channel: its CLI is one process per query.
        None
    }

    fn death_signal(&self) -> DeathSignal {
        // One server per session, so the attachment dying IS the session dying.
        DeathSignal::Eof
    }

    fn event_source(&self) -> EventSource {
        EventSource::Poll {
            interval_ms: ZELLIJ_POLL_MS,
        }
    }

    fn select_window_plan(&self, target: &str) -> Vec<String> {
        // `go-to-tab` counts tabs from ONE while xmux (and zellij's own `position`)
        // count from zero, so the index is shifted here, at the one place that speaks
        // zellij's argv. The target is `<session>:<index>` from `mux::window_target`;
        // the split is on the LAST colon because zellij forbids only `/` in a session
        // name, so a colon inside one is legal.
        let (session, index) = match target.rsplit_once(':') {
            Some((s, i)) => (s, i.parse::<i64>().unwrap_or(0)),
            None => (target, 0),
        };
        let one_based = index.saturating_add(1).max(1).to_string();
        self.action(session, &["go-to-tab", &one_based])
    }

    fn new_session_plan(&self, name: &str) -> Vec<String> {
        // `attach -b` is zellij's create-detached: it starts the session's server
        // without attaching this process to it. It prints nothing and requires the name
        // it is given (`assigns_new_session_name` is false, so the manage layer names an
        // empty request before building this plan and never reads its stdout). Unlike
        // tmux's `-A` it is not create-or-attach: a name already in use fails, surfaced
        // as the mux's own message.
        vec![
            self.bin.clone(),
            "attach".to_string(),
            "-b".to_string(),
            name.to_string(),
        ]
    }
}

/// Wall-clock seconds since the epoch: the reference the reported session AGE is
/// subtracted from to reach a `last_attached` on the same scale tmux reports. A
/// clock before the epoch reads as zero rather than panicking.
fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Returns one canned `list-sessions` result, ignoring the command.
    struct CannedRunner(Mutex<Option<Result<Vec<u8>, RunError>>>);

    impl CannedRunner {
        fn ok(out: &str) -> Self {
            CannedRunner(Mutex::new(Some(Ok(out.as_bytes().to_vec()))))
        }
        fn err(e: RunError) -> Self {
            CannedRunner(Mutex::new(Some(Err(e))))
        }
    }

    #[async_trait]
    impl Runner for CannedRunner {
        async fn run(&self, _name: &str, _args: &[String]) -> Result<Vec<u8>, RunError> {
            self.0
                .lock()
                .unwrap()
                .take()
                .unwrap_or_else(|| Ok(Vec::new()))
        }
    }

    fn zellij() -> Zellij {
        Zellij {
            bin: "zellij".into(),
        }
    }

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    fn ssh(alias: &str) -> Box<dyn Transport> {
        crate::transport::ssh(alias.into(), String::new(), "linux".into())
    }

    #[test]
    fn zellij_is_per_session_polled_and_dies_by_eof() {
        // The shape zellij shares with psmux: a server per session, no push channel, and
        // an attachment whose end IS the session's end.
        let m = zellij();
        assert_eq!(m.kind(), "zellij");
        assert_eq!(m.server_model(), ServerModel::PerSession);
        assert_eq!(m.death_signal(), DeathSignal::Eof);
        assert_eq!(
            m.event_source(),
            EventSource::Poll {
                interval_ms: ZELLIJ_POLL_MS
            }
        );
        assert!(
            m.control_argv().is_none() && m.control_protocol().is_none(),
            "zellij has no control-mode channel"
        );
        assert!(
            m.switch_in_place("jup", "api", Some("/dev/pts/3"))
                .is_none(),
            "no client can be named from outside its session, so no in-place switch"
        );
        let _object_safe: Box<dyn Mux> = Box::new(zellij());
    }

    #[test]
    fn attach_is_plain_so_showing_a_session_never_creates_one() {
        // `attach -c` would resurrect a session that died between the scan and the
        // attach; xmux displays what it enumerated and lets the failure be the EOF.
        assert_eq!(
            zellij().attach_plan("api"),
            argv(&["zellij", "attach", "api"])
        );
    }

    #[test]
    fn selecting_a_window_shifts_to_zellij_one_based_tabs() {
        // `go-to-tab` counts from one; xmux and zellij's own `position` count from zero.
        let m = zellij();
        assert_eq!(
            m.select_window_plan(&crate::mux::window_target("api", 0)),
            argv(&["zellij", "--session", "api", "action", "go-to-tab", "1"])
        );
        assert_eq!(
            m.select_window_plan(&crate::mux::window_target("api", 2)),
            argv(&["zellij", "--session", "api", "action", "go-to-tab", "3"])
        );
        // zellij forbids only `/` in a session name, so a colon inside one is legal and
        // the session/index split has to be on the LAST colon.
        assert_eq!(
            m.select_window_plan(&crate::mux::window_target("a:b", 1)),
            argv(&["zellij", "--session", "a:b", "action", "go-to-tab", "2"])
        );
    }

    /// The live client's own environment is where a `switch-session` can be seen, and
    /// the only place: zellij pushes no notification, and its session listing marks only
    /// the session the listing itself ran inside, which xmux is never in. The variable
    /// belongs to the client PROCESS, so what it answers is xmux's own client.
    #[test]
    fn the_client_carries_the_session_it_is_on_in_its_own_environment() {
        assert_eq!(zellij().display_session_env(), Some("ZELLIJ_SESSION_NAME"));
    }

    #[test]
    fn creating_a_session_is_a_silent_detached_attach() {
        assert_eq!(
            zellij().new_session_plan("dev"),
            argv(&["zellij", "attach", "-b", "dev"])
        );
        assert!(
            !zellij().assigns_new_session_name(),
            "the create prints nothing, so stdout is never a name and an empty              request is named by the manage layer"
        );
    }

    #[tokio::test]
    async fn enumerate_reads_the_unformatted_listing() {
        let m = zellij();
        let runner = CannedRunner::ok("api [Created 5m ago] \nbuild [Created 1h ago] \n");
        let got = m.enumerate(&ssh("jup"), &runner).await.unwrap();
        let names: Vec<&str> = got.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["api", "build"]);
        assert!(got.iter().all(|s| s.source == "jup" && s.mux == "zellij"));
        assert!(
            got[0].last_attached > got[1].last_attached,
            "the newer session sorts ahead of the older one"
        );
    }

    #[tokio::test]
    async fn an_idle_zellij_is_empty_and_an_unreachable_host_is_an_error() {
        // zellij reports "no sessions" with a plain non-zero exit, so the exit code
        // alone cannot tell an idle mux from a dead host: the message decides.
        let idle = CannedRunner::err(RunError::Exit {
            stderr: "No active zellij sessions found.".into(),
            code: 1,
        });
        assert!(zellij()
            .enumerate(&ssh("jup"), &idle)
            .await
            .unwrap()
            .is_empty());
        let down = CannedRunner::err(RunError::Other(
            "ssh: connect to host jup port 22: Connection timed out".into(),
        ));
        assert!(zellij().enumerate(&ssh("jup"), &down).await.is_err());
        // A missing binary is never a healthy-but-idle mux.
        let missing = CannedRunner::err(RunError::Exit {
            stderr: "zellij: command not found".into(),
            code: 127,
        });
        assert!(zellij().enumerate(&ssh("jup"), &missing).await.is_err());
    }
    #[test]
    fn the_listed_plan_is_the_argv_enumerate_issues() {
        // The plan exists to be SHOWN on the unreachable screen, so it must be the real
        // listing: zellij takes none of the tmux format flags, and a screen stating one
        // would name a command zellij never ran.
        assert_eq!(
            zellij().list_sessions_plan(),
            vec!["zellij", "list-sessions", "-n"]
        );
    }
}
