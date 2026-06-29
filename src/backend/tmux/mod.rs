//! tmux: one aggregate server (`ServerModel::Shared`), a `-CC` control stream, and
//! a `switch-client` move of one host attachment.

use super::*;

use crate::backend::ControlProtocol;
use crate::host::HostEvent;
use crate::mux::{quote_target, PANE_FORMAT, SESSION_FORMAT};

pub mod control_proto;

use control_proto::{classify, Line, Notif};

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

    async fn enumerate(
        &self,
        transport: &Transport,
        runner: &dyn Runner,
    ) -> Result<Vec<Session>, RunError> {
        let (name, args) = transport.exec_argv(false, &mux::list_sessions(&self.bin));
        match runner.run(&name, &args).await {
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

    fn control_protocol(&self) -> Option<&'static dyn ControlProtocol> {
        Some(&TMUX_CONTROL)
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

/// The shared `'static` tmux control protocol. Stateless (every method is pure over
/// its args), so one zero-sized instance serves every tmux host.
static TMUX_CONTROL: TmuxControl = TmuxControl;

/// The tmux `-CC` wire protocol: line classification, the notification→event policy
/// table, and the control-mode command-line builders. A unit struct because it holds
/// no state — `Tmux::control_protocol` hands out a shared `'static` reference.
pub struct TmuxControl;

impl ControlProtocol for TmuxControl {
    fn classify<'a>(&self, line: &'a str) -> Line<'a> {
        classify(line)
    }

    /// Maps one notification to the cockpit event it triggers (the metadata client
    /// holds no per-session display state, so notifications emit events, not mutate it).
    fn notif_event(
        &self,
        host: &str,
        notif: Notif<'_>,
        last_error: &Option<String>,
    ) -> Option<HostEvent> {
        match notif {
            Notif::SessionsChanged
            | Notif::WindowAdd { .. }
            | Notif::WindowClose { .. }
            | Notif::WindowRenamed { .. } => {
                // The server's session/window STRUCTURE changed; the cockpit refetches
                // (list-sessions + re-list every session's panes), so the sidebar's
                // session list AND per-session active-window markers resync (#5). The
                // notification carries only an id, so a blanket refetch is simplest.
                Some(HostEvent::Changed {
                    host: host.to_string(),
                })
            }
            Notif::SessionWindowChanged { .. } => {
                // A session's ACTIVE WINDOW switched (e.g. another client did prefix-n).
                // WindowChanged refetches the markers like Changed AND has the cockpit
                // probe the displayed session's new active window so the sidebar cursor
                // follows it (#2). The notification carries only ids ($session @window),
                // so the cursor target is resolved by the cockpit's probe, not here.
                Some(HostEvent::WindowChanged {
                    host: host.to_string(),
                })
            }
            // `%session-changed` (the metadata client's own auto-attached session) and
            // `%window-pane-changed` (a pane became active) do not affect the sidebar
            // tree — the per-session PTY attachments own the live pane — so they are inert.
            Notif::SessionChanged { .. } | Notif::WindowPaneChanged { .. } => None,
            Notif::Exit { reason } => {
                // `%exit` may carry its own reason; otherwise fall back to the last error
                // block ("no sessions" / "no server running") so an empty mux is not
                // mistaken for a dead host.
                Some(HostEvent::Exited {
                    host: host.to_string(),
                    reason: reason.map(str::to_string).or_else(|| last_error.clone()),
                })
            }
            Notif::ClientDetached { client } => Some(HostEvent::ClientDetached {
                host: host.to_string(),
                client: client.to_string(),
            }),
            // %pause/%continue are output flow-control; with `no-output` set there is no
            // output to pause, so they are inert for this metadata-only client.
            Notif::Pause { .. } | Notif::Continue { .. } => None,
            Notif::LayoutChange { .. } | Notif::Other => None,
        }
    }

    fn connect_lines(&self) -> Vec<String> {
        // SUPPRESS %output — this control connection is a metadata / change-event /
        // `select-window` channel ONLY; the per-session PTY attaches own the pixels, so
        // streaming pane output here is pure waste (and risks flooding the loop).
        // `no-output` keeps notifications flowing but stops %output. An older mux that
        // lacks the flag just %errors it (correlated as Ignore) — harmless.
        vec!["refresh-client -f no-output\n".to_string()]
    }

    fn list_sessions_line(&self) -> String {
        // SESSION_FORMAT contains TABs; single-quote it so tmux's line parser keeps it
        // as one arg (an unquoted tab would split the format).
        format!("list-sessions -F '{SESSION_FORMAT}'\n")
    }

    fn list_panes_line(&self, session: &str) -> String {
        // Quote the target so a session name with spaces/quotes survives the
        // control-mode command parser (it splits on whitespace).
        format!(
            "list-panes -s -t {} -F '{}'\n",
            quote_target(session),
            PANE_FORMAT
        )
    }

    fn active_window_line(&self, session: &str) -> String {
        // The format braces are escaped (so `#{window_index}` reaches tmux literally)
        // and a session name with spaces is quoted for the control-mode parser.
        format!(
            "display-message -p -t {} '#{{window_index}}'\n",
            quote_target(session)
        )
    }

    fn select_window_line(&self, target: &str) -> String {
        format!("select-window -t {}\n", quote_target(target))
    }

    fn switch_client_line(&self, display_tty: &str, session: &str) -> String {
        format!(
            "switch-client -c {} -t {}\n",
            display_tty,
            quote_target(session)
        )
    }

    fn size_line(&self, cols: u16, rows: u16) -> String {
        // `refresh-client -C WxH` — the `x`-form is correct for 3.3.x (`[research §7]`).
        format!("refresh-client -C {cols}x{rows}\n")
    }
}

#[cfg(test)]
mod control_tests {
    use super::*;

    #[test]
    fn size_line_uses_x_form() {
        assert_eq!(TmuxControl.size_line(80, 24), "refresh-client -C 80x24\n"); // x-form, NOT comma
    }
}
