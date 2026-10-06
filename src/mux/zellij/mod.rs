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

/// Where an attach run through the machine's shell records its client's process id, keyed
/// per attachment so a query never reads the record of a client an earlier attach left.
/// Under `/tmp`, which every POSIX machine has and lets its user write.
fn pid_record_path(record_key: &str) -> String {
    let token: String = record_key
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("/tmp/.xmux-zc-{token}")
}

/// The shell text, run after `p` holds the client's process id, that prints the `ss -xn`
/// rows of the server ends connected to that client's sockets.
const CONNECTED_SERVER_END: &str = r#"[ -n "$p" ] || exit 0; i=$(ls -l /proc/"$p"/fd 2>/dev/null | sed -n 's/.*socket:\[\([0-9]*\)\].*/\1/p' | tr '\n' ' '); [ -n "$i" ] || exit 0; ss -xn 2>/dev/null | awk -v i="$i" 'BEGIN { n = split(i, a, " "); for (k = 1; k <= n; k++) w[a[k]] = 1 } $NF in w && $5 != "*"'"#;

/// The attach `attach` run so that the shell running it records its own process id at
/// the record for `record_key`, then becomes the client with `exec`, so the recorded id
/// is the client's. A record that cannot be written leaves the attach unaffected.
pub(super) fn recording_attach(attach: &[String], record_key: &str) -> Vec<String> {
    let attach: Vec<String> = attach
        .iter()
        .map(|arg| crate::transport::vocab::quote(arg))
        .collect();
    vec![
        "sh".to_string(),
        "-c".to_string(),
        format!(
            "{{ echo $$ >{}; }} 2>/dev/null; exec {}",
            pid_record_path(record_key),
            attach.join(" ")
        ),
    ]
}

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

    /// zellij 0.45 can give an attaching client the id its own session probe has just
    /// released, and the probe's late cleanup then removes the new client while the
    /// session stays up (zellij-org/zellij#5270, zellij-org/zellij#5546). The CLI runs
    /// the probe and the connection back to back in one process, so xmux cannot order
    /// its attach around it. zellij `main` allocates ids without reuse, but no release
    /// carries that yet.
    fn drops_fresh_client(&self) -> bool {
        true
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
        //
        // The listing already proved each session live, and the count only decorates
        // its card. zellij 0.45 loses a CLI client's reply when the server hands the
        // client an id a probe has just released (zellij-org/zellij#5270), so the
        // query can exit with no tab list while the session runs on. Such a session
        // keeps an unknown count, which shows no count, for this sweep: failing the
        // sweep instead would mark a live host unreachable and end its polling.
        for session in &mut sessions {
            let argv = vec![
                self.bin.clone(),
                "--session".into(),
                session.name.clone(),
                "action".into(),
                "list-tabs".into(),
                "--json".into(),
            ];
            let counted = match runner.run_spec(&transport.exec_argv(false, &argv)).await {
                Ok(out) => parse::tab_count(&out).map_err(|e| e.to_string()),
                Err(e) => Err(e.to_string()),
            };
            match counted {
                Ok(windows) => session.windows = windows,
                Err(error) => {
                    tracing::debug!(session = %session.name, error, "zellij_tab_count_unanswered")
                }
            }
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
    /// zellij itself reports the move nowhere: the session listing's current-session
    /// marker names the session the LISTING COMMAND ITSELF ran inside, and xmux polls
    /// from outside every session, so that marker is never present. Where the client's
    /// process memory cannot be read, [`display_client_query`](Mux::display_client_query)
    /// asks the kernel instead.
    fn display_session_env(&self) -> Option<&str> {
        Some("ZELLIJ_SESSION_NAME")
    }

    /// Asks the kernel, not zellij, which session server the client is connected to.
    /// zellij's own CLI has no answer: `list-clients` numbers clients per server with
    /// nothing that ties one to a process, and the client's rewritten variable lives in
    /// its process memory, which `/proc/<pid>/environ` does not show (that file holds the
    /// environment the process was started with).
    ///
    /// The client holds one connection to the server of the session it is on, and the
    /// server's end of it carries the session's socket path. The query lists the
    /// client's socket inodes from `/proc/<pid>/fd` and has `ss -xn` print the rows whose
    /// peer is one of them, which leaves that server end. It is one short shell run that
    /// reads two kernel tables and attaches to nothing. A machine without `/proc` or `ss`
    /// prints nothing, which is no signal.
    fn display_client_query(&self, client: &DisplayClient) -> Option<Vec<String>> {
        let pid = match client {
            DisplayClient::Pid(pid) => pid.to_string(),
            DisplayClient::Recorded(key) => {
                format!("$(cat {} 2>/dev/null)", pid_record_path(key))
            }
        };
        Some(vec![
            "sh".to_string(),
            "-c".to_string(),
            format!("p={pid}; {CONNECTED_SERVER_END}"),
        ])
    }

    fn parse_display_client(&self, out: &str) -> Option<String> {
        parse::connected_session(out)
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
        crate::model::host_def::runner_spec_via_argv!();
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
        crate::model::host_def::runner_spec_via_argv!();
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

    /// Every answer zellij 0.45 gives a tab query whose reply it lost: nothing with
    /// success, another command's text, a session it failed to find, a panic, a hang.
    #[tokio::test]
    async fn an_unanswered_tab_count_leaves_the_listed_session_without_a_count() {
        for tabs in [
            Ok(b"".to_vec()),
            Ok(b"not JSON".to_vec()),
            Ok(b"{}".to_vec()),
            Err(RunError::Exit {
                stderr: "Session 'api' not found. The following sessions are active:".into(),
                code: 1,
            }),
            Err(RunError::Exit {
                stderr: "thread 'main' panicked".into(),
                code: 101,
            }),
            Err(RunError::Other("tab query timed out".into())),
        ] {
            let runner = TabRunner {
                calls: Mutex::new(Vec::new()),
                tabs,
            };
            let got = zellij()
                .enumerate(&crate::transport::Local::default(), &runner)
                .await
                .expect("the listing answered, so the sweep answers");
            assert_eq!(
                got.iter()
                    .map(|s| (s.name.as_str(), s.windows))
                    .collect::<Vec<_>>(),
                vec![("my build", 0), ("api", 0)],
                "each live session stays listed with no count"
            );
            assert_eq!(
                runner.calls.lock().unwrap().len(),
                3,
                "every live session is still asked"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_hung_tab_query_is_bounded_by_the_poll_sweep() {
        struct HungTabs;
        #[async_trait]
        impl Runner for HungTabs {
            crate::model::host_def::runner_spec_via_argv!();
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

    /// The live client's own environment is where zellij records a `switch-session`:
    /// zellij pushes no notification, and its session listing marks only the session the
    /// listing itself ran inside, which xmux is never in. The variable belongs to the
    /// client PROCESS, so what it answers is xmux's own client.
    #[test]
    fn the_client_carries_the_session_it_is_on_in_its_own_environment() {
        assert_eq!(zellij().display_session_env(), Some("ZELLIJ_SESSION_NAME"));
    }

    /// A shell-run attach records its own process id, then `exec`s into the client, so
    /// the record names the client. The session name is quoted like any argument.
    #[test]
    fn a_recording_attach_records_its_pid_then_becomes_the_client() {
        assert_eq!(
            recording_attach(&argv(&["zellij", "attach", "my build"]), "jup-x;rm-1"),
            argv(&[
                "sh",
                "-c",
                "{ echo $$ >/tmp/.xmux-zc-jup-x_rm-1; } 2>/dev/null; exec zellij attach 'my build'"
            ])
        );
    }

    /// The query names the client the way the display path knows it: by the attach
    /// child's own pid on this machine, or by the record a shell-run attach wrote. Both
    /// read the same record path the recording attach writes.
    #[test]
    fn the_client_query_names_the_client_by_pid_or_by_its_record() {
        let by_pid = zellij()
            .display_client_query(&DisplayClient::Pid(4242))
            .unwrap();
        assert_eq!(&by_pid[..2], &argv(&["sh", "-c"])[..]);
        assert!(by_pid[2].starts_with("p=4242; "), "{by_pid:?}");
        let recorded = zellij()
            .display_client_query(&DisplayClient::Recorded("jup-x;rm-1".into()))
            .unwrap();
        assert!(
            recorded[2].starts_with("p=$(cat /tmp/.xmux-zc-jup-x_rm-1 2>/dev/null); "),
            "{recorded:?}"
        );
        assert!(by_pid[2].ends_with(CONNECTED_SERVER_END));
        assert_eq!(
            zellij()
                .parse_display_client(
                    "u_str ESTAB 0 0 /tmp/zellij-1000/contract_version_1/api 7 * 8
"
                )
                .as_deref(),
            Some("api")
        );
    }

    /// The query is POSIX shell run by `sh`. Where `sh` exists, it must parse, and a
    /// client that is not there answers nothing rather than failing.
    #[cfg(unix)]
    #[test]
    fn the_client_query_is_valid_shell_and_a_missing_client_answers_nothing() {
        let query = zellij()
            .display_client_query(&DisplayClient::Recorded("absent-record".into()))
            .unwrap();
        let out = std::process::Command::new(&query[0])
            .args(&query[1..])
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        assert!(out.stdout.is_empty(), "{out:?}");
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
        assert!(got.iter().all(|s| s.host == "jup" && s.mux == "zellij"));
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
