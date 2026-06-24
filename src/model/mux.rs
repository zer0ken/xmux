//! One mux backend per mux. `Box<dyn Mux>` lives inside a `Host`. The method set is
//! exactly what the supervisor + control reader + manage layer call — no feature
//! catalogue, and NO session-level lifecycle (no end-to-end caller in this plan).
//! The backend owns its binary name and `ServerModel`, so nothing above threads a
//! `bin: &str` or branches on a `remote` bool to pick the model. Every method is
//! transport-blind except `enumerate` (which runs a probe).

use async_trait::async_trait;

use crate::model::plan::{DeathSignal, EventSource, SwitchPlan};
use crate::model::server_model::ServerModel;
use crate::model::transport::Transport;
use crate::mux;
use crate::session::Session;
use crate::source::{is_no_sessions, ExecRunner, RunError, Runner};

/// One mux backend. Methods are the EXACT set the supervisor + control reader +
/// manage layer call. `enumerate` takes `&Transport` because the per-session model
/// runs a probe (registry read + one list-sessions); the shared model runs one
/// command. Every other method is transport-blind: `switch_plan` returns intent,
/// the `Transport` lowers it.
#[async_trait]
pub trait Mux: Send + Sync {
    /// The binary name (`"tmux"` / `"psmux"`), for diagnostics + env-stripping.
    fn kind(&self) -> &str;

    /// Per-session vs shared. The supervisor reads this instead of `remote`.
    fn server_model(&self) -> ServerModel;

    /// Lists this host's sessions over `transport`. A reachable empty mux =>
    /// `Ok(vec![])`; unreachable => `Err`.
    async fn enumerate(&self, transport: &Transport) -> Result<Vec<Session>, RunError>;

    /// The interactive attach argv (`argv[0]` = binary). The window is selected
    /// separately via `select_window_plan`; the transport folds it for a remote
    /// attach when composing the final connection.
    fn attach_plan(&self, session: &str, window: Option<i64>) -> Vec<String>;

    /// TRANSPORT-BLIND intent: how (or whether) to move the host's ONE shared
    /// attachment to `session`. `Shared` => `Switch { session }`; `PerSession` =>
    /// `PerSessionNoOp`. Names NO transport — the `Transport` lowers it.
    fn switch_plan(&self, session: &str) -> SwitchPlan;

    /// The mux's own `switch-client` argv given the captured display tty + target.
    /// The supervisor closes over the host's display tty and hands this to
    /// `Transport::lower_switch`. The ONLY mux method that names the tty; it does NOT
    /// decide local-vs-ssh.
    fn switch_client_argv(&self, display_tty: &str, session: &str) -> Vec<String>;

    /// The control argv for a `-CC` metadata channel. `None` for a mux with no
    /// host-level control stream (it is polled).
    fn control_argv(&self) -> Option<Vec<String>>;

    /// How this host learns a session/attachment died.
    fn death_signal(&self) -> DeathSignal;

    /// The change/event channel for this mux.
    fn event_source(&self) -> EventSource;

    fn list_panes_plan(&self, session: &str) -> Vec<String>;
    fn new_window_plan(&self, session: &str, name: &str) -> Vec<String>;
    fn split_window_plan(&self, target: &str, vertical: bool) -> Vec<String>;
    fn select_window_plan(&self, target: &str) -> Vec<String>;
    fn kill_window_plan(&self, target: &str) -> Vec<String>;
    fn rename_window_plan(&self, target: &str, new: &str) -> Vec<String>;
}

/// Reports whether `err` means "reachable but no sessions" rather than
/// "unreachable" (the `is_no_sessions` rule). Shared by the backends' `enumerate`.
fn benign_empty(err: &RunError) -> bool {
    is_no_sessions(err)
}

/// The `-CC` control argv `[bin, -CC, attach]`, shared by the backends.
fn mux_control_argv(bin: &str) -> Vec<String> {
    vec![bin.to_string(), "-CC".to_string(), "attach".to_string()]
}

/// tmux: one aggregate server (`ServerModel::Shared`), a `-CC` control stream, and
/// a `switch-client` move of one host attachment.
pub struct Tmux;

#[async_trait]
impl Mux for Tmux {
    fn kind(&self) -> &str { "tmux" }

    fn server_model(&self) -> ServerModel { ServerModel::Shared }

