//! psmux: one server per session (`ServerModel::PerSession`), enumerated from the
//! filesystem registry, polled for change, each session keeping its own attachment.

use super::*;

pub mod display;
mod registry;

pub use display::PsmuxDriver;

/// The local-psmux poll cadence (psmux is one-server-per-session with no event
/// push, so changes are discovered by re-enumeration). Mirrors the supervisor's
/// loop constant; held here so the supervisor reads it off the mux, not a literal.
const PSMUX_POLL_MS: u64 = 1500;

/// psmux: one server per session (`ServerModel::PerSession`), enumerated from the
/// filesystem registry, polled for change, each session keeping its own attachment.
pub struct Psmux {
    pub bin: String,
}

#[async_trait]
impl Mux for Psmux {
    fn kind(&self) -> &str {
        "psmux"
    }

    fn bin(&self) -> &str {
        &self.bin
    }

    fn server_model(&self) -> ServerModel {
        ServerModel::PerSession
    }

    fn driver(&self) -> Box<dyn crate::driver::MuxDriver> {
        Box::new(PsmuxDriver)
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
        // The local-registry merge is a LOCAL-psmux behavior: `~/.psmux` is THIS
        // machine's registry, with no remote awareness. A REMOTE psmux host has its
        // own registry on the far side, unreachable here, so it must enumerate the
        // generic way (list-sessions over ssh) — identical to a remote tmux. Folding
        // the local registry into a remote host would inject local session names as
        // phantoms and (worse) swallow an ssh failure into a fake empty/populated list.
        if transport.is_remote() {
            return crate::mux::enumerate_via_list_sessions(&self.bin, transport, runner).await;
        }
        // Local psmux: the registry (`~/.psmux/<name>.port`) is the authoritative
        // existence set; one list-sessions supplies display detail (empty on a
        // default-route miss).
        let names = registry::read_psmux_registry_dir(&registry::psmux_registry_dir());
        let (name, args) = transport.exec_argv(false, &mux::list_sessions(&self.bin));
        let detail = match runner.run(&name, &args).await {
            Ok(out) => mux::parse_sessions(transport.host_id(), &String::from_utf8_lossy(&out)),
            Err(_) => Vec::new(),
        };
        Ok(registry::merge_psmux_sessions(
            transport.host_id(),
            names,
            detail,
        ))
    }

    fn attach_plan(&self, session: &str) -> Vec<String> {
        // psmux is one-server-per-session: each session is its own server on its own
        // port (`~/.psmux/<name>.port`). A bare `attach -t <name>` on the DEFAULT
        // socket does not reach that session's server — it lands on a warm clone / the
        // default session — so selecting another session shows the wrong content.
        // `new-session -A -s <name>` (attach-if-exists, no `-d`) routes to the
        // session's OWN server and attaches the REAL session (verified: `-A -s
        // <existing>` finds it without creating a duplicate; cf. the `-CC` form in
        // 679bf3b). Window selection stays separate via `select_window_plan`.
        vec![
            self.bin.clone(),
            "new-session".to_string(),
            "-A".to_string(),
            "-s".to_string(),
            session.to_string(),
        ]
    }

    fn switch_in_place(
        &self,
        _host_key: &str,
        session: &str,
        display_tty: Option<&str>,
    ) -> Option<SwitchPlan> {
        // The psmux in-place switch moves xmux's own display client across per-session
        // servers on the default socket (`switch-client -c <tty> -t <session>`), then
        // forces a full repaint (`refresh-client -t <tty>`) so the new session fills the
        // grid. It runs ONLY with a non-empty captured tty; otherwise `None`, so the
        // driver reattaches instead (the 4a5f053 guard — no regression).
        display_tty.filter(|t| !t.is_empty()).map(|tty| {
            SwitchPlan::Exec(vec![
                vec![
                    self.bin.clone(),
                    "switch-client".to_string(),
                    "-c".to_string(),
                    tty.to_string(),
                    "-t".to_string(),
                    mux::quote_target(session),
                ],
                vec![
                    self.bin.clone(),
                    "refresh-client".to_string(),
                    "-t".to_string(),
                    tty.to_string(),
                ],
            ])
        })
    }

    fn control_argv(&self) -> Option<Vec<String>> {
        None
    }

    fn death_signal(&self) -> DeathSignal {
        DeathSignal::PathStat {
            dir_is_psmux_registry: true,
        }
    }

