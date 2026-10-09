//! tmux: one aggregate server (`ServerModel::Shared`), a `-CC` control stream, and
//! a `switch-client` move of one host attachment.

use super::*;

use crate::link::HostEvent;
use crate::mux::{quote_target, SESSION_FORMAT};
use crate::mux::{ControlProtocol, DisplayTtyRead};
use std::sync::OnceLock;

pub mod control_proto;
pub mod display;

pub use display::TmuxDriver;

use control_proto::{classify, Line, Notif};

/// The `-CC` control argv `[bin, -u, -CC, attach]`.
///
/// `-u` tells tmux that its client reads UTF-8, which xmux does. tmux otherwise decides
/// from the remote shell's locale, and a shell without a UTF-8 locale (Windows OpenSSH
/// forwards none) makes tmux print every non-printable byte of a reply as `_`: the
/// recorded display tty then reads `/dev/pts/3_`, and no `switch-client` can name it.
fn mux_control_argv(bin: &str) -> Vec<String> {
    vec![
        bin.to_string(),
        "-u".to_string(),
        "-CC".to_string(),
        "attach".to_string(),
    ]
}

/// `host_key` as a safe filename and buffer-name token, so a host id with shell
/// metacharacters cannot break out of the path when the record prefix is embedded in a
/// remote shell command, nor out of a control-mode command line.
fn display_tty_token(host_key: &str) -> String {
    host_key
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Stable for this process and different for a later process, including one that reuses
/// the same public instance name. The timestamp and process id together distinguish
/// sequential runs, while the attachment id distinguishes records within this run.
fn display_tty_run_token() -> &'static str {
    static TOKEN: OnceLock<String> = OnceLock::new();
    TOKEN.get_or_init(|| {
        let started = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        format!("{}-{started}", std::process::id())
    })
}

fn display_tty_key_for_run(
    host_key: &str,
    instance_name: &str,
    run_token: &str,
    attach_id: u64,
) -> String {
    format!("{host_key}-{instance_name}-{run_token}-{attach_id}")
}

pub(crate) fn display_tty_key(host_key: &str, instance_name: &str, attach_id: u64) -> String {
    display_tty_key_for_run(host_key, instance_name, display_tty_run_token(), attach_id)
}

/// The per-host file where tmux's display client records its own tty: one file per
/// shared host so a switch reads back THIS client's tty and moves only it. Under
/// `/tmp` (present + writable on every POSIX machine).
fn display_tty_path(host_key: &str) -> String {
    format!("/tmp/.xmux-cli-{}", display_tty_token(host_key))
}

/// The named paste buffer the record file is staged in while the control connection
/// reads it back. Keyed like the file, so two xmux instances on one server never read
/// or delete each other's buffer.
fn display_tty_buffer(host_key: &str) -> String {
    format!("xmux-cli-{}", display_tty_token(host_key))
}

