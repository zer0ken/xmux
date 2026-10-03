//! herdr: each session has a persistent server, metadata is polled from one JSON
//! listing, and display changes use fresh client attachments.

use super::*;
use crate::model::source::RunError;
use crate::session::Session;
use crate::transport::Transport;
use serde::Deserialize;

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

    fn attach_plan(&self, session: &str) -> Vec<String> {
        vec![
            self.bin.clone(),
            "session".to_string(),
            "attach".to_string(),
            session.to_string(),
        ]
    }

    fn control_argv(&self) -> Option<Vec<String>> {
        None
    }

    fn death_signal(&self) -> DeathSignal {
        DeathSignal::None
    }

    fn event_source(&self) -> EventSource {
        EventSource::Poll
    }

    fn new_session_plan(&self, _name: &str) -> Vec<String> {
        // ponytail: herdr has no detached create; this prompt health check is the
        // ceiling until it adds one, and the reselected session's first attach creates it.
        self.list_sessions_plan()
    }
}

fn parse_sessions(source: &str, mux: &str, out: &[u8]) -> Result<Vec<Session>, RunError> {
    let listing: SessionListing = serde_json::from_slice(out)
        .map_err(|e| RunError::Other(format!("invalid herdr session listing: {e}")))?;
    Ok(listing
        .sessions
        .into_iter()
        .filter(|session| session.running && session.connection_error.is_none())
        .map(|session| Session {
            source: source.to_string(),
            name: session.name,
            mux: mux.to_string(),
            windows: 0,
            attached: false,
        })
        .collect())
}

#[cfg(test)]
mod tests {
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
        crate::model::source::runner_spec_via_argv!();
        async fn run(&self, _name: &str, _args: &[String]) -> Result<Vec<u8>, RunError> {
            self.0.lock().unwrap().take().unwrap()
        }
    }

    fn herdr() -> Herdr {
        Herdr {
            bin: "herdr".into(),
        }
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
            argv(&["herdr", "session", "attach", "api"])
        );
        assert_eq!(mux.new_session_plan("dev"), listing);
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
        {"name": "denied", "running": false, "connection_error": "permission denied"},
        {"name": "inconsistent", "running": true, "connection_error": "unreachable"}
      ]
    }"#;

    #[tokio::test]
    async fn enumerate_offers_only_running_reachable_sessions() {
        let sessions = herdr()
            .enumerate(&ssh("jup"), &CannedRunner::ok(LISTING))
            .await
            .unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].source, "jup");
        assert_eq!(sessions[0].mux, "herdr");
        assert_eq!(sessions[0].name, "live");
        assert_eq!(sessions[0].windows, 0);
        assert!(!sessions[0].attached);
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
                assert_eq!(sessions.len(), 1);
                assert!(err.is_none());
            }
            _ => panic!("want Sessions"),
        }
    }
}
