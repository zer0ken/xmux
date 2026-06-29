//! tmux: one aggregate server (`ServerModel::Shared`), a `-CC` control stream, and
//! a `switch-client` move of one host attachment.

use super::*;

/// Reports whether `err` means "reachable but no sessions" rather than
/// "unreachable" (the `is_no_sessions` rule). Used by `Tmux::enumerate`.
fn benign_empty(err: &RunError) -> bool {
    is_no_sessions(err)
}

/// The `-CC` control argv `[bin, -CC, attach]`.
fn mux_control_argv(bin: &str) -> Vec<String> {
    vec![bin.to_string(), "-CC".to_string(), "attach".to_string()]
}

/// tmux: one aggregate server (`ServerModel::Shared`), a `-CC` control stream, and
/// a `switch-client` move of one host attachment.
pub struct Tmux {
    pub bin: String,
}

#[async_trait]
impl Backend for Tmux {
    fn kind(&self) -> &str {
        "tmux"
    }

    fn bin(&self) -> &str {
        &self.bin
    }

    fn server_model(&self) -> ServerModel {
        ServerModel::Shared
    }

    fn select(&self) -> SelectOutcome {
        SelectOutcome::SharedSwitch
    }

    async fn enumerate(&self, transport: &Transport) -> Result<Vec<Session>, RunError> {
        let (name, args) = transport.exec_argv(false, &mux::list_sessions(&self.bin));
        match ExecRunner.run(&name, &args).await {
            Ok(out) => Ok(mux::parse_sessions(
                transport.host_id(),
                &String::from_utf8_lossy(&out),
            )),
            Err(e) if benign_empty(&e) => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    fn attach_plan(&self, session: &str, _window: Option<i64>) -> Vec<String> {
        mux::attach(&self.bin, session)
    }

    fn switch_plan(&self, session: &str) -> SwitchPlan {
        SwitchPlan::Switch {
            session: session.to_string(),
        }
    }

    fn switch_client_argv(&self, display_tty: &str, session: &str) -> Vec<String> {
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
        Some(mux_control_argv(&self.bin))
    }

    fn death_signal(&self) -> DeathSignal {
        DeathSignal::ControlNotice
    }

    fn event_source(&self) -> EventSource {
        EventSource::Control
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
