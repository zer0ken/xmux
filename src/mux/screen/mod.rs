//! screen: one per-user daemon holding every session, no control-mode channel, every
//! query a separate `screen` process. There is no `switch-client`, so the display
//! reattaches on every session change (the zellij shape).

use super::*;

pub mod display;
mod vocab;

pub use display::ScreenDriver;

/// The screen poll cadence. screen pushes no change events, so the session list is
/// discovered by re-enumeration; one sweep costs one `-ls` plus one `-Q windows` per
/// session, each a separate process (over ssh, a separate connection), so the cadence
/// mirrors psmux's polled local read.
const SCREEN_POLL_MS: u64 = 1500;

/// screen: one per-user daemon, enumerated from `-ls`, polled for change, each session
/// displayed through its own attachment.
pub struct Screen {
    pub bin: String,
}

#[async_trait]
impl Mux for Screen {
    /// screen has no tmux-style `-S <path>` server-socket flag; its `-S` is a session
    /// NAME, so a server socket must never be handed to it.
    fn takes_server_socket(&self) -> bool {
        false
    }

    fn kind(&self) -> &str {
        "screen"
    }

    fn bin(&self) -> &str {
        &self.bin
    }

    fn server_model(&self) -> ServerModel {
        // One daemon holds every session, but there is no in-place `switch-client`, so
        // the display reattaches per session like a per-session mux.
        ServerModel::PerSession
    }

    fn driver(&self) -> Box<dyn crate::driver::MuxDriver> {
        Box::new(ScreenDriver)
    }

    fn clone_box(&self) -> Box<dyn Mux> {
        Box::new(Self {
            bin: self.bin.clone(),
        })
    }

