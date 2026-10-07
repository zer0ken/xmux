//! herdr: each session has a persistent server, metadata is polled from one JSON
//! listing, and display changes use fresh client attachments.

use super::*;
use crate::model::host_def::RunError;
use crate::session::Session;
use crate::transport::Transport;
use serde::Deserialize;

mod client;
pub mod display;

pub use display::HerdrDriver;

/// herdr mux behavior and command plans.
pub struct Herdr {
    pub bin: String,
}

#[derive(Deserialize)]
struct SessionListing {
    sessions: Vec<ListedSession>,
}

#[derive(Deserialize)]
struct ListedSession {
    name: String,
    running: bool,
    /// herdr's reserved session, listed on every machine whether or not it was used.
    #[serde(default)]
    default: bool,
    #[serde(default)]
    connection_error: Option<String>,
}

#[async_trait]
impl Mux for Herdr {
    fn takes_server_socket(&self) -> bool {
        false
    }

    fn assigns_new_session_name(&self) -> bool {
        false
    }

    fn kind(&self) -> &str {
        "herdr"
    }

    fn bin(&self) -> &str {
        &self.bin
    }

    fn identity_probes(&self) -> Vec<Vec<String>> {
        vec![vec![self.bin.clone(), "--version".to_string()]]
    }

    fn classify_identity(&self, outputs: &[Option<String>]) -> Option<&'static str> {
        named_mux(outputs.first()?.as_deref()?)
    }

    fn server_model(&self) -> ServerModel {
        ServerModel::PerSession
    }

    /// herdr queries the terminal at startup and drops every key it reads until it draws.
    fn drops_input_before_first_frame(&self) -> bool {
        true
    }

    fn driver(&self) -> Box<dyn crate::driver::MuxDriver> {
        Box::new(HerdrDriver)
    }

    fn clone_box(&self) -> Box<dyn Mux> {
        Box::new(Self {
            bin: self.bin.clone(),
        })
    }

    fn list_sessions_plan(&self) -> Vec<String> {
        vec![
            self.bin.clone(),
            "session".to_string(),
            "list".to_string(),
            "--json".to_string(),
        ]
    }

    async fn enumerate(
        &self,
        transport: &dyn Transport,
        runner: &dyn Runner,
    ) -> Result<Vec<Session>, RunError> {
        let argv = self.list_sessions_plan();
        let command = transport.exec_argv(false, &argv);
        let out = runner.run_spec(&command).await?;
        parse_sessions(transport.host_id(), self.kind(), &out)
    }

    /// Connect-only client mode cannot start a missing session server.
    fn attach_plan(&self, session: &str) -> Vec<String> {
        vec![
            self.bin.clone(),
            "--session".to_string(),
            session.to_string(),
            "client".to_string(),
        ]
    }

    fn needs_attach_preparation(&self) -> bool {
        true
    }

    async fn prepare_attach(
        &self,
        transport: &dyn Transport,
        runner: &dyn Runner,
        session: &str,
    ) -> Result<(), RunError> {
        let sessions = self.enumerate(transport, runner).await?;
        let Some(target) = sessions.iter().find(|target| target.name == session) else {
            return Err(RunError::Other(format!(
                "herdr: session '{session}' no longer exists"
            )));
        };
        if target.stopped {
            self.start_session(transport, runner, session).await?;
        }
        Ok(())
    }

    /// The user's herdr processes, xmux's client pid, the host name, and herdr's saved
    /// machine listing; see the `client` module for why each is needed.
    fn display_client_query(&self, client: &DisplayClient) -> Option<Vec<String>> {
        Some(client::query(&self.bin, client))
    }

    fn parse_display_client(&self, out: &str) -> Option<ClientAt> {
        client::parse(out)
    }

    fn control_argv(&self) -> Option<Vec<String>> {
        None
    }

    /// Each session has its own persistent server, so an attachment ending is not a
    /// session ending and is never reported as one.
    fn death_signal(&self) -> DeathSignal {
        DeathSignal::None
    }

    fn event_source(&self) -> EventSource {
        EventSource::Poll
    }

    fn new_session_plan(&self, name: &str) -> Vec<String> {
        vec![
            self.bin.clone(),
            "--session".into(),
            name.into(),
            "server".into(),
        ]
    }

    async fn create_session(
        &self,
        transport: &dyn Transport,
        runner: &dyn Runner,
        name: &str,
    ) -> Result<Vec<u8>, RunError> {
        if self
            .enumerate(transport, runner)
            .await?
            .iter()
            .any(|session| session.name == name && !session.stopped)
        {
            return Ok(Vec::new());
        }
        self.start_session(transport, runner, name).await
    }
}