    fn event_source(&self) -> EventSource {
        EventSource::Poll {
            interval_ms: PSMUX_POLL_MS,
        }
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
        async fn run(&self, _name: &str, _args: &[String]) -> Result<Vec<u8>, RunError> {
            self.0
                .lock()
                .unwrap()
                .take()
                .unwrap_or_else(|| Ok(Vec::new()))
        }
    }

    fn psmux() -> Psmux {
        Psmux {
            bin: "psmux".into(),
        }
    }

    fn ssh(alias: &str) -> Box<dyn Transport> {
        crate::machine::ssh(alias.into(), String::new(), "linux".into())
    }

    #[test]
    fn psmux_switch_in_place_is_exec_plan_with_tty_and_none_without() {
        // With a captured tty the psmux in-place switch is an exec plan: switch-client to
        // move xmux's own display client across per-session servers, then refresh-client
        // to force a full repaint. Without a tty there is no plan (the driver reattaches —
        // the 4a5f053 guard), and an empty tty is treated the same.
        let m = psmux();
        let SwitchPlan::Exec(v) = m
            .switch_in_place("local", "target", Some("/dev/pts/3"))
            .expect("a captured tty yields an in-place exec plan")
        else {
            panic!("psmux switches via an exec plan, not a shell command");
        };
        assert_eq!(
            v[0].iter().map(String::as_str).collect::<Vec<_>>(),
            vec!["psmux", "switch-client", "-c", "/dev/pts/3", "-t", "target"]
        );
        assert_eq!(
            v[1].iter().map(String::as_str).collect::<Vec<_>>(),
            vec!["psmux", "refresh-client", "-t", "/dev/pts/3"]
        );
        assert!(m.switch_in_place("local", "target", None).is_none());
        assert!(m.switch_in_place("local", "target", Some("")).is_none());
    }

    #[tokio::test]
    async fn remote_psmux_enumerates_via_list_sessions_no_local_registry() {
        // A REMOTE psmux host must NOT read this machine's `~/.psmux` registry: its
        // sessions come solely from the list-sessions output, tagged with the remote
        // host id. The result is EXACTLY the parsed rows — no local registry name is
        // merged in as a phantom (the regression `for_binary("psmux")` would cause).
        let m = psmux();
        let runner = CannedRunner::ok("2\t1\t100\teditor\n1\t0\t0\tbuild\n");
        let got = m.enumerate(&ssh("prod"), &runner).await.unwrap();
        let names: Vec<&str> = got.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["editor", "build"],
            "exactly the list-sessions rows"
        );
        assert!(
            got.iter().all(|s| s.source == "prod"),
            "tagged with the remote host id, not local: {got:?}"
        );
    }

    #[tokio::test]
    async fn remote_psmux_ssh_failure_is_error_not_empty() {
        // An ssh-unreachable remote psmux host must surface as `Err`, exactly like a
        // remote tmux. The local-registry arm's `Err(_) => Vec::new()` would have
        // hidden the failure as a (fake) reachable host — the second half of the bug.
        let m = psmux();
        let runner = CannedRunner::err(RunError::Other(
            "ssh: connect to host prod port 22: Connection timed out".into(),
        ));
        assert!(m.enumerate(&ssh("prod"), &runner).await.is_err());
    }

    #[tokio::test]
    async fn remote_psmux_benign_no_server_is_empty_not_error() {
        // A reachable-but-empty remote mux ("no server running") is `Ok(vec![])`,
        // matching the generic path's `is_no_sessions` classification.
        let m = psmux();
        let runner = CannedRunner::err(RunError::Exit {
            stderr: "no server running on /tmp/psmux-1000/default".into(),
            code: 1,
        });
        assert!(m.enumerate(&ssh("prod"), &runner).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn local_psmux_swallows_error_into_registry_merge() {
        // The LOCAL arm keeps its one-server-per-session registry-merge behavior: a
        // list-sessions error is swallowed to empty DETAIL and merged with the
        // registry names, so it returns `Ok(...)` (the registry set, possibly empty) —
        // it never errors. This is the exact opposite of the remote arm above, which
        // pins that the Local-vs-Ssh dispatch is intact.
        let m = psmux();
        let runner = CannedRunner::err(RunError::Other("psmux: default route is dead".into()));
        let got = m.enumerate(&crate::machine::local(None), &runner).await;
        assert!(
            got.is_ok(),
            "local psmux swallows the error into the registry merge, never errors: {got:?}"
        );
    }
}