    async fn enumerate(
        &self,
        transport: &dyn Transport,
        runner: &dyn Runner,
    ) -> Result<Vec<Session>, RunError> {
        let (name, args) = transport.exec_argv(false, &vocab::list_sessions(&self.bin));
        match runner.run(&name, &args).await {
            Ok(out) => Ok(vocab::parse_sessions(
                transport.host_id(),
                self.kind(),
                &String::from_utf8_lossy(&out),
            )),
            // screen exits 1 (stdout "No Sockets found") when it is reachable but empty —
            // the benign no-sessions case, distinct from a dead host.
            Err(RunError::Exit { code: 1, .. }) => Ok(Vec::new()),
            Err(e) if crate::mux::is_no_sessions(&e) => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    fn attach_plan(&self, session: &str) -> Vec<String> {
        vocab::attach(&self.bin, session)
    }

    fn control_argv(&self) -> Option<Vec<String>> {
        // screen has no control-mode channel; its CLI is one process per query.
        None
    }

    fn death_signal(&self) -> DeathSignal {
        // The display reattaches per session, so the attachment dying IS the shown
        // session dying.
        DeathSignal::Eof
    }

    fn event_source(&self) -> EventSource {
        EventSource::Poll {
            interval_ms: SCREEN_POLL_MS,
        }
    }

    fn list_panes_plan(&self, session: &str) -> Vec<String> {
        vocab::windows(&self.bin, session)
    }

    fn parse_panes(&self, out: &str) -> Vec<WindowPanes> {
        vocab::parse_windows(out)
    }

    fn select_window_plan(&self, target: &str) -> Vec<String> {
        // `select_window_plan` receives a `session:window` target; screen addresses a
        // session with `-S` and a window by `select <index>`.
        let (session, index) = match target.rsplit_once(':') {
            Some((s, i)) => (s, i.parse::<i64>().unwrap_or(0)),
            None => (target, 0),
        };
        vocab::select_window(&self.bin, session, index)
    }

    fn new_session_plan(&self, name: &str) -> Vec<String> {
        vocab::new_session(&self.bin, name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

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

    fn screen() -> Screen {
        Screen {
            bin: "screen".into(),
        }
    }
    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }
    fn ssh(alias: &str) -> Box<dyn Transport> {
        crate::transport::ssh(alias.into(), String::new(), "linux".into())
    }

    #[test]
    fn screen_is_polled_dies_by_eof_and_takes_no_socket() {
        let m = screen();
        assert_eq!(m.kind(), "screen");
        assert_eq!(m.server_model(), ServerModel::PerSession);
        assert_eq!(m.death_signal(), DeathSignal::Eof);
        assert_eq!(
            m.event_source(),
            EventSource::Poll {
                interval_ms: SCREEN_POLL_MS
            }
        );
        assert!(
            !m.takes_server_socket(),
            "screen's -S is a session name, not a socket"
        );
        assert!(m.control_argv().is_none() && m.control_protocol().is_none());
        assert!(m
            .switch_in_place("jup", "api", Some("/dev/pts/3"))
            .is_none());
        let _object_safe: Box<dyn Mux> = Box::new(screen());
    }

    #[test]
    fn attach_is_multi_display_dash_x() {
        assert_eq!(screen().attach_plan("api"), argv(&["screen", "-x", "api"]));
    }

    #[test]
    fn new_session_is_a_silent_detached_creation() {
        assert_eq!(
            screen().new_session_plan("dev"),
            argv(&["screen", "-dmS", "dev"])
        );
    }

    #[test]
    fn selecting_a_window_uses_s_select() {
        assert_eq!(
            screen().select_window_plan(&crate::mux::window_target("dev", 2)),
            argv(&["screen", "-S", "dev", "-X", "select", "2"])
        );
    }

    #[test]
    fn the_window_query_is_s_q_windows() {
        assert_eq!(
            screen().list_panes_plan("api"),
            argv(&["screen", "-S", "api", "-Q", "windows"])
        );
    }

    #[tokio::test]
    async fn enumerate_reads_the_ls_listing() {
        let m = screen();
        let out = "There are screens on:\n\t123.work\t(08/25/2026 10:00:00 PM)\t(Detached)\n\t124.dev\t(08/25/2026 10:00:01 PM)\t(Attached)\n";
        let runner = CannedRunner::ok(out);
        let got = m.enumerate(&ssh("jup"), &runner).await.unwrap();
        let names: Vec<&str> = got.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["work", "dev"]);
        assert!(got.iter().all(|s| s.source == "jup" && s.mux == "screen"));
    }

    #[tokio::test]
    async fn an_idle_screen_is_empty_and_an_unreachable_host_is_an_error() {
        // `screen -ls` exits 1 (stdout "No Sockets found") when reachable but empty.
        let idle = CannedRunner::err(RunError::Exit {
            stderr: String::new(),
            code: 1,
        });
        assert!(screen()
            .enumerate(&ssh("jup"), &idle)
            .await
            .unwrap()
            .is_empty());
        let down = CannedRunner::err(RunError::Other(
            "ssh: connect to host jup port 22: Connection timed out".into(),
        ));
        assert!(screen().enumerate(&ssh("jup"), &down).await.is_err());
        let missing = CannedRunner::err(RunError::Exit {
            stderr: "screen: command not found".into(),
            code: 127,
        });
        assert!(screen().enumerate(&ssh("jup"), &missing).await.is_err());
    }

    // LIVE: enumerate over a real remote screen server. `#[ignore]` (needs ssh jupiter00).
    //   cargo test --lib screen::tests::screen_enumerate_live -- --ignored --nocapture
    #[ignore = "live: needs ssh jupiter00 with screen"]
    #[tokio::test]
    async fn screen_enumerate_live() {
        use crate::model::source::ExecRunner;
        let ssh = crate::transport::ssh("jupiter00".into(), String::new(), "linux".into());
        let got = screen().enumerate(&ssh, &ExecRunner).await;
        eprintln!("jupiter00/screen sessions: {got:?}");
    }

    // LIVE: the `-v` detection probe identifies a real remote screen. `#[ignore]`.
    //   cargo test --lib screen::tests::screen_detect_live -- --ignored --nocapture
    #[ignore = "live: needs ssh jupiter00 with screen"]
    #[tokio::test]
    async fn screen_detect_live() {
        use crate::model::source::ExecRunner;
        let ssh = crate::transport::ssh("jupiter00".into(), String::new(), "linux".into());
        let got = crate::mux::detect_backend(&ssh, "screen", &ExecRunner).await;
        eprintln!(
            "jupiter00/screen detected -> {:?}",
            got.as_ref().map(|m| (m.kind(), m.server_model()))
        );
        assert_eq!(got.as_ref().map(|m| m.kind()), Some("screen"));
    }
}