impl Herdr {
    async fn start_session(
        &self,
        transport: &dyn Transport,
        runner: &dyn Runner,
        name: &str,
    ) -> Result<Vec<u8>, RunError> {
        let deadline = tokio::time::Instant::now() + crate::mux::POLL_SWEEP_BUDGET;
        crate::model::host_def::within_deadline(deadline, async {
            let start =
                transport.detached_argv(&self.new_session_plan(name), Some("HERDR_STARTUP_CWD"));
            runner.run_spec(&start).await?;
            let status = transport.exec_argv(
                false,
                &[
                    self.bin.clone(),
                    "--session".into(),
                    name.into(),
                    "status".into(),
                    "server".into(),
                    "--json".into(),
                ],
            );
            loop {
                let out = runner.run_spec(&status).await?;
                #[derive(Deserialize)]
                struct ServerStatus {
                    running: bool,
                }
                let status: ServerStatus = serde_json::from_slice(&out).map_err(|error| {
                    RunError::Other(format!("invalid herdr server status: {error}"))
                })?;
                if status.running {
                    return Ok(Vec::new());
                }
                if tokio::time::Instant::now() + std::time::Duration::from_secs(1) >= deadline {
                    return Err(RunError::Other(format!(
                        "herdr: session '{name}' server did not become ready"
                    )));
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        })
        .await
    }
}

/// Parses `herdr session list --json`, the complete metadata answer. A running entry is a
/// live session and a stopped one a stopped session, which the attach starts again. An
/// entry with a connection error is not offered, since no server answers for it, and
/// neither is the stopped `default` entry: herdr lists it on every machine, so it does not
/// say a session was ever there. The listing reports no window count or attachment state,
/// so offered sessions keep the domain defaults for both rather than invented values.
fn parse_sessions(host: &str, mux: &str, out: &[u8]) -> Result<Vec<Session>, RunError> {
    let listing: SessionListing = serde_json::from_slice(out)
        .map_err(|e| RunError::Other(format!("invalid herdr session listing: {e}")))?;
    Ok(listing
        .sessions
        .into_iter()
        .filter(|session| {
            session.connection_error.is_none() && (session.running || !session.default)
        })
        .map(|session| Session {
            host: host.to_string(),
            name: session.name,
            mux: mux.to_string(),
            // The listing names a session only; herdr has no rename to tell apart.
            id: String::new(),
            windows: 0,
            clients: 0,
            stopped: !session.running,
        })
        .collect())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::Mutex;

    struct CannedRunner(Mutex<Option<Result<Vec<u8>, RunError>>>);

    impl CannedRunner {
        fn ok(out: &str) -> Self {
            Self(Mutex::new(Some(Ok(out.as_bytes().to_vec()))))
        }

        fn err(error: RunError) -> Self {
            Self(Mutex::new(Some(Err(error))))
        }
    }

    #[async_trait]
    impl Runner for CannedRunner {
        crate::model::host_def::runner_spec_via_argv!();
        async fn run(&self, _name: &str, _args: &[String]) -> Result<Vec<u8>, RunError> {
            self.0.lock().unwrap().take().unwrap()
        }
    }

    fn herdr() -> Herdr {
        Herdr {
            bin: "herdr".into(),
        }
    }

    pub(crate) fn attach_runner() -> std::sync::Arc<dyn Runner> {
        struct RunningSessions;
        #[async_trait]
        impl Runner for RunningSessions {
            crate::model::host_def::runner_spec_via_argv!();
            async fn run(&self, _name: &str, _args: &[String]) -> Result<Vec<u8>, RunError> {
                Ok(br#"{"sessions":[{"name":"a","running":true},{"name":"b","running":true},{"name":"target","running":true}]}"#.to_vec())
            }
        }
        std::sync::Arc::new(RunningSessions)
    }

    pub(crate) fn missing_attach_runner() -> std::sync::Arc<dyn Runner> {
        std::sync::Arc::new(CannedRunner::ok(r#"{"sessions":[]}"#))
    }

    struct TraceRunner {
        outputs: Mutex<std::collections::VecDeque<Result<Vec<u8>, RunError>>>,
        commands: Mutex<Vec<Vec<String>>>,
    }

    impl TraceRunner {
        fn new(outputs: &[&str]) -> Self {
            Self {
                outputs: Mutex::new(
                    outputs
                        .iter()
                        .map(|out| Ok(out.as_bytes().to_vec()))
                        .collect(),
                ),
                commands: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl Runner for TraceRunner {
        crate::model::host_def::runner_spec_via_argv!();
        async fn run(&self, name: &str, args: &[String]) -> Result<Vec<u8>, RunError> {
            self.commands.lock().unwrap().push(
                std::iter::once(name.to_string())
                    .chain(args.iter().cloned())
                    .collect(),
            );
            self.outputs
                .lock()
                .unwrap()
                .pop_front()
                .expect("only expected commands run")
        }
    }

    #[tokio::test]
    async fn missing_attach_runs_only_the_listing() {
        let runner = TraceRunner::new(&[r#"{"sessions":[]}"#]);
        let error = herdr()
            .prepare_attach(&crate::transport::local(None), &runner, "gone")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("no longer exists"));
        assert_eq!(
            *runner.commands.lock().unwrap(),
            vec![argv(&["herdr", "session", "list", "--json"])]
        );
    }

    #[tokio::test]
    async fn stopped_attach_starts_only_the_existing_saved_session_and_waits_for_readiness() {
        let runner = TraceRunner::new(&[
            r#"{"sessions":[{"name":"parked","running":false}]}"#,
            "",
            r#"{"running":false}"#,
            r#"{"running":true}"#,
        ]);
        herdr()
            .prepare_attach(&crate::transport::local(None), &runner, "parked")
            .await
            .unwrap();
        let commands = runner.commands.lock().unwrap();
        assert_eq!(commands.len(), 4);
        let launch = commands[1].last().unwrap();
        assert!(launch.contains("HERDR_STARTUP_CWD"));
        assert!(launch.contains("parked") && launch.contains("server"));
        assert_eq!(
            commands[2],
            argv(&["herdr", "--session", "parked", "status", "server", "--json"])
        );
        assert_eq!(commands[2], commands[3]);
    }

    #[tokio::test]
    async fn explicit_creation_completes_the_server_before_returning_its_name() {
        let runner = TraceRunner::new(&[r#"{"sessions":[]}"#, "", r#"{"running":true}"#]);
        let host = crate::model::Host::new(crate::transport::local(None), Box::new(herdr()));
        assert_eq!(
            crate::link::manage::create(&host, &runner, "new")
                .await
                .unwrap(),
            "new"
        );
        assert_eq!(runner.commands.lock().unwrap().len(), 3);
        let running = TraceRunner::new(&[r#"{"sessions":[{"name":"new","running":true}]}"#]);
        assert_eq!(
            crate::link::manage::create(&host, &running, "new")
                .await
                .unwrap(),
            "new"
        );
        assert_eq!(
            running.commands.lock().unwrap().len(),
            1,
            "an existing server is kept"
        );
    }

    #[tokio::test]
    async fn an_invalid_readiness_answer_is_a_creation_failure() {
        let runner = TraceRunner::new(&[r#"{"sessions":[]}"#, "", "not json"]);
        assert!(herdr()
            .create_session(&crate::transport::local(None), &runner, "new")
            .await
            .unwrap_err()
            .to_string()
            .contains("invalid herdr server status"));
    }

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| (*part).to_string()).collect()
    }

    fn ssh(alias: &str) -> Box<dyn Transport> {
        crate::transport::ssh(alias.into(), String::new(), "linux".into())
    }

    #[test]
    fn identity_model_and_flags_match_herdr() {
        let mux = herdr();
        assert_eq!(mux.kind(), "herdr");
        assert_eq!(mux.identity_probes(), vec![argv(&["herdr", "--version"])]);
        assert_eq!(
            mux.classify_identity(&[Some("herdr 0.9.2-preview.2026-09-29-8e78f929d8f0".into())]),
            Some("herdr")
        );
        assert_eq!(mux.server_model(), ServerModel::PerSession);
        assert_eq!(mux.death_signal(), DeathSignal::None);
        assert_eq!(mux.event_source(), EventSource::Poll);
        assert!(!mux.takes_server_socket());
        assert!(!mux.assigns_new_session_name());
        assert!(mux.control_argv().is_none());
        assert!(mux.switch_in_place("jup", "api", None).is_none());
        assert!(mux.display_session_env().is_none());
    }

    #[test]
    fn command_plans_match_herdr() {
        let mux = herdr();
        let listing = argv(&["herdr", "session", "list", "--json"]);
        assert_eq!(
            mux.attach_plan("api"),
            argv(&["herdr", "--session", "api", "client"])
        );
        assert_eq!(
            mux.new_session_plan("dev"),
            argv(&["herdr", "--session", "dev", "server"])
        );
        assert_eq!(mux.list_sessions_plan(), listing);
    }

    const LISTING: &str = r#"{
      "sessions": [
        {
          "default": false,
          "name": "live",
          "running": true,
          "session_dir": "/tmp/herdr/live",
          "socket_path": "/tmp/herdr/live.sock",
          "future_field": {"ignored": true}
        },
        {"default": true, "name": "default", "running": false},
        {"default": false, "name": "parked", "running": false, "session_dir": "/tmp/herdr/parked"},
        {"name": "denied", "running": false, "connection_error": "permission denied"},
        {"name": "inconsistent", "running": true, "connection_error": "unreachable"}
      ]
    }"#;

    #[tokio::test]
    async fn enumerate_offers_reachable_sessions_running_or_stopped() {
        let sessions = herdr()
            .enumerate(&ssh("jup"), &CannedRunner::ok(LISTING))
            .await
            .unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].host, "jup");
        assert_eq!(sessions[0].mux, "herdr");
        assert_eq!(sessions[0].name, "live");
        assert_eq!(sessions[0].windows, 0);
        assert_eq!(sessions[0].clients, 0);
        assert!(!sessions[0].stopped);
        assert_eq!(sessions[1].name, "parked");
        assert!(sessions[1].stopped, "a stopped entry is a stopped session");
    }

    /// herdr lists its reserved session on every machine, stopped until something starts
    /// it, so a stopped `default` says nothing about the user's sessions. A running one is
    /// a session like any other.
    #[tokio::test]
    async fn the_reserved_session_is_offered_only_while_it_runs() {
        let running = r#"{"sessions":[{"default":true,"name":"default","running":true}]}"#;
        let sessions = herdr()
            .enumerate(&ssh("jup"), &CannedRunner::ok(running))
            .await
            .unwrap();
        assert_eq!(sessions.len(), 1);
        assert!(!sessions[0].stopped);
    }

    #[tokio::test]
    async fn invalid_listing_and_command_failure_stay_errors() {
        assert!(herdr()
            .enumerate(&ssh("jup"), &CannedRunner::ok("not json"))
            .await
            .is_err());
        assert!(herdr()
            .enumerate(
                &ssh("jup"),
                &CannedRunner::err(RunError::Other("connection timed out".into()))
            )
            .await
            .is_err());
    }

    #[tokio::test]
    async fn poll_once_emits_one_sessions_event() {
        let mut events = Vec::new();
        herdr()
            .poll_once(
                "local",
                &crate::transport::local(None),
                &CannedRunner::ok(LISTING),
                &mut |event| events.push(event),
            )
            .await;
        assert_eq!(events.len(), 1);
        match &events[0] {
            HostEvent::Sessions { sessions, err, .. } => {
                assert_eq!(sessions.len(), 2);
                assert!(err.is_none());
            }
            _ => panic!("want Sessions"),
        }
    }
}