/// The shell prefix a shared attach prepends to its remote command so the attach shell
/// records its OWN controlling tty to the per-host file before exec'ing the attach - the
/// value `switch_in_place` reads back to move xmux's own display client, never the user's
/// (which `list-clients` cannot tell apart). Out-of-band (a file, not the pty stream) so
/// the Windows ConPTY cannot consume it. A implementation-private free fn (not a `Mux` method) so
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
    /// `-S <path>` is tmux's own flag for choosing which server to talk to.
    fn takes_server_socket(&self) -> bool {
        true
    }

    /// tmux names its own creations: `new-session` auto-names an empty request and the
    /// plan's `-P -F` prints the final name for the manage layer to read back.
    fn assigns_new_session_name(&self) -> bool {
        true
    }

    fn kind(&self) -> &str {
        "tmux"
    }

    fn bin(&self) -> &str {
        &self.bin
    }

    /// tmux names itself only in `-V` (`tmux <version>`), and it has no `help`
    /// command (`tmux help` exits non-zero). The help probe runs FIRST because psmux
    /// installs a `tmux` alias of itself whose `-V` mimics tmux's version line while
    /// the alias's own help names it: a successful help naming a mux names ANOTHER
    /// mux, and that is the alias correction.
    fn identity_probes(&self) -> Vec<Vec<String>> {
        vec![
            vec![self.bin.clone(), "help".to_string()],
            vec![self.bin.clone(), "-V".to_string()],
        ]
    }

    fn classify_identity(&self, outputs: &[Option<String>]) -> Option<&'static str> {
        // A help that succeeded names another mux (real tmux has no help): the
        // psmux-behind-the-alias case, answered before the version line is read.
        if let Some(help) = outputs.first().and_then(|o| o.as_deref()) {
            if let Some(kind) = named_mux_excluding(help, "tmux") {
                return Some(kind);
            }
        }
        // The version line names the mux it belongs to; tmux's own is `tmux <version>`.
        named_mux(outputs.get(1).and_then(|o| o.as_deref())?)
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
        crate::mux::enumerate_via_list_sessions(&self.bin, self.kind(), transport, runner).await
    }

    fn attach_plan(&self, session: &str) -> Vec<String> {
        mux::attach(&self.bin, session)
    }

    /// `ignore-size` leaves the display client out of window sizing while any client
    /// without the flag is attached, so xmux's narrower view never resizes the user's own
    /// client, and a session only xmux shows still takes xmux's size. The flag rides a
    /// command queued behind the attach rather than `attach -f`, which tmux before 3.2
    /// refuses along with the whole attach: `if-shell` parses its command only when it
    /// runs, so an older server fails that one command and keeps the attach. The queued
    /// command runs before the attach resizes anything, and the flag stays with the
    /// client through every later `switch-client`.
    fn display_attach_plan(&self, session: &str) -> Vec<String> {
        let mut argv = mux::attach(&self.bin, session);
        argv.extend(
            [";", "if-shell", "-F", "1", "refresh-client -f ignore-size"].map(String::from),
        );
        argv
    }

    fn switch_in_place(
        &self,
        host_key: &str,
        session: &str,
        display_tty: Option<&str>,
    ) -> Option<SwitchPlan> {
        let b = &self.bin;
        let s = mux::quote_target(session);
        // A client tty the caller already knows moves that client with a plain exec, so a
        // machine that runs no machine shell still switches in place. The follow-up
        // `refresh-client` forces the new session to repaint the whole screen. The client
        // stops sizing sessions first (`display_attach_plan` says why it rides
        // `if-shell`), so landing on a session the user also has open cannot resize the
        // user's client before the session-changed notice settles it.
        if let Some(tty) = display_tty.filter(|t| !t.is_empty()) {
            return Some(SwitchPlan::Exec(vec![
                vec![
                    b.clone(),
                    "if-shell".to_string(),
                    "-F".to_string(),
                    "1".to_string(),
                    format!("refresh-client -t {tty} -f ignore-size"),
                    ";".to_string(),
                    "switch-client".to_string(),
                    "-c".to_string(),
                    tty.to_string(),
                    "-t".to_string(),
                    s,
                ],
                vec![
                    b.clone(),
                    "refresh-client".to_string(),
                    "-t".to_string(),
                    tty.to_string(),
                ],
            ]));
        }
        // Otherwise read the tty THIS host's display attach recorded to its file, then
        // move ONLY that client - guarded on a non-empty value so a missing/empty file
        // never runs `switch-client -c ""` (which would move an arbitrary client). The
        // switch is a raw shell command, so a machine with no machine shell cannot run it
        // and the driver reattaches.
        let path = display_tty_path(host_key);
        Some(SwitchPlan::Shell(format!(
            "c=$(cat {path} 2>/dev/null); [ -n \"$c\" ] && {{ {b} if-shell -F 1 \"refresh-client -t $c -f ignore-size\" \\; switch-client -c \"$c\" -t {s}; {b} refresh-client -t \"$c\"; }}"
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
/// no state - `Tmux::control_protocol` hands out a shared `'static` reference.
pub struct TmuxControl;

impl ControlProtocol for TmuxControl {
    fn mux_kind(&self) -> &'static str {
        "tmux"
    }

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
            | Notif::SessionRenamed { .. }
            | Notif::WindowAdd { .. }
            | Notif::WindowClose { .. }
            | Notif::WindowRenamed { .. } => {
                // The server's session/window STRUCTURE changed; the app refetches
                // (list-sessions), so the nav's session list resyncs. The
                // notification carries only an id, so a blanket refetch is simplest.
                Some(HostEvent::Changed {
                    host: host.to_string(),
                })
            }
            Notif::SessionWindowChanged { .. } => {
                // A session's ACTIVE WINDOW switched (e.g. another client did prefix-n).
                // The card names no window any more, and the display PTY follows the
                // mux's own state as a live mirror, so a window change needs no xmux
                // reaction - it is a display detail inside the session.
                None
            }
            Notif::ClientSessionChanged { client, name, .. } => {
                // ANOTHER client's attached session changed. When that client is xmux's OWN
                // display attach (the app matches `client` against `Host.display_tty`), the
                // display PTY was moved to `name` by the mux itself - e.g. the user pressed
                // `prefix`+`s` in the terminal view. Carry the client tty + new session name
                // so the app can match and follow the nav selection. A third party's own
                // client can never match the display tty, so it is structurally inert there.
                Some(HostEvent::ClientSessionChanged {
                    host: host.to_string(),
                    client: client.to_string(),
                    session: name.to_string(),
                })
            }
            // `%session-changed` names the session the metadata client itself is
            // attached to, where tmux counts it as a client like any other.
            Notif::SessionChanged { name, .. } => Some(HostEvent::ControlSession {
                host: host.to_string(),
                session: name.to_string(),
            }),
            // `%window-pane-changed` (a pane became active) does not affect the card list:
            // the per-session PTY attachments own the live pane.
            Notif::WindowPaneChanged { .. } => None,
            Notif::Exit { reason } => {
                // `%exit` may carry its own reason; otherwise fall back to the last error
                // block ("no sessions" / "no server running") so an empty mux is not
                // mistaken for a dead host. tmux names a reason for every orderly end of
                // a client (`server exited`, `detached`, `too far behind`, ...) and sends
                // a bare notice only to a client whose attached session was destroyed
                // while the server keeps running, which is the one exit flagged a detach.
                Some(HostEvent::Exited {
                    host: host.to_string(),
                    detached: reason.is_none(),
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
        // SUPPRESS %output - this control connection is a metadata / change-event /
        // `switch-client` channel ONLY; the per-session PTY attaches own the pixels, so
        // streaming pane output here is pure waste (and risks flooding the loop).
        // `no-output` keeps notifications flowing but stops %output. An older mux that
        // lacks the flag just %errors it (correlated as Ignore) - harmless.
        // `ignore-size` keeps this client out of window sizing: it attaches to whatever
        // session tmux picks, often the one xmux itself runs in, and must never shrink it,
        // so this client also never sends a client size (`refresh-client -C`).
        vec!["refresh-client -f no-output,ignore-size\n".to_string()]
    }

    fn list_sessions_line(&self) -> String {
        // Single-quote the format so tmux's command parser keeps `#{...}` as one arg
        // and reads neither `#` as a comment nor `{` as a block.
        format!("list-sessions -F '{SESSION_FORMAT}'\n")
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

    fn session_clients_line(&self, session: &str) -> String {
        format!(
            "list-clients -t {} -F '#{{client_tty}} #{{client_control_mode}}'\n",
            quote_target(&format!("={session}"))
        )
    }

    fn session_shared(&self, body: &[String], display_tty: &str) -> bool {
        body.iter().any(|line| {
            let mut fields = line.split_whitespace();
            matches!(
                (fields.next(), fields.next()),
                (Some(tty), Some("0")) if tty != display_tty
            )
        })
    }

    /// `ignore-size` leaves a client out of window sizing only while some client without
    /// it is attached anywhere on the server, which is why the flag is cleared again
    /// whenever xmux's client is alone on its session. tmux sizes windows only when
    /// something asks it to and a flag change does not, so the flag is followed by
    /// setting and unsetting a user option of xmux's own: an option change makes tmux
    /// size every window again and redraw every client, and it notifies no control
    /// client, so no xmux instance on the server answers it. Sizing again matters both
    /// ways: a client moved inside xmux's view into a session the user also has open has
    /// already resized it, and a client left alone still shows the size the departed
    /// client gave.
    fn display_size_lines(&self, display_tty: &str, shared: bool) -> Vec<String> {
        let flag = if shared {
            "ignore-size"
        } else {
            "!ignore-size"
        };
        vec![
            format!("refresh-client -t {display_tty} -f {flag}\n"),
            "set-option @xmux-size 1\n".to_string(),
            "set-option -u @xmux-size\n".to_string(),
        ]
    }

    fn display_size_yield_line(&self, display_tty: &str) -> String {
        format!("refresh-client -t {display_tty} -f ignore-size\n")
    }

    /// Reads back the file the display attach wrote its own controlling tty to before
    /// exec'ing (`record_prefix`). Only xmux's own attach writes that file, so the tty it
    /// answers with is xmux's own client by construction. A client listing would name the
    /// user's own clients and any client an earlier attach left behind in exactly the same
    /// shape, with nothing in the reply to tell them apart - and a host xmux attaches to
    /// and detaches from repeatedly accumulates them, which is when picking wrong becomes
    /// likely.
    ///
    /// The file goes through a named paste buffer because `show-buffer` prints into its
    /// own reply block, while `run-shell` prints its command's output after its block has
    /// closed, where no reply is read. Every line rides the open control connection and
    /// asks the host for no second one. A missing file leaves no buffer, so `show-buffer`
    /// answers with an error that names no tty.
    fn display_tty_lines(&self, host_key: &str) -> DisplayTtyRead {
        let buffer = display_tty_buffer(host_key);
        DisplayTtyRead {
            stage: format!("load-buffer -b {buffer} {}\n", display_tty_path(host_key)),
            read: format!("show-buffer -b {buffer}\n"),
            clear: format!("delete-buffer -b {buffer}\n"),
        }
    }

    fn parse_display_tty(&self, body: &[String]) -> Option<String> {
        control_proto::parse_display_tty(body)
    }
}

#[cfg(test)]
mod control_tests {
    use super::*;

    /// A session is shared when a client besides xmux's display client is attached to
    /// it and sizes windows; a control client, xmux's own metadata client among them,
    /// sizes none.
    #[test]
    fn a_session_is_shared_only_by_another_window_sizing_client() {
        assert_eq!(
            TmuxControl.session_clients_line("my build"),
            "list-clients -t '=my build' -F '#{client_tty} #{client_control_mode}'\n"
        );
        let body = |lines: &[&str]| lines.iter().map(|l| l.to_string()).collect::<Vec<_>>();
        let tmux = TmuxControl;
        assert!(!tmux.session_shared(&body(&["/dev/pts/3 0"]), "/dev/pts/3"));
        assert!(!tmux.session_shared(&body(&["/dev/pts/3 0", "/dev/pts/2 1"]), "/dev/pts/3"));
        assert!(tmux.session_shared(&body(&["/dev/pts/3 0", "/dev/pts/9 0"]), "/dev/pts/3"));
        assert!(!tmux.session_shared(&body(&[]), "/dev/pts/3"));
    }

    /// Each line is one command, so each answers with exactly one reply block; the flag
    /// is followed by an option change that makes tmux size its windows again.
    #[test]
    fn display_size_lines_set_the_flag_then_resize_the_windows() {
        assert_eq!(
            TmuxControl.display_size_lines("/dev/pts/3", true),
            vec![
                "refresh-client -t /dev/pts/3 -f ignore-size\n",
                "set-option @xmux-size 1\n",
                "set-option -u @xmux-size\n",
            ]
        );
        assert_eq!(
            TmuxControl.display_size_lines("/dev/pts/3", false)[0],
            "refresh-client -t /dev/pts/3 -f !ignore-size\n"
        );
        assert_eq!(
            TmuxControl.display_size_yield_line("/dev/pts/3"),
            "refresh-client -t /dev/pts/3 -f ignore-size\n"
        );
    }

    #[test]
    fn connect_keeps_the_metadata_client_out_of_window_sizing() {
        assert_eq!(
            TmuxControl.connect_lines(),
            vec!["refresh-client -f no-output,ignore-size\n".to_string()]
        );
    }
}

#[cfg(test)]
mod display_identity_tests {
    use super::*;

    #[test]
    fn same_name_restart_uses_a_different_tty_record() {
        let previous = display_tty_path(&display_tty_key_for_run("jup", "steady", "first-run", 1));
        let restarted =
            display_tty_path(&display_tty_key_for_run("jup", "steady", "second-run", 1));

        assert_ne!(
            previous, restarted,
            "a restarted instance must not read the prior run's tty record"
        );
    }

    #[test]
    fn each_attach_uses_a_different_tty_record() {
        let first = display_tty_path(&display_tty_key_for_run("jup", "steady", "run", 1));
        let second = display_tty_path(&display_tty_key_for_run("jup", "steady", "run", 2));

        assert_ne!(first, second, "each attach owns its tty record");
    }

    /// tmux's in-place switch is an opaque `SwitchPlan::Shell`: a self-contained remote
    /// shell command that READS the tty the attach recorded to its per-host file, then
    /// moves ONLY that client to the session - guarded on a non-empty value so a
    /// missing/empty file never runs `switch-client -c ""`. (`display_tty` is ignored;
    /// tmux reads its own recorded file.)
    #[test]
    fn tmux_switch_in_place_returns_a_remote_shell_plan_reading_its_recorded_tty() {
        let SwitchPlan::Shell(cmd) = Tmux { bin: "tmux".into() }
            .switch_in_place("jup", "test2", None)
            .expect("a shared mux switches in place via its recorded tty")
        else {
            panic!("tmux switches through the machine shell, not an exec plan");
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
        assert!(
            cmd.contains("if-shell -F 1 \"refresh-client -t $c -f ignore-size\" \\; switch-client"),
            "the client stops sizing sessions before it moves: {cmd}"
        );
    }

    /// A caller that already KNOWS the client tty gets a plain exec plan instead: stop
    /// that client from sizing the session it lands on, move it, then force the new
    /// session to repaint the whole screen. No machine shell is involved, so a machine
    /// that runs none still switches in place.
    #[test]
    fn tmux_switch_in_place_takes_a_known_tty_without_a_machine_shell() {
        let SwitchPlan::Exec(argvs) = Tmux { bin: "tmux".into() }
            .switch_in_place("local", "test2", Some("/dev/pts/3"))
            .expect("a known tty switches in place")
        else {
            panic!("a known tty needs no machine shell");
        };
        let plan: Vec<Vec<&str>> = argvs
            .iter()
            .map(|a| a.iter().map(String::as_str).collect())
            .collect();
        assert_eq!(
            plan,
            vec![
                vec![
                    "tmux",
                    "if-shell",
                    "-F",
                    "1",
                    "refresh-client -t /dev/pts/3 -f ignore-size",
                    ";",
                    "switch-client",
                    "-c",
                    "/dev/pts/3",
                    "-t",
                    "test2"
                ],
                vec!["tmux", "refresh-client", "-t", "/dev/pts/3"],
            ],
            "stop ONLY that client from sizing, move it, then repaint it"
        );
    }

    /// An EMPTY tty is not a known one: `switch-client -c ""` moves an arbitrary client,
    /// so it falls back to the recorded-file plan (itself guarded on a non-empty read).
    #[test]
    fn tmux_switch_in_place_treats_an_empty_tty_as_unknown() {
        assert!(
            matches!(
                Tmux { bin: "tmux".into() }.switch_in_place("jup", "test2", Some("")),
                Some(SwitchPlan::Shell(_))
            ),
            "an empty tty names no client, so the recorded file decides"
        );
    }

    /// tmux's display client records its OWN tty to a per-host file before exec'ing the
    /// attach (`record_prefix`), so a later `switch_in_place` reads that file and targets
    /// THAT client - never the user's own attached client (the bug the `-CC` "first
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

    #[test]
    fn record_prefix_keys_the_tty_file_per_instance_not_just_per_host() {
        // Two xmux instances sharing one remote host pass `{host}-{instance}` keys, so
        // each records/reads its OWN display client's tty - a `switch-client` then moves
        // THIS instance's client, never the other instance's. A single per-host file
        // would otherwise be overwritten by the second instance (last-writer-wins) and
        // the first would switch the wrong client.
        let a = record_prefix("jup-first");
        let b = record_prefix("jup-second");
        assert_ne!(a, b, "distinct instances get distinct tty-record files");
        let shared = record_prefix("jup");
        assert_ne!(
            a, shared,
            "the instance key extends, never reuses, the bare host key"
        );
    }
}