    async fn enumerate(&self, transport: &Transport) -> Result<Vec<Session>, RunError> {
        let (name, args) = transport.exec_argv(false, &mux::list_sessions("tmux"));
        match ExecRunner.run(&name, &args).await {
            Ok(out) => Ok(mux::parse_sessions(transport.host_id(), &String::from_utf8_lossy(&out))),
            Err(e) if benign_empty(&e) => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    fn attach_plan(&self, session: &str, _window: Option<i64>) -> Vec<String> {
        mux::attach("tmux", session)
    }

    fn switch_plan(&self, session: &str) -> SwitchPlan {
        SwitchPlan::Switch { session: session.to_string() }
    }

    fn switch_client_argv(&self, display_tty: &str, session: &str) -> Vec<String> {
        vec![
            "tmux".to_string(),
            "switch-client".to_string(),
            "-c".to_string(),
            display_tty.to_string(),
            "-t".to_string(),
            mux::quote_target(session),
        ]
    }

    fn control_argv(&self) -> Option<Vec<String>> {
        Some(mux_control_argv("tmux"))
    }

    fn death_signal(&self) -> DeathSignal { DeathSignal::ControlNotice }

    fn event_source(&self) -> EventSource { EventSource::Control }

    fn list_panes_plan(&self, session: &str) -> Vec<String> { mux::list_panes("tmux", session) }
    fn new_window_plan(&self, session: &str, name: &str) -> Vec<String> { mux::new_window("tmux", session, name) }
    fn split_window_plan(&self, target: &str, vertical: bool) -> Vec<String> { mux::split_window("tmux", target, vertical) }
    fn select_window_plan(&self, target: &str) -> Vec<String> { mux::select_window("tmux", target) }
    fn kill_window_plan(&self, target: &str) -> Vec<String> { mux::kill_window("tmux", target) }
    fn rename_window_plan(&self, target: &str, new: &str) -> Vec<String> { mux::rename_window("tmux", target, new) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn tmux_is_shared_and_named() {
        let m = Tmux;
        assert_eq!(m.kind(), "tmux");
        assert_eq!(m.server_model(), ServerModel::Shared);
    }

    #[test]
    fn tmux_is_object_safe() {
        // The whole point: a Box<dyn Mux> must compile. If the trait gains a
        // non-dispatchable method this stops compiling.
        let _m: Box<dyn Mux> = Box::new(Tmux);
    }

    #[test]
    fn tmux_attach_plan_is_plain_attach() {
        assert_eq!(Tmux.attach_plan("api", None), argv(&["tmux", "attach", "-t", "api"]));
        // The window is selected separately (select_window_plan); attach stays plain.
        assert_eq!(Tmux.attach_plan("api", Some(2)), argv(&["tmux", "attach", "-t", "api"]));
    }

    #[test]
    fn tmux_switch_plan_is_transport_blind_intent() {
        // Shared => Switch{session}. It names NO transport (codex C2): the Transport
        // lowers it. A psmux-style NotShared is impossible to produce here.
        assert_eq!(Tmux.switch_plan("api"), SwitchPlan::Switch { session: "api".into() });
    }

    #[test]
    fn tmux_switch_client_argv_targets_the_captured_tty() {
        // The argv the supervisor closes over (with the captured DisplayTty) and
        // hands to Transport::lower_switch. It names the tty + session, never ssh.
        assert_eq!(
            Tmux.switch_client_argv("/dev/pts/3", "api"),
            argv(&["tmux", "switch-client", "-c", "/dev/pts/3", "-t", "api"])
        );
        // A session with control-mode metacharacters is quote_target-safe.
        assert_eq!(
            Tmux.switch_client_argv("/dev/pts/3", "my proj"),
            argv(&["tmux", "switch-client", "-c", "/dev/pts/3", "-t", "'my proj'"])
        );
    }

    #[test]
    fn tmux_control_attach_and_event_and_death() {
        assert_eq!(Tmux.control_argv(), Some(argv(&["tmux", "-CC", "attach"])));
        assert_eq!(Tmux.event_source(), EventSource::Control);
        assert_eq!(Tmux.death_signal(), DeathSignal::ControlNotice);
    }

    #[test]
    fn tmux_window_plans_match_mux_builders() {
        assert_eq!(Tmux.list_panes_plan("work"), mux::list_panes("tmux", "work"));
        assert_eq!(Tmux.select_window_plan("api:2"), mux::select_window("tmux", "api:2"));
        assert_eq!(Tmux.new_window_plan("work", "logs"), mux::new_window("tmux", "work", "logs"));
        assert_eq!(Tmux.split_window_plan("work:1", true), mux::split_window("tmux", "work:1", true));
        assert_eq!(Tmux.kill_window_plan("api:2"), mux::kill_window("tmux", "api:2"));
        assert_eq!(Tmux.rename_window_plan("api:2", "logs"), mux::rename_window("tmux", "api:2", "logs"));
    }

    // LIVE: enumerate over a real local tmux server. `#[ignore]` (needs tmux + a
    // server). Run on demand:
    //   cargo test --lib model::mux::tests::tmux_enumerate_live -- --ignored --nocapture
    #[ignore = "live: needs a running local tmux server"]
    #[tokio::test]
    async fn tmux_enumerate_live() {
        let t = Transport::Local { socket: None };
        let sessions = Tmux.enumerate(&t).await.expect("reachable tmux (empty is Ok)");
        eprintln!("local tmux sessions: {:?}", sessions.iter().map(|s| &s.name).collect::<Vec<_>>());
    }
}
