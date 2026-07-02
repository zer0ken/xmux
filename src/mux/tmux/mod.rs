//! tmux: one aggregate server (`ServerModel::Shared`), a `-CC` control stream, and
//! a `switch-client` move of one host attachment.

use super::*;

use crate::host::HostEvent;
use crate::mux::ControlProtocol;
use crate::mux::{quote_target, PANE_FORMAT, SESSION_FORMAT};

pub mod control_proto;
pub mod display;

pub use display::TmuxDriver;

use control_proto::{classify, Line, Notif};

/// The `-CC` control argv `[bin, -CC, attach]`.
fn mux_control_argv(bin: &str) -> Vec<String> {
    vec![bin.to_string(), "-CC".to_string(), "attach".to_string()]
}

/// The per-host file where tmux's display client records its own tty: one file per
/// shared host so a switch reads back THIS client's tty and moves only it. Under
/// `/tmp` (present + writable on every POSIX host). `host_key` is sanitized to a safe
/// filename token so a host id with shell metacharacters cannot break out of the path
/// when the record prefix is embedded in a remote shell command.
fn display_tty_path(host_key: &str) -> String {
    let safe: String = host_key
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("/tmp/.xmux-cli-{safe}")
}

/// The shell prefix a shared attach prepends to its remote command so the attach shell
/// records its OWN controlling tty to the per-host file before exec'ing the attach — the
/// value `switch_in_place` reads back to move xmux's own display client, never the user's
/// (which `list-clients` cannot tell apart). Out-of-band (a file, not the pty stream) so
/// the Windows ConPTY cannot consume it. A family-private free fn (not a `Mux` method) so
/// the `tty >file` mechanism never leaks across the mux boundary.
pub(super) fn record_prefix(host_key: &str) -> String {
    format!("tty >{} 2>/dev/null; ", display_tty_path(host_key))
}

/// tmux: one aggregate server (`ServerModel::Shared`), a `-CC` control stream, and
/// a `switch-client` move of one host attachment.
pub struct Tmux {
    pub bin: String,
}

#[async_trait]
impl Mux for Tmux {
    fn kind(&self) -> &str {
        "tmux"
    }

    fn bin(&self) -> &str {
        &self.bin
    }

    fn server_model(&self) -> ServerModel {
        ServerModel::Shared
    }

    fn driver(&self) -> Box<dyn crate::driver::MuxDriver> {
        Box::new(TmuxDriver)
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
        crate::mux::enumerate_via_list_sessions(&self.bin, transport, runner).await
    }

    fn attach_plan(&self, session: &str) -> Vec<String> {
        mux::attach(&self.bin, session)
    }

    fn switch_in_place(
        &self,
        host_key: &str,
        session: &str,
        _display_tty: Option<&str>,
    ) -> Option<SwitchPlan> {
        // Read the tty THIS host's display attach recorded to its file, then move ONLY
        // that client — guarded on a non-empty value so a missing/empty file never runs
        // `switch-client -c ""` (which would move an arbitrary client). The follow-up
        // `refresh-client` forces the new session to repaint the whole screen. tmux reads
        // its recorded file, so `display_tty` is ignored; the switch is a raw shell command
        // (the driver runs it via the host shell, and a LOCAL tmux has none → reattach).
        let path = display_tty_path(host_key);
        let b = &self.bin;
        let s = mux::quote_target(session);
        Some(SwitchPlan::Shell(format!(
            "c=$(cat {path} 2>/dev/null); [ -n \"$c\" ] && {{ {b} switch-client -c \"$c\" -t {s}; {b} refresh-client -t \"$c\"; }}"
        )))
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

    /// Maps one notification to the app event it triggers (the metadata client
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
                // The server's session/window STRUCTURE changed; the app refetches
                // (list-sessions + re-list every session's panes), so the tree view's
                // session list AND per-session active-window markers resync (#5). The
                // notification carries only an id, so a blanket refetch is simplest.
                Some(HostEvent::Changed {
                    host: host.to_string(),
                })
            }
            Notif::SessionWindowChanged { session, window } => {
                // A session's ACTIVE WINDOW switched (e.g. another client did prefix-n).
                // Carry the notification's SESSION id ($session) + WINDOW id (@window)
                // through so the app probes THAT SPECIFIC session's new active window
                // and follows the tree selection to it (#2). Dropping the payload here
                // would force the app to GUESS the displayed session, which mismatches
                // when a non-displayed session's active window changes.
                Some(HostEvent::ActiveWindowChanged {
                    host: host.to_string(),
                    session_id: session.to_string(),
                    window_id: window.to_string(),
                })
            }
            // `%session-changed` (the metadata client's own auto-attached session) and
            // `%window-pane-changed` (a pane became active) do not affect the tree view
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

