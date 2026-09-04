//! abduco: one server per session, no control mode, no windows — every session is a
//! single PTY. Sessions are enumerated from `abduco`'s own listing and polled for
//! change; there is no per-session query, so each session resolves as the session
//! alone (one window, its own name).

use super::*;
use crate::model::source::RunError;
use crate::session::Session;
use crate::transport::Transport;

pub mod display;

pub use display::AbducoDriver;

/// The abduco poll cadence. abduco pushes no change events, so the session list is
/// re-enumerated; one sweep costs one `abduco` process spawn (no per-session query
/// exists), which is cheaper than zellij's per-session ssh round trips but not as
/// cheap as psmux's direct local registry read.
const ABDUCO_POLL_MS: u64 = 2000;

/// abduco: one server per session, enumerated from its listing, polled for change,
/// each session displayed through its own attachment.
pub struct Abduco {
    pub bin: String,
}

#[async_trait]
impl Mux for Abduco {
    /// abduco has no server-socket flag; sessions live under `~/.abduco`.
    fn takes_server_socket(&self) -> bool {
        false
    }

    /// `abduco -n` requires the name it is given and prints nothing back, so any
    /// stdout under the create is noise, never a name.
    fn assigns_new_session_name(&self) -> bool {
        false
    }

    fn kind(&self) -> &str {
        "abduco"
    }

    fn bin(&self) -> &str {
        &self.bin
    }

    /// abduco names itself in `-v` (lowercase) output and rejects `-V` outright; it
    /// has no help command. One probe, so detection never asks anything else.
    fn identity_probes(&self) -> Vec<Vec<String>> {
        vec![vec![self.bin.clone(), "-v".to_string()]]
    }

