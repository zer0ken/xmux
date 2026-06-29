//! psmux: one server per session (`ServerModel::PerSession`), enumerated from the
//! filesystem registry, polled for change, each session keeping its own attachment.

use super::*;

mod registry;

// Lift the psmux-registry helpers to the `backend` level (re-exported there) so the
// legacy `source::Source` path can reach them at a backend path, not the reverse.
pub(crate) use registry::{merge_psmux_sessions, psmux_registry_dir, read_psmux_registry_dir};

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
impl Backend for Psmux {
    fn kind(&self) -> &str {
        "psmux"
    }

    fn bin(&self) -> &str {
        &self.bin
    }

    fn server_model(&self) -> ServerModel {
        ServerModel::PerSession
    }

    fn select(&self) -> SelectOutcome {
        SelectOutcome::PerSessionReattach
    }

    async fn enumerate(&self, transport: &Transport) -> Result<Vec<Session>, RunError> {
        // The registry (`~/.psmux/<name>.port`) is the authoritative existence set;
        // one list-sessions supplies display detail (empty on a default-route miss).
        let names = registry::read_psmux_registry_dir(&registry::psmux_registry_dir());
        let (name, args) = transport.exec_argv(false, &mux::list_sessions(&self.bin));
        let detail = match ExecRunner.run(&name, &args).await {
            Ok(out) => mux::parse_sessions(transport.host_id(), &String::from_utf8_lossy(&out)),
            Err(_) => Vec::new(),
        };
        Ok(registry::merge_psmux_sessions(
            transport.host_id(),
            names,
            detail,
        ))
    }

    fn attach_plan(&self, session: &str, _window: Option<i64>) -> Vec<String> {
        mux::attach(&self.bin, session)
    }

    fn switch_plan(&self, _session: &str) -> SwitchPlan {
        SwitchPlan::PerSessionNoOp
    }

    fn switch_client_argv(&self, display_tty: &str, session: &str) -> Vec<String> {
        // PerSession never lowers a switch (switch_plan is PerSessionNoOp, so
        // lower_switch returns None before this is reached). The trait is total, so
        // the argv is defined for completeness and uses the psmux binary.
        vec![
            self.bin.clone(),
            "switch-client".to_string(),
            "-c".to_string(),
            display_tty.to_string(),
            "-t".to_string(),
            mux::quote_target(session),
        ]
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

    fn list_panes_plan(&self, session: &str) -> Vec<String> {
        mux::list_panes(&self.bin, session)
    }
    fn new_window_plan(&self, session: &str, name: &str) -> Vec<String> {
        mux::new_window(&self.bin, session, name)
    }
    fn split_window_plan(&self, target: &str, vertical: bool) -> Vec<String> {
        mux::split_window(&self.bin, target, vertical)
    }
    fn select_window_plan(&self, target: &str) -> Vec<String> {
        mux::select_window(&self.bin, target)
    }
    fn kill_window_plan(&self, target: &str) -> Vec<String> {
        mux::kill_window(&self.bin, target)
    }
    fn rename_window_plan(&self, target: &str, new: &str) -> Vec<String> {
        mux::rename_window(&self.bin, target, new)
    }
}
