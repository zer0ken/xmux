//! tuios: one daemon owns every session, metadata is polled from one JSON listing,
//! and display changes use fresh client attachments because no external command can
//! retarget a named client.

use super::*;
use crate::model::source::RunError;
use crate::session::Session;
use crate::transport::Transport;
use serde::Deserialize;

pub mod display;

pub use display::TuiosDriver;

/// tuios mux behavior and command plans.
pub struct Tuios {
    pub bin: String,
}

#[derive(Deserialize)]
struct ListedSession {
    name: String,
    #[serde(default)]
    window_count: i64,
    #[serde(default)]
    attached: bool,
    #[serde(default)]
    saved: bool,
}

#[async_trait]
impl Mux for Tuios {
    fn takes_server_socket(&self) -> bool {
        false
    }

    fn assigns_new_session_name(&self) -> bool {
        false
    }

    fn kind(&self) -> &str {
        "tuios"
    }

    fn bin(&self) -> &str {
        &self.bin
    }

    /// tuios identifies itself with `--version`, never `-V`.
    fn identity_probes(&self) -> Vec<Vec<String>> {
        vec![vec![self.bin.clone(), "--version".to_string()]]
    }

    fn classify_identity(&self, outputs: &[Option<String>]) -> Option<&'static str> {
        named_mux(outputs.first()?.as_deref()?)
    }

    fn server_model(&self) -> ServerModel {
        // Callers use this model to choose display behavior. tuios has one daemon, but
        // its clients cannot be named or retargeted externally, so every change needs
        // the same reattach behavior as a per-session mux.
        ServerModel::PerSession
    }

    fn driver(&self) -> Box<dyn crate::driver::MuxDriver> {
        Box::new(TuiosDriver)
    }

    fn clone_box(&self) -> Box<dyn Mux> {
        Box::new(Self {
            bin: self.bin.clone(),
        })
    }

    fn list_sessions_plan(&self) -> Vec<String> {
        vec![self.bin.clone(), "ls".to_string(), "--json".to_string()]
    }

    async fn enumerate(
        &self,
        transport: &dyn Transport,
        runner: &dyn Runner,
    ) -> Result<Vec<Session>, RunError> {
        let argv = self.list_sessions_plan();
        let command = transport.exec_argv(false, &argv);
        match runner.run_spec(&command).await {
            Ok(out) => parse_sessions(transport.host_id(), self.kind(), &out),
            // Exit 3 means no live daemon: a reachable host with no live session.
            Err(RunError::Exit { code: 3, .. }) => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    fn attach_plan(&self, session: &str) -> Vec<String> {
        vec![self.bin.clone(), "attach".to_string(), session.to_string()]
    }

    // No `display_session_env` override: the client does not rewrite `TUIOS_SESSION`
    // when it moves between sessions, so that variable is no display truth.

    fn control_argv(&self) -> Option<Vec<String>> {
        None
    }

    /// Ending an attachment does not end its daemon session, so a detach or a client
    /// quit has no session-death push; a later asked-for poll supplies the inventory.
    fn death_signal(&self) -> DeathSignal {
        DeathSignal::None
    }

    fn event_source(&self) -> EventSource {
        EventSource::Poll
    }

    fn new_session_plan(&self, name: &str) -> Vec<String> {
        vec![
            self.bin.clone(),
            "new".to_string(),
            name.to_string(),
            "--detach".to_string(),
        ]
    }
}

/// Parses `tuios ls --json`, the complete metadata answer: it carries each session's
/// window count and attachment state, so no per-session window query exists, and another
/// command would break the one-command poll. Saved records are not live sessions and are
/// never offered.
fn parse_sessions(source: &str, mux: &str, out: &[u8]) -> Result<Vec<Session>, RunError> {
    let listed: Vec<ListedSession> = serde_json::from_slice(out)
        .map_err(|e| RunError::Other(format!("invalid tuios session listing: {e}")))?;
    Ok(listed
        .into_iter()
        .filter(|session| !session.saved)
        .map(|session| Session {
            source: source.to_string(),
            name: session.name,
            mux: mux.to_string(),
            windows: session.window_count,
            attached: session.attached,
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

    fn tuios() -> Tuios {
        Tuios {
            bin: "tuios".into(),
        }
    }

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| (*part).to_string()).collect()
    }

    fn ssh(alias: &str) -> Box<dyn Transport> {
        crate::transport::ssh(alias.into(), String::new(), "linux".into())
    }

    #[test]
    fn identity_model_and_flags_match_tuios() {
        let mux = tuios();
        assert_eq!(mux.kind(), "tuios");
        assert_eq!(mux.identity_probes(), vec![argv(&["tuios", "--version"])]);
        assert_eq!(
            mux.classify_identity(&[Some("tuios version 0.8.4 [pure-go backend]".into())]),
            Some("tuios")
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
    fn command_plans_match_tuios() {
        let mux = tuios();
        assert_eq!(mux.attach_plan("api"), argv(&["tuios", "attach", "api"]));
        assert_eq!(
            mux.new_session_plan("dev"),
            argv(&["tuios", "new", "dev", "--detach"])
        );
        assert_eq!(mux.list_sessions_plan(), argv(&["tuios", "ls", "--json"]));
    }

    const LISTING: &str = r#"[
      {
        "name": "session-0",
        "id": "2569c353-385d-40e3-842a-3d58ded8a03e",
        "created": 1790912827,
        "last_active": 1790913212,
        "window_count": 1,
        "attached": true,
        "width": 150,
        "height": 118,
        "windows": [{"id":"w", "title":"some title", "workspace":1}],
        "current_workspace": 1,
        "dir": "~"
      },
      {"name":"saved", "window_count":2, "attached":false, "saved":true}
    ]"#;

    #[tokio::test]
    async fn enumerate_parses_live_json_and_ignores_saved_entries() {
        let sessions = tuios()
            .enumerate(&ssh("jup"), &CannedRunner::ok(LISTING))
            .await
            .unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].source, "jup");
        assert_eq!(sessions[0].mux, "tuios");
        assert_eq!(sessions[0].name, "session-0");
        assert_eq!(sessions[0].windows, 1);
        assert!(sessions[0].attached);
    }

    #[tokio::test]
    async fn daemon_down_exit_three_is_empty_for_both_messages() {
        for stderr in [
            "The TUIOS daemon is not running, and no sessions are saved on disk.",
            "",
        ] {
            let runner = CannedRunner::err(RunError::Exit {
                stderr: stderr.into(),
                code: 3,
            });
            assert!(tuios()
                .enumerate(&ssh("jup"), &runner)
                .await
                .unwrap()
                .is_empty());
        }
    }

    #[tokio::test]
    async fn missing_binary_and_transport_failure_stay_errors() {
        for error in [
            RunError::Exit {
                stderr: "tuios: command not found".into(),
                code: 127,
            },
            RunError::Other("connection timed out".into()),
        ] {
            assert!(tuios()
                .enumerate(&ssh("jup"), &CannedRunner::err(error))
                .await
                .is_err());
        }
    }

    #[tokio::test]
    async fn poll_once_emits_one_sessions_event() {
        let mut events = Vec::new();
        tuios()
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
