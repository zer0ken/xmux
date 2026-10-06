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

/// zellij: one server per session, enumerated from `list-sessions`, polled for change,
/// each session displayed through its own attachment.
pub struct Zellij {
    pub bin: String,
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
        let command = transport.exec_argv(false, &argv);
        let mut sessions = match runner.run_spec(&command).await {
            Ok(out) => parse::parse_sessions(transport.host_id(), &String::from_utf8_lossy(&out)),
            Err(e) if crate::mux::is_no_sessions(&e) => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        // Zellij's session listing has no tab count. One extra non-attaching query
        // per live session is required; run them in order over the same transport.
        for session in &mut sessions {
            let argv = vec![
                self.bin.clone(),
                "--session".into(),
                session.name.clone(),
                "action".into(),
                "list-tabs".into(),
                "--json".into(),
            ];
            let out = runner.run_spec(&transport.exec_argv(false, &argv)).await?;
            session.windows = parse::tab_count(&out).map_err(|e| {
                RunError::Other(format!("zellij list-tabs for {:?}: {e}", session.name))
            })?;
        }
        Ok(sessions)
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
        EventSource::Poll
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
        crate::model::source::runner_spec_via_argv!();
        async fn run(&self, _name: &str, _args: &[String]) -> Result<Vec<u8>, RunError> {
            self.0
                .lock()
                .unwrap()
                .take()
                .unwrap_or_else(|| Ok(br#"[{"tab_id":0,"name":"Tab #1"}]"#.to_vec()))
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

    struct TabRunner {
        calls: Mutex<Vec<Vec<String>>>,
        tabs: Result<Vec<u8>, RunError>,
    }

    #[async_trait]
    impl Runner for TabRunner {
        crate::model::source::runner_spec_via_argv!();
        async fn run(&self, name: &str, args: &[String]) -> Result<Vec<u8>, RunError> {
            let mut call = vec![name.to_string()];
            call.extend_from_slice(args);
            self.calls.lock().unwrap().push(call);
            if args == ["list-sessions", "-n"] {
                Ok(b"my build [Created 1m ago] \ngone [Created 2m ago] (EXITED - attach to resurrect)\napi [Created 3m ago] \n".to_vec())
            } else {
                match &self.tabs {
                    Ok(out) => Ok(out.clone()),
                    Err(RunError::Exit { stderr, code }) => Err(RunError::Exit {
                        stderr: stderr.clone(),
                        code: *code,
                    }),
                    Err(RunError::Other(reason)) => Err(RunError::Other(reason.clone())),
                }
            }
        }
    }

    #[tokio::test]
    async fn enumeration_counts_tabs_without_attaching_and_skips_exited_records() {
        let runner = TabRunner {
            calls: Mutex::new(Vec::new()),
            tabs: Ok(
                br#"[{"tab_id":0,"name":"same\nname"},{"tab_id":2,"name":"same\nname"}]"#.to_vec(),
            ),
        };
        let got = zellij()
            .enumerate(&crate::transport::Local::default(), &runner)
            .await
            .unwrap();
        assert_eq!(
            got.iter().map(|s| s.windows).collect::<Vec<_>>(),
            vec![2, 2]
        );
        assert_eq!(
            *runner.calls.lock().unwrap(),
            vec![
                argv(&["zellij", "list-sessions", "-n"]),
                argv(&[
                    "zellij",
                    "--session",
                    "my build",
                    "action",
                    "list-tabs",
                    "--json"
                ]),
                argv(&[
                    "zellij",
                    "--session",
                    "api",
                    "action",
                    "list-tabs",
                    "--json"
                ]),
            ]
        );
    }

    #[tokio::test]
    async fn an_unanswered_tab_count_is_an_enumeration_error() {
        for tabs in [
            Err(RunError::Other("tab query timed out".into())),
            Err(RunError::Exit {
                stderr: "There is no active session!".into(),
                code: 1,
            }),
            Ok(b"not JSON".to_vec()),
            Ok(b"{}".to_vec()),
        ] {
            let runner = TabRunner {
                calls: Mutex::new(Vec::new()),
                tabs,
            };
            assert!(zellij()
                .enumerate(&crate::transport::Local::default(), &runner)
                .await
                .is_err());
            assert_eq!(
                runner.calls.lock().unwrap().len(),
                2,
                "stop after the failed query"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_hung_tab_query_is_bounded_by_the_poll_sweep() {
        struct HungTabs;
        #[async_trait]
        impl Runner for HungTabs {
            crate::model::source::runner_spec_via_argv!();
            async fn run(&self, _name: &str, args: &[String]) -> Result<Vec<u8>, RunError> {
                if args == ["list-sessions", "-n"] {
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    Ok(b"api [Created 1m ago] \n".to_vec())
                } else {
                    std::future::pending().await
                }
            }
        }
        let started = tokio::time::Instant::now();
        let mut events = Vec::new();
        zellij()
            .poll_once(
                "local",
                &crate::transport::Local::default(),
                &HungTabs,
                &mut |ev| events.push(ev),
            )
            .await;
        assert_eq!(started.elapsed(), POLL_SWEEP_BUDGET);
        assert!(
            matches!(&events[0], HostEvent::Sessions { sessions, err: Some(_) , .. } if sessions.is_empty())
        );
    }

    #[test]
    fn zellij_is_per_session_polled_and_dies_by_eof() {
        // The shape zellij shares with psmux: a server per session, no push channel, and
        // an attachment whose end IS the session's end.
        let m = zellij();
        assert_eq!(m.kind(), "zellij");
        assert_eq!(m.server_model(), ServerModel::PerSession);
        assert_eq!(m.death_signal(), DeathSignal::Eof);
        assert_eq!(m.event_source(), EventSource::Poll);
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