    fn classify_identity(&self, outputs: &[Option<String>]) -> Option<&'static str> {
        named_mux(outputs.first()?.as_deref()?)
    }

    fn server_model(&self) -> ServerModel {
        ServerModel::PerSession
    }

    fn driver(&self) -> Box<dyn crate::driver::MuxDriver> {
        Box::new(AbducoDriver)
    }

    fn clone_box(&self) -> Box<dyn Mux> {
        Box::new(Self {
            bin: self.bin.clone(),
        })
    }

    /// The bare binary with no arguments IS the listing.
    fn list_sessions_plan(&self) -> Vec<String> {
        vec![self.bin.clone()]
    }

    async fn enumerate(
        &self,
        transport: &dyn Transport,
        runner: &dyn Runner,
    ) -> Result<Vec<Session>, RunError> {
        let argv = self.list_sessions_plan();
        let (name, args) = transport.exec_argv(false, &argv);
        match runner.run(&name, &args).await {
            Ok(out) => Ok(parse_sessions(
                transport.host_id(),
                self.kind(),
                &String::from_utf8_lossy(&out),
            )),
            Err(e) if crate::mux::is_no_sessions(&e) => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    fn attach_plan(&self, session: &str) -> Vec<String> {
        vec![self.bin.clone(), "-a".to_string(), session.to_string()]
    }

    fn control_argv(&self) -> Option<Vec<String>> {
        // abduco has no control-mode channel: its CLI is one process per query.
        None
    }

    fn death_signal(&self) -> DeathSignal {
        // One server per session, so the attachment dying IS the session dying.
        DeathSignal::Eof
    }

    fn event_source(&self) -> EventSource {
        EventSource::Poll {
            interval_ms: ABDUCO_POLL_MS,
        }
    }

    fn new_session_plan(&self, name: &str) -> Vec<String> {
        // `-n` creates a session without attaching, running abduco's default command
        // (typically dvtm, the user's tool inside the session — out of xmux's scope).
        // abduco requires the name it is given (`assigns_new_session_name` is false, so
        // the manage layer names an empty request before building this plan).
        vec![self.bin.clone(), "-n".to_string(), name.to_string()]
    }
}

/// Parses `abduco`'s listing into sessions tagged with `source`/`mux`. Each line is
/// `<status> <Day>\t<YYYY-MM-DD HH:MM:SS>\t<pid>\t<name>`; the header and any banner
/// carry no tabs and are skipped. `attached` reads the leading status char (`*` = a
/// client attached; `+` = command terminated while unattached; ` ` = running,
/// unattached).
pub fn parse_sessions(source: &str, mux: &str, out: &str) -> Vec<Session> {
    let mut sessions = Vec::new();
    for ln in out.split('\n') {
        let ln = ln.strip_suffix('\r').unwrap_or(ln);
        let fields: Vec<&str> = ln.split('\t').collect();
        if fields.len() < 4 {
            continue;
        }
        let status = fields[0].chars().next().unwrap_or(' ');
        let name = fields[3..].join("\t");
        if name.is_empty() {
            continue;
        }
        sessions.push(Session {
            source: source.to_string(),
            name,
            mux: mux.to_string(),
            windows: 1,
            attached: status == '*',
        });
    }
    sessions
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Returns one canned listing result, ignoring the command.
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

    fn abduco() -> Abduco {
        Abduco {
            bin: "abduco".into(),
        }
    }

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    fn ssh(alias: &str) -> Box<dyn Transport> {
        crate::transport::ssh(alias.into(), String::new(), "linux".into())
    }

    #[test]
    fn abduco_is_per_session_polled_and_dies_by_eof() {
        // The shape abduco shares with psmux/zellij: a server per session, no push
        // channel, an attachment whose end IS the session's end, no server-socket flag.
        let m = abduco();
        assert_eq!(m.kind(), "abduco");
        assert_eq!(m.server_model(), ServerModel::PerSession);
        assert_eq!(m.death_signal(), DeathSignal::Eof);
        assert!(!m.takes_server_socket(), "abduco has no -S flag");
        assert_eq!(
            m.event_source(),
            EventSource::Poll {
                interval_ms: ABDUCO_POLL_MS
            }
        );
        assert!(
            m.control_argv().is_none() && m.control_protocol().is_none(),
            "abduco has no control-mode channel"
        );
        assert!(
            m.switch_in_place("jup", "api", Some("/dev/pts/3"))
                .is_none(),
            "no client can be named from outside its session, so no in-place switch"
        );
        let _object_safe: Box<dyn Mux> = Box::new(abduco());
    }

    #[test]
    fn attach_is_plain_attach() {
        // `-a` attaches the client's stdio to that session's own server.
        assert_eq!(abduco().attach_plan("api"), argv(&["abduco", "-a", "api"]));
    }

    #[test]
    fn creating_a_session_is_a_detached_create() {
        // `-n` creates without attaching; the name is always concrete by the time the
        // plan is built (the manage layer names an empty request), and the plan itself
        // stays a pure argv mapping.
        assert!(!abduco().assigns_new_session_name());
        assert_eq!(
            abduco().new_session_plan("dev"),
            argv(&["abduco", "-n", "dev"])
        );
        assert_eq!(abduco().new_session_plan(""), argv(&["abduco", "-n", ""]));
    }

    #[test]
    fn the_listed_plan_is_the_bare_binary() {
        // The plan exists to be SHOWN on the unreachable screen, so it must be the
        // real listing: `abduco` with no arguments IS the listing.
        assert_eq!(abduco().list_sessions_plan(), vec!["abduco"]);
    }

    #[tokio::test]
    async fn enumerate_reads_the_listing() {
        let m = abduco();
        let out = concat!(
            "Active sessions (on host localhost)\n",
            "* Tue\t2026-08-25 23:04:28\t168265\tsess1\n",
            "  Tue\t2026-08-25 23:03:45\t168097\tbuild\n",
        );
        let runner = CannedRunner::ok(out);
        let got = m.enumerate(&ssh("jup"), &runner).await.unwrap();
        let names: Vec<&str> = got.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["sess1", "build"]);
        assert!(got[0].attached, "the * marker means a client is attached");
        assert!(!got[1].attached, "the space marker means unattached");
        assert!(got.iter().all(|s| s.source == "jup" && s.mux == "abduco"));
        assert_eq!(got[0].windows, 1);
    }

    #[tokio::test]
    async fn enumerate_skips_banners_and_short_lines() {
        let m = abduco();
        let out = concat!(
            "some login banner without tabs\n",
            "\n",
            "  Tue\t2026-08-25 23:03:45\t168097\tbuild\n",
            "bad line\n",
        );
        let runner = CannedRunner::ok(out);
        let got = m.enumerate(&ssh("jup"), &runner).await.unwrap();
        let names: Vec<&str> = got.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["build"]);
    }

    #[tokio::test]
    async fn enumerate_rejoins_a_name_with_a_tab() {
        let m = abduco();
        let out = "  Tue\t2026-08-25 23:03:45\t168097\tproj\tname\n";
        let runner = CannedRunner::ok(out);
        let got = m.enumerate(&ssh("jup"), &runner).await.unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "proj\tname");
    }

    #[tokio::test]
    async fn an_idle_abduco_is_empty_and_an_unreachable_host_is_an_error() {
        // abduco's empty listing is just the header line and a success exit.
        let idle = CannedRunner::ok("Active sessions (on host localhost)\n");
        assert!(abduco()
            .enumerate(&ssh("jup"), &idle)
            .await
            .unwrap()
            .is_empty());
        let down = CannedRunner::err(RunError::Other(
            "ssh: connect to host jup port 22: Connection timed out".into(),
        ));
        assert!(abduco().enumerate(&ssh("jup"), &down).await.is_err());
        // A missing binary is never a healthy-but-idle mux.
        let missing = CannedRunner::err(RunError::Exit {
            stderr: "abduco: command not found".into(),
            code: 127,
        });
        assert!(abduco().enumerate(&ssh("jup"), &missing).await.is_err());
    }

    /// A poll sweep over a listing resolves each session's card directly: one
    /// `Sessions` event, then one empty `Panes` per session (the session alone) —
    /// abduco has no per-session query to run.
    #[tokio::test]
    async fn poll_once_resolves_each_session_without_a_per_session_query() {
        use crate::link::HostEvent;
        let m = abduco();
        let out = concat!(
            "Active sessions (on host localhost)\n",
            "* Tue\t2026-08-25 23:04:28\t168265\tsess1\n",
            "  Tue\t2026-08-25 23:03:45\t168097\tbuild\n",
        );
        let runner = CannedRunner::ok(out);
        let transport = crate::transport::local(None);
        let mut events: Vec<HostEvent> = Vec::new();
        m.poll_once("local", &transport, &runner, &mut |ev| events.push(ev))
            .await;
        assert_eq!(events.len(), 1, "one Sessions event");
        match &events[0] {
            HostEvent::Sessions { sessions, err, .. } => {
                assert_eq!(sessions.len(), 2);
                assert!(err.is_none());
            }
            _ => panic!("want Sessions first"),
        }
    }
}
