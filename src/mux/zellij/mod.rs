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
    fn kind(&self) -> &str {
        "zellij"
    }

    fn bin(&self) -> &str {
        &self.bin
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

    async fn enumerate(
        &self,
        transport: &dyn Transport,
        runner: &dyn Runner,
    ) -> Result<Vec<Session>, RunError> {
        // `-n` (no formatting) is the machine-readable listing: `-s` prints bare names
        // but drops the marker that separates a live session from a resurrectable
        // record, and the default output wraps every field in colour escapes.
        let argv = vec![
            self.bin.clone(),
            "list-sessions".to_string(),
            "-n".to_string(),
        ];
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

    fn list_panes_plan(&self, session: &str) -> Vec<String> {
        // A zellij TAB is what xmux calls a window, and the TAB listing is the query
        // that answers the window row: it reports each tab's position, name, whether a
        // client focuses it, and how many panes it holds. zellij's pane listing marks a
        // focused pane per tab and per layer, so it cannot name the one active tab.
        self.action(session, &["list-tabs", "-a", "-j"])
    }

    fn parse_panes(&self, out: &str) -> Vec<WindowPanes> {
        parse::parse_tabs(out)
    }

    fn window_label(&self, _index: i64, name: &str) -> String {
        // zellij's tab bar shows tab NAMES and nothing else - no index, no prefix - and
        // a tab it names itself is already called `Tab #1`, so the number a reader looks
        // for is inside the name. Prefixing tmux's `{index}:` would invent a second
        // number, and a zero-based one at that, while `Ctrl t` + a digit counts from one.
        name.to_string()
    }

    fn show_option_plan(&self, _name: &str) -> Vec<String> {
        // zellij has no server options: its appearance is configured in a KDL file the
        // server never reports. An empty plan tells the caller so without running
        // anything, and the view border falls back to xmux's own configuration.
        Vec::new()
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
        // without attaching this process to it. It prints nothing, so `manage::create`
        // keeps the requested name. Unlike tmux's `-A` it is not create-or-attach: a
        // name already in use fails, and zellij cannot auto-name a background session,
        // so an empty name fails too. Both surface as the mux's own message.
        vec![
            self.bin.clone(),
            "attach".to_string(),
            "-b".to_string(),
            name.to_string(),
        ]
    }
}

/// Wall-clock seconds since the epoch: the reference the reported session AGE is
/// subtracted from to reach a recency key on the same scale tmux reports. A clock
/// before the epoch reads as zero, which sorts the host's sessions last rather than
/// panicking.
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
        crate::machine::ssh(alias.into(), String::new(), "linux".into())
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
    fn the_window_query_is_the_tab_listing_and_it_reads_json() {
        let m = zellij();
        assert_eq!(
            m.list_panes_plan("api"),
            argv(&[
                "zellij",
                "--session",
                "api",
                "action",
                "list-tabs",
                "-a",
                "-j"
            ])
        );
        let out = r#"[{"position":0,"name":"shell","active":true,
            "selectable_tiled_panes_count":2,"selectable_floating_panes_count":0}]"#;
        let got = m.parse_panes(out);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "shell");
        assert!(got[0].active);
        assert_eq!(got[0].panes.len(), 2);
        assert!(
            crate::mux::parse_panes(out).is_empty(),
            "the tmux parser cannot read it, which is why the shape is a Mux decision"
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

    #[test]
    fn creating_a_session_is_a_silent_detached_attach() {
        assert_eq!(
            zellij().new_session_plan("dev"),
            argv(&["zellij", "attach", "-b", "dev"])
        );
    }

    #[test]
    fn there_are_no_server_options_to_read() {
        // An empty plan is the honest answer for a mux whose appearance lives in a KDL
        // file its server never reports; `manage::show_option` runs nothing for it.
        assert!(zellij().show_option_plan("pane-border-style").is_empty());
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
}
