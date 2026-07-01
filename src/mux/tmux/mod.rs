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

    async fn enumerate(
        &self,
        transport: &Transport,
        runner: &dyn Runner,
    ) -> Result<Vec<Session>, RunError> {
        crate::mux::enumerate_via_list_sessions(&self.bin, transport, runner).await
    }

    fn attach_plan(&self, session: &str, _window: Option<i64>) -> Vec<String> {
        mux::attach(&self.bin, session)
    }

    fn display_tty_record_prefix(&self, host_key: &str) -> Option<String> {
        // The attach shell writes its OWN controlling tty to the per-host file before
        // exec'ing the attach, so a later `switch-client -c <tty>` moves THIS client —
        // never the user's own attached client (which `list-clients` cannot tell apart).
        // A file (not the pty stream) survives the Windows ConPTY, which consumes an
        // in-band OSC marker before the pump reads it.
        Some(format!("tty >{} 2>/dev/null; ", display_tty_path(host_key)))
    }

    fn switch_via_recorded_tty_cmd(&self, host_key: &str, session: &str) -> Option<String> {
        // Read the tty THIS host's display attach recorded to its file, then move ONLY
        // that client — guarded on a non-empty value so a missing/empty file never runs
        // `switch-client -c ""` (which would move an arbitrary client). The follow-up
        // `refresh-client` forces the new session to repaint the whole screen.
        let path = display_tty_path(host_key);
        let b = &self.bin;
        let s = mux::quote_target(session);
        Some(format!(
            "c=$(cat {path} 2>/dev/null); [ -n \"$c\" ] && {{ {b} switch-client -c \"$c\" -t {s}; {b} refresh-client -t \"$c\"; }}"
        ))
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
    fn new_session_plan(&self, name: &str) -> Vec<String> {
        mux::new_session(&self.bin, name)
    }
    fn kill_session_plan(&self, name: &str) -> Vec<String> {
        mux::kill_session(&self.bin, name)
    }
    fn rename_session_plan(&self, old: &str, new: &str) -> Vec<String> {
        mux::rename_session(&self.bin, old, new)
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
                // (list-sessions + re-list every session's panes), so the sidebar's
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
                // and follows the sidebar cursor to it (#2). Dropping the payload here
                // would force the app to GUESS the displayed session, which mismatches
                // when a non-displayed session's active window changes.
                Some(HostEvent::ActiveWindowChanged {
                    host: host.to_string(),
                    session_id: session.to_string(),
                    window_id: window.to_string(),
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
    use crate::mux::psmux::Psmux;

    /// tmux's display client records its OWN tty to a per-host file before exec'ing the
    /// attach; a later switch READS that file so it targets THAT client — never the
    /// user's own attached client (the bug the `-CC` "first non-control client" heuristic
    /// caused). The record prefix and the switch command MUST name the SAME per-host file,
    /// and the switch must guard on a non-empty value (never `switch-client -c ""`).
    #[test]
    fn tmux_records_and_switches_its_own_display_tty_via_one_per_host_file() {
        let t = Tmux { bin: "tmux".into() };
        let prefix = t
            .display_tty_record_prefix("jup")
            .expect("a shared mux records its display tty to a file");
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

        let switch = t
            .switch_via_recorded_tty_cmd("jup", "test2")
            .expect("a shared mux switches via its recorded tty");
        assert!(
            switch.contains("cat ") && switch.contains("jup"),
            "the switch READS the same per-host file: {switch}"
        );
        assert!(
            switch.contains("switch-client -c") && switch.contains("test2"),
            "and moves that client to the session: {switch}"
        );
        assert!(
            switch.contains("[ -n"),
            "guarded so an empty file never runs switch-client -c \"\": {switch}"
        );
    }

    /// A host key with shell metacharacters cannot break out of the recorded path
    /// (the prefix is embedded in a remote shell command). The path stays a single
    /// safe filename token.
    #[test]
    fn record_prefix_sanitizes_the_host_key_into_a_safe_path() {
        let t = Tmux { bin: "tmux".into() };
        let prefix = t.display_tty_record_prefix("a; rm -rf /").unwrap();
        assert!(
            !prefix.contains("rm -rf /"),
            "the key is sanitized, not injected: {prefix}"
        );
    }

    /// psmux identifies its display client by the session it shows (one server per
    /// session), not by a recorded-tty file — so it uses no file-record strategy.
    #[test]
    fn psmux_does_not_use_the_file_record_strategy() {
        let p = Psmux {
            bin: "psmux".into(),
        };
        assert!(p.display_tty_record_prefix("local").is_none());
        assert!(p.switch_via_recorded_tty_cmd("local", "work").is_none());
    }
}