    fn active_window_line(&self, target: &str) -> String {
        // The format braces are escaped (so `#{session_name}`/`#{window_index}` reach
        // tmux literally) and a target with spaces is quoted for the control-mode parser.
        // Both fields come back so the reply resolves the SESSION NAME (the probe targets
        // a session id) alongside the active window index.
        format!(
            "display-message -p -t {} '#{{session_name}}\t#{{window_index}}'\n",
            quote_target(target)
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

    fn refresh_client_line(&self, display_tty: &str) -> String {
        format!("refresh-client -t {}\n", display_tty)
    }

    fn size_line(&self, cols: u16, rows: u16) -> String {
        // `refresh-client -C WxH` — the `x`-form is correct for 3.3.x (`[research §7]`).
        format!("refresh-client -C {cols}x{rows}\n")
    }

    fn display_clients_line(&self) -> String {
        "list-clients -F '#{client_tty} #{client_flags}'\n".to_string()
    }

    fn parse_display_client_tty(&self, body: &[String]) -> Option<String> {
        control_proto::parse_display_client_tty(body)
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

#[cfg(test)]
mod display_identity_tests {
    use super::*;

    /// tmux's in-place switch is an opaque `SwitchPlan::Shell`: a self-contained remote
    /// shell command that READS the tty the attach recorded to its per-host file, then
    /// moves ONLY that client to the session — guarded on a non-empty value so a
    /// missing/empty file never runs `switch-client -c ""`. (`display_tty` is ignored;
    /// tmux reads its own recorded file.)
    #[test]
    fn tmux_switch_in_place_returns_a_remote_shell_plan_reading_its_recorded_tty() {
        let SwitchPlan::Shell(cmd) = Tmux { bin: "tmux".into() }
            .switch_in_place("jup", "test2", None)
            .expect("a shared mux switches in place via its recorded tty")
        else {
            panic!("tmux switches through the host shell, not an exec plan");
        };
        assert!(
            cmd.contains("cat ") && cmd.contains("jup"),
            "the switch READS the same per-host file: {cmd}"
        );
        assert!(
            cmd.contains("switch-client -c") && cmd.contains("test2"),
            "and moves that client to the session: {cmd}"
        );
        assert!(
            cmd.contains("[ -n"),
            "guarded so an empty file never runs switch-client -c \"\": {cmd}"
        );
    }

    /// tmux's display client records its OWN tty to a per-host file before exec'ing the
    /// attach (`record_prefix`), so a later `switch_in_place` reads that file and targets
    /// THAT client — never the user's own attached client (the bug the `-CC` "first
    /// non-control client" heuristic caused). The prefix writes `$(tty)` to the per-host
    /// file and sanitizes the host key into a safe path token (it is embedded in a remote
    /// shell command, so a key with shell metacharacters must not break out of the path).
    #[test]
    fn record_prefix_records_the_per_host_tty_file_and_sanitizes_the_key() {
        let prefix = record_prefix("jup");
        assert!(
            prefix.contains("tty >"),
            "writes $(tty) to a file: {prefix}"
        );
        assert!(
            prefix.contains("jup"),
            "the file is keyed per host: {prefix}"
        );
        assert!(
            prefix.trim_end().ends_with(';'),
            "a prefix the attach argv appends `exec …` to: {prefix}"
        );
        let danger = record_prefix("a; rm -rf /");
        assert!(
            !danger.contains("rm -rf /"),
            "the key is sanitized, not injected: {danger}"
        );
    }
}
