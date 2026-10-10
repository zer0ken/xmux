use super::*;
use crate::state::{LoginDraft, LoginFocus, State};

/// A roster whose every ssh machine writes `tmux` as its mux, so the host registry holds
/// one host for each alias named.
fn fake_roster(aliases: &[&str]) -> crate::provision::env::Roster {
    let cfg = crate::provision::config::Config {
        machines: aliases
            .iter()
            .filter(|a| **a != crate::session::LOCAL_MACHINE)
            .map(|a| crate::provision::config::MachineConfig {
                ssh: a.to_string(),
                mux: "tmux".into(),
            })
            .collect(),
        ..Default::default()
    };
    crate::provision::env::Roster {
        cfg,
        local_muxes: vec!["tmux".into()],
        ssh_aliases: aliases
            .iter()
            .filter(|a| **a != crate::session::LOCAL_MACHINE)
            .map(|a| a.to_string())
            .collect(),
        ..Default::default()
    }
}

fn fake_env_with_machines(aliases: &[&str]) -> Env {
    fake_env_from(fake_roster(aliases))
}

/// An env over `fake_roster(written)` plus the ssh machines `auto`, which write no mux and
/// so have no host until they answer which muxes they serve.
fn fake_env_with_auto_machines(written: &[&str], auto: &[&str]) -> Env {
    fake_env_from(auto_roster(written, auto))
}

fn auto_roster(written: &[&str], auto: &[&str]) -> crate::provision::env::Roster {
    let mut roster = fake_roster(written);
    roster
        .ssh_aliases
        .extend(auto.iter().map(|a| a.to_string()));
    roster
}

fn fake_env_from(roster: crate::provision::env::Roster) -> Env {
    // A real throwaway dir, not `.`: tests that exercise pref persistence (e.g.
    // resize_axis saving nav_height) write `<xmux_dir>/<file>`, and `.` would
    // pollute the repository root with stray pref files.
    static NEXT_ENV: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = NEXT_ENV.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let xmux_dir = std::env::temp_dir().join(format!("xmux-test-env-{}-{id}", std::process::id()));
    let _ = std::fs::create_dir_all(&xmux_dir);
    Env::new(roster, "C-g".into(), xmux_dir, None, None)
}

#[test]
fn selection_from_session_row_target() {
    let t = TerminalViewTarget {
        host: "jupiter06".into(),
        target: "api".into(),
    };
    let sel = selection_from_target(&t);
    assert_eq!(sel.host, "jupiter06");
    assert_eq!(sel.session, "api");
    assert_eq!(
        crate::session::Address::new(&sel.host, &sel.session).display(),
        "jupiter06/api"
    );
    assert!(!sel.is_empty());
}

#[test]
fn selection_keeps_a_colon_inside_the_session_name() {
    // A session name holding a colon (a zellij session may) is the session whole, as
    // the card carries it - no suffix is parted off.
    let t = TerminalViewTarget {
        host: "local:zellij".into(),
        target: "a:b".into(),
    };
    let sel = selection_from_target(&t);
    assert_eq!(sel.session, "a:b");
    assert_eq!(sel.host, "local:zellij");
}

#[test]
fn selection_from_empty_target_is_empty() {
    let sel = selection_from_target(&TerminalViewTarget::default());
    assert!(sel.is_empty());
}

#[test]
fn display_key_is_per_host_for_shared_and_reattach_psmux() {
    // Shared tmux and reattach psmux both use one PTY per HOST. The key is shaped
    // by mux behavior, read off the Host - never the transport's remote flag.
    let mut hosts = crate::model::Hosts::default();
    hosts.insert(crate::model::Host::new(
        crate::transport::ssh("jup".into(), String::new(), "linux".into()),
        crate::mux::for_binary("tmux").unwrap(), // Shared
    ));
    hosts.insert(crate::model::Host::new(
        crate::transport::local(None),            // host id == "local"
        crate::mux::for_binary("psmux").unwrap(), // PerSession
    ));
    let rsel = Selection {
        host: "jup".into(),
        session: "api".into(),
    };
    assert_eq!(display_key(&hosts, &rsel), "jup", "shared → per-host key");
    let lsel = Selection {
        host: "local".into(),
        session: "work".into(),
    };
    assert_eq!(
        display_key(&hosts, &lsel),
        "local",
        "reattach per-session muxes use a per-host key"
    );
}

#[test]
fn scan_result_corrects_tmux_config_to_psmux_poll() {
    let mut hosts = crate::model::Hosts::default();
    hosts.insert(crate::model::Host::new(
        crate::transport::local(None),
        crate::mux::for_binary("tmux").unwrap(),
    ));

    apply_scan_result(
        &mut hosts,
        "local",
        Some(crate::mux::for_kind("psmux", "tmux").unwrap()),
    );

    let host = hosts.get("local").unwrap();
    assert!(host.detected);
    assert_eq!(host.mux.kind(), "psmux");
    assert_eq!(host.mux.bin(), "tmux");
    assert!(matches!(
        host.mux.event_source(),
        crate::model::EventSource::Poll
    ));
}

#[test]
fn scan_result_corrects_psmux_config_to_tmux_control() {
    let mut hosts = crate::model::Hosts::default();
    hosts.insert(crate::model::Host::new(
        crate::transport::local(None),
        crate::mux::for_binary("psmux").unwrap(),
    ));

    apply_scan_result(
        &mut hosts,
        "local",
        Some(crate::mux::for_kind("tmux", "psmux").unwrap()),
    );

    let host = hosts.get("local").unwrap();
    assert!(host.detected);
    assert_eq!(host.mux.kind(), "tmux");
    assert_eq!(host.mux.bin(), "psmux");
    assert!(matches!(
        host.mux.event_source(),
        crate::model::EventSource::Control
    ));
}

/// A remote-shaped transport whose control channel runs only a disposable local child.
#[derive(Clone)]
struct TestRemote(String);

impl crate::transport::Transport for TestRemote {
    fn host_id(&self) -> &str {
        &self.0
    }
    fn is_remote(&self) -> bool {
        true
    }
    fn runs_through_shell(&self) -> bool {
        true
    }
    fn exec_argv(&self, _tty: bool, _argv: &[String]) -> crate::transport::CommandSpec {
        #[cfg(windows)]
        let argv = ["cmd.exe", "/c", "exit 0"];
        #[cfg(not(windows))]
        let argv = ["sh", "-c", "exit 0"];
        crate::transport::CommandSpec::from_argv(argv.map(String::from).to_vec())
    }
    fn interactive_attach_argv(&self, argv: &[String]) -> crate::transport::CommandSpec {
        self.exec_argv(true, argv)
    }
    fn control_argv(&self, argv: &[String]) -> crate::transport::CommandSpec {
        self.exec_argv(false, argv)
    }
    fn clone_box(&self) -> Box<dyn crate::transport::Transport> {
        Box::new(self.clone())
    }
    fn clone_as(&self, id: &str) -> Box<dyn crate::transport::Transport> {
        Box::new(Self(id.into()))
    }
    fn machine_kind(&self) -> crate::transport::MachineKind {
        crate::transport::MachineKind::Ssh {
            id: self.0.clone(),
            alias: "fixture.invalid".into(),
            control_path: String::new(),
            os: "linux".into(),
        }
    }
}

#[tokio::test]
async fn dispatch_detected_host_connects_remote_hosts() {
    // Control-event (tmux) hosts get a control client at startup; poll hosts
    // enumerate off the loop (no control client). The gate is the host's
    // event_source, read off the Host - not the transport remote flag. The
    // control child runs locally in isolation.
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<HostEvent>();
    let mut mgr = HostManager::new(tx);
    let mut hosts = crate::model::Hosts::default();
    let mut host = crate::model::Host::new(
        Box::new(TestRemote("jupiter06".into())),
        crate::mux::for_binary("tmux").unwrap(), // Control event source
    );
    host.detected = true;
    hosts.insert(host);
    dispatch_detected_host(&mut mgr, &hosts, "jupiter06", 80, 24);
    assert!(
        mgr.get("jupiter06").is_some(),
        "control host got a control client from the registry alone"
    );
    mgr.teardown_all();
}

#[tokio::test]
async fn scan_or_dispatch_host_detects_from_hosts_without_env() {
    // An UNDETECTED host is routed to detection using ONLY the Hosts registry - no
    // Env/by_alias. The detection branch marks the host in `detecting`; the probe
    // clones the host's transport + mux rather than re-deriving from a HostDef.
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<HostEvent>();
    let mut mgr = HostManager::new(tx);
    let mut hosts = crate::model::Hosts::default();
    hosts.insert(crate::model::Host::new(
        crate::transport::local(None),
        crate::mux::for_kind("psmux", "psmux-no-such-binary").unwrap(),
    )); // Host::new leaves it undetected
    let mut model = AppModel::from_hosts(vec!["local".to_owned()]);
    let gate = std::sync::Arc::new(tokio::sync::Semaphore::new(
        crate::provision::config::SCAN_CONCURRENCY_MAX,
    ));
    scan_or_dispatch_host(&mut mgr, &hosts, &mut model, "local", 80, 24, &gate);
    assert!(
        model.detecting.contains("local"),
        "an undetected host is queued for detection straight from the registry"
    );
}

#[tokio::test]
async fn dispatch_scanned_without_a_resolved_mux_opens_no_channel() {
    // A detection probe that resolved no mux (the machine is unreachable or does not run
    // the assumed mux) must NOT open a control channel: a doomed child would die and
    // overwrite the machine's real reason (locked / unreachable) with "connection
    // closed". So a `DispatchScanned { detected: None }` leaves the host channel-less.
    let mut rt = test_rt(fake_env_with_machines(&[]));
    let mut hosts = crate::model::Hosts::default();
    hosts.insert(crate::model::Host::new(
        crate::transport::ssh("jup".into(), String::new(), "linux".into()),
        crate::mux::for_binary("tmux").unwrap(),
    )); // undetected
    rt.hosts = hosts;
    rt.execute_host_effect_for_test(crate::model::EventEffect::DispatchScanned {
        host: "jup".into(),
        detected: None,
        err: None,
    });
    assert!(
        rt.mgr.get("jup").is_none(),
        "a host whose mux did not resolve gets no control channel"
    );
}

#[tokio::test]
async fn a_detach_reopens_the_control_channel_once() {
    // tmux detached the control client of a connected host: the runtime reaps it and
    // opens one new channel. That channel's exit before it lists sessions reopens nothing.
    let mut rt = test_rt(fake_env_with_machines(&[]));
    let mut host = crate::model::Host::new(
        Box::new(TestRemote("jup".into())),
        crate::mux::for_binary("tmux").unwrap(),
    );
    host.detected = true;
    rt.hosts.insert(host);
    rt.mgr.insert_fake("jup");
    rt.model.connected.insert("jup".into());
    let detach = || HostEvent::Exited {
        host: "jup".into(),
        reason: None,
        detached: true,
    };

    rt.handle_host_event(detach());
    assert!(
        rt.mgr.get("jup").is_some(),
        "the detach reopened the channel"
    );

    rt.handle_host_event(detach());
    assert!(
        rt.mgr.get("jup").is_none(),
        "a reopened channel that never listed sessions is not reopened again"
    );
}

#[tokio::test]
async fn machine_connected_dispatches_a_detected_control_host() {
    // A machine that connected resolves each host it serves onto its metadata channel.
    // A detected tmux host gets a control client backed by a disposable local child.
    let mut rt = test_rt(fake_env_with_machines(&[]));
    let mut hosts = crate::model::Hosts::default();
    let mut host = crate::model::Host::new(
        Box::new(TestRemote("jup".into())),
        crate::mux::for_binary("tmux").unwrap(),
    );
    host.detected = true;
    hosts.insert(host);
    rt.hosts = hosts;
    rt.execute_host_effect_for_test(crate::model::EventEffect::MachineConnected {
        shell: None,
        machine: "jup".into(),
        rescan: false,
    });
    assert!(
        rt.mgr.get("jup").is_some(),
        "the connected machine's detected control host got a channel"
    );
    std::mem::replace(
        &mut rt.mgr,
        HostManager::new(tokio::sync::mpsc::unbounded_channel().0),
    )
    .teardown_all();
}

#[tokio::test]
async fn a_dropped_channel_is_reopened_by_a_user_action_and_by_nothing_else() {
    // A host that connected once (detected) and then lost its metadata channel - its key
    // removed, its ControlMaster closed, the machine rebooted - stays without one until
    // something the user did asks for it. There is no beat that reopens it: a channel
    // reopened on a timer is a login attempt repeated on a timer, which is what a locked
    // machine's own defences read as an attack rather than as a client.
    //
    // The observable is the reopen path itself. `ensure_current_host` is what a keystroke
    // on the card runs, and it is the only thing left that can open this channel; nothing
    // in the runtime calls it without an input event behind it.
    let mut rt = test_rt(fake_env_with_machines(&[]));
    let mut hosts = crate::model::Hosts::default();
    let mut host = crate::model::Host::new(
        crate::transport::ssh("jup".into(), String::new(), "linux".into()),
        crate::mux::for_binary("tmux").unwrap(),
    );
    host.detected = true; // it connected once
    hosts.insert(host);
    rt.hosts = hosts;
    rt.model.switcher.apply_host_result(
        "jup".into(),
        vec![],
        Some("hrlee@jup: Permission denied (publickey,password).".into()),
        &mut rt.model.state,
    );
    assert!(rt.mgr.get("jup").is_none(), "precondition: no channel");
    // A LOCKED card is refused even on that user action: a `-CC` that dies on auth would
    // overwrite the locked reason with "connection closed", and it is one more refused
    // login on a machine that already refused one.
    ensure_current_host(
        &mut rt.mgr,
        &rt.hosts,
        &rt.model.switcher,
        rt.cols,
        rt.body_rows,
        rt.model.nav_width,
    );
    assert!(
        rt.mgr.get("jup").is_none(),
        "a locked card opens no channel, so xmux never retries an auth that failed"
    );
}

#[test]
fn terminal_view_size_zero_tree_is_full_width() {
    // Hidden tree (sentinel 0): full cols, no nav border subtracted.
    assert_eq!(
        terminal_view_size(
            80,
            23,
            crate::ui::switcher::NavSize::hidden(crate::ui::switcher::NAV_WIDTH)
        ),
        (80, 24)
    );
    // Shown tree: cols - nav_width - 1 (nav border). The prefix hint lives inside the
    // nav column, so the terminal view keeps every row. Wide enough to STAY a column: a row is
    // two columns tall, so the column survives only while `w - nav - 1` beats twice the
    // rows (200 - 49 = 151 against 48).
    assert_eq!(
        terminal_view_size(200, 23, crate::ui::switcher::NavSize::visible(48)),
        (151, 24)
    );
    // Degenerate widths clamp to at least 1.
    assert_eq!(
        terminal_view_size(
            0,
            0,
            crate::ui::switcher::NavSize::hidden(crate::ui::switcher::NAV_WIDTH)
        ),
        (1, 1)
    );
}

#[test]
fn terminal_view_size_keeps_full_height_when_the_tree_is_shown() {
    use crate::ui::switcher::NAV_WIDTH;
    // Nav hidden (sentinel 0): terminal view spans the full height.
    let (_, full) = terminal_view_size(
        120,
        39,
        crate::ui::switcher::NavSize::hidden(crate::ui::switcher::NAV_WIDTH),
    );
    assert_eq!(full, 40);
    // Tree shown in a column: the prefix hint is the NAV column's first row, not a full-width
    // strip, so the terminal view costs nothing in height.
    // 220 wide keeps the side column at 40 rows (171 against 80); at 120 the column would
    // leave a terminal squarer than it looks, and the horizontal nav would take over.
    let (_, shown) = terminal_view_size(220, 39, crate::ui::switcher::NavSize::visible(NAV_WIDTH));
    assert_eq!(
        shown, 40,
        "the nav-local prefix hint costs the terminal view no rows"
    );
}

#[test]
fn reconciled_nav_width_hides_only_when_focused_and_enabled_and_no_prefix() {
    // Tree focused (terminal_focused = false): always the natural width.
    assert_eq!(
        reconciled_nav_width(false, true, false, false, 48, false, "C-g"),
        48
    );
    assert_eq!(
        reconciled_nav_width(false, false, false, true, 48, false, "C-g"),
        48
    );
    // Terminal view focused + setting on + no prefix interaction: hidden (0).
    assert_eq!(
        reconciled_nav_width(true, true, false, false, 48, false, "C-g"),
        0
    );
    // Terminal view focused + setting on + prefix active: shown.
    assert_eq!(
        reconciled_nav_width(true, true, false, true, 48, false, "C-g"),
        48
    );
    // Terminal view focused + setting off: stays shown regardless.
    assert_eq!(
        reconciled_nav_width(true, false, false, false, 48, false, "C-g"),
        48
    );
    assert_eq!(
        reconciled_nav_width(true, false, false, true, 48, false, "C-g"),
        48
    );
    assert_eq!(
        reconciled_nav_width(false, false, false, false, 48, true, "C-g"),
        3,
        "collapsed is exactly the prefix wide"
    );
    assert_eq!(
        reconciled_nav_width(true, true, false, false, 48, true, "C-g"),
        0,
        "auto-hide wins over collapse"
    );
    // A nav that crowds the terminal view hides like auto-hide, and only on its terms.
    assert_eq!(
        reconciled_nav_width(true, false, true, false, 48, false, "C-g"),
        0,
        "a crowding nav hides while the terminal view holds the focus"
    );
    assert_eq!(
        reconciled_nav_width(true, false, true, true, 48, false, "C-g"),
        48,
        "a prefix interaction brings a crowding nav back"
    );
    assert_eq!(
        reconciled_nav_width(false, false, true, false, 48, false, "C-g"),
        48,
        "a focused nav keeps its width however small the window"
    );
}

/// A runtime whose one machine `pwbox` needs a login, sized `cols` by `rows`, with the
/// selection on its card and the terminal view, where its login pane is, focused.
fn login_pane_rt(cols: u16, rows: u16) -> Runtime {
    let mut rt = test_rt(fake_env_with_machines(&[]));
    let mut state = crate::state::State::from_hosts(vec!["pwbox".into()]);
    let mut switcher = crate::ui::switcher::Switcher::from_hosts(&mut state);
    switcher.apply_host_result(
        "pwbox".into(),
        Vec::new(),
        Some("alice@pwbox: Permission denied (publickey,password).".into()),
        &mut state,
    );
    state
        .focus
        .set_view_focus(crate::app::focus::ViewFocus::Terminal);
    rt.model.state = state;
    rt.model.switcher = switcher;
    rt.cols = cols;
    rt.body_rows = rows - 1;
    // Past the frame gate, so each prepare_and_draw paints.
    rt.last_draw = std::time::Instant::now() - std::time::Duration::from_secs(1);
    rt
}

fn drawn_text(term: &ratatui::Terminal<ratatui::backend::TestBackend>) -> String {
    let buf = term.backend().buffer();
    let mut out = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            out.push_str(buf[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

#[test]
fn a_small_window_gives_the_focused_login_pane_its_whole_width() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let mut rt = login_pane_rt(40, 12);
    let mut term = Terminal::new(TestBackend::new(40, 12)).unwrap();
    rt.prepare_and_draw(&mut term);
    let out = drawn_text(&term);
    assert_eq!(rt.model.nav_width, 0, "the nav steps aside:\n{out}");
    assert_eq!(rt.model.render_plan.regions.terminal.width, 40, "{out}");
    for row in [
        "machine pwbox",
        "address*   pwbox",
        "port*      22",
        "username*",
        "password   optional",
    ] {
        assert!(out.contains(row), "the pane shows `{row}` whole:\n{out}");
    }

    // What a field states beside its value continues under the value column rather
    // than being cut at the window's edge.
    let rows: Vec<&str> = out.lines().collect();
    let address = rows
        .iter()
        .position(|l| l.contains("address*   pwbox"))
        .unwrap_or_else(|| panic!("{out}"));
    assert_eq!(rows[address + 1].trim(), "host name", "{out}");

    // Every stop the form takes keys at scrolls into a window too short for the form.
    for stop in [
        "port*",
        "username*",
        "password",
        "(*) do nothing",
        "( ) save connection to ssh config",
        "( ) register my public key",
        "[ Log in ]",
        "[ ] details",
    ] {
        rt.model.state.feed_login("pwbox", b"\t");
        rt.last_draw = std::time::Instant::now() - std::time::Duration::from_secs(1);
        rt.dirty = true;
        rt.prepare_and_draw(&mut term);
        let out = drawn_text(&term);
        assert!(
            out.contains(stop),
            "the focused `{stop}` is on screen:\n{out}"
        );
    }

    // Focusing the nav brings it back at the same size: the small window then belongs
    // to the view the user moved to.
    rt.model
        .state
        .focus
        .set_view_focus(crate::app::focus::ViewFocus::Nav);
    rt.last_draw = std::time::Instant::now() - std::time::Duration::from_secs(1);
    rt.prepare_and_draw(&mut term);
    assert_eq!(rt.model.nav_width, rt.model.nav_width_natural);
    assert!(drawn_text(&term).contains("pwbox"));
}

#[test]
fn a_narrow_view_wraps_the_login_choices_instead_of_cutting_them() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    // 80x24 keeps the 48-column nav, so the pane has 31 columns.
    let mut rt = login_pane_rt(80, 24);
    let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
    rt.prepare_and_draw(&mut term);
    assert_eq!(rt.model.render_plan.regions.terminal.width, 31);
    let view: Vec<String> = drawn_text(&term)
        .lines()
        .map(|l| l.chars().skip(49).collect::<String>())
        .collect();
    let out = view.join("\n");
    let at = view
        .iter()
        .position(|l| l.contains("( ) save connection"))
        .unwrap_or_else(|| panic!("{out}"));
    let choice = format!("{} {}", view[at].trim(), view[at + 1].trim());
    assert_eq!(choice, "( ) save connection to ssh config", "{out}");
    assert!(
        view[at + 1].starts_with("        "),
        "the rest of the choice continues under its text:\n{out}"
    );
}

#[test]
fn a_narrow_view_wraps_a_long_login_value_under_its_column() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let host = "build-runner-07.internal.example.net";
    let mut rt = login_pane_rt(80, 24);
    // The prefilled address and what is typed after it are wider than the value column.
    rt.model.state.feed_login("pwbox", host.as_bytes());
    let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
    rt.prepare_and_draw(&mut term);
    let view: Vec<String> = drawn_text(&term)
        .lines()
        .map(|l| l.chars().skip(49).collect::<String>())
        .collect();
    let out = view.join("\n");
    let at = view
        .iter()
        .position(|l| l.contains("address*"))
        .unwrap_or_else(|| panic!("{out}"));
    let value: String = view[at..]
        .iter()
        .take_while(|l| !l.contains("port*"))
        .map(|l| l.chars().skip(14).collect::<String>().trim().to_string())
        .collect();
    assert!(
        value.starts_with(&format!("pwbox{host}")),
        "the whole address is on screen:\n{out}"
    );
}

#[test]
fn a_narrow_view_keeps_every_note_of_a_login_field() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let mut rt = login_pane_rt(80, 24);
    let value = |value: &str| crate::provision::env::LoginValue {
        value: value.into(),
        provenance: "from ssh config",
    };
    rt.model.state.chrome.set_login_defaults(
        std::collections::HashMap::from([(
            "pwbox".into(),
            crate::provision::env::LoginDefaults {
                address: value("pwbox"),
                port: value("22"),
                username: value("alice"),
                ssh_effective: None,
            },
        )]),
        Default::default(),
    );
    let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
    rt.prepare_and_draw(&mut term);
    let view: Vec<String> = drawn_text(&term)
        .lines()
        .map(|l| l.chars().skip(49).collect::<String>())
        .collect();
    let out = view.join("\n");
    let at = view
        .iter()
        .position(|l| l.contains("username*"))
        .unwrap_or_else(|| panic!("{out}"));
    let field: String = view[at..]
        .iter()
        .take_while(|l| !l.contains("password"))
        .cloned()
        .collect();
    assert!(field.contains("from ssh config"), "{out}");
    assert!(field.contains('✗'), "the failure mark is on screen:\n{out}");
}

#[test]
fn a_window_with_room_for_both_keeps_the_nav_beside_the_focused_pane() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let mut rt = login_pane_rt(100, 30);
    let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
    rt.prepare_and_draw(&mut term);
    assert_eq!(rt.model.nav_width, rt.model.nav_width_natural);
    assert!(drawn_text(&term).contains("username*"));
}

#[test]
fn apply_width_delta_is_write_free_and_reports_change() {
    let mut w = 48u16;
    assert!(
        apply_width_delta(1, &mut w, "C-g"),
        "a real delta reports changed"
    );
    assert_eq!(w, 49);
    assert!(
        !apply_width_delta(0, &mut w, "C-g"),
        "a zero delta reports unchanged"
    );
    assert_eq!(w, 49);
    // Clamp at the max: a delta that cannot move the width reports unchanged.
    let mut hi = NAV_WIDTH_MAX;
    assert!(
        !apply_width_delta(10, &mut hi, "C-g"),
        "a clamped no-op reports unchanged"
    );
    assert_eq!(hi, NAV_WIDTH_MAX);
}

#[test]
fn spinner_frame_advances_with_wall_clock() {
    use std::time::Duration;
    assert_eq!(spinner_frame_at(Duration::from_millis(0)), 0);
    assert_eq!(spinner_frame_at(Duration::from_millis(SPINNER_FRAME_MS)), 1);
    assert_eq!(
        spinner_frame_at(Duration::from_millis(SPINNER_FRAME_MS * 3 + 10)),
        3
    );
}

#[test]
fn nav_width_adjust_clamps() {
    // The floor holds a card's indent, a two-digit number, and eight cells of name.
    let min = nav_width_min("C-g");
    assert_eq!(adjust_nav_width(48, 1, "C-g"), 49);
    assert_eq!(adjust_nav_width(48, -1, "C-g"), 47);
    assert_eq!(
        adjust_nav_width(min, -1, "C-g"),
        min,
        "clamped at the card floor"
    );
    assert_eq!(
        adjust_nav_width(NAV_WIDTH_MAX, 1, "C-g"),
        NAV_WIDTH_MAX,
        "clamped at max"
    );
    assert_eq!(nav_width_min("C-g"), 14);
    assert_eq!(
        nav_width_min("C-Space-Shift-Alt-Ctrl"),
        25,
        "a prefix wider than the card floor raises the floor"
    );
}

#[test]
fn terminal_view_size_subtracts_tree_and_nav_border() {
    use crate::ui::switcher::NAV_WIDTH;
    let (vc, vr) = terminal_view_size(143, 39, crate::ui::switcher::NavSize::visible(NAV_WIDTH));
    assert_eq!(
        vc,
        143 - (NAV_WIDTH + 1),
        "cols minus tree minus nav border"
    );
    // The prefix hint sits inside the nav column, so the terminal view keeps the full
    // terminal height (body_rows + 1).
    assert_eq!(vr, 40, "the nav-local prefix hint costs no terminal rows");
}

#[test]
fn terminal_view_size_clamps_to_at_least_one() {
    use crate::ui::switcher::NAV_WIDTH;
    // A 10-col terminal can't fit the 48-col tree beside it, so the layout goes to the horizontal nav
    // and the terminal keeps full width; a zero-row body still clamps the height up to 1. The
    // invariant this guards is that neither dimension is ever 0 (degenerate PTY size).
    let (vc, vr) = terminal_view_size(10, 0, crate::ui::switcher::NavSize::visible(NAV_WIDTH));
    assert!(vc >= 1, "width never zero, got {vc}");
    assert_eq!(vr, 1, "0.max(1) = 1: height clamps up for a zero-row body");
}

#[tokio::test]
async fn host_exited_before_connect_marks_unreachable() {
    use crate::ui::run::dump_screen;
    use crate::ui::switcher::Switcher;
    let mut state = crate::state::State::from_hosts(vec!["jupiter00".into()]);
    let mut switcher = Switcher::from_hosts(&mut state);
    let mut connected: HashSet<String> = HashSet::new();
    assert!(
        note_host_exited(
            &mut switcher,
            &mut state,
            &mut connected,
            "jupiter00",
            Some("no route to host".into())
        ),
        "a never-connected host is marked unreachable on exit"
    );
    let out = dump_screen(
        &switcher,
        None,
        80,
        24,
        &state,
        &crate::ui::switcher::RenderPlan::default(),
    );
    assert!(
        out.contains("unreachable"),
        "host reads unreachable:\n{out}"
    );
    assert!(
        out.contains("no route to host"),
        "shows the exit reason:\n{out}"
    );
}

#[test]
fn a_blocked_host_shows_the_login_view_screen() {
    use crate::ui::run::dump_screen;
    // A blocked host (reached, credentials refused) shows the login pane: its
    // state word, not the unreachable word, and ssh's own reason. It also
    use crate::ui::switcher::Switcher;
    let mut state = crate::state::State::from_hosts(vec!["pwbox".into()]);
    let mut switcher = Switcher::from_hosts(&mut state);
    switcher.apply_host_result(
        "pwbox".into(),
        Vec::new(),
        Some("pwtest@127.0.0.1: Permission denied (publickey,password).".into()),
        &mut state,
    );
    let out = dump_screen(
        &switcher,
        None,
        100,
        24,
        &state,
        &crate::ui::switcher::RenderPlan::default(),
    );
    assert!(
        out.contains("login needed"),
        "the login view names its state:\n{out}"
    );
    assert!(
        !out.contains("unreachable"),
        "a blocked host is not the unreachable state:\n{out}"
    );
    assert!(
        out.contains("pwtest@127.0.0.1"),
        "ssh's own reason is on the login pane:\n{out}"
    );
}

#[test]
fn a_machine_whose_name_did_not_resolve_stays_unreachable() {
    use crate::ui::run::dump_screen;
    // Name resolution is not an authentication refusal, so it does not invite the
    // user to submit credentials that cannot reach the machine.
    use crate::ui::switcher::Switcher;
    let mut state = crate::state::State::from_hosts(vec!["jupiter00".into()]);
    let mut switcher = Switcher::from_hosts(&mut state);
    switcher.apply_host_result(
        "jupiter00".into(),
        Vec::new(),
        Some(
            "command failed (exit 255): ssh: Could not resolve hostname jupiter00: \
             No address associated with hostname"
                .into(),
        ),
        &mut state,
    );
    let out = dump_screen(
        &switcher,
        None,
        80,
        24,
        &state,
        &crate::ui::switcher::RenderPlan::default(),
    );
    assert!(
        out.contains("unreachable"),
        "an unresolved name stays a connectivity failure:\n{out}"
    );
    assert!(
        !out.contains("login required"),
        "it does not open the login pane:\n{out}"
    );
}

#[tokio::test]
async fn host_exited_with_no_sessions_marks_empty_not_unreachable() {
    use crate::ui::run::dump_screen;
    use crate::ui::switcher::Switcher;
    let mut state = crate::state::State::from_hosts(vec!["jupiter06".into()]);
    let mut switcher = Switcher::from_hosts(&mut state);
    let mut connected: HashSet<String> = HashSet::new();
    // A reachable machine whose mux has no server is empty, not unreachable.
    assert!(
        !note_host_exited(
            &mut switcher,
            &mut state,
            &mut connected,
            "jupiter06",
            Some("no sessions".into())
        ),
        "an empty mux is reachable, not unreachable"
    );
    let out = dump_screen(
        &switcher,
        None,
        80,
        24,
        &state,
        &crate::ui::switcher::RenderPlan::default(),
    );
    assert!(
        out.contains("no sessions"),
        "an empty host reads 'no sessions':\n{out}"
    );
    assert!(
        !out.contains("unreachable"),
        "must NOT read unreachable:\n{out}"
    );
}

#[tokio::test]
async fn host_exited_after_connect_keeps_tree() {
    use crate::ui::switcher::Switcher;
    let mut state = crate::state::State::from_hosts(vec!["jupiter06".into()]);
    let mut switcher = Switcher::from_hosts(&mut state);
    let mut connected: HashSet<String> = HashSet::new();
    connected.insert("jupiter06".into());
    assert!(
        !note_host_exited(&mut switcher, &mut state, &mut connected, "jupiter06", None),
        "an already-connected host is not marked unreachable on exit"
    );
    assert!(
        !connected.contains("jupiter06"),
        "exit must clear the connected mark so a failed reconnect can later resolve"
    );
}

#[tokio::test]
async fn refresh_after_a_dropped_host_resolves_instead_of_loading_forever() {
    // Bug: refresh → tree stuck on "loading…" forever. A once-connected host stays
    // pinned in `connected`, so every exit is a no-op; a refresh sets it scanning and
    // a reconnect that then fails never clears it. After the fix, the first drop keeps
    // the nav (no flash) but clears `connected`; a refresh + a failed reconnect (no
    // sessions) must resolve to "(empty)", not spin.
    use crate::ui::run::dump_screen;
    use crate::ui::switcher::Switcher;
    let mut state = crate::state::State::from_hosts(vec!["jupiter06".into()]);
    let mut switcher = Switcher::from_hosts(&mut state);
    let mut connected: HashSet<String> = HashSet::new();
    connected.insert("jupiter06".into());
    // First drop of the connected host: keeps last-known tree, clears connected.
    note_host_exited(&mut switcher, &mut state, &mut connected, "jupiter06", None);
    // User hits refresh → the host goes back to a scanning skeleton.
    switcher.request_rescan(&mut state);
    assert!(
        dump_screen(
            &switcher,
            None,
            80,
            24,
            &state,
            &crate::ui::switcher::RenderPlan::default()
        )
        .contains("scanning"),
        "scanning after refresh"
    );
    // The reconnect fails with "no sessions": it must resolve scanning → empty.
    note_host_exited(
        &mut switcher,
        &mut state,
        &mut connected,
        "jupiter06",
        Some("no sessions".into()),
    );
    let out = dump_screen(
        &switcher,
        None,
        80,
        24,
        &state,
        &crate::ui::switcher::RenderPlan::default(),
    );
    assert!(
        out.contains("no sessions"),
        "failed reconnect resolves to an empty host:\n{out}"
    );
    assert!(
        !out.contains("scanning"),
        "scanning must clear, not load forever:\n{out}"
    );
}

#[test]
fn prefix_s_toggles_state() {
    use crate::app::focus::Focus;
    let mut focus = Focus::default();
    assert!(focus.is_nav_focused());
    focus.toggle();
    assert_eq!(focus, Focus::Terminal);
    focus.toggle();
    assert!(focus.is_nav_focused());
}

// Suppress unused warnings for the test-only env builder kept for future loop tests.
#[test]
fn fake_env_builder_constructs() {
    let env = fake_env_with_machines(&["local", "jupiter06"]);
    assert_eq!(env.hosts().def_list().len(), 2);
}

#[test]
fn apply_inventory_effect_folds_sessions_into_host_inventory() {
    // C1: the control reader carries its parsed sessions on the HostEvent; the
    // loop folds them into the single owner (`model::Host.inventory`) and applies
    // them to the nav. There is no shared `Arc<Mutex<HostInventory>>` to read.
    use crate::ui::cards::Group;
    use crate::ui::switcher::{Scan, Switcher};

    let scan = Scan {
        groups: vec![Group {
            host: "jup".into(),
            err: None,
            sessions: vec![],
        }],
    };
    let mut state = crate::state::State::from_scan(scan);
    let switcher = Switcher::new(&mut state);
    let mut hosts = crate::model::Hosts::default();
    hosts.insert(crate::model::Host::new(
        crate::transport::ssh("jup".into(), String::new(), "linux".into()),
        crate::mux::for_binary("tmux").unwrap(),
    ));
    let mut rt = test_rt(fake_env_with_machines(&[]));
    rt.mgr.insert_fake("jup"); // a control client so the display attach has a sink
    rt.hosts = hosts;
    rt.model.state = state;
    rt.model.switcher = switcher;

    let sessions = vec![crate::session::Session {
        mux: String::new(),
        host: "jup".into(),
        name: "api".into(),
        ..Default::default()
    }];
    let outcome = rt.execute_effects(vec![Effect::Event(
        crate::model::EventEffect::ApplyInventory {
            host: "jup".into(),
            sessions: sessions.clone(),
        },
    )]);
    assert_eq!(
        outcome,
        (false, false, false),
        "ApplyInventory does not change loop signals"
    );
    // The single owner now holds the carried sessions - folded by the loop.
    let owned = &rt
        .hosts
        .get("jup")
        .expect("host present")
        .inventory
        .sessions;
    assert_eq!(owned.len(), 1, "sessions folded into model::Host.inventory");
    assert_eq!(owned[0].name, "api");
    // And the nav group reflects the same sessions.
    let group = rt
        .model
        .state
        .groups
        .iter()
        .find(|g| g.host == "jup")
        .expect("jup group");
    assert_eq!(group.sessions.len(), 1, "tree applied the carried sessions");
    assert_eq!(group.sessions[0].name, "api");
}

#[test]
fn inventory_rename_precedes_display_session_sync() {
    let (state, switcher) = with_switcher(one_session_scan());
    let mut rt = test_rt(fake_env_with_machines(&[]));
    rt.mgr.insert_fake("jup");
    rt.hosts.insert(crate::model::Host::new(
        crate::transport::ssh("jup".into(), String::new(), "linux".into()),
        crate::mux::for_binary("tmux").unwrap(),
    ));
    rt.model.state = state;
    rt.model.switcher = switcher;
    let renamed = vec![crate::session::Session {
        host: "jup".into(),
        name: "renamed".into(),
        mux: "tmux".into(),
        id: "7$0".into(),
        windows: 2,
        clients: 0,
        stopped: false,
    }];

    let (_, followups) = rt.perform_host_effect(crate::model::EventEffect::ApplyInventory {
        host: "jup".into(),
        sessions: renamed,
    });

    assert!(matches!(
        followups.as_slice(),
        [
            Effect::Event(crate::model::EventEffect::RenameDisplayed {
                host: renamed_host,
                from,
                to,
            }),
            Effect::Event(crate::model::EventEffect::SyncInventorySessions {
                host: synced_host,
                ..
            }),
        ] if renamed_host == "jup"
            && synced_host == "jup"
            && from == "api"
            && to == "renamed"
    ));
}

#[tokio::test]
async fn prefix_r_probes_the_selected_machine_without_a_discovery_pass() {
    // The one-machine re-scan asks that machine alone: one reachability probe, no roster
    // resolution and no probe of any other machine.
    use crate::session::Session;
    use crate::ui::cards::Group;
    use crate::ui::switcher::{Scan, Switcher};

    let group = |host: &str| Group {
        host: host.into(),
        err: None,
        sessions: vec![Session {
            mux: String::new(),
            id: String::new(),
            host: host.into(),
            name: "api".into(),
            windows: 1,
            clients: 0,
            stopped: false,
        }],
    };
    let mut state = crate::state::State::from_scan(Scan {
        groups: vec![group("jup"), group("sat")],
    });
    let switcher = Switcher::new(&mut state);
    let mut rt = test_rt(fake_env_with_machines(&[]));
    rt.model.state = state;
    rt.model.switcher = switcher;
    let selected = rt.model.switcher.current_host().unwrap();

    let mut width_changed = false;
    let _ = rt.handle_nav_bytes(b"\x07r", &mut width_changed);

    assert_eq!(rt.machine_rescans, std::slice::from_ref(&selected));
    assert_eq!(rt.discovery_runs, 0, "no full discovery pass");
    assert!(
        rt.model.state.scanning.len() == 1 && rt.model.state.scanning.contains(&selected),
        "only the selected machine is in flight: {:?}",
        rt.model.state.scanning
    );
    assert!(
        rt.model.state.groups.iter().all(|g| !g.sessions.is_empty()),
        "its cards stay while it is asked"
    );
}

#[tokio::test]
async fn capital_r_rescan_rebuilds_nav_and_kicks_discovery() {
    // The client-initiated `R` re-scan resets the nav to its scanning skeleton and
    // re-lists each host. Repeated `R` keys in one stdin read still form one pass.
    use crate::session::Session;
    use crate::ui::cards::Group;
    use crate::ui::switcher::{Scan, Switcher};

    let scan = Scan {
        groups: vec![Group {
            host: "jup".into(),
            err: None,
            sessions: vec![Session {
                mux: String::new(),
                id: String::new(),
                host: "jup".into(),
                name: "api".into(),
                windows: 1,
                clients: 0,
                stopped: false,
            }],
        }],
    };
    let mut state = crate::state::State::from_scan(scan);
    let switcher = Switcher::new(&mut state);

    // A detected CONTROL (tmux) host with a live control client sink.
    let mut hosts = crate::model::Hosts::default();
    let mut host = crate::model::Host::new(
        crate::transport::ssh("jup".into(), String::new(), "linux".into()),
        crate::mux::for_binary("tmux").unwrap(),
    );
    host.detected = true;
    hosts.insert(host);

    let mut rt = test_rt(fake_env_with_machines(&[]));
    rt.mgr.insert_fake("jup");
    rt.hosts = hosts;
    rt.model.state = state;
    rt.model.switcher = switcher;

    let mut width_changed = false;
    let _ = rt.handle_nav_bytes(b"\x07R", &mut width_changed);

    assert!(
        rt.model.state.groups.iter().all(|g| g.sessions.is_empty()),
        "the production nav input path cleared the loaded sessions"
    );
    assert!(
        rt.model.state.scanning.contains("jup"),
        "the production nav input path marked the host scanning"
    );
    assert_eq!(rt.discovery_runs, 1, "one read starts one discovery pass");

    rt.discovery_runs = 0;
    let _ = rt.handle_nav_bytes(b"\x07R\x07R", &mut width_changed);
    assert_eq!(
        rt.discovery_runs, 1,
        "repeated rescan keys in one read share one discovery pass"
    );
}

struct CreateRecordingOps {
    created: tokio::sync::mpsc::UnboundedSender<String>,
}

#[async_trait::async_trait]
impl crate::ui::switcher::Ops for CreateRecordingOps {
    fn hosts(&self) -> Vec<String> {
        Vec::new()
    }

    async fn list_sessions(&self, _host: &str) -> anyhow::Result<Vec<crate::session::Session>> {
        Ok(Vec::new())
    }

    async fn new_session(&self, host: &str, name: &str) -> anyhow::Result<crate::session::Session> {
        let _ = self.created.send(format!("{host}/{name}"));
        Ok(crate::session::Session {
            host: host.into(),
            name: name.into(),
            ..Default::default()
        })
    }

    async fn login_command(
        &self,
        _host: &str,
        _login: &crate::transport::Login,
        _password: String,
    ) -> anyhow::Result<Option<crate::transport::CommandSpec>> {
        Ok(None)
    }

    fn write_login_stanza(
        &self,
        _host: &str,
        _login: &crate::transport::Login,
    ) -> Result<(), String> {
        Ok(())
    }
    async fn register_login_key(
        &self,
        _host: &str,
        _login: &crate::transport::Login,
        _register: crate::ui::ops::KeyRegistration,
    ) -> crate::ui::ops::RegistrationOutcome {
        crate::ui::ops::RegistrationOutcome::NotRequested
    }
}

#[tokio::test]
async fn new_session_nav_input_spawns_the_create_op() {
    let mut rt = test_rt(fake_env_with_machines(&["local"]));
    let (created_tx, mut created_rx) = tokio::sync::mpsc::unbounded_channel();
    rt.ops = Arc::new(CreateRecordingOps {
        created: created_tx,
    });
    let mut width_changed = false;

    let _ = rt.handle_nav_bytes(b"\x07nwork\r", &mut width_changed);

    let created = tokio::time::timeout(std::time::Duration::from_secs(1), created_rx.recv())
        .await
        .expect("create op should be spawned")
        .expect("recording channel stays open");
    assert_eq!(created, "local/work");
}

#[test]
fn current_grid_returns_none_for_empty_displayed() {
    // An empty `displayed` (host "") misses `hosts.get`, so no driver is
    // built and no grid is produced - the blank-terminal case on first launch.
    let mut hosts = crate::model::Hosts::default();
    let mut registry = AttachRegistry::new();
    let (ptx, _prx) = tokio::sync::mpsc::unbounded_channel();
    let worker = crate::display::DisplayWorker::new(ptx);
    let (pty_tx, _pty_rx) = tokio::sync::mpsc::unbounded_channel::<PtyEvent>();
    let mut attach_seq = 0u64;
    let displayed = Selection::default();
    let mgr = HostManager::new(tokio::sync::mpsc::unbounded_channel().0);
    let grid = current_grid(
        &displayed,
        &crate::driver::DriverCtx {
            registry: &mut registry,
            hosts: &mut hosts,
            instance_name: "test",
            mgr: &mgr,
            worker: &worker,
            pty_tx: &pty_tx,
            attach_seq: &mut attach_seq,
            viewport: (31, 25),
        },
    );
    assert!(grid.is_none(), "empty displayed yields no grid");
}

#[test]
fn draw_observer_reports_change_only_on_new_fingerprint() {
    let mut obs = DrawObserver::default();
    // First paint of a key → a switch (INFO-grade transition, first frame).
    assert_eq!(obs.observe("jup/api", "api", 1), FpOutcome::Switched);
    // Same key, same fingerprint → unchanged (no event, no map update).
    assert_eq!(obs.observe("jup/api", "api", 1), FpOutcome::Unchanged);
    // Same key, same session, new fingerprint → steady-state repaint (TRACE).
    assert_eq!(obs.observe("jup/api", "api", 2), FpOutcome::Steady);
    // Same key, different session → a switch (INFO).
    assert_eq!(obs.observe("jup/api", "db", 3), FpOutcome::Switched);
}

#[tokio::test(flavor = "current_thread")]
async fn shared_host_reuses_one_attachment_and_in_flight_guards_current() {
    let mgr = HostManager::new(tokio::sync::mpsc::unbounded_channel().0);
    let mut hosts = crate::model::Hosts::default();
    hosts.insert(crate::model::Host::new(
        crate::transport::ssh("jup".into(), String::new(), "linux".into()),
        crate::mux::for_binary("tmux").unwrap(),
    ));
    let (ptx, _prx) = tokio::sync::mpsc::unbounded_channel();
    let worker = crate::display::DisplayWorker::new(ptx);
    let mut registry = AttachRegistry::new();
    let mut attach_seq = 0u64;
    // No control client registered ⇒ select_attach falls back to the dispatched-switch
    // path (this test exercises attach/in-flight latching, not the switch transport).
    let (pty_tx, _ptx_rx) = tokio::sync::mpsc::unbounded_channel::<PtyEvent>();

    let sel_a = Selection {
        host: "jup".into(),
        session: "a".into(),
    };
    let sel_b = Selection {
        host: "jup".into(),
        session: "b".into(),
    };

    // First attach (session a): requests off-loop, latches display.current[jup]=a, marks in-flight.
    assert!(select_attach(
        &sel_a,
        &mut crate::driver::DriverCtx {
            registry: &mut registry,
            hosts: &mut hosts,
            instance_name: "test",
            mgr: &mgr,
            worker: &worker,
            pty_tx: &pty_tx,
            attach_seq: &mut attach_seq,
            viewport: (31, 25),
        }
    ));
    assert_eq!(hosts.get("jup").unwrap().display.shows("jup"), Some("a"));
    assert!(
        hosts.get("jup").unwrap().display.in_flight_contains("jup"),
        "first attach is in flight"
    );

    // Select session b of the SAME host before a's Ready arrives: must NOT overwrite the
    // shown session (else the switch-client to b after a lands would never fire).
    assert!(select_attach(
        &sel_b,
        &mut crate::driver::DriverCtx {
            registry: &mut registry,
            hosts: &mut hosts,
            instance_name: "test",
            mgr: &mgr,
            worker: &worker,
            pty_tx: &pty_tx,
            attach_seq: &mut attach_seq,
            viewport: (31, 25),
        }
    ));
    assert_eq!(
        hosts.get("jup").unwrap().display.shows("jup"),
        Some("a"),
        "an in-flight attach must not latch the shown session to the new target"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn psmux_selection_replaces_the_single_display_attachment() {
    let mgr = HostManager::new(tokio::sync::mpsc::unbounded_channel().0);
    let mut hosts = crate::model::Hosts::default();
    hosts.insert(crate::model::Host::new(
        crate::transport::local(None),
        crate::mux::for_binary("psmux").unwrap(),
    ));
    let (ptx, _prx) = tokio::sync::mpsc::unbounded_channel();
    let mut worker = crate::display::DisplayWorker::with_spawner(
        ptx,
        Box::new(|_argv, _cols, _rows, id, _events, _env_clear| {
            Ok(crate::display::attachment::fake_attachment(id))
        }),
    );
    let mut registry = AttachRegistry::new();
    let mut attach_seq = 0u64;
    let (pty_tx, _ptx_rx) = tokio::sync::mpsc::unbounded_channel::<PtyEvent>();

    let sel_test2 = Selection {
        host: "local".into(),
        session: "test2".into(),
    };
    let sel_test = Selection {
        host: "local".into(),
        session: "test".into(),
    };

    assert!(select_attach(
        &sel_test2,
        &mut crate::driver::DriverCtx {
            registry: &mut registry,
            hosts: &mut hosts,
            instance_name: "test",
            mgr: &mgr,
            worker: &worker,
            pty_tx: &pty_tx,
            attach_seq: &mut attach_seq,
            viewport: (31, 25),
        }
    ));
    let ready = tokio::time::timeout(std::time::Duration::from_millis(100), worker.recv())
        .await
        .expect("worker replies")
        .expect("ready");
    if let crate::display::DisplayEvent::Ready {
        seq,
        key,
        attachment,
    } = ready
    {
        let h = hosts.get_mut("local").unwrap();
        let id = attachment.id();
        assert!(
            matches!(
                h.display
                    .resolve_ready(&key, seq, id, false, None, std::time::Instant::now()),
                crate::model::ReadyOutcome::Install { .. }
            ),
            "the current reply installs"
        );
        registry.insert(&key, attachment);
    } else {
        panic!("expected ready");
    }
    assert!(registry.contains("local"), "psmux display is keyed by host");
    assert_eq!(
        hosts.get("local").unwrap().display.shows("local"),
        Some("test2")
    );

    assert!(select_attach(
        &sel_test,
        &mut crate::driver::DriverCtx {
            registry: &mut registry,
            hosts: &mut hosts,
            instance_name: "test",
            mgr: &mgr,
            worker: &worker,
            pty_tx: &pty_tx,
            attach_seq: &mut attach_seq,
            viewport: (31, 25),
        }
    ));

    let h = hosts.get("local").unwrap();
    assert_eq!(h.display.shows("local"), Some("test"));
    assert!(h.display.in_flight_contains("local"));
    assert!(
        registry.contains("local"),
        "old psmux display attach is HELD on screen until the reattach paints"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn psmux_select_attach_does_not_trust_stale_display_bookkeeping() {
    let mgr = HostManager::new(tokio::sync::mpsc::unbounded_channel().0);
    let mut hosts = crate::model::Hosts::default();
    hosts.insert(crate::model::Host::new(
        crate::transport::local(None),
        crate::mux::for_binary("psmux").unwrap(),
    ));
    hosts
        .get_mut("local")
        .unwrap()
        .display
        .set_shows("local", "target");

    let (ptx, _prx) = tokio::sync::mpsc::unbounded_channel();
    let worker = crate::display::DisplayWorker::with_spawner(
        ptx,
        Box::new(|_argv, _cols, _rows, id, _events, _env_clear| {
            Ok(crate::display::attachment::fake_attachment(id))
        }),
    );
    let mut registry = AttachRegistry::new();
    registry.insert("local", crate::display::attachment::fake_attachment(99));
    let mut attach_seq = 0u64;
    let (pty_tx, _ptx_rx) = tokio::sync::mpsc::unbounded_channel::<PtyEvent>();

    let sel = Selection {
        host: "local".into(),
        session: "target".into(),
    };

    assert!(select_attach(
        &sel,
        &mut crate::driver::DriverCtx {
            registry: &mut registry,
            hosts: &mut hosts,
            instance_name: "test",
            mgr: &mgr,
            worker: &worker,
            pty_tx: &pty_tx,
            attach_seq: &mut attach_seq,
            viewport: (31, 25),
        }
    ));

    let h = hosts.get("local").unwrap();
    assert!(h.display.in_flight_contains("local"));
    assert!(
        registry.contains("local"),
        "psmux select_attach requests a reattach while holding the prior grid until paint"
    );
}

#[test]
fn should_attach_fires_on_change_and_never_storms_in_flight() {
    let a = Selection {
        host: "h".into(),
        session: "api".into(),
    };
    let b = Selection {
        session: "db".into(),
        ..a.clone()
    };
    let gate = |selection: &Selection, displayed: &Selection, in_flight, astray| {
        let s = crate::state::State {
            selection: selection.clone(),
            displayed: displayed.clone(),
            ..crate::state::State::default()
        };
        s.should_attach(in_flight, astray)
    };
    // Settled: displayed == selection, nothing in flight → no attach. Whether that
    // session's display PTY is still alive does not enter: a PTY that died is reported,
    // never answered with another connection, so the gate has no reason to read it.
    assert!(!gate(&a, &a, false, false));
    // Selection moved off the displayed session → attach.
    assert!(gate(&b, &a, false, false));
    // An attach for the key is already in flight → never re-fire (no storm).
    assert!(!gate(&b, &a, true, false));
    // Everything xmux itself recorded agrees - the selection is what it last put on
    // screen - and the client is on another session anyway. Only the astray leg can see
    // it, and this is the split it closes.
    assert!(gate(&a, &a, false, true));
    // Even then, not on top of an attach already carrying the display there.
    assert!(!gate(&a, &a, true, true));
}

#[tokio::test(flavor = "current_thread")]
async fn psmux_select_attach_supersedes_in_flight_attach() {
    let mgr = HostManager::new(tokio::sync::mpsc::unbounded_channel().0);
    let mut hosts = crate::model::Hosts::default();
    hosts.insert(crate::model::Host::new(
        crate::transport::local(None),
        crate::mux::for_binary("psmux").unwrap(),
    ));
    hosts
        .get_mut("local")
        .unwrap()
        .display
        .mark_in_flight("local", 7);

    let (ptx, _prx) = tokio::sync::mpsc::unbounded_channel();
    let worker = crate::display::DisplayWorker::with_spawner(
        ptx,
        Box::new(|_argv, _cols, _rows, id, _events, _env_clear| {
            Ok(crate::display::attachment::fake_attachment(id))
        }),
    );
    let mut registry = AttachRegistry::new();
    let mut attach_seq = 7u64;
    let (pty_tx, _ptx_rx) = tokio::sync::mpsc::unbounded_channel::<PtyEvent>();

    let sel = Selection {
        host: "local".into(),
        session: "target".into(),
    };

    assert!(select_attach(
        &sel,
        &mut crate::driver::DriverCtx {
            registry: &mut registry,
            hosts: &mut hosts,
            instance_name: "test",
            mgr: &mgr,
            worker: &worker,
            pty_tx: &pty_tx,
            attach_seq: &mut attach_seq,
            viewport: (31, 25),
        }
    ));

    let h = hosts.get("local").unwrap();
    assert_eq!(h.display.in_flight_seq("local"), Some(8));
}

/// A headless `Runtime` for exercising the `&mut self` arm/effect methods: a fake
/// attach worker (no real PTYs), dropped receiver halves, hosts built from `env`.
/// A test overrides the fields it cares about (`rt.hosts`, `rt.model.state`, ...).
#[tokio::test]
async fn first_frame_does_not_wait_for_startup_roster_and_applies_its_answer() {
    let mut env = fake_env_from(crate::provision::env::Roster::default());
    env.startup_pending = true;
    let env = std::sync::Arc::new(env);
    let (mut rt, mut io) = Runtime::new(env);
    assert_eq!(cards(&rt), vec!["local"], "the first frame has a skeleton");
    assert!(rt.model.state.machine_scanning.contains("local"));
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    spawn_startup_resolution_with(
        rt.mgr.events(),
        async move {
            release_rx.await.expect("release startup resolution");
            Some(StartupResolution {
                roster: fake_roster(&["local", "stage"]),
                own_session: None,
                force_askpass: true,
            })
        },
        async { Some(fake_roster(&["local", "stage", "neighbor"])) },
    );

    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 25)).unwrap();
    rt.prepare_and_draw(&mut term);
    assert!(!rt.dirty, "the initial frame was painted");
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(10), io.host_rx.recv())
            .await
            .is_err(),
        "the roster provider is still pending"
    );

    release_tx.send(()).unwrap();
    let event = tokio::time::timeout(std::time::Duration::from_secs(1), io.host_rx.recv())
        .await
        .expect("startup resolution completed")
        .expect("startup event");
    rt.on_host_event(event, &mut io.host_rx);
    assert!(
        rt.hosts.def("stage").is_some(),
        "the answer reached the app"
    );
    // The full roster follows the quick one on the same task (one event batch may carry
    // both) and adds what only the neighbor scan names, keeping every machine the quick
    // answer put on screen.
    while rt.hosts.def("neighbor").is_none() {
        let event = tokio::time::timeout(std::time::Duration::from_secs(1), io.host_rx.recv())
            .await
            .expect("full roster completed")
            .expect("full roster event");
        rt.on_host_event(event, &mut io.host_rx);
    }
    assert!(
        rt.hosts.def("neighbor").is_some(),
        "the full roster adds the neighbor"
    );
    assert!(
        rt.hosts.def("stage").is_some(),
        "and keeps the quick answer's hosts"
    );
}

#[tokio::test]
async fn a_re_scan_roster_adds_a_machine_it_now_names() {
    // The point of re-resolving on a re-scan: a machine that was not reachable at launch
    // (a tailnet peer that has since come online, a machine the user just wrote into the
    // config) turns into a card without a restart.
    let mut rt = test_rt(fake_env_with_machines(&["prod"]));
    assert!(rt.hosts.get("stage").is_none(), "nothing knows stage yet");
    rt.execute_host_effect_for_test(crate::model::EventEffect::ApplyRoster {
        roster: Box::new(fake_roster(&["prod", "stage"])),
        startup: None,
        rescan: false,
    });
    assert!(
        rt.hosts.get("stage").is_some(),
        "the loop's registry has it"
    );
    assert!(
        rt.hosts.def("stage").is_some(),
        "and so do the off-loop ops, which resolve a host through the registry"
    );
    assert!(
        rt.model.state.groups.iter().any(|g| g.host == "stage"),
        "and it has a card"
    );
    assert!(
        rt.hosts.get("prod").is_some(),
        "a machine that was already there is untouched"
    );
}

#[tokio::test]
async fn a_re_scan_after_a_mux_edit_keeps_the_loop_and_the_operations_on_one_mux() {
    // Config now names zellij for a host that stands as tmux. A surviving host keeps
    // its live `Host`, and the operations read that same `Host`, so a new session lands on
    // the mux the card lists rather than on one it never enumerates.
    let mut rt = test_rt(fake_env_with_machines(&["prod"]));
    let mut roster = fake_roster(&["prod"]);
    roster.cfg.machines[0].mux = "zellij".into();
    rt.execute_host_effect_for_test(crate::model::EventEffect::ApplyRoster {
        roster: Box::new(roster),
        startup: None,
        rescan: false,
    });
    let standing = rt.hosts.get("prod").unwrap().mux.bin().to_string();
    assert_eq!(rt.hosts.def("prod").unwrap().binary, standing);
}

#[tokio::test]
async fn a_re_scan_roster_drops_a_machine_it_stopped_naming() {
    // The mirror case: the config turned a provider off, or a peer went offline. The
    // registry and the nav have to let go, or the nav paints a card nothing can reach.
    let mut rt = test_rt(fake_env_with_machines(&["prod", "stage"]));
    assert!(rt.hosts.get("stage").is_some(), "precondition");
    rt.model.connected.insert("stage".into());
    rt.model.detecting.insert("stage".into());
    rt.execute_host_effect_for_test(crate::model::EventEffect::ApplyRoster {
        roster: Box::new(fake_roster(&["prod"])),
        startup: None,
        rescan: false,
    });
    assert!(rt.hosts.get("stage").is_none(), "the registry let go");
    assert!(rt.hosts.def("stage").is_none(), "the off-loop ops let go");
    assert!(
        !rt.model.state.groups.iter().any(|g| g.host == "stage"),
        "and the card is gone"
    );
    assert!(!rt.model.connected.contains("stage"));
    assert!(!rt.model.detecting.contains("stage"));
    assert!(rt.hosts.get("prod").is_some(), "prod is still named");
}

#[tokio::test]
async fn a_discovered_mux_becomes_a_host_on_the_spot() {
    // The whole point of discovering asynchronously: the machine's answer arrives after
    // the app is up, and the mux nobody wrote down turns into a card RIGHT THEN.
    let mut rt = test_rt(fake_env_with_machines(&["prod"]));
    assert!(
        rt.hosts.get("prod:zellij").is_none(),
        "nothing knows about zellij yet"
    );
    rt.execute_host_effect_for_test(crate::model::EventEffect::AddDiscoveredHosts {
        machine: "prod".into(),
        muxes: Ok(vec!["tmux".into(), "zellij".into()]),
    });
    // tmux is what `prod` was already painted as, so it is left exactly as it is: its
    // BARE id is what the frozen order, the saved selection, and anything the user typed
    // are keyed to.
    assert!(rt.hosts.get("prod").is_some(), "the bare id is untouched");
    assert!(
        rt.hosts.get("prod:tmux").is_none(),
        "the mux it already serves is not added a second time"
    );
    // zellij is new, so it becomes its own host under a qualified id, scanning.
    let h = rt.hosts.get("prod:zellij").expect("the discovered host");
    assert_eq!(h.mux.kind(), "zellij");
    assert_eq!(h.transport.host_id(), "prod:zellij", "it answers as itself");
    // And the OFF-LOOP ops resolve it: they look a host up in the set the registry
    // publishes, so the discovered host is there without a second registration.
    let src = rt
        .hosts
        .def("prod:zellij")
        .expect("the off-loop ops know the discovered host");
    assert_eq!(
        src.binary, "zellij",
        "and reach it with zellij's own binary"
    );
    assert_eq!(
        src.host().transport.host_id(),
        "prod:zellij",
        "over the same machine the loop's host uses"
    );
    assert!(
        rt.model
            .state
            .groups
            .iter()
            .any(|g| g.host == "prod:zellij"),
        "and it has a card: {:?}",
        rt.model
            .state
            .groups
            .iter()
            .map(|g| &g.host)
            .collect::<Vec<_>>()
    );
    assert!(
        rt.model.state.scanning.contains("prod:zellij"),
        "the card reads scanning until its first result"
    );
    // Idempotent: the same answer twice adds nothing.
    let before = rt.model.state.groups.len();
    rt.execute_host_effect_for_test(crate::model::EventEffect::AddDiscoveredHosts {
        machine: "prod".into(),
        muxes: Ok(vec!["tmux".into(), "zellij".into()]),
    });
    assert_eq!(rt.model.state.groups.len(), before, "no duplicate card");
}

#[tokio::test]
async fn a_discovered_host_sorts_into_place_and_leaves_the_selection_put() {
    // A card the user is looking at must not move because another machine answered:
    // the discovered card sorts into its name position, and the selection stays put.
    let mut rt = test_rt(fake_env_with_machines(&["prod", "db"]));
    let selected = {
        let t = rt.model.switcher.terminal_view_target();
        (t.host, t.target)
    };
    rt.execute_host_effect_for_test(crate::model::EventEffect::AddDiscoveredHosts {
        machine: "db".into(),
        muxes: Ok(vec!["zellij".into()]),
    });
    let after: Vec<String> = rt
        .model
        .state
        .groups
        .iter()
        .map(|g| g.host.clone())
        .collect();
    assert_eq!(
        after,
        vec!["local", "db", "db:zellij", "prod"],
        "the discovered card sorts into name order"
    );
    let now = rt.model.switcher.terminal_view_target();
    assert_eq!((now.host, now.target), selected, "the selection stays put");
}

/// Every card id on the nav, in card order: each host, and each machine standing on its
/// own under its name.
fn cards(rt: &Runtime) -> Vec<String> {
    let state = &rt.model.state;
    let mut ids: Vec<String> = state
        .groups
        .iter()
        .map(|g| g.host.clone())
        .chain(
            state
                .hostless_machines()
                .into_iter()
                .map(|m| m.name.clone()),
        )
        .collect();
    ids.sort_by(|a, b| crate::ui::cards::card_order(a, b));
    ids
}

#[tokio::test]
async fn a_machine_that_writes_no_mux_is_one_card_with_no_host() {
    // Nothing is assumed about a machine that left its muxes to xmux: it is a card that
    // reads the machine alone and spins, and it has no host for any op to reach.
    let rt = test_rt(fake_env_with_auto_machines(&[], &["win"]));
    assert_eq!(cards(&rt), vec!["local", "win"]);
    assert!(
        rt.model.state.machine_scanning.contains("win"),
        "the card spins"
    );
    assert!(
        rt.model.state.groups.iter().all(|g| g.host != "win"),
        "the machine is not a host"
    );
    assert!(rt.hosts.get("win").is_none(), "no mux is assumed for it");
    assert!(rt.hosts.def("win").is_none());
    assert_eq!(
        rt.hosts.machines(),
        vec!["local", "win"],
        "the machine is still probed"
    );
}

#[tokio::test]
async fn a_windows_machine_serving_psmux_is_one_psmux_card() {
    // psmux installs a `tmux` alias of itself. Only the machine's own answer decides what it
    // serves, and it answers psmux alone, so it is one card, on psmux's own binary.
    let mut rt = test_rt(fake_env_with_auto_machines(&[], &["win"]));
    rt.execute_host_effect_for_test(crate::model::EventEffect::AddDiscoveredHosts {
        machine: "win".into(),
        muxes: Ok(vec!["psmux".into()]),
    });
    assert_eq!(
        cards(&rt),
        vec!["local", "win"],
        "one card, under the machine's name"
    );
    let h = rt.hosts.get("win").expect("the answered host");
    assert_eq!((h.mux.kind(), h.mux.bin()), ("psmux", "psmux"));
    assert!(
        h.detected,
        "the answer came from psmux's own identity probe"
    );
    assert_eq!(
        rt.hosts.def("win").expect("the ops know it").binary,
        "psmux"
    );
    assert!(
        rt.model.state.scanning.contains("win"),
        "the card spins until its first listing"
    );
}

/// Answers every command with one session name and records each argv it ran.
struct NamingRunner {
    commands: std::sync::Mutex<Vec<Vec<String>>>,
}

#[async_trait::async_trait]
impl crate::model::host_def::Runner for NamingRunner {
    crate::model::host_def::runner_spec_via_argv!();
    async fn run(
        &self,
        name: &str,
        args: &[String],
    ) -> Result<Vec<u8>, crate::model::host_def::RunError> {
        let mut argv = vec![name.to_string()];
        argv.extend(args.iter().cloned());
        self.commands.lock().unwrap().push(argv);
        Ok(b"api
"
        .to_vec())
    }
}

#[tokio::test]
async fn a_host_mux_discovery_added_accepts_a_new_session() {
    // The mux answers after the app is up, so the host reaches only the runtime
    // registry. The off-loop operations resolve hosts through that same registry, so
    // creating a session on it works without the host being registered anywhere else.
    let mut rt = test_rt(fake_env_with_auto_machines(&[], &["win"]));
    let runner = std::sync::Arc::new(NamingRunner {
        commands: Default::default(),
    });
    rt.hosts.set_runner(runner.clone());
    rt.execute_host_effect_for_test(crate::model::EventEffect::AddDiscoveredHosts {
        machine: "win".into(),
        muxes: Ok(vec!["psmux".into()]),
    });
    let session = rt
        .ops
        .new_session("win", "api")
        .await
        .expect("the discovered host accepts the operation");
    assert_eq!(
        (session.host.as_str(), session.name.as_str()),
        ("win", "api")
    );
    let commands = runner.commands.lock().unwrap();
    assert!(
        commands
            .last()
            .is_some_and(|argv| argv.iter().any(|a| a.contains("psmux new-session"))),
        "the create ran over the discovered mux: {commands:?}"
    );
}

#[tokio::test]
async fn a_machine_answering_several_muxes_has_a_card_for_each() {
    let mut rt = test_rt(fake_env_with_auto_machines(&[], &["win"]));
    rt.execute_host_effect_for_test(crate::model::EventEffect::AddDiscoveredHosts {
        machine: "win".into(),
        muxes: Ok(vec!["psmux".into(), "zellij".into()]),
    });
    assert_eq!(
        cards(&rt),
        vec!["local", "win:psmux", "win:zellij"],
        "each mux names itself, and the card that stood for the machine is gone"
    );
}

/// A runtime whose one remote machine `win` has no host known yet and refused its scan
/// for a reason a login answers.
fn hostless_machine_needing_login_rt() -> Runtime {
    let mut rt = test_rt(fake_env_with_auto_machines(&[], &["win"]));
    rt.model.switcher.apply_machine_result(
        "win",
        Some("alice@win: Permission denied (publickey,password).".into()),
        &mut rt.model.state,
    );
    rt
}

/// The login on `win` works and its machine answers two muxes, each of which lists a
/// session: the card that stood for the machine gives way to a card per host.
fn log_in_and_discover_two_hosts(rt: &mut Runtime) {
    update(
        &mut rt.model,
        Msg::LoginSettled {
            host: "win".into(),
            credential_held: true,
        },
    );
    rt.execute_host_effect_for_test(crate::model::EventEffect::AddDiscoveredHosts {
        machine: "win".into(),
        muxes: Ok(vec!["zellij".into(), "psmux".into()]),
    });
    for (host, name) in [("win:psmux", "api"), ("win:zellij", "logs")] {
        let session = crate::session::Session {
            host: host.into(),
            name: name.into(),
            windows: 1,
            ..Default::default()
        };
        rt.model
            .switcher
            .apply_host_result(host.into(), vec![session], None, &mut rt.model.state);
    }
}

/// Asserts the selection is on the machine `win`, whose screen fills the terminal view
/// and links both hosts the login found.
fn assert_on_the_machine_screen_with_both_hosts(rt: &mut Runtime) {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let machine = crate::model::Node::Machine("win".into());
    assert_eq!(rt.model.switcher.selected_node(), Some(machine.clone()));
    assert_eq!(
        rt.model.switcher.current_view_screen(&rt.model.state),
        Some(crate::model::ViewScreen::Machine)
    );
    let links: Vec<_> = rt
        .model
        .switcher
        .screen_links(&machine, &rt.model.state)
        .into_iter()
        .filter_map(|l| l.node().cloned())
        .collect();
    assert_eq!(
        links,
        vec![
            crate::model::Node::Host("win:psmux".into()),
            crate::model::Node::Host("win:zellij".into()),
        ]
    );
    assert!(rt
        .model
        .switcher
        .current_attach_target(&rt.model.state)
        .is_none());
    rt.cols = 140;
    rt.body_rows = 30;
    rt.last_draw = std::time::Instant::now() - std::time::Duration::from_secs(1);
    let mut term = Terminal::new(TestBackend::new(rt.cols, rt.body_rows + 1)).unwrap();
    rt.prepare_and_draw(&mut term);
    let out = drawn_text(&term);
    assert!(
        out.contains("machine win"),
        "the machine screen stays:\n{out}"
    );
    assert!(!out.contains("host win/"), "no host screen opens:\n{out}");
    for mux in ["psmux", "zellij"] {
        assert!(
            out.lines()
                .any(|l| l.contains(mux) && l.contains("1 session")),
            "the screen links the {mux} host:\n{out}"
        );
    }
}

#[tokio::test]
async fn a_working_login_opens_the_machine_screen_with_its_hosts_scanning() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let mut rt = hostless_machine_needing_login_rt();
    assert!(rt.model.switcher.open_host("win", &mut rt.model.state));
    assert_eq!(
        rt.model.switcher.current_view_screen(&rt.model.state),
        Some(crate::model::ViewScreen::Login)
    );
    update(
        &mut rt.model,
        Msg::LoginSettled {
            host: "win".into(),
            credential_held: true,
        },
    );
    assert_eq!(
        rt.model.switcher.current_view_screen(&rt.model.state),
        Some(crate::model::ViewScreen::Machine),
        "the login pane leaves as soon as the login worked"
    );
    rt.cols = 140;
    rt.body_rows = 30;
    rt.last_draw = std::time::Instant::now() - std::time::Duration::from_secs(1);
    let mut term = Terminal::new(TestBackend::new(rt.cols, rt.body_rows + 1)).unwrap();
    rt.prepare_and_draw(&mut term);
    let out = drawn_text(&term);
    let spin = crate::ui::spinner_glyph(rt.model.state.chrome.spinner_frame);
    assert!(out.contains("machine win"), "{out}");
    assert!(
        out.lines()
            .any(|l| l.contains("hosts") && l.contains(&format!("{spin} scanning"))),
        "the hosts row states the scan with the spinner:
{out}"
    );
    assert!(
        !out.contains("authenticate"),
        "no login step is left:
{out}"
    );
}

#[tokio::test]
async fn a_login_that_reveals_hosts_keeps_the_machine_selected() {
    let mut rt = hostless_machine_needing_login_rt();
    assert!(rt.model.switcher.open_host("win", &mut rt.model.state));
    assert_eq!(
        rt.model.switcher.selected_node(),
        Some(crate::model::Node::Machine("win".into()))
    );
    log_in_and_discover_two_hosts(&mut rt);
    assert_on_the_machine_screen_with_both_hosts(&mut rt);
}

#[tokio::test]
async fn a_login_from_the_landing_that_reveals_hosts_keeps_the_machine_selected() {
    let mut rt = hostless_machine_needing_login_rt();
    rt.model.switcher.open_landing();
    sync_test_render_plan(&mut rt);
    rt.handle_stdin_bytes(b"\x1b[B", &Selection::default());
    assert_eq!(
        rt.model.switcher.selected_node(),
        Some(crate::model::Node::Machine("win".into()))
    );
    rt.handle_stdin_bytes(b"\r", &Selection::default());
    assert!(
        !rt.model.switcher.landing_open(),
        "Enter executes the machine"
    );
    log_in_and_discover_two_hosts(&mut rt);
    assert_on_the_machine_screen_with_both_hosts(&mut rt);
}

#[tokio::test]
async fn a_login_on_the_preselected_landing_card_keeps_the_machine_selected() {
    // The machine is the only card, so the launch preselects it and Enter executes it
    // without a move: the execution, not a move, makes it the user's choice, and the
    // first session card the login reveals does not take the selection.
    let mut roster = auto_roster(&[], &["win"]);
    roster.local_muxes.clear();
    let mut rt = test_rt(fake_env_from(roster));
    rt.model.switcher.apply_machine_result(
        "win",
        Some("alice@win: Permission denied (publickey,password).".into()),
        &mut rt.model.state,
    );
    rt.model.switcher.open_landing();
    sync_test_render_plan(&mut rt);
    assert_eq!(
        rt.model.switcher.selected_node(),
        Some(crate::model::Node::Machine("win".into()))
    );
    rt.handle_stdin_bytes(b"\r", &Selection::default());
    assert!(
        !rt.model.switcher.landing_open(),
        "Enter executes the machine"
    );
    log_in_and_discover_two_hosts(&mut rt);
    assert_on_the_machine_screen_with_both_hosts(&mut rt);
}

#[tokio::test]
async fn a_logout_gathers_the_hosts_back_onto_the_machine_card() {
    let mut rt = hostless_machine_needing_login_rt();
    rt.model.switcher.open_host("win", &mut rt.model.state);
    log_in_and_discover_two_hosts(&mut rt);
    for host in ["win:psmux", "win:zellij"] {
        rt.model.switcher.apply_host_result(
            host.into(),
            Vec::new(),
            Some(crate::model::LOGGED_OUT.into()),
            &mut rt.model.state,
        );
    }
    assert_eq!(
        rt.model.switcher.selected_node(),
        Some(crate::model::Node::Machine("win".into()))
    );
    assert!(matches!(
        rt.model.switcher.selected_card(),
        Some(crate::state::RowRef::Machine { machine, .. }) if machine == "win"
    ));
}

#[tokio::test]
async fn a_machine_where_no_mux_answers_has_no_card() {
    // The machine connected and answered nothing, so there is nothing to show: it has no
    // card, exactly as this box has no local card when nothing is installed here.
    let mut rt = test_rt(fake_env_with_auto_machines(&[], &["win"]));
    rt.execute_host_effect_for_test(crate::model::EventEffect::AddDiscoveredHosts {
        machine: "win".into(),
        muxes: Ok(Vec::new()),
    });
    assert_eq!(cards(&rt), vec!["local"]);
    assert!(rt.hosts.get("win").is_none());
}

#[tokio::test]
async fn a_machine_that_could_not_be_asked_keeps_its_card_with_the_reason() {
    // A connection that failed while the machine was being asked says nothing about what
    // it serves, so the card stays, settled, and says why; it is not taken for a machine with
    // nothing installed.
    let mut rt = test_rt(fake_env_with_auto_machines(&[], &["win"]));
    rt.execute_host_effect_for_test(crate::model::EventEffect::AddDiscoveredHosts {
        machine: "win".into(),
        muxes: Err("command failed (exit 255): Connection reset".into()),
    });
    assert_eq!(cards(&rt), vec!["local", "win"]);
    assert!(!rt.model.state.machine_scanning.contains("win"), "settled");
    let m = rt.model.state.machine("win").unwrap();
    assert!(m.err.as_deref().unwrap().contains("Connection reset"));
    assert!(rt.model.state.groups.iter().all(|g| g.host != "win"));
    // Asked again (a re-scan or a login), it answers, and its host takes the card over
    // as in flight rather than inheriting the failure.
    rt.execute_host_effect_for_test(crate::model::EventEffect::AddDiscoveredHosts {
        machine: "win".into(),
        muxes: Ok(vec!["psmux".into()]),
    });
    let g = rt
        .model
        .state
        .groups
        .iter()
        .find(|g| g.host == "win")
        .unwrap();
    assert!(g.err.is_none());
    assert!(rt.model.state.scanning.contains("win"));
}

#[tokio::test]
async fn a_failed_ask_leaves_a_machine_that_serves_hosts_alone() {
    let mut rt = test_rt(fake_env_with_machines(&["prod"]));
    rt.execute_host_effect_for_test(crate::model::EventEffect::AddDiscoveredHosts {
        machine: "prod".into(),
        muxes: Err("timed out".into()),
    });
    let g = rt
        .model
        .state
        .groups
        .iter()
        .find(|g| g.host == "prod")
        .unwrap();
    assert!(g.err.is_none(), "its own host reports for it");
    assert!(rt.model.state.scanning.contains("prod"));
}

#[tokio::test]
async fn the_card_of_a_machine_with_no_host_says_how_the_machine_is_reached() {
    let mut rt = test_rt(fake_env_with_machines(&["prod"]));
    rt.execute_host_effect_for_test(crate::model::EventEffect::ApplyRoster {
        roster: Box::new(auto_roster(&["prod"], &["win"])),
        startup: None,
        rescan: false,
    });
    let reach = rt
        .model
        .state
        .chrome
        .host_reach
        .get("win")
        .expect("the machine has a reach entry");
    assert!(reach.machine.contains("win"), "{reach:?}");
    assert!(!reach.probe.is_empty(), "the reachability probe is shown");
    assert!(
        reach.mux.is_empty() && reach.kind.is_empty(),
        "no mux is named"
    );
}

#[tokio::test]
async fn a_host_found_on_a_machine_is_reached_as_the_machine_is() {
    // The machine's probe read its shell family before it was asked for its muxes, so the
    // host it answered with composes its first command for that family.
    let mut rt = test_rt(fake_env_with_auto_machines(&[], &["win"]));
    rt.hosts.for_each_transport_of("win", |t| {
        t.set_remote_shell(crate::transport::vocab::RemoteShell::Other)
    });
    rt.execute_host_effect_for_test(crate::model::EventEffect::AddDiscoveredHosts {
        machine: "win".into(),
        muxes: Ok(vec!["psmux".into()]),
    });
    let h = rt.hosts.get("win").unwrap();
    assert_eq!(
        h.transport.remote_shell(),
        crate::transport::vocab::RemoteShell::Other
    );
    assert_eq!(
        h.transport.host_id(),
        "win",
        "it answers as its own host id"
    );
}

#[tokio::test]
async fn a_re_scan_keeps_what_a_machine_that_writes_no_mux_answered() {
    // The fresh roster names the machine and none of its hosts, since those came from its
    // own answer. The registry keeps them, so a re-scan tears no card down.
    let mut rt = test_rt(fake_env_with_auto_machines(&[], &["win"]));
    rt.execute_host_effect_for_test(crate::model::EventEffect::AddDiscoveredHosts {
        machine: "win".into(),
        muxes: Ok(vec!["psmux".into()]),
    });
    rt.execute_host_effect_for_test(crate::model::EventEffect::ApplyRoster {
        roster: Box::new(auto_roster(&[], &["win"])),
        startup: None,
        rescan: false,
    });
    assert!(rt.hosts.get("win").is_some(), "the registry keeps it");
    assert!(rt.hosts.def("win").is_some(), "the off-loop ops keep it");
    assert_eq!(cards(&rt), vec!["local", "win"], "and the card stays put");
}

#[tokio::test]
async fn a_re_scan_adds_and_drops_the_card_of_a_machine_that_writes_no_mux() {
    let mut rt = test_rt(fake_env_with_machines(&["prod"]));
    rt.execute_host_effect_for_test(crate::model::EventEffect::ApplyRoster {
        roster: Box::new(auto_roster(&["prod"], &["win"])),
        startup: None,
        rescan: false,
    });
    assert_eq!(cards(&rt), vec!["local", "prod", "win"]);
    assert!(rt.model.state.machine_scanning.contains("win"));
    rt.execute_host_effect_for_test(crate::model::EventEffect::ApplyRoster {
        roster: Box::new(fake_roster(&["prod"])),
        startup: None,
        rescan: false,
    });
    assert_eq!(cards(&rt), vec!["local", "prod"]);
    assert!(!rt.hosts.machines().contains(&"win".to_string()));
}

fn test_rt(env: Env) -> Runtime {
    let env = std::sync::Arc::new(env);
    let (host_tx, _host_rx) = tokio::sync::mpsc::unbounded_channel();
    let mgr = HostManager::new(host_tx);
    let (wtx, _wrx) = tokio::sync::mpsc::unbounded_channel::<PtyEvent>();
    let worker = DisplayWorker::with_spawner_and_runner(
        wtx,
        Box::new(|_argv, _cols, _rows, id, _events, _env_clear| {
            Ok(crate::display::attachment::fake_attachment(id))
        }),
        crate::mux::herdr_attach_runner(),
    );
    let (pty_tx, _pty_rx) = tokio::sync::mpsc::unbounded_channel::<PtyEvent>();
    let roster = env.roster();
    let hosts = crate::model::Hosts::build(
        &roster.cfg,
        &roster.ssh_aliases,
        &roster.wsl_distros,
        "windows",
        &roster.local_muxes,
        &env.xmux_dir,
        env.local_socket.clone(),
    );
    drop(roster);
    let mut state = crate::state::State::from_roster(hosts.ids().to_vec(), hosts.machines());
    let switcher = crate::ui::switcher::Switcher::from_hosts(&mut state);
    let ops = env.ops(hosts.defs());
    let (op_tx, _op_rx) = tokio::sync::mpsc::unbounded_channel();
    let prefix = crate::display::term::parse_prefix(Some(&env.ui_prefix));
    let model = AppModel {
        switcher,
        render_plan: crate::ui::switcher::RenderPlan::default(),
        state,
        nav_width: crate::ui::switcher::NAV_WIDTH,
        nav_width_natural: crate::ui::switcher::NAV_WIDTH,
        nav_collapsed: false,
        nav_height: 0,
        nav_position: crate::ui::switcher::NavPosition::Left,
        nav_position_pinned: None,
        nav_default: crate::ui::switcher::NavPosition::Left,
        max_fps: crate::provision::config::DEFAULT_MAX_FPS,
        floating_rect: None,
        floating_lock_until: None,
        floating_drag: None,
        applied_nav_height: u16::MAX,
        applied_nav_collapsed: true,
        auto_hide_nav: false,
        nav_was_focused: true,
        mouse_state: MouseState::default(),
        connected: HashSet::new(),
        detecting: HashSet::new(),
        config_last_mtime: None,
        width_dirty: false,
        width_flush_at: None,
        rescan: None,
        logout: None,
        running_logins: Vec::new(),
        saved_logins: HashMap::new(),
    };
    let mut rt = Runtime {
        instance_name: "test".into(),
        env,
        ops,
        hosts,
        mgr,
        registry: AttachRegistry::new(),
        worker,
        model,
        scan_pool: std::sync::Arc::new(tokio::sync::Semaphore::new(
            crate::provision::config::SCAN_CONCURRENCY_MAX,
        )),
        attach_seq: 0,
        driver_pty_tx: pty_tx,
        op_tx,
        key_gates: Default::default(),
        cols: 80,
        body_rows: 24,
        term_input: crate::display::input::TermInput::new(prefix),
        nav_decoder: crate::display::decode::KeyDecoder::new(),
        paste: Default::default(),
        window_focused: true,
        child_focus: None,
        keyboard_pushed: false,
        keyboard_flags: 0,
        prefix,
        draw_observer: DrawObserver::default(),
        images: Default::default(),
        kitty_images: Default::default(),
        spinner_start: std::time::Instant::now(),
        dirty: true,
        clear_pending: false,
        last_draw: std::time::Instant::now(),
        cursor_shape: 0,
        display_sync_held: false,
        rescan_pending: false,
        display_probe: DisplayProbe::default(),
        held_input: None,
        passthrough: Vec::new(),
        title: None,
        discovery_runs: 0,
        machine_rescans: Vec::new(),
    };
    sync_test_render_plan(&mut rt);
    rt
}

fn sync_test_render_plan(rt: &mut Runtime) {
    let area = ratatui::layout::Rect::new(0, 0, rt.cols, rt.body_rows.saturating_add(1));
    let nav = rt.nav_size();
    rt.model.render_plan =
        rt.model
            .switcher
            .layout(area, nav, &rt.model.state, &rt.model.render_plan);
}

#[test]
fn execute_commands_runs_quit_and_attach_in_one_batch() {
    let mut rt = test_rt(fake_env_with_machines(&["local"]));
    rt.dirty = false;
    let selection = Selection {
        host: "local".into(),
        session: "work".into(),
    };

    let outcome = rt.execute_commands(vec![
        crate::model::Command::Quit,
        crate::model::Command::Attach(selection),
    ]);

    assert_eq!(outcome, (true, false));
    assert_eq!(rt.attach_seq, 1, "the attach reaches the display driver");
    assert!(rt.dirty, "the attach marks the frame dirty");
}

#[test]
fn host_event_and_command_run_through_the_same_executor() {
    let mut rt = test_rt(fake_env_with_machines(&["local"]));
    let mut effects = update(
        &mut rt.model,
        Msg::HostEvent {
            event: HostEvent::DisplayTty {
                host: "local".to_owned(),
                tty: Some("/dev/pts/41".to_owned()),
            },
            logged_in: HashSet::new(),
        },
    );
    effects.extend(update(
        &mut rt.model,
        Msg::Action(crate::model::Action::Quit),
    ));

    let outcome = rt.execute_effects(effects);

    assert_eq!(outcome, (true, false, false));
    assert_eq!(
        rt.hosts.get("local").unwrap().display_tty.0.as_deref(),
        Some("/dev/pts/41")
    );
}

#[tokio::test]
async fn rescan_discovery_waits_for_the_batch_boundary() {
    let mut rt = test_rt(fake_env_with_machines(&[]));
    let effects = update(
        &mut rt.model,
        Msg::Commands(vec![
            crate::model::Command::Rescan,
            crate::model::Command::Quit,
        ]),
    );

    let outcome = rt.execute_effects(effects);

    assert_eq!(outcome, (true, false, false));
    assert!(rt.rescan_pending);
    assert_eq!(
        rt.discovery_runs, 0,
        "the executor finishes the command batch before discovery"
    );

    rt.flush_rescan();
    assert!(!rt.rescan_pending);
    assert_eq!(rt.discovery_runs, 1);
}

#[tokio::test]
async fn full_scan_uses_the_probe_already_running_for_a_selected_machine() {
    let rt = test_rt(fake_env_with_machines(&["local"]));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    probe_machines(&rt.hosts, tx.clone(), &rt.scan_pool, true, Some("local"));
    assert!(rx.try_recv().is_err(), "the machine is not probed twice");
    probe_machines(&rt.hosts, tx, &rt.scan_pool, true, None);
    assert!(matches!(
        rx.try_recv(),
        Ok(HostEvent::MachineProbed { machine, .. }) if machine == "local"
    ));
}

#[test]
fn coalesced_nav_keys_observe_each_preceding_model_transition() {
    let mut rt = test_rt(fake_env_with_machines(&["local"]));
    let mut width_changed = false;

    let (_, quit, _, _, _, _) = rt.handle_nav_bytes(b"\x07?q", &mut width_changed);

    assert!(!quit, "the help modal owns the following q");
    assert!(matches!(
        rt.model.state.modal,
        Some(crate::state::Modal::Help { .. })
    ));
}

#[test]
fn tick_commands_persist_and_attach_through_the_runtime_executor() {
    let dir = std::env::temp_dir().join(format!(
        "xmux-command-executor-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let mut env = fake_env_with_machines(&["local"]);
    env.xmux_dir = dir.clone();
    let mut rt = test_rt(env);
    let selection = Selection {
        host: "local".into(),
        session: "work".into(),
    };
    let started = std::time::Instant::now();

    rt.dispatch_action(crate::model::Action::Select(selection));
    rt.drive_attach_beat(started);
    rt.drive_attach_beat(started + std::time::Duration::from_millis(100));

    assert_eq!(
        std::fs::read_to_string(dir.join("last_session")).unwrap(),
        "local\nwork"
    );
    assert_eq!(rt.attach_seq, 1, "the settled selection also attaches");
    let _ = std::fs::remove_dir_all(dir);
}

fn detach_test_hosts(alias: &str) -> crate::model::Hosts {
    let mut hosts = crate::model::Hosts::default();
    hosts.insert(crate::model::Host::new(
        crate::transport::ssh(alias.to_string(), String::new(), "linux".into()),
        crate::mux::for_binary("tmux").unwrap(),
    ));
    hosts
}

#[tokio::test(flavor = "current_thread")]
async fn invalidated_ssh_auth_reaps_display_and_pending_attach() {
    let mut rt = test_rt(fake_env_with_machines(&[]));
    rt.hosts = detach_test_hosts("jup");
    rt.registry.insert_fake("jup", 7);
    rt.hosts
        .get_mut("jup")
        .unwrap()
        .display
        .mark_in_flight("jup", 9);
    rt.execute_host_effect_for_test(crate::model::EventEffect::DisconnectMachine {
        machine: "jup".into(),
    });
    assert!(!rt.registry.contains("jup"));
    assert!(rt.hosts.get("jup").unwrap().display.in_flight_is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn display_auth_tracks_the_live_host_connection_across_session_switches() {
    let mut rt = test_rt(fake_env_with_machines(&[]));
    rt.hosts = detach_test_hosts("jup");
    rt.registry.insert_fake("jup", 7);
    let (_tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    rt.on_pty_event(
        PtyEvent::AuthObserved {
            id: 7,
            method: crate::model::AuthMethod::PublicKey,
        },
        &mut rx,
    );
    assert_eq!(
        rt.model.state.display_auth_methods.get("jup"),
        Some(&crate::model::AuthMethod::PublicKey)
    );
    rt.hosts
        .get_mut("jup")
        .unwrap()
        .display
        .set_shows("jup", "other");
    assert_eq!(
        rt.model.state.display_auth_methods.get("jup"),
        Some(&crate::model::AuthMethod::PublicKey)
    );
    rt.registry
        .park_pending("jup", crate::display::attachment::fake_attachment(8));
    rt.on_pty_event(
        PtyEvent::AuthObserved {
            id: 8,
            method: crate::model::AuthMethod::Password,
        },
        &mut rx,
    );
    assert_eq!(
        rt.model.state.display_auth_methods.get("jup"),
        Some(&crate::model::AuthMethod::PublicKey)
    );
    rt.registry.remove_pending("jup");
    rt.hosts
        .get_mut("jup")
        .unwrap()
        .display
        .mark_in_flight("jup", 9);
    rt.on_display_event(DisplayEvent::Ready {
        seq: 9,
        key: "jup".into(),
        attachment: crate::display::attachment::fake_attachment(9),
    });
    assert!(rt.promote_due_pending(std::time::Instant::now() + std::time::Duration::from_secs(10)));
    assert!(!rt.model.state.display_auth_methods.contains_key("jup"));
    rt.on_pty_event(
        PtyEvent::AuthObserved {
            id: 7,
            method: crate::model::AuthMethod::Password,
        },
        &mut rx,
    );
    assert!(!rt.model.state.display_auth_methods.contains_key("jup"));
    rt.on_pty_event(
        PtyEvent::AuthObserved {
            id: 9,
            method: crate::model::AuthMethod::Password,
        },
        &mut rx,
    );
    assert_eq!(
        rt.model.state.display_auth_methods.get("jup"),
        Some(&crate::model::AuthMethod::Password)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn display_tty_event_records_on_the_owning_host() {
    let mut hosts = detach_test_hosts("jup");
    let mut registry = AttachRegistry::new();
    registry.insert_fake("jup", 7); // Shared key == host id
    record_display_tty(&mut hosts, &registry, 7, "/dev/pts/3".into());
    assert_eq!(
        hosts.get("jup").unwrap().display_tty.0.as_deref(),
        Some("/dev/pts/3"),
        "the captured tty lands on the host that owns the attach id"
    );
    // An id with no attachment is ignored (no panic, no write).
    record_display_tty(&mut hosts, &registry, 999, "/dev/pts/9".into());
    assert_eq!(
        hosts.get("jup").unwrap().display_tty.0.as_deref(),
        Some("/dev/pts/3")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn client_detached_matching_our_tty_reaps_display_and_rearms() {
    let mut state = crate::state::State::from_hosts(vec!["jup".into()]);
    let switcher = crate::ui::switcher::Switcher::from_hosts(&mut state);
    let mut rt = test_rt(fake_env_with_machines(&[]));
    rt.hosts = detach_test_hosts("jup");
    rt.model.state = state;
    rt.model.switcher = switcher;

    rt.hosts.get_mut("jup").unwrap().display_tty =
        crate::model::DisplayTty(Some("/dev/pts/3".into()));
    rt.registry.insert_fake("jup", 7); // live attach under key = host id (Shared)
    assert!(rt.registry.contains("jup"));

    // An UNRELATED client detaches → inert.
    let rearm = rt.handle_host_event(HostEvent::ClientDetached {
        host: "jup".into(),
        client: "/dev/pts/9".into(),
    });
    assert!(!rearm, "an unrelated client's detach must not rearm");
    assert!(
        rt.registry.contains("jup"),
        "an unrelated client's detach must not reap our attach"
    );
    assert_eq!(
        rt.hosts.get("jup").unwrap().display_tty.0.as_deref(),
        Some("/dev/pts/3"),
        "an unrelated detach must not clear our captured tty"
    );

    // OUR display client (the captured tty) detaches → reap + rearm.
    let rearm = rt.handle_host_event(HostEvent::ClientDetached {
        host: "jup".into(),
        client: "/dev/pts/3".into(),
    });
    assert!(rearm, "our own client's detach must rearm recovery");
    assert!(
        !rt.registry.contains("jup"),
        "our display attach is reaped so it cannot persist dead"
    );
    assert!(
        rt.hosts.get("jup").unwrap().display_tty.0.is_none(),
        "the dead client's tty is forgotten so no later switch-client targets it"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn client_session_changed_matching_our_tty_syncs_display_belief() {
    // The mux moved a client to another session (e.g. the user pressed prefix+s in the
    // terminal view). When that client is OUR display attach (its tty == Host.display_tty),
    // sync the display belief so the next reconcile's show() guard dispatches NO switch-client;
    // a third party's own client can never match, so it is inert.
    let mut state = crate::state::State::from_hosts(vec!["jup".into()]);
    let switcher = crate::ui::switcher::Switcher::from_hosts(&mut state);
    let mut rt = test_rt(fake_env_with_machines(&[]));
    rt.hosts = detach_test_hosts("jup");
    rt.model.state = state;
    rt.model.switcher = switcher;

    rt.hosts.get_mut("jup").unwrap().display_tty =
        crate::model::DisplayTty(Some("/dev/pts/3".into()));
    // The one per-host PTY (Shared key == host id) is currently believed on session "api".
    rt.hosts
        .get_mut("jup")
        .unwrap()
        .display
        .set_shows("jup", "api");

    // An UNRELATED client switched sessions → inert: our display belief is untouched.
    rt.handle_host_event(HostEvent::ClientSessionChanged {
        host: "jup".into(),
        client: "/dev/pts/9".into(),
        session: "db".into(),
    });
    assert_eq!(
        rt.hosts.get("jup").unwrap().display.shows("jup"),
        Some("api"),
        "an unrelated client's switch must not move our display belief"
    );

    // OUR display client (the captured tty) switched to "db" via the mux → sync the belief.
    rt.handle_host_event(HostEvent::ClientSessionChanged {
        host: "jup".into(),
        client: "/dev/pts/3".into(),
        session: "db".into(),
    });
    assert_eq!(
        rt.hosts.get("jup").unwrap().display.shows("jup"),
        Some("db"),
        "our own client's mux-driven switch syncs the display belief to the new session"
    );
}

/// The command lines a recording control client received, in order, leaving out the
/// session listing a client report also refetches.
fn sent_lines(commands: &std::sync::mpsc::Receiver<crate::link::HostCmd>) -> Vec<String> {
    let listing = crate::link::test_control_proto().list_sessions_line();
    commands
        .try_iter()
        .filter_map(|cmd| match cmd {
            crate::link::HostCmd::Send(line) => Some(line),
            crate::link::HostCmd::Query { line, .. } => Some(line),
            crate::link::HostCmd::Shutdown => None,
        })
        .filter(|line| *line != listing)
        .collect()
}

#[tokio::test(flavor = "current_thread")]
async fn every_client_change_asks_who_shares_the_displayed_session() {
    // A client the user attaches to the session xmux shows, or one leaving it, changes
    // whether xmux's display client may size that session. Each report asks which clients
    // are attached to the session the display client is on, the new one when the report
    // is the display client's own move.
    let mut rt = test_rt(fake_env_with_machines(&[]));
    rt.hosts = detach_test_hosts("jup");
    let commands = rt.mgr.insert_recording("jup");
    rt.hosts.get_mut("jup").unwrap().display_tty =
        crate::model::DisplayTty(Some("/dev/pts/3".into()));
    rt.hosts
        .get_mut("jup")
        .unwrap()
        .display
        .set_shows("jup", "api");
    let proto = crate::link::test_control_proto();

    rt.handle_host_event(HostEvent::ClientSessionChanged {
        host: "jup".into(),
        client: "/dev/pts/9".into(),
        session: "api".into(),
    });
    assert_eq!(
        sent_lines(&commands),
        vec![proto.session_clients_line("api")],
        "a user's client arriving on the shown session asks about that session"
    );

    rt.handle_host_event(HostEvent::ClientSessionChanged {
        host: "jup".into(),
        client: "/dev/pts/3".into(),
        session: "db".into(),
    });
    assert_eq!(
        sent_lines(&commands),
        vec![proto.session_clients_line("db")],
        "the display client's own move asks about the session it moved to"
    );

    rt.handle_host_event(HostEvent::ClientDetached {
        host: "jup".into(),
        client: "/dev/pts/9".into(),
    });
    assert_eq!(
        sent_lines(&commands),
        vec![proto.session_clients_line("db")],
        "a user's client leaving asks again"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn asking_who_shares_the_displayed_session_waits_for_the_display_tty() {
    // Without its tty the display client cannot be named, so nothing is sent and it keeps
    // the ignore-size its attach set; the captured tty then asks.
    let mut rt = test_rt(fake_env_with_machines(&[]));
    rt.hosts = detach_test_hosts("jup");
    let commands = rt.mgr.insert_recording("jup");
    rt.hosts
        .get_mut("jup")
        .unwrap()
        .display
        .set_shows("jup", "api");
    let proto = crate::link::test_control_proto();

    rt.handle_host_event(HostEvent::ClientDetached {
        host: "jup".into(),
        client: "/dev/pts/9".into(),
    });
    assert!(sent_lines(&commands).is_empty(), "no tty, nothing to name");

    rt.handle_host_event(HostEvent::DisplayTty {
        host: "jup".into(),
        tty: Some("/dev/pts/3".into()),
    });
    assert_eq!(
        sent_lines(&commands),
        vec![proto.session_clients_line("api")],
        "the captured tty asks about the session the display client shows"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn the_answer_sets_whether_the_display_client_sizes_its_session() {
    // A session another window-sizing client shares keeps that client's size, so xmux's
    // display client yields; alone, it sizes the session. An answer about a client that is
    // no longer the display client changes nothing.
    let mut rt = test_rt(fake_env_with_machines(&[]));
    rt.hosts = detach_test_hosts("jup");
    let commands = rt.mgr.insert_recording("jup");
    rt.hosts.get_mut("jup").unwrap().display_tty =
        crate::model::DisplayTty(Some("/dev/pts/3".into()));
    let proto = crate::link::test_control_proto();

    rt.handle_host_event(HostEvent::DisplaySessionClients {
        host: "jup".into(),
        display_tty: "/dev/pts/3".into(),
        shared: true,
    });
    assert_eq!(
        sent_lines(&commands),
        proto.display_size_lines("/dev/pts/3", true),
        "a shared session keeps the user's size"
    );

    rt.handle_host_event(HostEvent::DisplaySessionClients {
        host: "jup".into(),
        display_tty: "/dev/pts/3".into(),
        shared: false,
    });
    assert_eq!(
        sent_lines(&commands),
        proto.display_size_lines("/dev/pts/3", false),
        "alone, the display client sizes its session"
    );

    rt.handle_host_event(HostEvent::DisplaySessionClients {
        host: "jup".into(),
        display_tty: "/dev/pts/7".into(),
        shared: false,
    });
    assert!(
        sent_lines(&commands).is_empty(),
        "an answer about a client that is no longer the display client is dropped"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_client_session_change_before_the_tty_is_known_lands_once_it_is_captured() {
    // A remote attach records its tty on the machine before it execs the mux client, so the
    // capture made as the attach starts can find nothing, and the mux reports the client
    // before xmux knows it is its own. The report is kept until the tty is captured; only
    // xmux's own client's report then moves the display belief.
    let mut rt = test_rt(fake_env_with_machines(&[]));
    rt.hosts = detach_test_hosts("jup");
    rt.registry.insert_fake("jup", 7);
    rt.hosts
        .get_mut("jup")
        .unwrap()
        .display
        .set_shows("jup", "api");

    rt.handle_host_event(HostEvent::ClientSessionChanged {
        host: "jup".into(),
        client: "/dev/pts/3".into(),
        session: "db".into(),
    });
    rt.handle_host_event(HostEvent::ClientSessionChanged {
        host: "jup".into(),
        client: "/dev/pts/9".into(),
        session: "web".into(),
    });
    assert_eq!(
        rt.hosts.get("jup").unwrap().display.shows("jup"),
        Some("api"),
        "a report about a client not yet known to be ours moves nothing"
    );

    rt.handle_host_event(HostEvent::DisplayTty {
        host: "jup".into(),
        tty: Some("/dev/pts/3".into()),
    });
    assert_eq!(
        rt.hosts.get("jup").unwrap().display.shows("jup"),
        Some("db"),
        "once the tty is captured, our own client's earlier move is recorded"
    );
}

/// A host `jup` with two loaded sessions: `api` and `db`.
/// Lets a follow test assert the selection lands on the mux-moved session's card.
fn two_session_scan() -> crate::ui::switcher::Scan {
    use crate::session::Session;
    use crate::ui::cards::Group;
    use crate::ui::switcher::Scan;
    let sess = |name: &str, windows: i64| Session {
        mux: String::new(),
        id: String::new(),
        host: "jup".into(),
        name: name.into(),
        windows,
        clients: 0,
        stopped: false,
    };
    Scan {
        groups: vec![Group {
            host: "jup".into(),
            err: None,
            sessions: vec![sess("api", 1), sess("db", 2)],
        }],
    }
}

/// Settles the world the way a launch settles it: the selection derived from the nav,
/// the display truth agreeing with it, and no attach owed for it. A pass over this world
/// does nothing at all, which is what lets a test say that what happens next is the
/// scenario and not the launch catching up.
fn settled(rt: &mut Runtime) {
    sync_selection_from_switcher(&mut rt.model);
    rt.model.state.displayed = rt.model.state.selection.clone();
    rt.model.state.attach_pending = false;
    rt.model.state.attach_deadline = None;
}

/// One pass of the loop top, minus the drawing a headless test has no terminal for:
/// the two regions are reconciled against each other, the selection settles, and the
/// attach beat runs at `now`. Passing the SAME `now` to several passes holds the clock
/// still, so the debounce cannot elapse between them.
fn one_pass(rt: &mut Runtime, now: std::time::Instant) {
    rt.follow_selection_to_display();
    sync_selection_from_switcher(&mut rt.model);
    rt.drive_attach_beat(now);
}

#[tokio::test(flavor = "current_thread")]
async fn a_mux_side_switch_in_terminal_focus_moves_the_nav_to_that_session() {
    // With the terminal focused the user is driving the mux (prefix+s), so the session
    // they moved to is where they want to be: the NAV goes to its card, and the client
    // stays where it is.
    let mut state = crate::state::State::from_scan(two_session_scan());
    let mut switcher = crate::ui::switcher::Switcher::new(&mut state);
    switcher.select_address(&crate::session::Address::new("jup", "api")); // deterministic start (ignore any last_session)
    let mut rt = test_rt(fake_env_with_machines(&[]));
    rt.hosts = detach_test_hosts("jup");
    rt.hosts.get_mut("jup").unwrap().display_tty =
        crate::model::DisplayTty(Some("/dev/pts/3".into()));
    rt.hosts
        .get_mut("jup")
        .unwrap()
        .display
        .set_shows("jup", "api");
    rt.registry.insert_fake("jup", 7);
    rt.model.state = state;
    rt.model.switcher = switcher;
    rt.model
        .state
        .focus
        .set_view_focus(crate::app::focus::ViewFocus::Terminal);
    settled(&mut rt);
    let t0 = std::time::Instant::now();
    assert_eq!(
        rt.model.switcher.terminal_view_target().target,
        "api",
        "selection starts on api"
    );

    rt.handle_host_event(HostEvent::ClientSessionChanged {
        host: "jup".into(),
        client: "/dev/pts/3".into(),
        session: "db".into(),
    });
    one_pass(&mut rt, t0);
    assert_eq!(
        rt.model.switcher.terminal_view_target().target,
        "db",
        "the terminal-focused nav follows the mux switch to db's card"
    );
    assert_eq!(
        rt.hosts.get("jup").unwrap().display.shows("jup"),
        Some("db"),
        "the two regions name one session: the client is where the nav now is"
    );
}

/// The sessions `jup` answers with, in nav order, for the enumeration that carries a
/// session created after its switch was already seen.
fn jup_sessions(names: &[&str]) -> Vec<crate::session::Session> {
    names
        .iter()
        .map(|name| crate::session::Session {
            mux: String::new(),
            id: String::new(),
            host: "jup".into(),
            name: (*name).into(),
            windows: 1,
            clients: 0,
            stopped: false,
        })
        .collect()
}

#[tokio::test(flavor = "current_thread")]
async fn a_switch_onto_a_session_with_no_card_yet_moves_the_nav_when_its_card_appears() {
    // A session created moments ago has no card, so the move has nowhere to go on the
    // pass that first sees it. Nothing is remembered about that: the client is still on
    // the session, every later pass compares against where it is, and the first pass
    // after the enumeration that brings the card in moves the nav there.
    let mut state = crate::state::State::from_scan(two_session_scan());
    let mut switcher = crate::ui::switcher::Switcher::new(&mut state);
    switcher.select_address(&crate::session::Address::new("jup", "api"));
    let mut rt = test_rt(fake_env_with_machines(&[]));
    rt.hosts = detach_test_hosts("jup");
    rt.hosts.get_mut("jup").unwrap().display_tty =
        crate::model::DisplayTty(Some("/dev/pts/3".into()));
    rt.hosts
        .get_mut("jup")
        .unwrap()
        .display
        .set_shows("jup", "api");
    rt.registry.insert_fake("jup", 7);
    rt.model.state = state;
    rt.model.switcher = switcher;
    rt.model
        .state
        .focus
        .set_view_focus(crate::app::focus::ViewFocus::Terminal);
    settled(&mut rt);
    let t0 = std::time::Instant::now();

    rt.handle_host_event(HostEvent::ClientSessionChanged {
        host: "jup".into(),
        client: "/dev/pts/3".into(),
        session: "ops".into(),
    });
    one_pass(&mut rt, t0);
    assert_eq!(
        rt.model.switcher.terminal_view_target().target,
        "api",
        "there is no ops card to move to yet"
    );

    rt.handle_host_event(HostEvent::Sessions {
        host: "jup".into(),
        sessions: jup_sessions(&["api", "db", "ops"]),
        err: None,
    });
    one_pass(&mut rt, t0);
    assert_eq!(
        rt.model.switcher.terminal_view_target().target,
        "ops",
        "the card the enumeration brought in is where the nav goes"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_card_appearing_for_a_session_the_client_has_left_moves_nothing() {
    // The client sits on one session at a time and the comparison is against where it is
    // NOW, so a card arriving for a session it passed through moves nothing. There is no
    // older move to arrive late, because no move was ever written down.
    let mut state = crate::state::State::from_scan(two_session_scan());
    let mut switcher = crate::ui::switcher::Switcher::new(&mut state);
    switcher.select_address(&crate::session::Address::new("jup", "api"));
    let mut rt = test_rt(fake_env_with_machines(&[]));
    rt.hosts = detach_test_hosts("jup");
    rt.hosts.get_mut("jup").unwrap().display_tty =
        crate::model::DisplayTty(Some("/dev/pts/3".into()));
    rt.hosts
        .get_mut("jup")
        .unwrap()
        .display
        .set_shows("jup", "api");
    rt.registry.insert_fake("jup", 7);
    rt.model.state = state;
    rt.model.switcher = switcher;
    rt.model
        .state
        .focus
        .set_view_focus(crate::app::focus::ViewFocus::Terminal);
    settled(&mut rt);
    let t0 = std::time::Instant::now();

    for session in ["ops", "db"] {
        rt.handle_host_event(HostEvent::ClientSessionChanged {
            host: "jup".into(),
            client: "/dev/pts/3".into(),
            session: session.into(),
        });
        one_pass(&mut rt, t0);
    }
    assert_eq!(
        rt.model.switcher.terminal_view_target().target,
        "db",
        "the nav is on the session the client ended up on"
    );

    rt.handle_host_event(HostEvent::Sessions {
        host: "jup".into(),
        sessions: jup_sessions(&["api", "db", "ops"]),
        err: None,
    });
    one_pass(&mut rt, t0);
    assert_eq!(
        rt.model.switcher.terminal_view_target().target,
        "db",
        "the ops card appearing moves nothing: the client left ops"
    );
}

/// The id of the psmux client xmux itself spawned, so a test can say whether a pass kept
/// it or replaced it.
const OWN_CLIENT: u64 = 42;

/// A local psmux host holding sessions `a` and `b`.
fn psmux_scan() -> crate::ui::switcher::Scan {
    use crate::session::Session;
    use crate::ui::cards::Group;
    let sess = |name: &str| Session {
        mux: String::new(),
        id: String::new(),
        host: "local".into(),
        name: name.into(),
        windows: 1,
        clients: 0,
        stopped: false,
    };
    crate::ui::switcher::Scan {
        groups: vec![Group {
            host: "local".into(),
            err: None,
            sessions: vec![sess("a"), sess("b")],
        }],
    }
}

/// Puts a live client on the display key that answers `session` for the variable psmux
/// carries its session in. Replacing the entry with one answering another session is what
/// a psmux client does to itself when it moves: the same client, a rewritten environment,
/// and no server anywhere the wiser.
fn the_client_reports(rt: &mut Runtime, id: u64, session: &str) {
    rt.registry.insert(
        "local",
        crate::display::attachment::fake_attachment_answering_env(
            id,
            "PSMUX_SESSION_NAME",
            session,
        ),
    );
}

/// The settled world before anything happens: a local psmux host, the nav on `a`, and
/// xmux's own display client live on `a` and answering for the session it is on the way a
/// real psmux client does. Everything agrees, so no pass has anything to do.
fn a_settled_psmux_runtime() -> Runtime {
    let mut state = crate::state::State::from_scan(psmux_scan());
    let mut switcher = crate::ui::switcher::Switcher::new(&mut state);
    switcher.select_address(&crate::session::Address::new("local", "a"));
    let mut rt = test_rt(fake_env_with_machines(&[]));
    let mut hosts = crate::model::Hosts::default();
    hosts.insert(crate::model::Host::new(
        crate::transport::local(None),
        crate::mux::for_binary("psmux").unwrap(),
    ));
    rt.hosts = hosts;
    rt.model.state = state;
    rt.model.switcher = switcher;
    rt.hosts
        .get_mut("local")
        .unwrap()
        .display
        .set_shows("local", "a");
    the_client_reports(&mut rt, OWN_CLIENT, "a");
    settled(&mut rt);
    rt
}

/// Completes the reattach a pass requested: the worker's Ready reply installs the fresh
/// client, which answers for the session it was attached to.
fn the_reattach_lands(rt: &mut Runtime, session: &str) {
    let seq = rt
        .hosts
        .get("local")
        .unwrap()
        .display
        .in_flight_seq("local")
        .expect("a reattach is in flight");
    rt.on_display_event(DisplayEvent::Ready {
        seq,
        key: "local".into(),
        attachment: crate::display::attachment::fake_attachment_answering_env(
            OWN_CLIENT + 1,
            "PSMUX_SESSION_NAME",
            session,
        ),
    });
}

#[tokio::test(flavor = "current_thread")]
async fn a_settled_display_attaches_nothing_however_long_it_runs() {
    // The floor under the two tests below: with the nav and the client on one session,
    // passes at any spacing issue nothing at all. A reconcile acting on a difference it
    // imagined would show up here as a spawn.
    let mut rt = a_settled_psmux_runtime();
    let t0 = std::time::Instant::now();
    for ms in [0, 200, 3_000, 11_000] {
        one_pass(&mut rt, t0 + std::time::Duration::from_millis(ms));
    }
    assert!(
        rt.hosts.get("local").unwrap().display.in_flight_is_empty(),
        "nothing is attached while the two already name one session"
    );
    assert_eq!(
        rt.registry.get("local").map(|a| a.id()),
        Some(OWN_CLIENT),
        "the client on screen is the one that was there"
    );
}

// --- zellij display detach → automatic reattach --------------------------------
// zellij is per-session and polled: nothing pushes a detach, so the mirrored
// client's death arrives only as the attachment's PTY hitting EOF. These pin the
// recovery of that EOF while the session stays selected.

/// A local zellij host with the nav loaded from a scan of `a` and `b`.
fn zellij_scan() -> crate::ui::switcher::Scan {
    let sess = |name: &str| crate::session::Session {
        host: "local".into(),
        name: name.into(),
        mux: "zellij".into(),
        id: String::new(),
        windows: 1,
        clients: 0,
        stopped: false,
    };
    crate::ui::switcher::Scan {
        groups: vec![crate::ui::cards::Group {
            host: "local".into(),
            err: None,
            sessions: vec![sess("a"), sess("b")],
        }],
    }
}

/// The settled zellij world: the nav on `a`, xmux's own display client live on `a`,
/// and everything agreeing - so what a test does next is the scenario, not the
/// launch catching up.
fn a_settled_zellij_runtime() -> Runtime {
    let mut state = crate::state::State::from_scan(zellij_scan());
    let mut switcher = crate::ui::switcher::Switcher::new(&mut state);
    switcher.select_address(&crate::session::Address::new("local", "a"));
    let mut rt = test_rt(fake_env_with_machines(&[]));
    let mut hosts = crate::model::Hosts::default();
    hosts.insert(crate::model::Host::new(
        crate::transport::local(None),
        crate::mux::for_binary("zellij").unwrap(),
    ));
    rt.hosts = hosts;
    rt.model.state = state;
    rt.model.switcher = switcher;
    rt.hosts
        .get_mut("local")
        .unwrap()
        .display
        .set_shows("local", "a");
    rt.registry.insert(
        "local",
        crate::display::attachment::fake_attachment(OWN_CLIENT),
    );
    settled(&mut rt);
    rt
}

/// Delivers a machine-side query's answer about attachment `id`'s client.
fn the_query_answers(rt: &mut Runtime, id: u64, session: Option<&str>) {
    let (_tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    rt.on_pty_event(
        PtyEvent::DisplayClientSession {
            id,
            at: session.map(|session| crate::mux::ClientAt::Session(session.to_string())),
        },
        &mut rx,
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_queried_zellij_switch_in_terminal_focus_moves_the_nav() {
    // The user ran `zellij action switch-session b` inside xmux's own client on a machine
    // whose client cannot be read here, and the machine-side query found that client on
    // `b`. The nav goes to `b`, and the client the user moved stays on screen.
    let mut rt = a_settled_zellij_runtime();
    rt.model
        .state
        .focus
        .set_view_focus(crate::app::focus::ViewFocus::Terminal);
    rt.display_probe.in_flight = true;
    let t0 = std::time::Instant::now();

    the_query_answers(&mut rt, OWN_CLIENT, Some("b"));
    assert!(!rt.display_probe.in_flight, "the answer ends the query");
    for _ in 0..3 {
        one_pass(&mut rt, t0);
    }
    one_pass(&mut rt, t0 + std::time::Duration::from_secs(1));

    assert_eq!(rt.model.state.selection.session, "b");
    assert_eq!(
        rt.registry.get("local").map(|a| a.id()),
        Some(OWN_CLIENT),
        "the moved client is kept, not reattached"
    );
    assert!(rt.hosts.get("local").unwrap().display.in_flight_is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn a_query_answer_about_another_client_or_mid_reattach_is_not_recorded() {
    // An answer is about the attachment the query was started for. Once that client is
    // replaced, or while a reattach is on its way, the old client still sits on the
    // session the display is leaving, so its answer must not move the record.
    let mut rt = a_settled_zellij_runtime();
    the_query_answers(&mut rt, OWN_CLIENT + 7, Some("b"));
    assert_eq!(
        rt.hosts.get("local").unwrap().display.shows("local"),
        Some("a"),
        "an answer about a client that is not the live one is dropped"
    );

    the_query_answers(&mut rt, OWN_CLIENT, None);
    assert_eq!(
        rt.hosts.get("local").unwrap().display.shows("local"),
        Some("a"),
        "no answer is no signal"
    );

    rt.hosts
        .get_mut("local")
        .unwrap()
        .display
        .mark_in_flight("local", 9);
    the_query_answers(&mut rt, OWN_CLIENT, Some("b"));
    assert_eq!(
        rt.hosts.get("local").unwrap().display.shows("local"),
        Some("a"),
        "an answer arriving mid-reattach is dropped"
    );
}

// --- herdr display client moves -------------------------------------------------
// A herdr client moves itself between saved machines, one of which can be another
// session on the same machine. The host-side query reports where it went.

/// The settled herdr world: sessions `a` and `b` on `local`, the nav on `a`, and
/// xmux's own display client live on `a`.
fn a_settled_herdr_runtime() -> Runtime {
    let sess = |name: &str| crate::session::Session {
        host: "local".into(),
        name: name.into(),
        mux: "herdr".into(),
        id: String::new(),
        windows: 0,
        clients: 0,
        stopped: false,
    };
    let scan = crate::ui::switcher::Scan {
        groups: vec![crate::ui::cards::Group {
            host: "local".into(),
            err: None,
            sessions: vec![sess("a"), sess("b")],
        }],
    };
    let mut state = crate::state::State::from_scan(scan);
    let mut switcher = crate::ui::switcher::Switcher::new(&mut state);
    switcher.select_address(&crate::session::Address::new("local", "a"));
    let mut rt = test_rt(fake_env_with_machines(&[]));
    let mut hosts = crate::model::Hosts::default();
    hosts.insert(crate::model::Host::new(
        crate::transport::local(None),
        crate::mux::for_binary("herdr").unwrap(),
    ));
    rt.hosts = hosts;
    rt.model.state = state;
    rt.model.switcher = switcher;
    rt.hosts
        .get_mut("local")
        .unwrap()
        .display
        .set_shows("local", "a");
    rt.registry.insert(
        "local",
        crate::display::attachment::fake_attachment(OWN_CLIENT),
    );
    settled(&mut rt);
    rt
}

fn the_herdr_query_answers(rt: &mut Runtime, id: u64, at: crate::mux::ClientAt) {
    let (_tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    rt.on_pty_event(PtyEvent::DisplayClientSession { id, at: Some(at) }, &mut rx);
}

fn focus_terminal(rt: &mut Runtime) {
    rt.model
        .state
        .focus
        .set_view_focus(crate::app::focus::ViewFocus::Terminal);
}

#[tokio::test(flavor = "current_thread")]
async fn the_cursor_takes_the_session_clients_shape_only_while_its_view_has_focus() {
    // An editor in the session asks for a bar cursor. The terminal shows that shape
    // only while the cursor it shows is the session's; the nav keeps the default.
    let mut rt = a_settled_herdr_runtime();
    let grid = rt.registry.grid("local").expect("the displayed grid");
    grid.lock().unwrap().feed(b"\x1b[6 q");
    nav_text(&mut rt);
    assert_eq!(rt.cursor_shape, 0, "nav focus keeps the terminal's default");
    focus_terminal(&mut rt);
    rt.dirty = true;
    nav_text(&mut rt);
    assert_eq!(rt.cursor_shape, 6);
}

fn nav_text(rt: &mut Runtime) -> String {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    rt.cols = 80;
    rt.body_rows = 19;
    rt.last_draw = std::time::Instant::now() - std::time::Duration::from_secs(1);
    let mut term = Terminal::new(TestBackend::new(80, 20)).unwrap();
    rt.prepare_and_draw(&mut term);
    drawn_text(&term)
}

#[tokio::test(flavor = "current_thread")]
async fn a_herdr_move_to_a_session_here_in_terminal_focus_moves_the_nav_and_keeps_the_client() {
    // The user moved xmux's herdr client to a saved machine that is session `b` on the
    // same machine. The nav goes to `b`, and the client the user moved stays on screen.
    let mut rt = a_settled_herdr_runtime();
    focus_terminal(&mut rt);
    let t0 = std::time::Instant::now();

    the_herdr_query_answers(
        &mut rt,
        OWN_CLIENT,
        crate::mux::ClientAt::Session("b".into()),
    );
    for _ in 0..3 {
        one_pass(&mut rt, t0);
    }
    one_pass(&mut rt, t0 + std::time::Duration::from_secs(1));

    assert_eq!(rt.model.state.selection.session, "b");
    assert_eq!(
        rt.registry.get("local").map(|a| a.id()),
        Some(OWN_CLIENT),
        "the moved client is kept, not reattached"
    );
    assert!(rt.hosts.get("local").unwrap().display.in_flight_is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn a_herdr_move_elsewhere_names_the_place_on_the_card_and_moves_nothing() {
    // The client went to a saved machine the nav has no card for. The selection and the
    // client stay, in either focus, and the card of the session xmux opened says where
    // the view is.
    for terminal in [false, true] {
        let mut rt = a_settled_herdr_runtime();
        if terminal {
            focus_terminal(&mut rt);
        }
        let t0 = std::time::Instant::now();
        let away = crate::mux::ClientAt::Away("web/agents".into());

        the_herdr_query_answers(&mut rt, OWN_CLIENT, away.clone());
        let mut now = t0;
        for _ in 0..5 {
            one_pass(&mut rt, now);
            the_herdr_query_answers(&mut rt, OWN_CLIENT, away.clone());
            now += std::time::Duration::from_millis(400);
        }

        assert_eq!(rt.model.state.selection.session, "a");
        assert_eq!(rt.registry.get("local").map(|a| a.id()), Some(OWN_CLIENT));
        assert!(rt.hosts.get("local").unwrap().display.in_flight_is_empty());
        let out = nav_text(&mut rt);
        assert!(out.contains("a \u{2192} web/agents"), "{out}");

        // Back on the session it was attached for, the note goes.
        the_herdr_query_answers(
            &mut rt,
            OWN_CLIENT,
            crate::mux::ClientAt::Session("a".into()),
        );
        let out = nav_text(&mut rt);
        assert!(!out.contains("web/agents"), "{out}");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_herdr_client_that_starts_on_another_session_is_named_not_carried_back() {
    // herdr starts a client on the machine its user last chose, so a fresh attachment
    // for `a` can open on `b`. With the nav focused, carrying it back would reattach into
    // the same start again; the nav keeps `a` and its card names `b`.
    let mut rt = a_settled_herdr_runtime();
    let t0 = std::time::Instant::now();

    let mut now = t0;
    for _ in 0..10 {
        the_herdr_query_answers(
            &mut rt,
            OWN_CLIENT,
            crate::mux::ClientAt::Session("b".into()),
        );
        one_pass(&mut rt, now);
        now += std::time::Duration::from_millis(400);
    }
    assert_eq!(rt.model.state.selection.session, "a");
    assert_eq!(
        rt.registry.get("local").map(|a| a.id()),
        Some(OWN_CLIENT),
        "no reattach chases the start"
    );
    assert!(rt.hosts.get("local").unwrap().display.in_flight_is_empty());
    let out = nav_text(&mut rt);
    assert!(out.contains("a \u{2192} b"), "{out}");

    // A move the user then makes inside the client is followed as usual.
    focus_terminal(&mut rt);
    the_herdr_query_answers(
        &mut rt,
        OWN_CLIENT,
        crate::mux::ClientAt::Away("web/x".into()),
    );
    the_herdr_query_answers(
        &mut rt,
        OWN_CLIENT,
        crate::mux::ClientAt::Session("b".into()),
    );
    one_pass(&mut rt, now);
    assert_eq!(rt.model.state.selection.session, "b");
    assert_eq!(rt.registry.get("local").map(|a| a.id()), Some(OWN_CLIENT));
}

#[tokio::test(flavor = "current_thread")]
async fn the_away_note_goes_with_the_attachment_it_is_about() {
    let mut rt = a_settled_herdr_runtime();
    the_herdr_query_answers(
        &mut rt,
        OWN_CLIENT,
        crate::mux::ClientAt::Away("web/agents".into()),
    );
    assert!(nav_text(&mut rt).contains("web/agents"));
    rt.registry.remove("local");
    assert!(rt.drop_stale_display_away());
    assert!(!nav_text(&mut rt).contains("web/agents"));
}

#[tokio::test(flavor = "current_thread")]
async fn one_display_query_runs_at_a_time_on_its_cadence() {
    // A remote zellij client over a shared connection is asked where it is, one query at
    // a time and no more than once per cadence, so a slow machine never stacks queries.
    let mut rt = a_settled_zellij_runtime();
    let mut hosts = crate::model::Hosts::default();
    hosts.insert(crate::model::Host::new(
        crate::transport::ssh("local".into(), "/tmp/cm".into(), "linux".into()),
        crate::mux::for_binary("zellij").unwrap(),
    ));
    hosts
        .get_mut("local")
        .unwrap()
        .display
        .set_shows("local", "a");
    rt.hosts = hosts;
    let t0 = std::time::Instant::now();

    rt.start_display_probe(t0);
    assert!(rt.display_probe.in_flight, "the first query starts");
    let next = rt.display_probe.next;
    rt.start_display_probe(t0 + std::time::Duration::from_secs(5));
    assert_eq!(
        rt.display_probe.next, next,
        "no second query while one is out"
    );

    rt.display_probe.in_flight = false;
    rt.start_display_probe(t0 + std::time::Duration::from_millis(500));
    assert!(
        !rt.display_probe.in_flight,
        "the next waits for the cadence"
    );
    rt.start_display_probe(t0 + std::time::Duration::from_secs(1));
    assert!(rt.display_probe.in_flight, "and starts once it elapses");
}

/// Delivers the EOF the pump emits when the mirrored client detaches (or dies).
fn the_client_detaches(rt: &mut Runtime, id: u64) {
    let (_tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    rt.on_pty_event(PtyEvent::Exited { id }, &mut rx);
}

/// Completes the reattach the beat requested for the local host: the worker's Ready
/// installs the fresh client, which answers for the session it was attached to.
fn the_zellij_reattach_lands(rt: &mut Runtime, session: &str) {
    let seq = rt
        .hosts
        .get("local")
        .unwrap()
        .display
        .in_flight_seq("local")
        .expect("a reattach is in flight");
    rt.on_display_event(DisplayEvent::Ready {
        seq,
        key: "local".into(),
        attachment: crate::display::attachment::fake_attachment_answering_env(
            OWN_CLIENT + 1,
            "ZELLIJ_SESSION_NAME",
            session,
        ),
    });
}

#[tokio::test(flavor = "current_thread")]
async fn a_detached_zellij_display_serves_its_last_frame_and_reattaches_nothing() {
    // The detach of the mirrored client EOFs its attachment while the session stays
    // selected. The view keeps the last frame it drew and NOTHING reconnects - in either
    // focus, and however many beats pass. The client has served longer than an early
    // end, so even zellij's one reattach does not apply.
    //
    // This is the whole reason the automatic re-attach is gone. Every re-attach is a fresh
    // connection to that machine raised by the death of the connection before it, so when
    // the session on the far side is gone each attempt dies exactly like the last and the
    // chain never ends on its own. A machine watching its own accept log sees one client
    // reconnecting without pause, which is what its defences are built to stop.
    for terminal_focus in [false, true] {
        let mut rt = a_settled_zellij_runtime();
        if terminal_focus {
            rt.model
                .state
                .focus
                .set_view_focus(crate::app::focus::ViewFocus::Terminal);
        }
        let t0 = std::time::Instant::now();

        the_client_detaches(&mut rt, OWN_CLIENT);
        assert!(
            !rt.registry.contains("local"),
            "the dead attachment leaves the live map"
        );
        assert!(
            rt.registry.grid("local").is_some(),
            "its last frame still serves the view - the pane does not go blank"
        );
        assert!(
            rt.model.state.attach_deadline.is_none(),
            "the EOF arms no attach beat"
        );

        let mut now = t0;
        for _ in 0..10 {
            one_pass(&mut rt, now);
            now += std::time::Duration::from_millis(200);
        }
        assert!(
            rt.hosts.get("local").unwrap().display.in_flight_is_empty(),
            "ten beats on, nothing has reconnected (terminal_focus={terminal_focus})"
        );
        assert!(
            rt.registry.grid("local").is_some(),
            "and the last frame is still what the view holds"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn selecting_the_card_again_is_what_reattaches_a_dead_display() {
    // The recovery that remains is the user's. Selecting the card whose display died
    // attaches it again, so a pane the user wants back is one keystroke away - the
    // difference being that it is a connection somebody asked for.
    let mut rt = a_settled_zellij_runtime();
    let t0 = std::time::Instant::now();
    the_client_detaches(&mut rt, OWN_CLIENT);
    one_pass(&mut rt, t0);
    assert!(
        rt.hosts.get("local").unwrap().display.in_flight_is_empty(),
        "precondition: the dead display reconnected nothing on its own"
    );

    let selection = rt.model.state.selection.clone();
    rt.model.state.apply(crate::model::Action::ClearDisplay);
    rt.model
        .state
        .apply(crate::model::Action::Select(selection));
    one_pass(&mut rt, t0 + std::time::Duration::from_millis(200)); // arms the debounce
    one_pass(&mut rt, t0 + std::time::Duration::from_millis(400)); // fires it
    assert!(
        rt.hosts
            .get("local")
            .unwrap()
            .display
            .in_flight_contains("local"),
        "the user's own selection attaches the session again"
    );
    the_zellij_reattach_lands(&mut rt, "a");
    assert_eq!(rt.model.state.displayed.session, "a");
}

/// Moves the nav to `session` on the local host and runs the passes that settle the
/// selection, arm the debounce, and fire its attach. Returns the clock it ended on.
fn the_user_selects(rt: &mut Runtime, session: &str, t: std::time::Instant) -> std::time::Instant {
    rt.model
        .switcher
        .select_address(&crate::session::Address::new("local", session));
    one_pass(rt, t);
    let fired = t + std::time::Duration::from_millis(200);
    one_pass(rt, fired);
    fired
}

/// Runs passes over the next ten seconds and answers whether any of them started an
/// attach on the local host.
fn passes_attach_anything(rt: &mut Runtime, from: std::time::Instant) -> bool {
    let mut now = from;
    for _ in 0..50 {
        now += std::time::Duration::from_millis(200);
        one_pass(rt, now);
        if !rt.hosts.get("local").unwrap().display.in_flight_is_empty() {
            return true;
        }
    }
    false
}

fn local_in_flight(rt: &Runtime) -> bool {
    rt.hosts
        .get("local")
        .unwrap()
        .display
        .in_flight_contains("local")
}

#[tokio::test(flavor = "current_thread")]
async fn a_zellij_client_dropped_right_after_it_attaches_is_reattached_once() {
    // zellij 0.45 can hand a fresh client the id its own session probe has just freed,
    // and the probe's late cleanup then removes that client (zellij-org/zellij#5546).
    // The session is still there, so xmux attaches it once more. A second early end is
    // the normal ended state: the pane keeps its last frame and nothing reconnects.
    let mut rt = a_settled_zellij_runtime();
    let t0 = std::time::Instant::now();

    let t = the_user_selects(&mut rt, "b", t0);
    assert!(local_in_flight(&rt), "selecting b attaches it");
    the_zellij_reattach_lands(&mut rt, "b");
    the_client_detaches(&mut rt, OWN_CLIENT + 1);
    assert_eq!(rt.model.state.displayed.session, "b");

    assert!(
        passes_attach_anything(&mut rt, t),
        "the client zellij dropped right after it attached is attached once more"
    );
    the_zellij_reattach_lands(&mut rt, "b");
    assert_eq!(rt.model.state.displayed.session, "b");
    the_client_detaches(&mut rt, OWN_CLIENT + 1);

    assert!(
        !passes_attach_anything(&mut rt, t + std::time::Duration::from_secs(10)),
        "a second early end reattaches nothing"
    );
    assert!(
        rt.registry.grid("local").is_some(),
        "the pane keeps the last frame it drew"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn an_early_end_on_a_mux_that_keeps_its_clients_reattaches_nothing() {
    // The reattach answers one zellij defect. Any other mux whose fresh client ends
    // right away ended for a reason of its own, and the pane shows that end.
    let mut rt = a_settled_psmux_runtime();
    let t0 = std::time::Instant::now();

    let t = the_user_selects(&mut rt, "b", t0);
    assert!(local_in_flight(&rt), "selecting b attaches it");
    the_reattach_lands(&mut rt, "b");
    the_client_detaches(&mut rt, OWN_CLIENT + 1);
    assert_eq!(rt.model.state.displayed.session, "b");
    assert_eq!(rt.model.state.notify.toasts.len(), 1);

    assert!(
        !passes_attach_anything(&mut rt, t),
        "an early end on psmux reattaches nothing"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_failed_attach_waits_for_the_user_to_ask_again() {
    // An attach whose start failed is not retried by the beat, and the display is not
    // recorded on a session no client reached. The pane keeps what it shows until the
    // user asks for the session again, by Enter or by selecting it again, and each ask
    // starts exactly one attach.
    for psmux in [true, false] {
        let mut rt = if psmux {
            a_settled_psmux_runtime()
        } else {
            a_settled_zellij_runtime()
        };
        let t0 = std::time::Instant::now();

        let mut t = the_user_selects(&mut rt, "b", t0);
        let fail = |rt: &mut Runtime| {
            let seq = rt
                .hosts
                .get("local")
                .unwrap()
                .display
                .in_flight_seq("local")
                .expect("an attach is in flight");
            rt.on_display_event(DisplayEvent::Failed {
                seq,
                key: "local".into(),
                message: "spawn failed".into(),
            });
        };
        fail(&mut rt);
        assert_eq!(rt.model.state.notify.toasts.len(), 1);
        assert_eq!(
            rt.model.state.notify.toasts[0].notes[0].text,
            "spawn failed"
        );
        assert!(
            !passes_attach_anything(&mut rt, t),
            "the beat does not retry a failed attach (psmux={psmux})"
        );
        assert_eq!(
            rt.model.state.displayed.session, "a",
            "no client reached b, so b is not confirmed (psmux={psmux})"
        );
        t += std::time::Duration::from_secs(10);

        let _ = update(
            &mut rt.model,
            Msg::Focus(crate::model::FocusTarget::Terminal),
        );
        assert!(
            passes_attach_anything(&mut rt, t),
            "Enter on the card asks for the attach again (psmux={psmux})"
        );
        t += std::time::Duration::from_secs(10);
        fail(&mut rt);
        assert!(
            !passes_attach_anything(&mut rt, t),
            "that ask is answered once (psmux={psmux})"
        );
        t += std::time::Duration::from_secs(10);

        rt.model
            .state
            .focus
            .set_view_focus(crate::app::focus::ViewFocus::Nav);
        let _ = the_user_selects(&mut rt, "a", t);
        let _ = the_user_selects(&mut rt, "b", t + std::time::Duration::from_secs(1));
        assert!(
            local_in_flight(&rt),
            "selecting the card again asks for the attach again (psmux={psmux})"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_stale_psmux_card_reports_attach_failure_from_keys_and_ctl() {
    for keys in [true, false] {
        let mut rt = a_settled_psmux_runtime();
        rt.worker = crate::display::DisplayWorker::with_spawner(
            tokio::sync::mpsc::unbounded_channel().0,
            Box::new(|command, _, _, _, _, _| {
                assert_eq!(command.argv(), &["psmux", "-f", "NUL", "attach", "-t", "b"]);
                anyhow::bail!("psmux: can't find session: b")
            }),
        );
        if keys {
            let mut width_changed = false;
            rt.handle_nav_bytes(b"\x1b[B\r", &mut width_changed);
        } else {
            let crate::link::control::CtlRequest::Op(action) =
                crate::link::control::parse_ctl_op("switch local b")
            else {
                panic!("switch resolves to an action");
            };
            rt.dispatch_action(action);
        }
        let t = std::time::Instant::now();
        one_pass(&mut rt, t);
        one_pass(&mut rt, t + std::time::Duration::from_millis(200));
        let event = tokio::time::timeout(std::time::Duration::from_secs(1), rt.worker.recv())
            .await
            .expect("the attach answers")
            .expect("the worker is live");
        rt.on_display_event(event);
        assert_eq!(rt.model.state.notify.toasts.len(), 1, "keys={keys}");
        assert_eq!(
            rt.model.state.notify.toasts[0].notes[0].text,
            "psmux: can't find session: b"
        );
        assert!(!passes_attach_anything(&mut rt, t), "no retry: keys={keys}");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_stale_herdr_card_reports_attach_failure_from_keys_and_ctl_without_spawning_a_client() {
    for keys in [true, false] {
        let mut rt = a_settled_herdr_runtime();
        rt.worker = crate::display::DisplayWorker::with_spawner_and_runner(
            tokio::sync::mpsc::unbounded_channel().0,
            Box::new(|_, _, _, _, _, _| panic!("a missing session must not spawn a client")),
            crate::mux::herdr_missing_attach_runner(),
        );
        if keys {
            let mut width_changed = false;
            rt.handle_nav_bytes(b"\x1b[B\r", &mut width_changed);
        } else {
            let crate::link::control::CtlRequest::Op(action) =
                crate::link::control::parse_ctl_op("switch local b")
            else {
                panic!("switch resolves to an action");
            };
            rt.dispatch_action(action);
        }
        let t = std::time::Instant::now();
        one_pass(&mut rt, t);
        one_pass(&mut rt, t + std::time::Duration::from_millis(200));
        let event = tokio::time::timeout(std::time::Duration::from_secs(1), rt.worker.recv())
            .await
            .unwrap()
            .unwrap();
        rt.on_display_event(event);
        assert_eq!(rt.model.state.notify.toasts.len(), 1, "keys={keys}");
        assert_eq!(
            rt.model.state.notify.toasts[0].notes[0].text,
            "herdr: session 'b' no longer exists"
        );
        assert!(!passes_attach_anything(&mut rt, t), "no retry: keys={keys}");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_mux_side_switch_in_terminal_focus_keeps_the_client_the_user_moved() {
    // The other half of the same rule, on the mux that has to be READ for a switch. The
    // nav follows the client to `b`, which makes the selection differ from what is
    // confirmed on screen, so an attach fires for `b` - and a per-session mux reaches a
    // session by reattaching, which would kill the very client the user just moved. It is
    // held instead, on the client's own report that it is already there.
    let mut rt = a_settled_psmux_runtime();
    rt.model
        .state
        .focus
        .set_view_focus(crate::app::focus::ViewFocus::Terminal);
    let t0 = std::time::Instant::now();

    the_client_reports(&mut rt, OWN_CLIENT, "b");
    rt.observe_display_session();
    one_pass(&mut rt, t0);
    one_pass(&mut rt, t0 + std::time::Duration::from_millis(200));

    assert_eq!(
        rt.model.switcher.terminal_view_target().target,
        "b",
        "the nav follows the client the user moved"
    );
    assert!(
        rt.hosts.get("local").unwrap().display.in_flight_is_empty(),
        "nothing is spawned: the client is already on the selected session"
    );
    assert_eq!(
        rt.registry.get("local").map(|a| a.id()),
        Some(OWN_CLIENT),
        "the client the user moved stays on screen"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_mux_side_switch_in_nav_focus_ends_with_the_client_back_on_the_selection() {
    // In nav focus the selection is the user's own, so it is the CLIENT that yields: psmux
    // moved xmux's own client inside the client process, nothing was pushed anywhere, and
    // the beat that reads the client is what makes the difference visible. The nav stays
    // where the user left it and the client is carried back to it.
    let mut rt = a_settled_psmux_runtime();
    assert!(
        !rt.model.state.focus.is_terminal_focused(),
        "starts in nav focus"
    );

    the_client_reports(&mut rt, OWN_CLIENT, "b");
    assert!(
        rt.observe_display_session(),
        "the beat reads the client's own report of where it went"
    );

    let t0 = std::time::Instant::now();
    one_pass(&mut rt, t0);
    one_pass(&mut rt, t0 + std::time::Duration::from_millis(200));

    assert_eq!(
        rt.model.switcher.terminal_view_target().target,
        "a",
        "the mux does not move a selection the user is driving"
    );
    assert!(
        rt.hosts
            .get("local")
            .unwrap()
            .display
            .in_flight_contains("local"),
        "the client is carried back by a fresh attach for the selected session"
    );
    assert_eq!(
        rt.hosts.get("local").unwrap().display.shows("local"),
        Some("a"),
        "which is where the display then is"
    );

    the_reattach_lands(&mut rt, "a");
    one_pass(&mut rt, t0 + std::time::Duration::from_millis(400));
    assert_eq!(
        (
            rt.model.switcher.terminal_view_target().target.as_str(),
            rt.hosts.get("local").unwrap().display.shows("local")
        ),
        ("a", Some("a")),
        "the nav and the display name one session, and the reconcile is over"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_switch_asked_for_in_terminal_focus_is_not_dragged_back() {
    // The other way the nav and the client can differ: the SELECTION moved and the
    // display has not been carried to it yet. It looks exactly like a mux-side switch and
    // must not be answered like one - the nav would go back to the session the client is
    // still on and undo the switch that was just asked for. A ctl `switch` while the
    // terminal holds the focus is that case, and the attach owed for it is what tells the
    // two apart.
    let mut rt = a_settled_psmux_runtime();
    rt.model
        .state
        .focus
        .set_view_focus(crate::app::focus::ViewFocus::Terminal);
    let t0 = std::time::Instant::now();

    // What a ctl `switch local/b` dispatches to.
    rt.model
        .switcher
        .select_address(&crate::session::Address::new("local", "b"));
    one_pass(&mut rt, t0);
    one_pass(&mut rt, t0 + std::time::Duration::from_millis(50));
    assert_eq!(
        rt.model.switcher.terminal_view_target().target,
        "b",
        "the selection stays where the switch put it while its attach is owed"
    );

    one_pass(&mut rt, t0 + std::time::Duration::from_millis(200));
    assert_eq!(
        rt.model.switcher.terminal_view_target().target,
        "b",
        "and after the attach runs"
    );
    assert_eq!(
        rt.hosts.get("local").unwrap().display.shows("local"),
        Some("b"),
        "which is what carried the display there"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_pick_on_the_selected_card_leaves_the_nav_and_the_display_naming_one_session() {
    // The measured split, walked end to end: the user is in the terminal on `a`, goes to
    // the nav, something outside xmux moves xmux's own display client to `b`, and the user
    // then picks the card that was already selected and returns to the terminal. Picking a
    // card cancels nothing, because nothing is owed to cancel: the passes in between
    // compare where the client is against where the nav is, so the client is already back
    // on `a` before the pick.
    let mut rt = a_settled_psmux_runtime();
    rt.model
        .state
        .focus
        .set_view_focus(crate::app::focus::ViewFocus::Terminal);
    let t0 = std::time::Instant::now();
    one_pass(&mut rt, t0);

    // `focus nav`
    rt.model
        .state
        .focus
        .set_view_focus(crate::app::focus::ViewFocus::Nav);
    one_pass(&mut rt, t0);

    // From OUTSIDE xmux: the display client is moved to b.
    the_client_reports(&mut rt, OWN_CLIENT, "b");
    rt.observe_display_session();
    one_pass(&mut rt, t0);
    one_pass(&mut rt, t0 + std::time::Duration::from_millis(200));
    the_reattach_lands(&mut rt, "a");

    // The user picks local/a, the card already selected.
    rt.model
        .switcher
        .select_address(&crate::session::Address::new("local", "a"));

    // `focus terminal`
    rt.model
        .state
        .focus
        .set_view_focus(crate::app::focus::ViewFocus::Terminal);
    for ms in [400, 3_000, 11_000] {
        rt.observe_display_session();
        one_pass(&mut rt, t0 + std::time::Duration::from_millis(ms));
    }

    assert_eq!(
        rt.model.switcher.terminal_view_target().target,
        "a",
        "the nav is on the card the user picked"
    );
    assert_eq!(
        rt.hosts.get("local").unwrap().display.shows("local"),
        Some("a"),
        "and the display is on that same session, seconds later"
    );
}

// =========================================================================
// HUMAN VISUAL-GATE CHECKLIST (run in a REAL terminal - never headless):
// 1. Launch `xmux`. Confirm it enters the alternate screen cleanly and starts in
//    Focus::Nav: the Host·Session tree on the left, the live REAL
//    terminal of the selection's session on the right (a true attached mux client).
// 2. Move the selection between sessions. Confirm the terminal view shows each session's
//    real attached terminal instantly (it is pre-attached + kept alive), with a
//    spinner while a session's attach is still establishing.
// 3. Press Enter (or C-g → / C-g Tab) - focus the terminal (Focus::Terminal); the split
//    is unchanged (nav border turns green) and keystrokes reach the real attached pane.
//    C-g ← / C-g Esc / C-g Tab return focus to the nav. Confirm no blank/flash.
// 4. Create / kill a window or session inside a pane - confirm the nav view
//    syncs (remote via control events, local within the poll interval) and the
//    PTY set follows (new session attaches, killed session's PTY is reaped).
// 5. C-g then `q` - clean quit, terminal restored.
// 6. NEVER attach the session that owns xmux (xmux refuses to run inside a mux,
//    so in normal use no session mirrors the UI).
// 7. Mouse: dragging never selects native terminal text (the app captures the
//    mouse). A LEFT-button press in the UNFOCUSED view switches focus to it (focus
//    only - the click is not delivered); right-click never moves focus (it opens the
//    tree context menu). Once the terminal view is focused, clicks/scroll/
//    right-click reach the mux (status-bar click, pane select, scroll, context menu).
//    Mux mouse forwarding requires the mux to have `mouse on` (`set -g mouse on`);
//    xmux only forwards. (Windows: capture needs ENABLE_VIRTUAL_TERMINAL_INPUT +
//    the SGR DECSET that crossterm's WinAPI path omits - see display::term.)
// =========================================================================

#[test]
fn dispatch_action_switch_moves_cursor_focus_toggles_width_and_quit() {
    use crate::app::focus::Focus;
    use crate::model::{Action, FocusTarget};
    use crate::session::Session;
    use crate::ui::cards::Group;
    use crate::ui::switcher::{Scan, Switcher};
    let scan = Scan {
        groups: vec![Group {
            host: "jup".into(),
            err: None,
            sessions: vec![
                Session {
                    mux: String::new(),
                    id: String::new(),
                    host: "jup".into(),
                    name: "api".into(),
                    windows: 1,
                    clients: 0,
                    stopped: false,
                },
                Session {
                    mux: String::new(),
                    id: String::new(),
                    host: "jup".into(),
                    name: "db".into(),
                    windows: 1,
                    clients: 0,
                    stopped: false,
                },
            ],
        }],
    };
    let mut state = crate::state::State::from_scan(scan);
    let switcher = Switcher::new(&mut state);
    let mut rt = test_rt(fake_env_with_machines(&[]));
    rt.model.state = state;
    rt.model.switcher = switcher;
    rt.ops = crate::ui::switcher::tests_support::noop_ops();
    rt.model.nav_width_natural = 48;
    rt.model.auto_hide_nav = false;

    // Switch addr → selection lands on db; returns (quit=false, width_changed=false).
    assert_eq!(
        rt.dispatch_action(Action::Switch(crate::session::Address::new("jup", "db"))),
        (false, false)
    );
    assert_eq!(rt.model.switcher.terminal_view_target().target, "db");
    // Focus(Terminal) leaves nav focus → terminal focus.
    assert!(rt.model.state.focus.is_nav_focused());
    rt.dispatch_action(Action::Focus(FocusTarget::Terminal));
    assert_eq!(rt.model.state.focus, Focus::Terminal);
    // Focus(Tree) returns to nav focus.
    rt.dispatch_action(Action::Focus(FocusTarget::Nav));
    assert_eq!(rt.model.state.focus, Focus::Nav);
    // NavWidth adjusts the natural width and signals width_changed; Quit signals quit.
    assert_eq!(rt.dispatch_action(Action::NavWidth(1)), (false, true));
    assert_eq!(rt.model.nav_width_natural, 49);
    assert_eq!(
        rt.dispatch_action(Action::Quit),
        (true, false),
        "Quit signals quit"
    );
}

#[test]
fn status_line_names_the_listed_mux_before_the_reach_resolves() {
    use crate::session::Session;
    use crate::ui::cards::Group;
    use crate::ui::switcher::{Scan, Switcher};
    // No reach yet, so the host names no mux, but the listing does: the displayed path
    // names the mux the session's card names.
    let mut state = crate::state::State::from_scan(Scan {
        groups: vec![Group {
            host: "jup".into(),
            err: None,
            sessions: vec![Session {
                mux: "psmux".into(),
                id: String::new(),
                host: "jup".into(),
                name: "api".into(),
                windows: 1,
                clients: 0,
                stopped: false,
            }],
        }],
    });
    let sw = Switcher::new(&mut state);
    let line = status_line(&sw, &state, "amber-otter", true, "/tmp/x", "-");
    assert!(line.contains("\ttarget=jup/psmux/api\t"), "{line}");
}

#[test]
fn status_line_reports_focus_and_address() {
    use crate::session::Session;
    use crate::ui::cards::Group;
    use crate::ui::switcher::{Scan, Switcher};
    let scan = Scan {
        groups: vec![Group {
            host: "jup".into(),
            err: None,
            sessions: vec![Session {
                mux: String::new(),
                id: String::new(),
                host: "jup".into(),
                name: "api".into(),
                windows: 1,
                clients: 0,
                stopped: false,
            }],
        }],
    };
    let mut state = crate::state::State::from_scan(scan);
    state.chrome.set_host_reach(
        [(
            "jup".to_string(),
            crate::state::HostReach {
                kind: "tmux".into(),
                ..Default::default()
            },
        )]
        .into(),
    );
    let sw = Switcher::new(&mut state);
    // Tab-separated so a cwd containing spaces survives; cwd/tty are injected so
    // the assertion stays deterministic (no real env read). The displayed session is its
    // whole path, so the listing names the machine and the mux it runs under.
    let pid = std::process::id();
    assert_eq!(
        status_line(&sw, &state, "amber-otter", true, "/tmp/x", "-"),
        format!("name=amber-otter\tpid={pid}\tfocus=nav\ttarget=jup/tmux/api\tcwd=/tmp/x\ttty=-")
    );
    assert_eq!(
        status_line(&sw, &state, "amber-otter", false, "/tmp/x", "/dev/pts/3"),
        format!(
            "name=amber-otter\tpid={pid}\tfocus=terminal\ttarget=jup/tmux/api\tcwd=/tmp/x\ttty=/dev/pts/3"
        )
    );
}

#[test]
fn ctl_switch_syncs_canonical_selection_immediately() {
    use crate::model::Action;
    use crate::session::Session;
    use crate::ui::cards::Group;
    use crate::ui::switcher::{Scan, Switcher};

    let scan = Scan {
        groups: vec![Group {
            host: "jup".into(),
            err: None,
            sessions: vec![
                Session {
                    mux: String::new(),
                    id: String::new(),
                    host: "jup".into(),
                    name: "api".into(),
                    windows: 1,
                    clients: 0,
                    stopped: false,
                },
                Session {
                    mux: String::new(),
                    id: String::new(),
                    host: "jup".into(),
                    name: "db".into(),
                    windows: 1,
                    clients: 0,
                    stopped: false,
                },
            ],
        }],
    };
    let mut state = crate::state::State::from_scan(scan);
    let switcher = Switcher::new(&mut state);
    let mut rt = test_rt(fake_env_with_machines(&[]));
    rt.model.state = state;
    rt.model.switcher = switcher;

    sync_selection_from_switcher(&mut rt.model);
    // api (name order) is the preselected top card, so switch to db to exercise a real
    // selection move.
    rt.dispatch_action(Action::Switch(crate::session::Address::new("jup", "db")));

    // The switch moved the selection to db; the loop-top derive routes it through
    // apply(Select) - selection becomes jup/db and the attach is marked pending
    // (the deadline is armed by the next Tick, not here).
    assert!(sync_selection_from_switcher(&mut rt.model));
    assert_eq!(rt.model.state.selection.host, "jup");
    assert_eq!(rt.model.state.selection.session, "db");
    assert!(
        rt.model.state.attach_pending,
        "Select marks the attach pending"
    );
    assert!(
        rt.model.state.attach_deadline.is_none(),
        "Select arms no deadline - the trailing Tick does"
    );
}

#[test]
fn prefix_p_cycles_the_nav_position_and_persists_it() {
    use crate::ui::switcher::{NavPosition, Scan, Switcher};
    // `prefix p` moves the pin one step clockwise from the CURRENT effective position
    // and saves it at once, the same moment `prefix t` saves the auto-hide toggle. The
    // cycle never unpins: a pinned side cycles forward, and `left` is reached again
    // after `floating`.
    let mut state = crate::state::State::from_scan(Scan { groups: vec![] });
    let switcher = Switcher::new(&mut state);
    let mut rt = test_rt(fake_env_with_machines(&["local"]));
    rt.model.state = state;
    rt.model.switcher = switcher;
    rt.model.nav_position = NavPosition::Left; // effective = left (unpinned)
    let out = rt.handle_stdin_bytes(b"\x07p", &Selection::default());
    assert_eq!(rt.model.nav_position_pinned, Some(NavPosition::Top));
    assert!(
        std::fs::read_to_string(rt.env.xmux_dir.join("nav_position"))
            .unwrap()
            .contains("top"),
        "the pin is saved the moment the key cycles"
    );
    let _ = rt.handle_stdin_bytes(b"\x07p", &Selection::default());
    assert_eq!(rt.model.nav_position_pinned, Some(NavPosition::Right));
    let _ = rt.handle_stdin_bytes(b"\x07p", &Selection::default());
    assert_eq!(rt.model.nav_position_pinned, Some(NavPosition::Bottom));
    let _ = rt.handle_stdin_bytes(b"\x07p", &Selection::default());
    assert_eq!(rt.model.nav_position_pinned, Some(NavPosition::Floating));
    assert!(
        std::fs::read_to_string(rt.env.xmux_dir.join("nav_position"))
            .unwrap()
            .contains("floating"),
        "the cycle never unpins; floating persists"
    );
    let _ = rt.handle_stdin_bytes(b"\x07p", &Selection::default());
    assert_eq!(
        rt.model.nav_position_pinned,
        Some(NavPosition::Left),
        "the sixth step wraps back to left"
    );
    // The cycle itself does not claim the focus flags the outcome carries.
    assert!(!out.focus_terminal && !out.focus_nav && !out.quit);
}

#[test]
fn handle_stdin_bytes_quit_on_prefix_q_in_tree_focus() {
    use crate::ui::switcher::{Scan, Switcher};
    // prefix is Ctrl-G (0x07) in the default config; prefix then 'q' = quit.
    let scan = Scan { groups: vec![] };
    let mut state = crate::state::State::from_scan(scan); // nav focus
    let switcher = Switcher::new(&mut state);
    // The default fake env's prefix is "C-g" (0x07), matching this test's `\x07q`.
    let mut rt = test_rt(fake_env_with_machines(&["local"]));
    rt.hosts = crate::model::Hosts::default();
    rt.model.state = state;
    rt.model.switcher = switcher;
    let out = rt.handle_stdin_bytes(b"\x07q", &Selection::default());
    assert!(out.quit, "prefix+q in nav focus quits");
}

#[test]
fn prefix_m_and_prefix_question_close_what_they_opened_in_either_focus() {
    use crate::state::Modal;
    use crate::ui::switcher::{Scan, Switcher};
    let mut state = crate::state::State::from_scan(Scan { groups: vec![] });
    let switcher = Switcher::new(&mut state);
    let mut rt = test_rt(fake_env_with_machines(&["local"]));
    rt.hosts = crate::model::Hosts::default();
    rt.model.state = state;
    rt.model.switcher = switcher;
    let history = |rt: &Runtime| matches!(rt.model.state.modal, Some(Modal::History { .. }));
    rt.handle_stdin_bytes(b"\x07m", &Selection::default());
    assert!(history(&rt), "prefix m opens the history");
    rt.handle_stdin_bytes(b"\x07m", &Selection::default());
    assert!(rt.model.state.modal.is_none(), "prefix m closes it again");
    // The prefix and its key may arrive in two reads.
    rt.handle_stdin_bytes(b"\x07m", &Selection::default());
    rt.handle_stdin_bytes(b"\x07", &Selection::default());
    assert!(history(&rt), "the prefix alone closes nothing");
    rt.handle_stdin_bytes(b"m", &Selection::default());
    assert!(rt.model.state.modal.is_none());
    assert!(!rt.prefix_active(), "the key consumed the prefix");
    rt.handle_stdin_bytes(b"\x07?", &Selection::default());
    assert!(matches!(rt.model.state.modal, Some(Modal::Help { .. })));
    rt.handle_stdin_bytes(b"\x07?", &Selection::default());
    assert!(rt.model.state.modal.is_none(), "prefix ? closes the help");

    let mut rt = rt_terminal_focus_with_session();
    rt.handle_stdin_bytes(b"\x07m", &Selection::default());
    assert!(history(&rt), "prefix m opens it from the terminal view");
    rt.handle_stdin_bytes(b"\x07m", &Selection::default());
    assert!(rt.model.state.modal.is_none(), "and closes it there");
}

#[test]
fn arming_the_prefix_marks_the_frame_dirty_so_the_prefix_hint_swaps() {
    use crate::ui::switcher::{Scan, Switcher};
    // A live prefix opens the key list, so the bare prefix read is a VISIBLE change even
    // though it moves no selection and runs no action. If it did not mark the frame dirty
    // the key list would only appear on the next
    // unrelated redraw (a poll tick), which reads as the prefix doing nothing.
    let scan = Scan { groups: vec![] };
    let mut state = crate::state::State::from_scan(scan); // nav focus
    let switcher = Switcher::new(&mut state);
    let mut rt = test_rt(fake_env_with_machines(&["local"]));
    rt.hosts = crate::model::Hosts::default();
    rt.model.state = state;
    rt.model.switcher = switcher;
    assert!(!rt.prefix_active(), "starts unarmed");
    let out = rt.handle_stdin_bytes(b"\x07", &Selection::default());
    assert!(rt.prefix_active(), "the bare prefix arms");
    assert!(out.dirty, "arming redraws, so the key list shows at once");
    // The release is the key-up side of the press, a no-op: ready stays live, so the
    // bar stays up. A command key then consumes the chord and hides the bar.
    let _ = rt.handle_stdin_bytes(b"\x1b[7;5:3u", &Selection::default());
    assert!(
        rt.prefix_active(),
        "the release is a no-op: ready stays live"
    );
    let _ = rt.handle_stdin_bytes(b"t", &Selection::default());
    assert!(!rt.prefix_active(), "the command consumes the chord");
}

/// Builds a `Runtime` with one reachable session on host `jup`, focused on the
/// TERMINAL view - the setup the focus-independent tree-action tests share.
fn rt_terminal_focus_with_session() -> Runtime {
    use crate::session::Session;
    use crate::ui::cards::Group;
    use crate::ui::switcher::{Scan, Switcher};
    let scan = Scan {
        groups: vec![Group {
            host: "jup".into(),
            err: None,
            sessions: vec![Session {
                mux: String::new(),
                id: String::new(),
                host: "jup".into(),
                name: "api".into(),
                windows: 1,
                clients: 0,
                stopped: false,
            }],
        }],
    };
    let mut state = crate::state::State::from_scan(scan); // launches in nav focus
    let switcher = Switcher::new(&mut state);
    let mut rt = test_rt(fake_env_with_machines(&["jup"]));
    rt.hosts = crate::model::Hosts::default();
    rt.model.state = state;
    rt.model.switcher = switcher;
    // Descend to the api session so it is the selection, then focus the terminal view.
    rt.handle_stdin_bytes(b"l", &Selection::default());
    rt.model.state.apply(crate::model::Action::Focus(
        crate::model::FocusTarget::Terminal,
    ));
    assert!(
        !rt.model.state.focus.is_nav_focused() && !rt.model.state.focus.is_modal(),
        "precondition: the terminal view holds focus (not tree, not modal)"
    );
    rt
}

// A re-scan starts roster resolution off the loop, so the harness needs the runtime
// that the real loop always runs inside.
#[tokio::test]
async fn prefix_capital_r_in_terminal_focus_kicks_rescan() {
    // prefix R is focus-independent: from the terminal view it re-scans every host. The
    // re-scan clears each group's sessions, re-arms scanning, and flushes one discovery
    // pass after the terminal input batch.
    let mut rt = rt_terminal_focus_with_session();
    assert!(
        !rt.model.state.groups[0].sessions.is_empty(),
        "precondition: a session exists before the re-scan"
    );
    rt.handle_stdin_bytes(b"\x07R", &Selection::default());
    assert!(
        rt.model.state.groups[0].sessions.is_empty(),
        "prefix R in terminal focus cleared sessions for a re-scan"
    );
    assert!(
        rt.model.state.scanning.contains("jup"),
        "and re-armed scanning for the host"
    );
    assert_eq!(rt.discovery_runs, 1, "and started one discovery pass");
}

#[test]
fn repeated_prefix_bytes_keep_the_nav_steady_in_nav_focus() {
    use crate::ui::switcher::{Scan, Switcher};
    // In nav focus there is no pane to send a literal to, so arming is idempotent: a
    // held prefix's autorepeat neither toggles nor consumes ready, and the prefix hint
    // and the auto-hide nav show stay put until a command key consumes it.
    let mut state = crate::state::State::from_scan(Scan { groups: vec![] });
    let switcher = Switcher::new(&mut state);
    let mut rt = test_rt(fake_env_with_machines(&["local"]));
    rt.model.state = state;
    rt.model.switcher = switcher;
    assert!(!rt.prefix_active());
    rt.handle_stdin_bytes(b"\x07", &Selection::default());
    assert!(rt.prefix_active(), "the prefix arms");
    rt.handle_stdin_bytes(b"\x07", &Selection::default());
    assert!(
        rt.prefix_active(),
        "a repeat must not toggle the armed state"
    );
    rt.handle_stdin_bytes(b"\x07", &Selection::default());
    assert!(rt.prefix_active(), "more repeats stay steady");
    rt.handle_stdin_bytes(b"t", &Selection::default());
    assert!(!rt.prefix_active(), "a command key consumes the prefix");
}

#[test]
fn a_resize_keeps_the_prefix_live_for_its_resize_mode() {
    // A prefix is consumed when its FUNCTION ends. A resize starts the resize mode, so
    // the function is still running: the bar and the auto-hide nav show stay up across
    // the whole burst instead of dropping on the first arrow.
    let mut rt = rt_terminal_focus_with_session();
    rt.handle_stdin_bytes(b"\x07", &Selection::default()); // prefix: ready
    assert!(rt.prefix_active(), "the bar shows while ready");
    rt.handle_stdin_bytes(b"\x1b[1;5C", &Selection::default()); // Ctrl+Right resizes
    assert!(
        rt.prefix_active(),
        "the resize started the resize mode, so the function has not ended"
    );
    rt.handle_stdin_bytes(b"\x1b[1;5C", &Selection::default()); // bare Ctrl+Right
    assert!(rt.prefix_active(), "a bare repeat keeps the mode");
    // A key that is not a resize key ends the mode at once.
    rt.handle_stdin_bytes(b"z", &Selection::default());
    assert!(
        !rt.prefix_active(),
        "a key that is not a resize key ends the mode, ending the function"
    );
}

#[test]
fn a_non_repeating_command_ends_the_prefix_at_once() {
    // Nothing opens a window for `t`, so its function ends with the key.
    let mut rt = rt_terminal_focus_with_session();
    rt.handle_stdin_bytes(b"\x07", &Selection::default());
    assert!(rt.prefix_active());
    rt.handle_stdin_bytes(b"t", &Selection::default());
    assert!(
        !rt.prefix_active(),
        "the bar hides as the command completes"
    );
}

#[test]
fn one_escape_read_closes_the_prefix_key_list_in_either_focus() {
    // A lone ESC after the prefix is the whole key: the read that carries it ends the
    // chord and the next frame drops the key list, with no further input or timer.
    for nav_focus in [false, true] {
        let mut rt = rt_terminal_focus_with_session();
        if nav_focus {
            rt.model.state.focus = crate::state::Focus::Nav;
        }
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 25)).unwrap();
        assert!(!rt.on_stdin(b"\x07"));
        rt.last_draw -= std::time::Duration::from_secs(1);
        rt.prepare_and_draw(&mut term);
        assert!(rt.model.state.chrome.armed, "nav focus {nav_focus}");
        assert!(
            rt.model.render_plan.key_list.is_some(),
            "nav focus {nav_focus}"
        );
        assert!(!rt.on_stdin(b"\x1b"));
        assert!(rt.dirty, "the chord end repaints: nav focus {nav_focus}");
        rt.last_draw -= std::time::Duration::from_secs(1);
        rt.prepare_and_draw(&mut term);
        assert!(!rt.model.state.chrome.armed, "nav focus {nav_focus}");
        assert!(
            rt.model.render_plan.key_list.is_none(),
            "nav focus {nav_focus}"
        );
        assert!(
            !rt.on_stdin(b"q"),
            "the next key is no longer a prefix command: nav focus {nav_focus}"
        );
    }
}

#[test]
fn an_open_input_row_keeps_the_prefix_live_until_it_closes() {
    // A command that opens an input row owns the prefix until the row closes: the
    // prefix popup hosts the input, so it must stay expanded while the user types.
    let mut rt = rt_terminal_focus_with_session();
    rt.handle_stdin_bytes(b"\x07", &Selection::default());
    rt.handle_stdin_bytes(b"n", &Selection::default()); // new session: opens the input row
    assert!(rt.model.state.is_inputting(), "the input row is open");
    assert!(
        rt.prefix_active(),
        "the function has not ended, so the prefix is still live"
    );
    // The loop top hands the modal its focus dimension before the next read; without
    // it the Esc would route to the pane instead of the row.
    let kind = rt.model.state.modal_kind();
    rt.model.state.focus.sync_modal(kind);
    rt.handle_stdin_bytes(b"\x1b", &Selection::default()); // Esc cancels
    assert!(!rt.model.state.is_inputting(), "Esc closes the row");
    assert!(!rt.prefix_active(), "the function ended with the row");
}

#[test]
fn a_focus_switch_drops_the_left_views_prefix_latches() {
    // A prefix-driven focus switch leaves the prefix key physically held, and its
    // release is delivered to the view that GAINED focus, never to the one that lost
    // it. Without dropping the outgoing side's latches on the switch, a stale hold
    // would keep the status bar up forever. The switch must clear what the outgoing
    // view latched, in both directions.
    let mut rt = rt_terminal_focus_with_session();
    assert!(
        !rt.model.state.focus.is_nav_focused(),
        "precondition: terminal focus"
    );
    // Terminal → nav: a held prefix chord ends when prefix Left hands focus over.
    rt.handle_stdin_bytes(b"\x07", &Selection::default()); // prefix down: +ready
    assert!(rt.prefix_active());
    rt.handle_stdin_bytes(b"\x1b[D", &Selection::default()); // prefix Left → nav
    assert!(
        rt.model.state.focus.is_nav_focused(),
        "focus moved to the nav"
    );
    assert!(
        !rt.prefix_active(),
        "the switch drops the terminal-side hold so the bar hides"
    );
    // Nav → terminal: the nav-side latches a new prefix chord set are cleared the
    // moment prefix Right hands focus back.
    rt.handle_stdin_bytes(b"\x07", &Selection::default()); // prefix down on the nav
    assert!(rt.prefix_active());
    rt.handle_stdin_bytes(b"\x1b[C", &Selection::default()); // prefix Right → terminal
    assert!(
        !rt.model.state.focus.is_nav_focused(),
        "focus moved to the terminal"
    );
    assert!(
        !rt.prefix_active(),
        "the switch drops the nav-side hold so the bar hides"
    );
}

#[test]
fn a_mouse_action_disarms_the_prefix_and_a_hover_does_not() {
    use crate::ui::switcher::{Scan, Switcher};
    // A prefix waits for the NEXT input, and a mouse action is input. Mouse bytes are
    // scanned out of the stream before either focus path's key handling sees them, so
    // without an explicit disarm the chord stays half-open: its key list keeps floating
    // over the window, and the next key it swallows is one meant for the pane.
    let ev = |cb: u16, pressed: bool| crate::display::mouse::MouseEvent {
        cb,
        col: 10,
        row: 3,
        pressed,
    };
    // cb 0 = left press, cb 64 = wheel up, cb 0 with pressed=false = release.
    for (cb, pressed, what) in [
        (0u16, true, "a click"),
        (0, false, "a release"),
        (64, true, "a wheel"),
        (32, true, "a drag"), // motion WITH a button held
    ] {
        let mut state = crate::state::State::from_scan(Scan { groups: vec![] });
        let switcher = Switcher::new(&mut state);
        let mut rt = test_rt(fake_env_with_machines(&["local"]));
        rt.model.state = state;
        rt.model.switcher = switcher;
        rt.model.mouse_state.nav_armed = true;
        let dirty = rt.handle_mouse_event(
            &ev(cb, pressed),
            &Selection::default(),
            &mut false,
            &mut false,
            &mut false,
            &mut false,
        );
        assert!(!rt.prefix_active(), "{what} disarms the prefix");
        assert!(dirty, "{what} redraws, so the key list goes at once");
    }
    // Bare hover is the pointer sitting there, not an action: it must not break a chord
    // the user is still typing. cb 35 = motion bit with no button held.
    let mut state = crate::state::State::from_scan(Scan { groups: vec![] });
    let switcher = Switcher::new(&mut state);
    let mut rt = test_rt(fake_env_with_machines(&["local"]));
    rt.model.state = state;
    rt.model.switcher = switcher;
    rt.model.mouse_state.nav_armed = true;
    rt.handle_mouse_event(
        &ev(35, true),
        &Selection::default(),
        &mut false,
        &mut false,
        &mut false,
        &mut false,
    );
    assert!(rt.prefix_active(), "a hover leaves the chord alone");
}

#[test]
fn handle_mouse_event_nav_border_grab_sets_dragging() {
    use crate::ui::switcher::{Scan, Switcher};
    // A left-press exactly on the nav border column sets dragging_nav_border, as the
    // inline gate did (is_left_press && nav_width > 0 && col0 == nav_width).
    let scan = Scan { groups: vec![] };
    let mut state = crate::state::State::from_scan(scan);
    let switcher = Switcher::new(&mut state);
    let sel = Selection::default();
    let nav_width = crate::ui::switcher::NAV_WIDTH;
    // 0-based col0 = ev.col - 1 must equal nav_width to grab the nav border rule.
    let nav_border_col = nav_width + 1; // 1-based SGR column of the nav border
                                        // cb=0 → left button, press, no wheel/motion → is_left_press is true.
    let ev = crate::display::mouse::MouseEvent {
        cb: 0,
        col: nav_border_col,
        row: 3,
        pressed: true,
    };
    // Landscape enough to keep the side column, whose border this test grabs.
    let mut focus_toggle = false;
    let mut wheel = false;
    let mut rt = test_rt(fake_env_with_machines(&["local"]));
    rt.model.state = state;
    rt.model.switcher = switcher;
    // The handler cuts its own regions from the runtime's size, so the runtime has to be
    // landscape too or the border it looks for is a horizontal rule under the horizontal nav.
    rt.cols = 200;
    rt.body_rows = 23;
    sync_test_render_plan(&mut rt);
    rt.handle_mouse_event(
        &ev,
        &sel,
        &mut focus_toggle,
        &mut wheel,
        &mut false,
        &mut false,
    );
    assert!(
        rt.model.mouse_state.dragging_nav_border,
        "left-press on the nav border column grabs it"
    );
}

#[test]
fn focusing_the_nav_expands_a_collapsed_nav() {
    use crate::ui::switcher::{Scan, Switcher};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let mut state = crate::state::State::from_scan(Scan { groups: vec![] });
    state
        .focus
        .set_view_focus(crate::app::focus::ViewFocus::Terminal);
    let switcher = Switcher::new(&mut state);
    let mut rt = test_rt(fake_env_with_machines(&["local"]));
    rt.model.state = state;
    rt.model.switcher = switcher;
    rt.model.nav_collapsed = true;
    rt.model.nav_width = crate::ui::switcher::collapsed_nav_width(&rt.env.ui_prefix);
    rt.model.applied_nav_collapsed = true;
    rt.model.nav_was_focused = false;

    let out = rt.handle_stdin_bytes(b"\x07\x1b[D", &Selection::default());
    assert!(out.focus_nav, "the prefix-left path requests nav focus");
    let mut term = Terminal::new(TestBackend::new(80, 25)).unwrap();
    rt.prepare_and_draw(&mut term);
    assert!(!rt.model.nav_collapsed, "entering nav focus expands it");
    assert_eq!(rt.model.nav_width, rt.model.nav_width_natural);
}

#[test]
fn a_collapsed_nav_border_cannot_start_a_resize_drag() {
    use crate::ui::switcher::{compute_regions, Scan, Switcher};

    let mut state = crate::state::State::from_scan(Scan { groups: vec![] });
    let switcher = Switcher::new(&mut state);
    let mut rt = test_rt(fake_env_with_machines(&["local"]));
    rt.model.state = state;
    rt.model.switcher = switcher;
    rt.cols = 140;
    rt.body_rows = 29;
    rt.model.nav_collapsed = true;
    rt.model.nav_width = crate::ui::switcher::collapsed_nav_width(&rt.env.ui_prefix);
    sync_test_render_plan(&mut rt);
    let regions = compute_regions(ratatui::layout::Rect::new(0, 0, 140, 30), rt.nav_size());
    assert_eq!(rt.model.render_plan.regions.nav_border, regions.nav_border);
    let press = crate::display::mouse::MouseEvent {
        cb: 0,
        col: regions.nav_border.x + 1,
        row: regions.nav_border.y + 1,
        pressed: true,
    };
    rt.handle_mouse_event(
        &press,
        &Selection::default(),
        &mut false,
        &mut false,
        &mut false,
        &mut false,
    );
    assert!(!rt.model.mouse_state.dragging_nav_border);
}

#[test]
fn handle_mouse_event_top_layout_border_drag_resizes_height() {
    use crate::ui::switcher::{Scan, Switcher};
    // In a horizontal nav layout the nav border is a HORIZONTAL rule; a left-press on that
    // row grabs it and a drag sets the nav HEIGHT (not width). 40x60 carries the horizontal nav:
    // a horizontal nav carries its own prefix hint, so its auto height is ~40% of the whole
    // 60-row area = 24, putting the border at row 24 (0-based) = SGR row 25.
    let mut state = crate::state::State::from_scan(Scan { groups: vec![] });
    let switcher = Switcher::new(&mut state);
    let sel = Selection::default();
    let mut rt = test_rt(fake_env_with_machines(&["local"]));
    rt.model.state = state;
    rt.model.switcher = switcher;
    rt.cols = 40;
    rt.body_rows = 59;
    rt.model.nav_height = 0; // auto
    rt.model.nav_position = crate::ui::switcher::NavPosition::Top;
    sync_test_render_plan(&mut rt);

    let press = crate::display::mouse::MouseEvent {
        cb: 0,
        col: 5,
        row: 25,
        pressed: true,
    };
    let (mut ft, mut wheel) = (false, false);
    rt.handle_mouse_event(&press, &sel, &mut ft, &mut wheel, &mut false, &mut false);
    assert!(
        rt.model.mouse_state.dragging_nav_border,
        "left-press on the horizontal Top border grabs it"
    );

    // Drag DOWN to SGR row 30 (motion bit 0x20, left button held) → nav height = 30-1 = 29.
    let drag = crate::display::mouse::MouseEvent {
        cb: 0x20,
        col: 5,
        row: 30,
        pressed: true,
    };
    rt.handle_mouse_event(&drag, &sel, &mut ft, &mut wheel, &mut false, &mut false);
    assert_eq!(
        rt.model.nav_height, 29,
        "dragging the horizontal border sets the nav HEIGHT to the dragged row"
    );
}

#[test]
fn handle_mouse_event_bottom_layout_border_drag_resizes_height() {
    use crate::ui::switcher::{NavPosition, Scan, Switcher};
    // The bottom placement mirrors the top: the border is the row ABOVE the horizontal nav, and a
    // drag measures the height from the window's FAR edge. 40x60 pinned Bottom; the auto
    // height is 24, so the border is 0-based row 35 = SGR row 36.
    let mut state = crate::state::State::from_scan(Scan { groups: vec![] });
    let switcher = Switcher::new(&mut state);
    let sel = Selection::default();
    let mut rt = test_rt(fake_env_with_machines(&["local"]));
    rt.model.state = state;
    rt.model.switcher = switcher;
    rt.cols = 40;
    rt.body_rows = 59;
    rt.model.nav_height = 0; // auto
    rt.model.nav_position = NavPosition::Bottom;
    sync_test_render_plan(&mut rt);

    let press = crate::display::mouse::MouseEvent {
        cb: 0,
        col: 5,
        row: 36,
        pressed: true,
    };
    let (mut ft, mut wheel) = (false, false);
    rt.handle_mouse_event(&press, &sel, &mut ft, &mut wheel, &mut false, &mut false);
    assert!(
        rt.model.mouse_state.dragging_nav_border,
        "left-press on the horizontal bottom border grabs it"
    );

    // Drag DOWN to SGR row 40 → the horizontal nav keeps 60 - 40 = 20 rows.
    let drag = crate::display::mouse::MouseEvent {
        cb: 0x20,
        col: 5,
        row: 40,
        pressed: true,
    };
    rt.handle_mouse_event(&drag, &sel, &mut ft, &mut wheel, &mut false, &mut false);
    assert_eq!(
        rt.model.nav_height, 20,
        "dragging the bottom border measures the height from the far edge"
    );
}

#[test]
fn handle_mouse_event_right_layout_border_drag_resizes_width() {
    use crate::ui::switcher::{NavPosition, Scan, Switcher};
    // The right placement mirrors the left: the border is the column LEFT of the nav,
    // and a drag measures the width from the window's FAR edge. 140x30 pinned Right;
    // the border is 0-based col 91 = SGR col 92.
    let mut state = crate::state::State::from_scan(Scan { groups: vec![] });
    let switcher = Switcher::new(&mut state);
    let sel = Selection::default();
    let mut rt = test_rt(fake_env_with_machines(&["local"]));
    rt.model.state = state;
    rt.model.switcher = switcher;
    rt.cols = 140;
    rt.body_rows = 29;
    rt.model.nav_position = NavPosition::Right;
    sync_test_render_plan(&mut rt);

    let press = crate::display::mouse::MouseEvent {
        cb: 0,
        col: 92,
        row: 5,
        pressed: true,
    };
    let (mut ft, mut wheel) = (false, false);
    rt.handle_mouse_event(&press, &sel, &mut ft, &mut wheel, &mut false, &mut false);
    assert!(
        rt.model.mouse_state.dragging_nav_border,
        "left-press on the vertical right border grabs it"
    );

    // Drag RIGHT to SGR col 100 → the nav keeps 140 - 100 = 40 columns.
    let drag = crate::display::mouse::MouseEvent {
        cb: 0x20,
        col: 100,
        row: 5,
        pressed: true,
    };
    rt.handle_mouse_event(&drag, &sel, &mut ft, &mut wheel, &mut false, &mut false);
    assert_eq!(
        rt.model.nav_width_natural, 40,
        "dragging the right border measures the width from the far edge"
    );
}

#[test]
fn resize_keys_adjust_height_in_top_layout() {
    use crate::ui::switcher::{Scan, Switcher, ViewLayout, NAV_WIDTH};
    // In a horizontal nav layout the nav-resize keys (prefix h/l · Ctrl+←/→) adjust the
    // HEIGHT, not the width - seeded from the auto height the first time.
    let mut state = crate::state::State::from_scan(Scan { groups: vec![] });
    let switcher = Switcher::new(&mut state);
    let mut rt = test_rt(fake_env_with_machines(&["local"]));
    rt.model.state = state;
    rt.model.switcher = switcher;
    rt.cols = 40;
    rt.body_rows = 59;
    rt.model.nav_height = 0; // auto
    rt.model.nav_position = crate::ui::switcher::NavPosition::Top;
    let nav = crate::ui::switcher::NavSize::visible(NAV_WIDTH)
        .with_position(crate::ui::switcher::NavPosition::Top);
    rt.model.render_plan = rt.model.switcher.layout(
        ratatui::layout::Rect::new(0, 0, 40, 60),
        nav,
        &rt.model.state,
        &rt.model.render_plan,
    );
    assert_eq!(
        rt.model.render_plan.layout,
        ViewLayout::Horizontal,
        "portrait → Band"
    );

    let auto = crate::ui::switcher::default_nav_height(59);
    // Vertical axis (Ctrl+↓ = grow) resizes HEIGHT in a horizontal nav; horizontal (Ctrl+→) is a no-op here.
    assert!(
        !rt.resize_axis(true, 1),
        "horizontal resize is a no-op in a band"
    );
    assert!(rt.resize_axis(false, 1), "grow changes the height");
    assert_eq!(
        rt.model.nav_height,
        auto + 1,
        "a resize key grows the band nav height from the auto seed"
    );
    assert!(rt.resize_axis(false, -1), "shrink changes the height");
    assert_eq!(rt.model.nav_height, auto, "and shrinks it back");
}

#[test]
fn a_drag_moves_the_floating_nav_and_locks_it_for_a_minute() {
    use crate::ui::switcher::{Scan, Switcher};
    let mut state = crate::state::State::from_scan(Scan { groups: vec![] });
    let switcher = Switcher::new(&mut state);
    let sel = Selection::default();
    let mut rt = test_rt(fake_env_with_machines(&["local"]));
    rt.model.state = state;
    rt.model.switcher = switcher;
    rt.cols = 100;
    rt.body_rows = 59;
    rt.model.nav_position = crate::ui::switcher::NavPosition::Floating;
    let box_rect = ratatui::layout::Rect::new(70, 2, 30, 10);
    rt.model.floating_rect = Some(box_rect);
    sync_test_render_plan(&mut rt);
    let mut ft = false;
    let mut wheel = false;
    rt.handle_mouse_event(
        &mouse(0, 75, 5, true),
        &sel,
        &mut ft,
        &mut wheel,
        &mut false,
        &mut false,
    );
    assert!(
        rt.model.floating_drag.is_some(),
        "a left press on the floating box grabs it"
    );
    // Drag 5 right, 3 down (motion bit held): the box follows the pointer.
    rt.handle_mouse_event(
        &mouse(0x20, 80, 8, true),
        &sel,
        &mut ft,
        &mut wheel,
        &mut false,
        &mut false,
    );
    let moved = rt.model.floating_rect.unwrap();
    assert_eq!(
        (moved.x, moved.y),
        (70, 5),
        "the box follows the drag, clamped inside the screen: {moved:?}"
    );
    // Release ends the drag and locks the position for a minute.
    rt.handle_mouse_event(
        &mouse(0x20, 80, 8, false),
        &sel,
        &mut ft,
        &mut wheel,
        &mut false,
        &mut false,
    );
    assert!(rt.model.floating_drag.is_none());
    assert!(
        rt.model.floating_lock_until.is_some(),
        "release holds the position"
    );
}

#[test]
fn resize_keys_flip_direction_on_the_right_and_bottom() {
    use crate::ui::switcher::{NavPosition, Scan, Switcher, ViewLayout, NAV_WIDTH};
    // The resize key's direction is the border's movement, so with the nav on the
    // right or below the SAME key grows the nav the other way: in a right column the
    // →/l key (delta +1) shrinks the nav, and in a bottom horizontal nav the ↓ key (delta +1)
    // shrinks the height - the same flip as the focus-arrow pair.
    let mut state = crate::state::State::from_scan(Scan { groups: vec![] });
    let switcher = Switcher::new(&mut state);
    let mut rt = test_rt(fake_env_with_machines(&["local"]));
    rt.model.state = state;
    rt.model.switcher = switcher;
    rt.cols = 140;
    rt.body_rows = 29;
    rt.model.nav_position = NavPosition::Right;
    let nav = crate::ui::switcher::NavSize::visible(NAV_WIDTH).with_position(NavPosition::Right);
    rt.model.render_plan = rt.model.switcher.layout(
        ratatui::layout::Rect::new(0, 0, 140, 30),
        nav,
        &rt.model.state,
        &rt.model.render_plan,
    );
    assert_eq!(
        rt.model.render_plan.layout,
        ViewLayout::Vertical,
        "landscape → Column"
    );
    let w0 = rt.model.nav_width_natural;
    assert!(
        rt.resize_axis(true, 1),
        "→/l on the right changes the width"
    );
    assert_eq!(
        rt.model.nav_width_natural,
        w0 - 1,
        "→/l on the right shrinks (the border moves right)"
    );
    assert!(
        rt.resize_axis(true, -1),
        "←/h on the right changes the width"
    );
    assert_eq!(
        rt.model.nav_width_natural, w0,
        "←/h on the right grows (the border moves left)"
    );
    assert!(!rt.resize_axis(false, 1), "height is a no-op in a column");

    // The same flip on the horizontal nav: a bottom nav's ↓ key shrinks the height.
    rt.model.nav_position = NavPosition::Bottom;
    rt.cols = 40;
    rt.body_rows = 59;
    rt.model.nav_height = 0; // auto
    let nav = crate::ui::switcher::NavSize::visible(NAV_WIDTH).with_position(NavPosition::Bottom);
    rt.model.render_plan = rt.model.switcher.layout(
        ratatui::layout::Rect::new(0, 0, 40, 60),
        nav,
        &rt.model.state,
        &rt.model.render_plan,
    );
    assert_eq!(
        rt.model.render_plan.layout,
        ViewLayout::Horizontal,
        "portrait → Band"
    );
    let auto = crate::ui::switcher::default_nav_height(59);
    assert!(
        rt.resize_axis(false, 1),
        "↓ on the bottom changes the height"
    );
    assert_eq!(
        rt.model.nav_height,
        auto - 1,
        "↓ on the bottom shrinks (the border moves down)"
    );
    assert!(
        rt.resize_axis(false, -1),
        "↑ on the bottom changes the height"
    );
    assert_eq!(
        rt.model.nav_height, auto,
        "↑ on the bottom grows (the border moves up)"
    );
    assert!(!rt.resize_axis(true, 1), "width is a no-op in a band");
}

#[test]
fn loop_top_resolves_the_pinned_nav_position() {
    use crate::ui::switcher::{Scan, Switcher, ViewLayout};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    // The pin wins outright: whatever the aspect says, the loop top resolves the nav to
    // the pinned side and the PTYs are sized for that split.
    let mut state = crate::state::State::from_scan(Scan { groups: vec![] });
    let switcher = Switcher::new(&mut state);
    let mut rt = test_rt(fake_env_with_machines(&["local"]));
    rt.model.state = state;
    rt.model.switcher = switcher;
    rt.model.nav_position_pinned = Some(crate::ui::switcher::NavPosition::Right);
    let mut term = Terminal::new(TestBackend::new(80, 25)).unwrap();
    rt.prepare_and_draw(&mut term);
    assert_eq!(
        rt.model.nav_position,
        crate::ui::switcher::NavPosition::Right,
        "the loop top applied the pin"
    );
    assert_eq!(
        rt.model.render_plan.layout,
        ViewLayout::Vertical,
        "right is a column"
    );
}

#[test]
fn loop_top_resolves_the_default_position_when_unpinned() {
    use crate::ui::switcher::{Scan, Switcher, ViewLayout};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    // No pin: the [ui] default (left) applies whatever the aspect, so the nav never
    // moves on its own. A portrait backend still gets the left column.
    let mut state = crate::state::State::from_scan(Scan { groups: vec![] });
    let switcher = Switcher::new(&mut state);
    let mut rt = test_rt(fake_env_with_machines(&["local"]));
    rt.model.state = state;
    rt.model.switcher = switcher;
    rt.cols = 40;
    rt.body_rows = 59;
    let mut term = Terminal::new(TestBackend::new(40, 60)).unwrap();
    rt.prepare_and_draw(&mut term);
    assert_eq!(
        rt.model.nav_position,
        crate::ui::switcher::NavPosition::Left,
        "the unpinned default wins whatever the aspect"
    );
    assert_eq!(
        rt.model.render_plan.layout,
        ViewLayout::Vertical,
        "left is a column"
    );
}

#[test]
fn forward_to_mux_reasserts_capture_and_encodes_the_sgr_press() {
    use crate::ui::switcher::{Scan, Switcher};
    // A left-press over the FOCUSED terminal view reaches the mux as an SGR press
    // re-encoded to the view-local cell, and the ForwardToMux arm re-asserts the CONIN
    // capture bits before the forward (a no-op off Windows, where the assertion is a
    // pure local read and write of xmux's own console handle).
    let mut state = crate::state::State::from_scan(Scan { groups: vec![] });
    let switcher = Switcher::new(&mut state);
    let sel = Selection {
        host: "local".into(),
        session: "work".into(),
    };
    let nav_width = crate::ui::switcher::NAV_WIDTH;
    let (att, log) = crate::display::attachment::fake_attachment_with_input_log(42);
    att.grid.lock().unwrap().feed(b"\x1b[?1000h\x1b[?1006h");
    let mut rt = test_rt(fake_env_with_machines(&["local"]));
    rt.model.state = state;
    rt.model.switcher = switcher;
    rt.model
        .state
        .focus
        .set_view_focus(crate::app::focus::ViewFocus::Terminal);
    sync_test_render_plan(&mut rt);
    rt.registry.insert(&display_key(&rt.hosts, &sel), att);

    // A left-button press inside the terminal view (SGR 1-based col/row, cb 0 = left).
    let press = crate::display::mouse::MouseEvent {
        cb: 0,
        col: nav_width + 12,
        row: 5,
        pressed: true,
    };
    let (mut ft, mut wheel) = (false, false);
    rt.handle_mouse_event(&press, &sel, &mut ft, &mut wheel, &mut false, &mut false);
    // Re-encoded to grid-local (1-based): col nav_width+12 → gc 11, row 5 → gr 5.
    let logged = log.lock().unwrap().clone();
    assert_eq!(
        logged,
        vec![b"\x1b[<0;11;5M".to_vec()],
        "the drag-start press reaches the mux, re-encoded to the view-local cell"
    );
}

/// The tty a live display attach gives its host is decided by the transport's own shape.
/// A machine that spawns the mux binary DIRECTLY puts the mux client in the very PTY xmux
/// opened, so that PTY's name IS the client's tty: identity by ownership, known before the
/// mux even registers a client, and unaffected by an external client sharing the session.
/// A machine that hops through a shell puts the client on a pty of the FAR side, so the
/// local PTY's name belongs to a stranger there and is dropped (switching that tty would
/// move someone else's terminal); such a host learns its tty from the attach's own record.
#[tokio::test(flavor = "current_thread")]
async fn ready_adopts_the_pty_name_only_where_the_child_is_the_mux_client() {
    let mut rt = test_rt(fake_env_with_machines(&[]));
    let mut hosts = crate::model::Hosts::default();
    hosts.insert(crate::model::Host::new(
        crate::transport::local(None),
        crate::mux::for_binary("tmux").unwrap(),
    ));
    hosts.insert(crate::model::Host::new(
        crate::transport::ssh("jup".into(), String::new(), "linux".into()),
        crate::mux::for_binary("tmux").unwrap(),
    ));
    rt.hosts = hosts;

    for (host, id) in [("local", 7u64), ("jup", 8u64)] {
        {
            let h = rt.hosts.get_mut(host).unwrap();
            h.display.set_shows(host, "work");
            h.display.mark_in_flight(host, 1);
            h.display.mark_pending(id, host);
        }
        rt.on_display_event(crate::display::DisplayEvent::Ready {
            seq: 1,
            key: host.to_string(),
            attachment: crate::display::attachment::fake_attachment_with_tty(id, "/dev/pts/3"),
        });
    }

    assert_eq!(
        rt.hosts.get("local").unwrap().display_tty.0.as_deref(),
        Some("/dev/pts/3"),
        "the mux client runs in the PTY xmux opened, so that PTY's name is its tty"
    );
    assert_eq!(
        rt.hosts.get("jup").unwrap().display_tty.0,
        None,
        "past a shell hop the client is elsewhere, so the local PTY names nothing here"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_warm_attach_for_another_host_does_not_take_the_terminal_view() {
    // A shared-model host warms a PTY on a session of its OWN choosing the moment its
    // inventory arrives, so an attach can land for a host the selection is not on. It
    // installs, which is what makes that host instant to reach - but it names a session
    // nobody selected, so the terminal view stays where the selection is. Confirming it
    // would move the view to another machine mid-scan while the cursor stands still,
    // which is exactly the two regions disagreeing at launch.
    let mut rt = a_settled_psmux_runtime();
    rt.hosts.insert(crate::model::Host::new(
        crate::transport::ssh("jup".into(), String::new(), "linux".into()),
        crate::mux::for_binary("tmux").unwrap(),
    ));
    {
        let jup = rt.hosts.get_mut("jup").unwrap();
        jup.display.set_shows("jup", "if-0");
        jup.display.mark_in_flight("jup", 9);
        jup.display.mark_pending(OWN_CLIENT + 2, "jup");
    }
    rt.on_display_event(DisplayEvent::Ready {
        seq: 9,
        key: "jup".into(),
        attachment: crate::display::attachment::fake_attachment(OWN_CLIENT + 2),
    });
    assert_eq!(
        (
            rt.model.state.displayed.host.as_str(),
            rt.model.state.displayed.session.as_str()
        ),
        ("local", "a"),
        "the view stays on the selected session"
    );
    assert!(
        rt.registry.contains("jup"),
        "the warm attachment still installs, so its host stays instant to reach"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn ready_holds_the_stale_frame_until_the_fresh_attachment_paints() {
    let mut rt = a_settled_psmux_runtime();
    rt.model.state.selection.session = "b".into();
    rt.hosts
        .get_mut("local")
        .unwrap()
        .display
        .set_shows("local", "b");
    the_reattach_lands_for(&mut rt, 11, "b");

    assert_eq!(
        rt.registry.get("local").map(|att| att.id()),
        Some(OWN_CLIENT)
    );
    assert_eq!(
        rt.registry.pending_address_of_id(OWN_CLIENT + 3).as_deref(),
        Some("local")
    );
    assert_eq!(
        rt.model.state.displayed.session, "a",
        "Ready alone keeps the stale frame confirmed"
    );

    let debounce_start = std::time::Instant::now();
    rt.drive_attach_beat(debounce_start);
    rt.drive_attach_beat(
        debounce_start + std::time::Duration::from_millis(crate::state::ATTACH_DEBOUNCE_MS + 1),
    );
    assert_eq!(
        rt.registry.pending_address_of_id(OWN_CLIENT + 3).as_deref(),
        Some("local"),
        "the attach debounce treats a paint-pending client as work already underway"
    );
    assert_eq!(
        rt.model.state.displayed.session, "a",
        "the pending client cannot confirm before its paint gate opens"
    );

    // Output that has left nothing visible (a clear-screen, terminal queries) must not
    // open the paint gate: the fresh grid is still empty.
    let unpainted_at = std::time::Instant::now();
    rt.note_pending_output(OWN_CLIENT + 3);
    assert!(
        !rt.promote_due_pending(unpainted_at + crate::model::host::PAINT_SETTLE),
        "bytes without a visible frame keep the stale frame up"
    );
    assert_eq!(rt.model.state.displayed.session, "a");

    let output_at = std::time::Instant::now();
    rt.registry
        .mark_pending_painted_for_test(OWN_CLIENT + 3, output_at);
    rt.note_pending_output(OWN_CLIENT + 3);
    assert!(rt.promote_due_pending(output_at + crate::model::host::PAINT_SETTLE));
    assert_eq!(
        rt.registry.get("local").map(|att| att.id()),
        Some(OWN_CLIENT + 3),
        "the painted attachment replaces the stale one"
    );
    assert_eq!(
        rt.model.state.displayed.session, "b",
        "the painted selected attachment confirms the view"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn first_display_installs_immediately_without_a_stale_attachment() {
    let mut rt = a_settled_psmux_runtime();
    rt.registry.remove("local");
    rt.model.state.selection = Selection {
        host: "local".into(),
        session: "b".into(),
    };
    rt.hosts
        .get_mut("local")
        .unwrap()
        .display
        .set_shows("local", "b");

    the_reattach_lands_for(&mut rt, 11, "b");

    assert_eq!(
        rt.registry.get("local").map(|att| att.id()),
        Some(OWN_CLIENT + 3)
    );
    assert!(rt.registry.pending_address_of_id(OWN_CLIENT + 3).is_none());
    assert_eq!(rt.model.state.displayed.session, "b");
}

#[tokio::test(flavor = "current_thread")]
async fn pending_exit_retires_the_stale_attachment_and_applies_the_exit() {
    let mut rt = a_settled_psmux_runtime();
    rt.model.state.selection = Selection {
        host: "local".into(),
        session: "b".into(),
    };
    rt.hosts
        .get_mut("local")
        .unwrap()
        .display
        .set_shows("local", "b");
    the_reattach_lands_for(&mut rt, 11, "b");
    let (_tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

    rt.on_pty_event(PtyEvent::Exited { id: OWN_CLIENT + 3 }, &mut rx);

    assert!(!rt.registry.contains("local"));
    assert_eq!(rt.registry.address_of_id(OWN_CLIENT), None);
    assert_eq!(rt.registry.address_of_id(OWN_CLIENT + 3), None);
    assert!(
        rt.registry.grid("local").is_some(),
        "the exited fresh attachment leaves its own final grid"
    );
    assert_eq!(rt.model.state.displayed.session, "b");
}

#[tokio::test(flavor = "current_thread")]
async fn newer_request_tears_down_the_older_pending_attachment() {
    let mut rt = a_settled_psmux_runtime();
    rt.hosts
        .get_mut("local")
        .unwrap()
        .display
        .set_shows("local", "b");
    the_reattach_lands_for(&mut rt, 11, "b");
    assert!(rt.registry.pending_address_of_id(OWN_CLIENT + 3).is_some());

    let selection = crate::model::Selection {
        host: "local".into(),
        session: "b".into(),
    };
    let id = crate::driver::DriverCtx {
        registry: &mut rt.registry,
        hosts: &mut rt.hosts,
        instance_name: &rt.instance_name,
        mgr: &rt.mgr,
        worker: &rt.worker,
        pty_tx: &rt.driver_pty_tx,
        attach_seq: &mut rt.attach_seq,
        viewport: (80, 24),
    }
    .request_attach(
        &selection,
        crate::transport::CommandSpec::from_argv(vec!["fake".into()]),
    )
    .expect("the local host exists");

    assert!(
        rt.registry.pending_address_of_id(OWN_CLIENT + 3).is_none(),
        "the superseded pending PTY is no longer owned"
    );
    assert_eq!(
        rt.hosts
            .get("local")
            .unwrap()
            .display
            .in_flight_seq("local"),
        Some(rt.attach_seq)
    );
    assert_ne!(id, OWN_CLIENT + 3);
}

/// Lands a worker `Ready` on the local host under `seq`, answering for `session`.
fn the_reattach_lands_for(rt: &mut Runtime, seq: u64, session: &str) {
    rt.hosts
        .get_mut("local")
        .unwrap()
        .display
        .mark_in_flight("local", seq);
    rt.on_display_event(DisplayEvent::Ready {
        seq,
        key: "local".into(),
        attachment: crate::display::attachment::fake_attachment_answering_env(
            OWN_CLIENT + 3,
            "PSMUX_SESSION_NAME",
            session,
        ),
    });
}

#[test]
fn a_probe_line_shows_every_word_it_runs() {
    // The words are what the user pastes into a shell, so a word with a space in it is
    // quoted; and a TAB - which every session format carries - is written as its escape,
    // because a terminal prints a raw one as nothing and the datum would be on screen
    // and unreadable.
    let argv: Vec<String> = ["tmux", "list-sessions", "-F", "a\tb", "two words"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let line = crate::driver::shell_line(&argv);
    assert_eq!(line, r"tmux list-sessions -F 'a\tb' 'two words'");
}

#[test]
fn a_hosts_reach_names_its_mux_and_the_machine_it_is_asked_over() {
    // What the unreachable screen states about a host, resolved from that host's own
    // config: the binary asked for, how the machine is addressed, and the listing command
    // itself.
    let s = crate::model::host_def::HostDef {
        alias: "prod".into(),
        binary: "tmux".into(),
        kind: crate::transport::MachineKind::Ssh {
            id: String::new(),
            alias: "prod".into(),
            control_path: "/tmp/cm-prod.sock".into(),
            os: "linux".into(),
        },
        runner: None,
        remote_shells: Default::default(),
        credentials: Default::default(),
    };
    let reach = super::handlers::host_reach(&s);
    assert_eq!(reach.mux, "tmux");
    assert_eq!(reach.socket, "/tmp/cm-prod.sock");
    assert!(
        reach.machine.contains("ssh to prod"),
        "the machine names its destination: {:?}",
        reach.machine
    );
    assert!(
        reach.probe.starts_with("ssh ") && reach.probe.contains("tmux list-sessions"),
        "the probe is the command a listing runs: {:?}",
        reach.probe
    );
}

#[test]
fn config_poll_records_baseline_then_reloads_on_change() {
    // The live config watch starts with the mtime read at startup. A
    // malformed edit keeps the last good config rather than blanking the UI.
    let dir = std::env::temp_dir().join(format!("xmux-poll-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.toml");
    std::fs::write(&path, "[ui]\ntheme = \"auto-dark\"\n").unwrap();
    let mut last = std::fs::metadata(&path).unwrap().modified().ok();
    // The same file again is not a change.
    assert!(super::handlers::poll_ui_config(&mut last, &path).is_none());
    assert!(super::handlers::poll_ui_config(&mut last, &path).is_none());
    // A real edit reloads the [ui] section.
    std::thread::sleep(std::time::Duration::from_millis(30));
    std::fs::write(&path, "[ui]\ntheme = \"auto-light\"\nmax-fps = 120\n").unwrap();
    let ui = super::handlers::poll_ui_config(&mut last, &path)
        .expect("a real change reloads")
        .expect("valid config");
    assert_eq!(ui.theme, "auto-light");
    assert_eq!(ui.max_fps, 120);
    // A malformed edit keeps the last good config (None) but is still recorded.
    std::thread::sleep(std::time::Duration::from_millis(30));
    std::fs::write(&path, "not [[ valid toml").unwrap();
    assert!(super::handlers::poll_ui_config(&mut last, &path)
        .unwrap()
        .is_err());
    std::thread::sleep(std::time::Duration::from_millis(30));
    std::fs::write(&path, "[ui]\ntheme = \"auto-light\"\n").unwrap();
    let ui = super::handlers::poll_ui_config(&mut last, &path)
        .unwrap()
        .unwrap();
    assert_eq!(ui.max_fps, 30);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn frame_interval_never_rounds_below_the_fps_limit() {
    for fps in [10, 30, 60, 90, 120] {
        let interval = super::frame_interval(fps);
        assert!(interval.as_nanos() * u128::from(fps) >= 1_000_000_000);
        assert!((interval.as_nanos() - 1) * u128::from(fps) < 1_000_000_000);
    }
}

#[test]
fn config_poll_ignores_a_missing_file() {
    // A deletion (or an editor's atomic-rename mid-save) is not a reload: record the
    // absence and wait for the file to return.
    let dir = std::env::temp_dir().join(format!("xmux-poll-missing-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.toml");
    std::fs::write(&path, "[ui]\ntheme = \"auto-dark\"\n").unwrap();
    let mut last = std::fs::metadata(&path).unwrap().modified().ok();
    assert!(super::handlers::poll_ui_config(&mut last, &path).is_none()); // unchanged
    std::fs::remove_file(&path).unwrap();
    assert!(super::handlers::poll_ui_config(&mut last, &path).is_none()); // gone: no reload
    std::fs::remove_dir_all(&dir).ok();
}

// --- host event update ----------------------------------------------------
// Host events enter the application update transition. These tests inspect the
// resulting application state and ordered runtime effects.
use crate::link::HostEvent;
use crate::model::EventEffect;
use crate::model::Group;
use crate::session::Session;
use crate::ui::switcher::{Scan, Switcher};
use std::collections::HashSet;

fn host_event_effects_for_test(
    state: &mut State,
    event: HostEvent,
    switcher: &mut Switcher,
    connected: &mut HashSet<String>,
) -> Vec<EventEffect> {
    let mut placeholder_state = State::default();
    let placeholder_switcher = Switcher::from_hosts(&mut placeholder_state);
    let mut model = AppModel::from_hosts(Vec::new());
    model.state = std::mem::take(state);
    model.switcher = std::mem::replace(switcher, placeholder_switcher);
    model.connected = std::mem::take(connected);
    let effects = update(
        &mut model,
        Msg::HostEvent {
            event,
            logged_in: HashSet::new(),
        },
    );
    *state = model.state;
    *switcher = model.switcher;
    *connected = model.connected;
    effects
        .into_iter()
        .flat_map(|effect| match effect {
            Effect::Event(effect) => vec![effect],
            Effect::EventBatch(effects) => effects,
            effect => panic!("host event emitted unrelated effect: {effect:?}"),
        })
        .collect()
}

#[test]
fn poll_rename_precedes_display_session_sync() {
    let (state, switcher) = with_switcher(one_session_scan());
    let mut model = AppModel::from_hosts(Vec::new());
    model.state = state;
    model.switcher = switcher;
    let effects = update(
        &mut model,
        Msg::HostEvent {
            event: HostEvent::Sessions {
                host: "jup".into(),
                sessions: vec![Session {
                    host: "jup".into(),
                    name: "renamed".into(),
                    mux: "tmux".into(),
                    id: "7$0".into(),
                    windows: 2,
                    clients: 0,
                    stopped: false,
                }],
                err: None,
            },
            logged_in: HashSet::new(),
        },
    );
    assert!(matches!(
        effects.as_slice(),
        [Effect::EventBatch(effects)] if matches!(
            effects.as_slice(),
            [
                EventEffect::RenameDisplayed { host: renamed_host, from, to },
                EventEffect::SyncPollSessions { host: synced_host, .. }
            ] if renamed_host == "jup"
                && synced_host == "jup"
                && from == "api"
                && to == "renamed"
        )
    ));
}

fn one_session_scan() -> Scan {
    Scan {
        groups: vec![Group {
            host: "jup".into(),
            err: None,
            sessions: vec![Session {
                host: "jup".into(),
                name: "api".into(),
                mux: "tmux".into(),
                id: "7$0".into(),
                windows: 2,
                clients: 0,
                stopped: false,
            }],
        }],
    }
}

fn with_switcher(scan: Scan) -> (State, Switcher) {
    let mut state = State::from_scan(scan);
    let sw = Switcher::new(&mut state);
    (state, sw)
}

#[test]
fn host_event_connected_marks_connected_and_emits_apply_inventory() {
    // The reader carries the parsed sessions on Connected/Inventory; update
    // records the connected mark and hands the sessions to the loop as an effect
    // (which folds them into `model::Host.inventory` - the single owner).
    let (mut state, mut sw) = with_switcher(one_session_scan());
    let mut connected = HashSet::new();
    let sessions = vec![crate::session::Session {
        host: "jup".into(),
        name: "api".into(),
        ..Default::default()
    }];
    let effects = host_event_effects_for_test(
        &mut state,
        HostEvent::Connected {
            host: "jup".into(),
            sessions: sessions.clone(),
        },
        &mut sw,
        &mut connected,
    );
    assert!(connected.contains("jup"), "Connected records the host");
    assert!(
        matches!(
            effects.as_slice(),
            [
                EventEffect::CheckSharedConnection { machine },
                EventEffect::ApplyInventory { host, sessions }
            ] if machine == "jup" && host == "jup" && sessions.len() == 1
        ),
        "Connected reads its shared connection and carries its sessions into one ApplyInventory effect: {effects:?}"
    );
    // Inventory applies the same sessions; the channel was already open.
    let effects = host_event_effects_for_test(
        &mut state,
        HostEvent::Inventory {
            host: "jup".into(),
            sessions,
        },
        &mut sw,
        &mut connected,
    );
    assert!(
        matches!(effects.as_slice(), [EventEffect::ApplyInventory { host, sessions }] if host == "jup" && sessions.len() == 1),
    );
}

#[test]
fn host_event_changed_emits_refetch() {
    let (mut state, mut sw) = with_switcher(one_session_scan());
    let mut connected = HashSet::new();
    let effects = host_event_effects_for_test(
        &mut state,
        HostEvent::Changed { host: "jup".into() },
        &mut sw,
        &mut connected,
    );
    assert!(
        matches!(effects.as_slice(), [EventEffect::Refetch { host }] if host == "jup"),
        "Changed returns one Refetch effect: {effects:?}"
    );
}

#[test]
fn host_event_client_detached_emits_reap_display_attach_with_no_state_change() {
    // The tty match + reap need the host registry (loop-owned); update only
    // forwards the descriptor and touches no State.
    let (mut state, mut sw) = with_switcher(one_session_scan());
    let mut connected = HashSet::new();
    let before_groups = state.groups.len();
    let before_sessions = state.groups[0].sessions.len();
    let effects = host_event_effects_for_test(
        &mut state,
        HostEvent::ClientDetached {
            host: "jup".into(),
            client: "/dev/pts/3".into(),
        },
        &mut sw,
        &mut connected,
    );
    assert!(
        matches!(
            effects.as_slice(),
            [
                EventEffect::ReapDisplayAttach { host, client },
                EventEffect::Refetch { host: refetched },
                EventEffect::SettleDisplaySize { host: settled },
            ] if host == "jup" && client == "/dev/pts/3" && refetched == "jup" && settled == "jup"
        ),
        "ClientDetached forwards a ReapDisplayAttach effect, refetches the counts, and asks \
         which clients share the displayed session: {effects:?}"
    );
    // ClientDetached mutates no State (the tree group set is untouched).
    assert_eq!(state.groups.len(), before_groups);
    assert_eq!(state.groups[0].sessions.len(), before_sessions);
    assert!(state.modal.is_none());
}

#[test]
fn host_event_client_session_changed_forwards_follow_effect_with_no_state_change() {
    // The tty match against Host.display_tty, the display-belief sync, and the nav
    // follow all need loop-owned state; update only forwards the descriptor and
    // touches no State (the selection follow happens in the loop, gated on the match).
    let (mut state, mut sw) = with_switcher(one_session_scan());
    let mut connected = HashSet::new();
    let before_groups = state.groups.len();
    let before_sessions = state.groups[0].sessions.len();
    let effects = host_event_effects_for_test(
        &mut state,
        HostEvent::ClientSessionChanged {
            host: "jup".into(),
            client: "/dev/pts/3".into(),
            session: "db".into(),
        },
        &mut sw,
        &mut connected,
    );
    assert!(
        matches!(
            effects.as_slice(),
            [
                EventEffect::FollowDisplaySession { host, client, session },
                EventEffect::Refetch { host: refetched },
                EventEffect::SettleDisplaySize { host: settled },
            ] if host == "jup"
                && client == "/dev/pts/3"
                && session == "db"
                && refetched == "jup"
                && settled == "jup"
        ),
        "ClientSessionChanged forwards a FollowDisplaySession effect, refetches the counts, \
         and asks which clients share the displayed session: {effects:?}"
    );
    // update mutates no State here (the tree group set is untouched); the tty match +
    // selection follow are loop-owned.
    assert_eq!(state.groups.len(), before_groups);
    assert_eq!(state.groups[0].sessions.len(), before_sessions);
}

#[test]
fn host_event_exited_marks_unreachable_and_emits_reap() {
    // A never-connected host exiting with a real failure marks the tree
    // unreachable (a State mutation) AND asks the loop to reap the client.
    let (mut state, mut sw) = with_switcher(one_session_scan());
    let mut connected = HashSet::new(); // not connected → not a transient drop
    let effects = host_event_effects_for_test(
        &mut state,
        HostEvent::Exited {
            host: "jup".into(),
            reason: Some("connection refused".into()),
            detached: false,
        },
        &mut sw,
        &mut connected,
    );
    assert!(
        matches!(effects.as_slice(), [EventEffect::ReapHost { host }] if host == "jup"),
        "Exited returns one ReapHost effect: {effects:?}"
    );
    let g = state.groups.iter().find(|g| g.host == "jup").unwrap();
    assert!(
        g.err.is_some(),
        "the host is marked unreachable in the tree"
    );
}

#[test]
fn host_event_exited_of_connected_host_keeps_tree_and_still_reaps() {
    // A transient drop of a once-connected host keeps its last-known tree (no
    // unreachable flash) but still reaps the dead client.
    let (mut state, mut sw) = with_switcher(one_session_scan());
    let mut connected = HashSet::new();
    connected.insert("jup".to_string());
    let effects = host_event_effects_for_test(
        &mut state,
        HostEvent::Exited {
            host: "jup".into(),
            reason: None,
            detached: false,
        },
        &mut sw,
        &mut connected,
    );
    assert!(matches!(effects.as_slice(), [EventEffect::ReapHost { host }] if host == "jup"),);
    assert!(
        !connected.contains("jup"),
        "the connected mark is cleared so a later failed reconnect resolves"
    );
    let g = state.groups.iter().find(|g| g.host == "jup").unwrap();
    assert!(
        g.err.is_none(),
        "a transient drop keeps the last-known tree"
    );
}

#[test]
fn host_event_sessions_applies_tree_and_emits_sync_on_success() {
    // A poll host's enumeration is self-contained: update applies the
    // sessions to the tree and hands the sessions back for the stale-attach /
    // sync follow-up the loop owns.
    let mut state = State::from_hosts(vec!["local".into()]);
    let mut sw = Switcher::from_hosts(&mut state);
    let mut connected = HashSet::new();
    let sessions = vec![Session {
        host: "local".into(),
        name: "work".into(),
        mux: "tmux".into(),
        id: String::new(),
        windows: 1,
        clients: 0,
        stopped: false,
    }];
    let effects = host_event_effects_for_test(
        &mut state,
        HostEvent::Sessions {
            host: "local".into(),
            sessions: sessions.clone(),
            err: None,
        },
        &mut sw,
        &mut connected,
    );
    assert!(
        !state.scanning.contains("local"),
        "the enumerated host is no longer scanning"
    );
    let g = state.groups.iter().find(|g| g.host == "local").unwrap();
    assert_eq!(g.sessions.len(), 1, "the session is in the tree");
    assert!(
        matches!(
            effects.as_slice(),
            [EventEffect::SyncPollSessions { host, sessions: s }]
                if host == "local" && s.len() == 1
        ),
        "a successful enumeration syncs terminals: {effects:?}"
    );
}

#[test]
fn host_event_sessions_with_error_applies_tree_but_emits_no_sync() {
    // A transient enumeration failure shows the error in the tree but keeps
    // attachments (the keep-alive guarantee) - no sync effect.
    let mut state = State::from_hosts(vec!["local".into()]);
    let mut sw = Switcher::from_hosts(&mut state);
    let mut connected = HashSet::new();
    let effects = host_event_effects_for_test(
        &mut state,
        HostEvent::Sessions {
            host: "local".into(),
            sessions: Vec::new(),
            err: Some("poll failed".into()),
        },
        &mut sw,
        &mut connected,
    );
    let g = state.groups.iter().find(|g| g.host == "local").unwrap();
    assert_eq!(g.err.as_deref(), Some("poll failed"));
    assert!(
        effects.is_empty(),
        "a failed enumeration keeps attachments - no sync effect: {effects:?}"
    );
}

#[test]
fn feed_login_fills_the_pane_and_submits_from_the_button() {
    // Enter passes the focus on from a text field, so filling the pane top to bottom
    // with Enter alone ends on the button, where Enter submits. The password is taken
    // out of the draft on submit so the draft keeps no second copy.
    let mut s = State::default();
    // Address and port are prefilled; username is entered before continuing.
    for _ in 0..2 {
        assert!(
            s.feed_login("prod", b"\r").is_none(),
            "a field passes focus on"
        );
    }
    assert!(s.feed_login("prod", b"alice").is_none());
    assert!(s.feed_login("prod", b"\r").is_none());
    assert!(s.feed_login("prod", b"hunter2").is_none(), "typing waits");
    assert!(
        s.feed_login("prod", b"\r").is_none(),
        "the password field passes focus on too"
    );
    assert!(
        s.feed_login("prod", b"\r").is_none(),
        "the first radio choice passes focus on"
    );
    assert!(s.feed_login("prod", b"\r").is_none());
    // The focus is on the public-key radio choice: Space picks it, Enter walks past.
    assert!(
        s.feed_login("prod", b" ").is_none(),
        "Space picks, never submits"
    );
    assert!(
        s.feed_login("prod", b"\r").is_none(),
        "Enter walks past the choice"
    );
    let cmd = s.feed_login("prod", b"\r").expect("the button submits");
    match cmd {
        crate::model::Command::RunLogin {
            host,
            password,
            after_login,
            ..
        } => {
            assert_eq!(host, "prod");
            assert_eq!(password, "hunter2");
            assert_eq!(after_login, crate::model::AfterLogin::RegisterKey);
        }
        other => panic!("expected RunLogin, got {other:?}"),
    }
    assert_eq!(
        s.login.as_ref().unwrap().password,
        "",
        "the submitted password is taken out of the draft"
    );
}

#[test]
fn login_draft_debug_redacts_the_password() {
    let draft = LoginDraft {
        password: "do-not-print-this".into(),
        ..LoginDraft::default()
    };

    let shown = format!("{draft:?}");
    assert!(!shown.contains("do-not-print-this"));
    assert!(shown.contains("[redacted]"));
}

#[test]
fn feed_login_walks_its_stops_with_tab_and_the_vertical_arrows() {
    let mut s = State::default();
    s.feed_login("prod", b"\t");
    assert_eq!(s.login.as_ref().unwrap().focus, LoginFocus::Port);
    s.feed_login("prod", b"\x1b[B");
    assert_eq!(s.login.as_ref().unwrap().focus, LoginFocus::Username);
    s.feed_login("prod", b"\x1b[A");
    assert_eq!(s.login.as_ref().unwrap().focus, LoginFocus::Port);
    s.feed_login("prod", b"\x1b[Z");
    assert_eq!(s.login.as_ref().unwrap().focus, LoginFocus::Address);
}

#[test]
fn feed_login_after_login_radio_keeps_one_choice() {
    let mut s = State::default();
    s.feed_login("prod", b"");
    assert_eq!(
        s.login.as_ref().unwrap().stops(false)[4],
        LoginFocus::AfterNothing
    );
    for _ in 0..4 {
        s.feed_login("prod", b"\t");
    }
    s.feed_login("prod", b" ");
    assert_eq!(
        s.login.as_ref().unwrap().after_login,
        crate::model::AfterLogin::Nothing
    );
    s.feed_login("prod", b"\t ");
    let d = s.login.as_ref().unwrap();
    assert_eq!(d.after_login, crate::model::AfterLogin::SshConfig);
    s.feed_login("prod", b"\t ");
    let d = s.login.as_ref().unwrap();
    assert_eq!(d.after_login, crate::model::AfterLogin::RegisterKey);
    s.feed_login("prod", b"\x1b[A ");
    assert_eq!(
        s.login.as_ref().unwrap().after_login,
        crate::model::AfterLogin::SshConfig
    );
}

/// A state whose login pane for `prod` starts at what its ssh config already sets:
/// 192.0.2.7, port 2222, user dev.
fn state_with_saved_login() -> State {
    use crate::provision::env::{LoginDefaults, LoginValue};
    let mut s = State::default();
    let value = |value: &str| LoginValue {
        value: value.into(),
        provenance: "from ssh config",
    };
    s.chrome.set_login_defaults(
        [(
            "prod".to_string(),
            LoginDefaults {
                address: value("192.0.2.7"),
                port: value("2222"),
                username: value("dev"),
                ssh_effective: Some(crate::transport::Login {
                    address: Some("192.0.2.7".into()),
                    port: Some(2222),
                    user: Some("dev".into()),
                }),
            },
        )]
        .into(),
        Default::default(),
    );
    s.feed_login("prod", b"");
    s
}

#[test]
fn feed_login_hides_the_ssh_config_choice_when_ssh_config_already_sets_the_values() {
    let s = state_with_saved_login();
    let d = s.login.as_ref().unwrap();
    assert!(!d.offers_ssh_config());
    assert!(!d.stops(false).contains(&LoginFocus::AfterSshConfig));
    // ssh compares host names without case, so a differently cased address is the same.
    let mut upper = d.clone();
    upper.address = "HOST.EXAMPLE".into();
    upper.ssh_effective.as_mut().unwrap().address = Some("host.example".into());
    assert!(!upper.offers_ssh_config());
}

#[test]
fn feed_login_offers_the_ssh_config_choice_when_any_value_differs() {
    let s = state_with_saved_login();
    let d = s.login.as_ref().unwrap();
    for edit in [
        |d: &mut LoginDraft| d.address = "192.0.2.8".into(),
        |d: &mut LoginDraft| d.port = "22".into(),
        |d: &mut LoginDraft| d.username = "root".into(),
    ] {
        let mut changed = d.clone();
        edit(&mut changed);
        assert!(changed.offers_ssh_config(), "{changed:?}");
        assert!(changed.stops(false).contains(&LoginFocus::AfterSshConfig));
    }
    // A value ssh does not report cannot be known to match.
    let mut unresolved = d.clone();
    unresolved.ssh_effective.as_mut().unwrap().user = None;
    assert!(unresolved.offers_ssh_config());
    // Without a block naming the machine, recording makes it known to ssh.
    let mut unnamed = d.clone();
    unnamed.ssh_effective = None;
    assert!(unnamed.offers_ssh_config());
}

/// The login pane `feed_login` opens for `prod` from `config_text`, with `ssh -G`
/// reporting what it reports for any name: the alias as the host name, port 22 unless a
/// block sets another, and the local user.
fn login_pane_from_ssh_config(config_text: &str) -> LoginDraft {
    discovered_login_pane(config_text, None)
}

/// [`login_pane_from_ssh_config`] for a machine discovery found at `provider_address`.
fn discovered_login_pane(config_text: &str, provider_address: Option<&str>) -> LoginDraft {
    let stanza = crate::provision::config::stanza_login(config_text, "prod");
    let effective = crate::transport::Login {
        address: stanza.address.or(Some("prod".into())),
        port: stanza.port.or(Some(22)),
        user: stanza.user.or(Some("local-user".into())),
    };
    let defaults = crate::provision::config::login_defaults(
        "prod",
        provider_address,
        Some(&effective),
        config_text,
    );
    let mut s = State::default();
    s.chrome
        .set_login_defaults([("prod".to_string(), defaults)].into(), Default::default());
    s.feed_login("prod", b"");
    s.login.unwrap()
}

#[test]
fn feed_login_offers_the_ssh_config_choice_for_a_host_without_a_config_entry() {
    let mut d = login_pane_from_ssh_config(
        "Host other
    User bob
",
    );
    assert!(d.offers_ssh_config(), "{d:?}");
    d.username = "local-user".into();
    assert!(d.offers_ssh_config(), "{d:?}");
}

#[test]
fn feed_login_hides_the_ssh_config_choice_when_a_block_sets_only_the_user() {
    // The address and port ssh uses are its defaults, the alias and 22, which the pane
    // starts at too; neither is labelled as coming from ssh config.
    let text = "Host other prod web
    User dev
";
    let d = login_pane_from_ssh_config(text);
    assert_eq!((d.address.as_str(), d.port.as_str()), ("prod", "22"));
    assert!(!d.offers_ssh_config(), "{d:?}");
    let defaults = crate::provision::config::login_defaults("prod", None, None, text);
    assert_eq!(defaults.address.provenance, "host name");
    assert_eq!(defaults.port.provenance, "default");
    assert_eq!(defaults.username.provenance, "from ssh config");
    // A block reached only through a pattern does not name the machine.
    let d = login_pane_from_ssh_config(
        "Host pro*
    User local-user
",
    );
    assert!(d.offers_ssh_config(), "{d:?}");
}

#[test]
fn feed_login_offers_the_ssh_config_choice_for_a_discovered_address() {
    // ssh would connect to the name itself, not to the address discovery found.
    let d = discovered_login_pane(
        "Host prod
    User dev
",
        Some("192.0.2.10"),
    );
    assert_eq!(d.address, "192.0.2.10");
    assert!(d.offers_ssh_config(), "{d:?}");
}

#[test]
fn feed_login_offers_the_ssh_config_choice_only_when_the_stanza_differs() {
    let text = "Host prod
    HostName 192.0.2.7
    Port 2222
    User dev
";
    let d = login_pane_from_ssh_config(text);
    assert!(!d.offers_ssh_config(), "{d:?}");
    let mut port = d.clone();
    port.port = "2200".into();
    assert!(port.offers_ssh_config(), "{port:?}");
}

#[test]
fn feed_login_hides_the_ssh_config_choice_after_xmux_saved_the_values() {
    let saved = crate::provision::config::upsert_managed_stanza(
        "Host other
    User bob
",
        "prod",
        &crate::transport::Login {
            address: Some("192.0.2.7".into()),
            port: Some(22),
            user: Some("dev".into()),
        },
    );
    let d = login_pane_from_ssh_config(&saved);
    assert!(!d.offers_ssh_config(), "{d:?}");
}

#[test]
fn feed_login_focus_skips_the_hidden_ssh_config_choice() {
    let mut s = state_with_saved_login();
    for _ in 0..4 {
        s.feed_login("prod", b"\t");
    }
    assert_eq!(s.login.as_ref().unwrap().focus, LoginFocus::AfterNothing);
    s.feed_login("prod", b"\t");
    assert_eq!(s.login.as_ref().unwrap().focus, LoginFocus::AfterPublicKey);
    s.feed_login("prod", b"\x1b[A");
    assert_eq!(s.login.as_ref().unwrap().focus, LoginFocus::AfterNothing);
}

#[test]
fn feed_login_drops_the_ssh_config_pick_once_the_choice_hides() {
    let mut s = state_with_saved_login();
    // Port 22 differs from the saved 2222, so the choice appears and is picked.
    s.feed_login("prod", b"\t\x7f\x7f\x7f\x7f22\t\t\t\t ");
    let d = s.login.as_ref().unwrap();
    assert_eq!(d.focus, LoginFocus::AfterSshConfig);
    assert_eq!(d.after_login, crate::model::AfterLogin::SshConfig);
    // Editing the port back to the saved value hides the choice and the pick with it.
    s.feed_login("prod", b"\x1b[A\x1b[A\x1b[A\x1b[A\x7f\x7f2222");
    let d = s.login.as_ref().unwrap();
    assert_eq!(d.focus, LoginFocus::Port);
    assert_eq!(d.after_login, crate::model::AfterLogin::Nothing);
    match s.feed_login("prod", b"\t\t\t\t\t\r") {
        Some(crate::model::Command::RunLogin { after_login, .. }) => {
            assert_eq!(after_login, crate::model::AfterLogin::Nothing)
        }
        other => panic!("expected RunLogin, got {other:?}"),
    }
}

#[test]
fn feed_login_backspace_edits_and_a_new_host_resets_the_draft() {
    let mut s = State::default();
    s.feed_login("prod", b"X");
    s.feed_login("prod", b"\x7f");
    assert_eq!(s.login.as_ref().unwrap().address, "prod");
    // Moving to another blocked host starts a fresh draft (no stale value carried).
    s.feed_login("stage", b"");
    let d = s.login.as_ref().unwrap();
    assert_eq!(d.host, "stage");
    assert_eq!(
        d.address, "stage",
        "the fresh draft starts at its own defaults"
    );
}

#[test]
fn feed_login_never_lets_an_escape_sequence_land_in_a_field() {
    // A function key xmux does not act on is still a key, not text: none of its bytes
    // reach a field.
    let mut s = State::default();
    s.feed_login("prod", b"\x7f\x7f\x7f\x7fab");
    s.feed_login("prod", b"\x1b[1;5C");
    s.feed_login("prod", b"\x1bOP");
    assert_eq!(s.login.as_ref().unwrap().address, "ab");
}

#[test]
fn machine_probe_connected_forwards_the_connect_to_the_loop() {
    // A machine that answered `true` carries no reason; which of its hosts to
    // resolve, and how, lives in the host registry, so the whole decision is the
    // loop's.
    let mut state = State::from_hosts(vec!["prod".into()]);
    let mut sw = crate::ui::switcher::Switcher::from_hosts(&mut state);
    let mut connected = HashSet::new();
    let effects = host_event_effects_for_test(
        &mut state,
        HostEvent::MachineProbed {
            shell: None,
            machine: "prod".into(),
            err: None,
            password_supplied: false,
            credential_rejection_generation: None,
            credential_held: false,
            credential_generation: 0,
            current_credential_generation: 0,
            rescan: false,
        },
        &mut sw,
        &mut connected,
    );
    assert!(
        matches!(
            &effects[..],
            [EventEffect::MachineConnected {
                machine,
                rescan: false,
                ..
            }] if machine == "prod"
        ),
        "{effects:?}"
    );
}

#[test]
fn machine_probe_auth_failure_marks_every_host_of_the_machine_locked() {
    // The reachability probe is the single classification site: an auth failure
    // (ssh's `Permission denied (` signature) marks EVERY host the machine serves
    // locked, and folds nothing itself for the loop to run - no channel is opened.
    let mut state = State::from_hosts(vec!["prod".into(), "prod:zellij".into(), "db".into()]);
    let mut sw = crate::ui::switcher::Switcher::from_hosts(&mut state);
    let mut connected = HashSet::new();
    let effects = host_event_effects_for_test(
        &mut state,
        HostEvent::MachineProbed {
            shell: None,
            machine: "prod".into(),
            err: Some(
                "command failed (exit 255): user@prod: Permission denied (publickey,password)."
                    .into(),
            ),
            password_supplied: false,
            credential_rejection_generation: None,
            credential_held: false,
            credential_generation: 0,
            current_credential_generation: 0,
            rescan: false,
        },
        &mut sw,
        &mut connected,
    );
    assert!(
        effects.is_empty(),
        "a failed probe opens no channel: {effects:?}"
    );
    for host in ["prod", "prod:zellij"] {
        let g = state
            .groups
            .iter()
            .find(|g| g.host == host)
            .unwrap_or_else(|| panic!("{host} group"));
        assert_eq!(
            g.failure(),
            Some(crate::model::FailureKind::Blocked),
            "{host} classifies locked: {:?}",
            g.err
        );
    }
    let other = state.groups.iter().find(|g| g.host == "db").unwrap();
    assert!(other.err.is_none(), "another machine is untouched");
}

#[test]
fn a_refusal_that_did_not_use_the_held_password_is_visible() {
    let mut state = State::from_hosts(vec!["prod".into()]);
    state.logged_in.insert("prod".into());
    let mut sw = crate::ui::switcher::Switcher::from_hosts(&mut state);
    let mut connected = HashSet::new();
    let _ = host_event_effects_for_test(
        &mut state,
        HostEvent::MachineProbed {
            shell: None,
            machine: "prod".into(),
            err: Some("dev@prod: Permission denied (publickey,password).".into()),
            password_supplied: false,
            credential_rejection_generation: None,
            credential_held: true,
            credential_generation: 1,
            current_credential_generation: 1,
            rescan: false,
        },
        &mut sw,
        &mut connected,
    );
    assert_eq!(
        state.groups[0].failure(),
        Some(crate::model::FailureKind::Blocked)
    );
}

#[test]
fn an_auth_refusal_from_an_older_credential_generation_is_ignored() {
    let mut state = State::from_hosts(vec!["prod".into()]);
    state.groups[0].err = None;
    let mut sw = crate::ui::switcher::Switcher::from_hosts(&mut state);
    let mut connected = HashSet::new();
    let _ = host_event_effects_for_test(
        &mut state,
        HostEvent::MachineProbed {
            shell: None,
            machine: "prod".into(),
            err: Some("dev@prod: Permission denied (publickey,password).".into()),
            password_supplied: false,
            credential_rejection_generation: None,
            credential_held: true,
            credential_generation: 3,
            current_credential_generation: 4,
            rescan: false,
        },
        &mut sw,
        &mut connected,
    );
    assert!(state.groups[0].err.is_none());
}

#[test]
fn any_probe_result_from_an_older_credential_generation_is_ignored() {
    let mut state = State::from_hosts(vec!["prod".into()]);
    state.groups[0].err = None;
    state.scanning.insert("prod".into());
    let mut sw = crate::ui::switcher::Switcher::from_hosts(&mut state);
    let mut connected = HashSet::new();
    let effects = host_event_effects_for_test(
        &mut state,
        HostEvent::MachineProbed {
            shell: None,
            machine: "prod".into(),
            err: Some("ssh: connect to host prod port 22: Connection refused".into()),
            password_supplied: false,
            credential_rejection_generation: None,
            credential_held: true,
            credential_generation: 3,
            current_credential_generation: 4,
            rescan: false,
        },
        &mut sw,
        &mut connected,
    );
    assert!(effects.is_empty());
    assert!(state.groups[0].err.is_none());
    assert!(state.scanning.contains("prod"));
}

#[test]
fn successful_probe_from_an_older_credential_generation_is_ignored() {
    let mut state = State::from_hosts(vec!["prod".into()]);
    let mut sw = crate::ui::switcher::Switcher::from_hosts(&mut state);
    let mut connected = HashSet::new();
    let effects = host_event_effects_for_test(
        &mut state,
        HostEvent::MachineProbed {
            shell: None,
            machine: "prod".into(),
            err: None,
            password_supplied: false,
            credential_rejection_generation: None,
            credential_held: true,
            credential_generation: 3,
            current_credential_generation: 4,
            rescan: false,
        },
        &mut sw,
        &mut connected,
    );
    assert!(effects.is_empty());
}

#[test]
fn probe_that_rejected_its_own_credential_is_not_discarded_as_stale() {
    let mut state = State::from_hosts(vec!["prod".into()]);
    let mut sw = crate::ui::switcher::Switcher::from_hosts(&mut state);
    let mut connected = HashSet::new();
    let _ = host_event_effects_for_test(
        &mut state,
        HostEvent::MachineProbed {
            shell: None,
            machine: "prod".into(),
            err: Some("dev@prod: Permission denied (publickey,password).".into()),
            password_supplied: true,
            credential_rejection_generation: Some(4),
            credential_held: false,
            credential_generation: 3,
            current_credential_generation: 4,
            rescan: false,
        },
        &mut sw,
        &mut connected,
    );
    assert_eq!(
        state.groups[0].failure(),
        Some(crate::model::FailureKind::Blocked)
    );
}

#[test]
fn rejected_probe_from_before_a_newer_key_login_is_ignored() {
    let mut state = State::from_hosts(vec!["prod".into()]);
    state.groups[0].err = None;
    let mut sw = crate::ui::switcher::Switcher::from_hosts(&mut state);
    let mut connected = HashSet::new();
    let effects = host_event_effects_for_test(
        &mut state,
        HostEvent::MachineProbed {
            shell: None,
            machine: "prod".into(),
            err: Some("dev@prod: Permission denied (publickey,password).".into()),
            password_supplied: true,
            credential_rejection_generation: Some(4),
            credential_held: false,
            credential_generation: 3,
            current_credential_generation: 5,
            rescan: false,
        },
        &mut sw,
        &mut connected,
    );
    assert!(effects.is_empty());
    assert!(state.groups[0].err.is_none());
}

#[test]
fn machine_probe_unreachable_marks_the_machine_unreachable_not_locked() {
    // A reach failure (refused/timeout/no route) is unreachable, never locked: only
    // ssh's auth-failure signature earns locked, so a machine that merely died stays a
    // plain unreachable card.
    let mut state = State::from_hosts(vec!["prod".into()]);
    let mut sw = crate::ui::switcher::Switcher::from_hosts(&mut state);
    let mut connected = HashSet::new();
    let _ = host_event_effects_for_test(
        &mut state,
        HostEvent::MachineProbed {
            shell: None,
            machine: "prod".into(),
            err: Some("ssh: connect to host prod port 22: Connection refused".into()),
            password_supplied: false,
            credential_rejection_generation: None,
            credential_held: true,
            credential_generation: 1,
            current_credential_generation: 1,
            rescan: false,
        },
        &mut sw,
        &mut connected,
    );
    let g = state.groups.iter().find(|g| g.host == "prod").unwrap();
    assert!(g.err.is_some(), "the card is unreachable");
    assert_eq!(
        g.failure(),
        Some(crate::model::FailureKind::Unreachable),
        "a reach failure is not locked: {:?}",
        g.err
    );
}

#[test]
fn host_event_scanned_emits_dispatch_carrying_the_detection() {
    // The detection box + the host-channel dispatch are loop-owned; update
    // forwards the descriptor. The host already has sessions (not scanning), so a
    // failed detection does not settle it - only a still-scanning card settles.
    let (mut state, mut sw) = with_switcher(one_session_scan());
    let mut connected = HashSet::new();
    let effects = host_event_effects_for_test(
        &mut state,
        HostEvent::Scanned {
            host: "jup".into(),
            detected: None,
            err: Some("command failed (exit 127): sh: tmux: not found".into()),
        },
        &mut sw,
        &mut connected,
    );
    assert!(
        matches!(
            effects.as_slice(),
            [EventEffect::DispatchScanned {
                host,
                detected: None,
                ..
            }] if host == "jup"
        ),
        "Scanned forwards a DispatchScanned effect: {effects:?}"
    );
    let g = state.groups.iter().find(|g| g.host == "jup").unwrap();
    assert!(
        g.err.is_none(),
        "a settled host keeps its state; a stray detection failure does not touch it"
    );
}

#[test]
fn a_connected_machines_failed_detection_settles_the_scanning_card() {
    // A host that reached the connection stage (a local/WSL machine connected
    // inline, or a remote whose machine probe succeeded) but whose mux detection
    // failed must leave the scanning state: it settles as unreachable with the
    // probe's error instead of spinning forever (issue 226).
    let mut state = State::from_hosts(vec!["jup".into()]);
    let mut sw = crate::ui::switcher::Switcher::from_hosts(&mut state);
    let mut connected = HashSet::new();
    assert!(state.scanning.contains("jup"), "precondition: scanning");
    let effects = host_event_effects_for_test(
        &mut state,
        HostEvent::Scanned {
            host: "jup".into(),
            detected: None,
            err: Some("command failed (exit 127): sh: tmux: not found".into()),
        },
        &mut sw,
        &mut connected,
    );
    assert!(
        !state.scanning.contains("jup"),
        "the failed detection settles the card out of scanning"
    );
    let g = state.groups.iter().find(|g| g.host == "jup").unwrap();
    assert_eq!(
        g.err.as_deref(),
        Some("command failed (exit 127): sh: tmux: not found"),
        "the card carries the detection error"
    );
    assert!(g.sessions.is_empty());
    assert!(
        matches!(
            effects.as_slice(),
            [EventEffect::DispatchScanned {
                host,
                detected: None,
                ..
            }] if host == "jup"
        ),
        "the detection box still forwards to the loop: {effects:?}"
    );
}

#[test]
fn a_stray_detection_failure_does_not_overwrite_a_settled_card() {
    // The reconnect sweep retries detection for undetected hosts even after they
    // settled unreachable/locked. That later failure must NOT overwrite the card's
    // existing reason - only a still-scanning card settles on detection failure.
    let mut state = State::from_hosts(vec!["jup".into()]);
    let mut sw = crate::ui::switcher::Switcher::from_hosts(&mut state);
    let mut connected = HashSet::new();
    let _ = host_event_effects_for_test(
        &mut state,
        HostEvent::MachineProbed {
            shell: None,
            machine: "jup".into(),
            err: Some("hrlee@jup: Permission denied (publickey,password).".into()),
            password_supplied: false,
            credential_rejection_generation: None,
            credential_held: false,
            credential_generation: 0,
            current_credential_generation: 0,
            rescan: false,
        },
        &mut sw,
        &mut connected,
    );
    assert!(
        !state.scanning.contains("jup"),
        "the machine probe settled the card first"
    );
    let _ = host_event_effects_for_test(
        &mut state,
        HostEvent::Scanned {
            host: "jup".into(),
            detected: None,
            err: Some("command failed (exit 255)".into()),
        },
        &mut sw,
        &mut connected,
    );
    let g = state.groups.iter().find(|g| g.host == "jup").unwrap();
    assert_eq!(
        g.err.as_deref(),
        Some("hrlee@jup: Permission denied (publickey,password)."),
        "a settled reason is not overwritten by a stray detection failure"
    );
}

#[test]
fn muxes_found_forwards_the_add_to_the_loop() {
    // Which muxes a machine ALREADY serves lives in the host registry, which this
    // layer does not hold, so the whole decision is forwarded rather than folded.
    let mut state = State::from_hosts(vec!["prod".into()]);
    let mut sw = crate::ui::switcher::Switcher::from_hosts(&mut state);
    let mut connected = HashSet::new();
    let before = state.groups.len();
    let effects = host_event_effects_for_test(
        &mut state,
        HostEvent::MuxesFound {
            machine: "prod".into(),
            muxes: Ok(vec!["tmux".into(), "zellij".into()]),
        },
        &mut sw,
        &mut connected,
    );
    assert!(
        matches!(
            &effects[..],
            [EventEffect::AddDiscoveredHosts { machine, muxes }]
                if machine == "prod"
                    && muxes == &Ok(vec!["tmux".to_string(), "zellij".to_string()])
        ),
        "{effects:?}"
    );
    assert_eq!(state.groups.len(), before, "and folds nothing itself");
}

#[test]
fn clear_screen_wipes_the_screen_and_repaints_every_cell() {
    use ratatui::widgets::Paragraph;
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(4, 2)).unwrap();

    // Frame 1 fills the screen, so the buffers and the backend hold non-default cells.
    term.draw(|f| f.render_widget(Paragraph::new("xxxx\nxxxx"), f.area()))
        .unwrap();
    // The wipe blanks the backend with no cursor query, and leaves no stale buffer
    // content for the next diff to treat as already drawn.
    clear_screen(&mut term).unwrap();
    term.backend().assert_buffer_lines(["    ", "    "]);
    // Frame 2 draws one cell: everything it does not draw is blank on the screen,
    // not frame 1's `x`.
    term.draw(|f| f.render_widget(Paragraph::new("y"), f.area()))
        .unwrap();
    term.backend().assert_buffer_lines(["y   ", "    "]);
}

fn collapse_rt(position: crate::ui::switcher::NavPosition) -> Runtime {
    use crate::ui::switcher::{Scan, Switcher};
    let mut state = crate::state::State::from_scan(Scan { groups: vec![] });
    let switcher = Switcher::new(&mut state);
    let mut rt = test_rt(fake_env_with_machines(&["local"]));
    rt.model.state = state;
    rt.model.switcher = switcher;
    rt.cols = 140;
    rt.body_rows = 29;
    rt.model.nav_position = position;
    rt.model.nav_position_pinned = Some(position);
    sync_test_render_plan(&mut rt);
    rt
}

const EVERY_POSITION: [crate::ui::switcher::NavPosition; 4] = [
    crate::ui::switcher::NavPosition::Left,
    crate::ui::switcher::NavPosition::Right,
    crate::ui::switcher::NavPosition::Top,
    crate::ui::switcher::NavPosition::Bottom,
];

fn mouse(cb: u16, col: u16, row: u16, pressed: bool) -> crate::display::mouse::MouseEvent {
    crate::display::mouse::MouseEvent {
        cb,
        col,
        row,
        pressed,
    }
}

#[test]
fn prefix_z_toggles_the_collapse_from_either_view() {
    let mut rt = collapse_rt(crate::ui::switcher::NavPosition::Left);
    rt.handle_stdin_bytes(b"\x07z", &Selection::default());
    assert!(rt.model.nav_collapsed, "prefix z collapses from nav focus");
    rt.handle_stdin_bytes(b"\x07z", &Selection::default());
    assert!(!rt.model.nav_collapsed, "a second prefix z expands");
    rt.model
        .state
        .focus
        .set_view_focus(crate::app::focus::ViewFocus::Terminal);
    let out = rt.handle_stdin_bytes(b"\x07z", &Selection::default());
    assert!(
        rt.model.nav_collapsed,
        "prefix z collapses from terminal focus"
    );
    assert!(!out.focus_nav, "the terminal keeps the focus");
}

#[test]
fn a_popup_takes_hover_and_a_click_on_its_entry_runs_it_as_enter_does() {
    let sel = Selection::default();
    let mut rt = collapse_rt(crate::ui::switcher::NavPosition::Left);
    let effects = update(&mut rt.model, Msg::TogglePalette);
    assert!(effects.is_empty());
    rt.handle_stdin_bytes(b"quit xmux", &sel);
    sync_test_render_plan(&mut rt);
    let r = rt.model.render_plan.popup_rect;
    // SGR cells are 1-based; the one match is the popup's first inner row under the query
    // field and its rule.
    let (col, row) = (r.x + 4, r.y + 2 + crate::ui::modal::PALETTE_LEAD);
    let event = |rt: &mut Runtime, ev| {
        let mut quit = false;
        let dirty = rt.handle_mouse_event(&ev, &sel, &mut false, &mut false, &mut quit, &mut false);
        (dirty, quit)
    };
    assert_eq!(
        event(&mut rt, mouse(35, col, row, true)),
        (true, false),
        "bare motion onto the entry sets the hover and redraws"
    );
    assert_eq!(rt.model.state.modal_hover(), Some(0));
    assert_eq!(
        event(&mut rt, mouse(35, col, row, true)),
        (false, false),
        "motion within the same entry changes nothing"
    );
    event(&mut rt, mouse(0, col, row, true));
    assert!(
        rt.model.state.modal.is_some(),
        "a press alone executes nothing"
    );
    let (_, quit) = event(&mut rt, mouse(0, col, row, false));
    assert!(quit, "the release on the pressed cell runs the entry");
    assert!(rt.model.state.modal.is_none());
}

/// A drag moves the popup under the pointer, so the hover set before it does not
/// outlive it: the popup shows its selection until the pointer moves again.
#[test]
fn a_popup_drag_drops_the_hover_it_started_on() {
    let sel = Selection::default();
    let mut rt = collapse_rt(crate::ui::switcher::NavPosition::Left);
    let effects = update(&mut rt.model, Msg::TogglePalette);
    assert!(effects.is_empty());
    rt.handle_stdin_bytes(b"quit xmux", &sel);
    sync_test_render_plan(&mut rt);
    let r = rt.model.render_plan.popup_rect;
    let (col, row) = (r.x + 4, r.y + 2 + crate::ui::modal::PALETTE_LEAD);
    let event = |rt: &mut Runtime, ev| {
        rt.handle_mouse_event(&ev, &sel, &mut false, &mut false, &mut false, &mut false)
    };
    event(&mut rt, mouse(35, col, row, true));
    assert_eq!(rt.model.state.modal_hover(), Some(0));
    event(&mut rt, mouse(0, col, row, true));
    event(&mut rt, mouse(32, col.saturating_sub(3), row + 2, true));
    event(&mut rt, mouse(0, col.saturating_sub(3), row + 2, false));
    assert!(rt.model.state.modal.is_some(), "a drag executes nothing");
    assert_eq!(rt.model.state.modal_hover(), None);
}

#[test]
fn dragging_the_nav_border_past_the_minimum_collapses_the_nav_at_every_position() {
    use crate::ui::switcher::NavPosition;
    let sel = Selection::default();
    for position in EVERY_POSITION {
        let mut rt = collapse_rt(position);
        let nav_border = rt.model.render_plan.regions.nav_border;
        rt.handle_mouse_event(
            &mouse(0, nav_border.x + 1, nav_border.y + 1, true),
            &sel,
            &mut false,
            &mut false,
            &mut false,
            &mut false,
        );
        assert!(
            rt.model.mouse_state.dragging_nav_border,
            "{position:?}: the press grabs the nav border"
        );
        let (col, row) = match position {
            NavPosition::Left => (1, nav_border.y + 1),
            NavPosition::Right => (140, nav_border.y + 1),
            NavPosition::Top => (nav_border.x + 1, 1),
            NavPosition::Bottom => (nav_border.x + 1, 30),
            NavPosition::Floating => unreachable!(),
        };
        rt.handle_mouse_event(
            &mouse(0x20, col, row, true),
            &sel,
            &mut false,
            &mut false,
            &mut false,
            &mut false,
        );
        assert!(
            rt.model.nav_collapsed,
            "{position:?}: dragging past the minimum collapses the nav"
        );
        let (col, row) = match position {
            NavPosition::Left => (61, nav_border.y + 1),
            NavPosition::Right => (80, nav_border.y + 1),
            NavPosition::Top => (nav_border.x + 1, 11),
            NavPosition::Bottom => (nav_border.x + 1, 20),
            NavPosition::Floating => unreachable!(),
        };
        rt.handle_mouse_event(
            &mouse(0x20, col, row, true),
            &sel,
            &mut false,
            &mut false,
            &mut false,
            &mut false,
        );
        assert!(
            !rt.model.nav_collapsed,
            "{position:?}: dragging back out expands it within the same drag"
        );
        rt.handle_mouse_event(
            &mouse(0, col, row, false),
            &sel,
            &mut false,
            &mut false,
            &mut false,
            &mut false,
        );
        assert!(!rt.model.mouse_state.dragging_nav_border);
    }
}

#[test]
fn a_click_anywhere_on_a_collapsed_nav_expands_it_at_every_position() {
    use crate::ui::switcher::NavPosition;
    for position in EVERY_POSITION {
        let mut rt = collapse_rt(position);
        rt.model.nav_collapsed = true;
        rt.model.applied_nav_collapsed = true;
        rt.model.nav_width = crate::ui::switcher::collapsed_nav_width(&rt.env.ui_prefix);
        sync_test_render_plan(&mut rt);
        let (col, row) = match position {
            NavPosition::Left | NavPosition::Top => (1, 1),
            NavPosition::Right => (140, 1),
            NavPosition::Bottom => (1, 30),
            NavPosition::Floating => unreachable!(),
        };
        let focus_before = rt.model.state.focus;
        let mut focus_toggle = false;
        rt.handle_mouse_event(
            &mouse(0, col, row, true),
            &Selection::default(),
            &mut focus_toggle,
            &mut false,
            &mut false,
            &mut false,
        );
        assert!(!rt.model.nav_collapsed, "{position:?}: the click expands");
        assert_eq!(rt.model.state.focus, focus_before, "{position:?}");
        assert!(!focus_toggle, "{position:?}: the click is not a focus move");
        assert!(
            !rt.model.mouse_state.dragging_nav_border,
            "{position:?}: the click is not a drag"
        );
    }
}

/// A collapsed side column is exactly the prefix wide and its border lies inside it, so
/// the expand target is those three columns on every row, border cells and prefix row
/// included, and the next column over already belongs to the terminal view. Once the
/// click expands the nav, the border stands in its own column again and only that
/// column grabs a resize drag.
#[test]
fn a_collapsed_side_nav_expands_from_exactly_its_prefix_column() {
    use crate::ui::switcher::NavPosition;
    let collapsed_rt = |position| {
        let mut rt = collapse_rt(position);
        rt.model.nav_collapsed = true;
        rt.model.applied_nav_collapsed = true;
        rt.model.nav_width = crate::ui::switcher::collapsed_nav_width(&rt.env.ui_prefix);
        sync_test_render_plan(&mut rt);
        rt
    };
    for position in [NavPosition::Left, NavPosition::Right] {
        // 1-based SGR columns of the three prefix cells and the first terminal column.
        let (inside, outside) = match position {
            NavPosition::Left => ([1, 2, 3], 4),
            _ => ([138, 139, 140], 137),
        };
        let border = collapsed_rt(position).model.render_plan.regions.nav_border;
        assert_eq!(
            border.x + 1,
            inside[if position == NavPosition::Left { 2 } else { 0 }]
        );
        for col in inside {
            for row in [1, 15, 30] {
                let mut rt = collapsed_rt(position);
                rt.handle_mouse_event(
                    &mouse(0, col, row, true),
                    &Selection::default(),
                    &mut false,
                    &mut false,
                    &mut false,
                    &mut false,
                );
                assert!(
                    !rt.model.nav_collapsed,
                    "{position:?}: a click at ({col}, {row}) expands"
                );
                assert!(!rt.model.mouse_state.dragging_nav_border);
            }
        }
        let mut rt = collapsed_rt(position);
        rt.handle_mouse_event(
            &mouse(0, outside, 1, true),
            &Selection::default(),
            &mut false,
            &mut false,
            &mut false,
            &mut false,
        );
        assert!(
            rt.model.nav_collapsed,
            "{position:?}: the column beside it is the terminal view"
        );

        let mut rt = collapsed_rt(position);
        rt.handle_mouse_event(
            &mouse(0, inside[0], 1, true),
            &Selection::default(),
            &mut false,
            &mut false,
            &mut false,
            &mut false,
        );
        rt.handle_mouse_event(
            &mouse(0, inside[0], 1, false),
            &Selection::default(),
            &mut false,
            &mut false,
            &mut false,
            &mut false,
        );
        rt.model.nav_width = rt.model.nav_width_natural;
        sync_test_render_plan(&mut rt);
        let border = rt.model.render_plan.regions.nav_border;
        let beside = if position == NavPosition::Left {
            border.x
        } else {
            border.x + 2
        };
        rt.handle_mouse_event(
            &mouse(0, beside, 1, true),
            &Selection::default(),
            &mut false,
            &mut false,
            &mut false,
            &mut false,
        );
        assert!(
            !rt.model.mouse_state.dragging_nav_border,
            "{position:?}: the cell beside the expanded border does not grab it"
        );
        rt.handle_mouse_event(
            &mouse(0, border.x + 1, 1, true),
            &Selection::default(),
            &mut false,
            &mut false,
            &mut false,
            &mut false,
        );
        assert!(
            rt.model.mouse_state.dragging_nav_border,
            "{position:?}: the expanded border grabs a resize drag"
        );
    }
}

#[test]
fn terminal_prefix_info_selects_the_host_screen() {
    let mut rt = rt_terminal_focus_with_session();
    rt.handle_stdin_bytes(b"\x07i", &Selection::default());
    assert_eq!(
        rt.model.switcher.current_view_screen(&rt.model.state),
        Some(crate::model::ViewScreen::Host)
    );
}

#[test]
fn unreachable_screen_details_take_terminal_input() {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut rt = test_rt(fake_env_with_machines(&["prod"]));
    crate::app::model::update(
        &mut rt.model,
        crate::app::model::Msg::HostEvent {
            event: crate::link::HostEvent::Sessions {
                host: "local".into(),
                sessions: vec![],
                err: None,
            },
            logged_in: Default::default(),
        },
    );
    crate::app::model::update(
        &mut rt.model,
        crate::app::model::Msg::HostEvent {
            event: crate::link::HostEvent::Sessions {
                host: "prod".into(),
                sessions: vec![],
                err: Some("connection refused".into()),
            },
            logged_in: Default::default(),
        },
    );
    rt.model.switcher.handle_key(
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        &mut rt.model.state,
    );
    rt.model.state.apply(crate::model::Action::Focus(
        crate::model::FocusTarget::Terminal,
    ));
    assert!(
        rt.model
            .switcher
            .current_unreachable_screen(&rt.model.state),
        "host={:?}, screen={:?}, groups={:?}",
        rt.model.switcher.current_host(),
        rt.model.switcher.current_view_screen(&rt.model.state),
        rt.model.state.groups
    );
    rt.handle_stdin_bytes(b"d", &Selection::default());
    assert!(rt.model.state.host_details.contains("prod"));
    rt.handle_stdin_bytes(b"dd", &Selection::default());
    assert!(rt.model.state.host_details.contains("prod"));
}

/// A registration already past its login holds the machine's key gate, so a logout's key
/// search waits for the appended line and finds it rather than finishing first.
#[tokio::test]
async fn a_logouts_key_search_waits_for_a_registration_under_way() {
    let gates = KeyGates::default();
    let order = Arc::new(std::sync::Mutex::new(Vec::new()));
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let registration = tokio::spawn({
        let gate = gates.of("box");
        let order = order.clone();
        let cancel = cancel.clone();
        async move {
            follow_ups_at_key_gate(&gate, &cancel, |go| async move {
                assert!(go, "the login was not cancelled when it took the gate");
                started_tx.send(()).unwrap();
                release_rx.await.unwrap();
                order.lock().unwrap().push("registered");
            })
            .await
        }
    });
    started_rx.await.unwrap();
    cancel.store(true, std::sync::atomic::Ordering::Release);
    let search = tokio::spawn({
        let gates = gates.clone();
        let order = order.clone();
        async move {
            gates.hold("box").await;
            order.lock().unwrap().push("searched");
        }
    });
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    assert!(order.lock().unwrap().is_empty(), "the search waits");
    let other = gates.of("elsewhere");
    assert!(other.try_lock().is_ok(), "another machine's gate is free");
    release_tx.send(()).unwrap();
    registration.await.unwrap();
    search.await.unwrap();
    assert_eq!(*order.lock().unwrap(), vec!["registered", "searched"]);
}

/// A logout cancels the login before its search takes the gate, so a login that reaches
/// the gate after the search registers nothing.
#[tokio::test]
async fn a_login_a_logout_cancelled_does_no_follow_ups_at_the_gate() {
    let gates = KeyGates::default();
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    gates.hold("box").await;
    cancel.store(true, std::sync::atomic::Ordering::Release);
    let login = tokio::spawn({
        let gate = gates.of("box");
        let cancel = cancel.clone();
        async move { follow_ups_at_key_gate(&gate, &cancel, |go| async move { go }).await }
    });
    tokio::task::yield_now().await;
    gates.release("box");
    assert!(
        !login.await.unwrap(),
        "the follow-ups are told to do nothing"
    );
}

/// A logout keeps the machine's gate from its key search until it clears the machine, so
/// no login follow-up on that machine runs between the search and the removal.
#[tokio::test]
async fn a_login_follow_up_waits_until_the_logout_releases_the_gate() {
    let gates = KeyGates::default();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    gates.hold("box").await;
    let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let login = tokio::spawn({
        let gate = gates.of("box");
        let ran = ran.clone();
        async move {
            follow_ups_at_key_gate(&gate, &cancel, |_| async move {
                ran.store(true, std::sync::atomic::Ordering::Release);
            })
            .await
        }
    });
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    assert!(
        !ran.load(std::sync::atomic::Ordering::Acquire),
        "the follow-up waits"
    );
    gates.release("box");
    login.await.unwrap();
    assert!(ran.load(std::sync::atomic::Ordering::Acquire));
}

/// A runtime whose nav holds `gpu` (one session) and `web` (two), on a 140x30 screen with
/// the nav on the left. The launch selection is `gpu/train`.
fn hierarchy_rt() -> Runtime {
    use crate::ui::switcher::{Scan, Switcher};
    let group = |host: &str, names: &[&str]| crate::model::Group {
        host: host.into(),
        err: None,
        sessions: names
            .iter()
            .map(|name| crate::session::Session {
                host: host.into(),
                name: (*name).into(),
                windows: 1,
                ..Default::default()
            })
            .collect(),
    };
    let mut state = State::from_scan(Scan {
        groups: vec![group("gpu", &["train"]), group("web", &["api", "deploy"])],
    });
    let switcher = Switcher::new(&mut state);
    let mut rt = test_rt(fake_env_with_machines(&["local"]));
    rt.hosts = crate::model::Hosts::default();
    rt.model.state = state;
    rt.model.switcher = switcher;
    rt.cols = 140;
    rt.body_rows = 29;
    sync_test_render_plan(&mut rt);
    rt
}

fn selected(rt: &Runtime) -> Option<crate::model::Node> {
    rt.model.switcher.selected_node()
}

fn session_node(host: &str, name: &str) -> Option<crate::model::Node> {
    Some(crate::model::Node::Session(crate::session::Address::new(
        host, name,
    )))
}

#[test]
fn ctrl_arrows_in_nav_focus_walk_the_hierarchy_and_the_prefix_layer_keeps_its_own() {
    use crate::model::Node;
    let mut rt = hierarchy_rt();
    assert_eq!(selected(&rt), session_node("gpu", "train"));
    rt.handle_stdin_bytes(b"\x1b[1;5A", &Selection::default());
    assert_eq!(selected(&rt), Some(Node::Host("gpu".into())));
    rt.handle_stdin_bytes(b"\x1b[1;5A", &Selection::default());
    assert_eq!(selected(&rt), Some(Node::Machine("gpu".into())));
    // Behind the prefix, Ctrl+↑ is the horizontal nav border, never a level step.
    rt.handle_stdin_bytes(b"\x07\x1b[1;5B", &Selection::default());
    assert_eq!(selected(&rt), Some(Node::Machine("gpu".into())));
    // A bare Ctrl+arrow right after it repeats the resize; once another key ends the
    // resize mode, the bare keys step the levels again.
    rt.model.mouse_state.resizing = false;
    rt.handle_stdin_bytes(b"\x1b[1;5B\x1b[1;5B", &Selection::default());
    assert_eq!(selected(&rt), session_node("gpu", "train"));
}

#[test]
fn the_arrows_and_enter_walk_and_open_a_screens_links_in_terminal_focus() {
    let mut rt = hierarchy_rt();
    rt.handle_stdin_bytes(b"\x1b[1;5A", &Selection::default()); // the gpu host
    rt.model
        .state
        .focus
        .set_view_focus(crate::app::focus::ViewFocus::Terminal);
    rt.model.switcher.sync_view_focus(true);
    sync_test_render_plan(&mut rt);
    // The host's links are its session, its actions, and last its machine: ↑ from the
    // session wraps to the machine and ↓ wraps back to the session.
    rt.handle_stdin_bytes(b"\x1b[A\x1b[B\r", &Selection::default());
    assert_eq!(selected(&rt), session_node("gpu", "train"));
    rt.handle_stdin_bytes(b"\x1b[1;5A", &Selection::default());
    assert_eq!(
        selected(&rt),
        session_node("gpu", "train"),
        "the terminal view's keys are the pane's, or the screen's own"
    );
}

#[test]
fn hovering_a_nav_card_previews_it_and_a_click_executes_it() {
    let mut rt = hierarchy_rt();
    let deploy = rt.model.switcher.session_row("web", "deploy").unwrap();
    let rect = rt
        .model
        .render_plan
        .nav_cells
        .iter()
        .find(|(i, _)| *i == deploy)
        .map(|(_, r)| *r)
        .unwrap();
    let at = |cb: u16| crate::display::mouse::MouseEvent {
        cb,
        col: rect.x + 1,
        row: rect.y + 1,
        pressed: true,
    };
    // cb 35: motion with no button held.
    let dirty = rt.handle_mouse_event(
        &at(35),
        &Selection::default(),
        &mut false,
        &mut false,
        &mut false,
        &mut false,
    );
    assert!(dirty, "a new hover repaints");
    assert_eq!(
        selected(&rt),
        session_node("gpu", "train"),
        "hovering moves nothing"
    );
    assert_eq!(rt.model.switcher.terminal_view_target().target, "deploy");
    assert!(rt.model.state.focus.is_nav_focused());

    rt.handle_mouse_event(
        &at(0),
        &Selection::default(),
        &mut false,
        &mut false,
        &mut false,
        &mut false,
    );
    assert_eq!(selected(&rt), session_node("web", "deploy"));
    assert!(
        rt.model.state.focus.is_terminal_focused(),
        "a click executes: the terminal view takes the focus"
    );
}

#[test]
fn moving_to_the_nav_border_ends_nav_and_link_hover() {
    for terminal in [false, true] {
        let mut rt = if terminal {
            host_screen_with_new_session_link().0
        } else {
            hierarchy_rt()
        };
        let rect = if terminal {
            rt.model.render_plan.view_links[0].1
        } else {
            let row = rt.model.switcher.session_row("web", "deploy").unwrap();
            rt.model
                .render_plan
                .nav_cells
                .iter()
                .find(|(i, _)| *i == row)
                .unwrap()
                .1
        };
        for rect in [rect, rt.model.render_plan.regions.nav_border] {
            let event = crate::display::mouse::MouseEvent {
                cb: 35,
                col: rect.x + 1,
                row: rect.y + 1,
                pressed: true,
            };
            rt.handle_mouse_event(
                &event,
                &Selection::default(),
                &mut false,
                &mut false,
                &mut false,
                &mut false,
            );
        }
        assert_eq!(rt.model.switcher.hover_targets(), (None, None));
        assert!(rt.model.mouse_state.hovered_nav_border);
    }
}

#[test]
fn inventory_changes_end_hover_before_a_host_card_becomes_a_title() {
    let mut rt = hierarchy_rt();
    update(
        &mut rt.model,
        Msg::ApplyInventory {
            host: "web".into(),
            sessions: vec![],
            live: true,
        },
    );
    sync_test_render_plan(&mut rt);
    // The host card follows gpu's session in nav order; its trailing cell is outside
    // the machine half and targets the whole host.
    let host_row = rt.model.render_plan.nav_cells.last().unwrap().1;
    update(
        &mut rt.model,
        Msg::Hover {
            col: host_row.right() - 1,
            row: host_row.y,
        },
    );
    assert!(rt.model.switcher.hover_targets().0.is_some());
    assert_eq!(
        rt.model.switcher.shown_node(),
        Some(crate::model::Node::Host("web".into()))
    );
    update(
        &mut rt.model,
        Msg::ApplyInventory {
            host: "web".into(),
            sessions: vec![crate::session::Session {
                host: "web".into(),
                name: "new".into(),
                ..Default::default()
            }],
            live: true,
        },
    );
    assert_eq!(rt.model.switcher.hover_targets(), (None, None));
}

#[test]
fn changed_link_geometry_ends_hover_without_moving_selection() {
    let (mut rt, _) = host_screen_with_new_session_link();
    let rect = rt.model.render_plan.view_links[0].1;
    update(
        &mut rt.model,
        Msg::Hover {
            col: rect.x,
            row: rect.y,
        },
    );
    assert!(rt.model.switcher.hover_targets().1.is_some());
    let selection = rt.model.switcher.selected_node();
    let mut resized = rt.model.render_plan.clone();
    resized.view_links[0].1.y += 1;
    update(&mut rt.model, Msg::ReconcileHoverGeometry(resized));
    assert_eq!(rt.model.switcher.hover_targets(), (None, None));
    assert_eq!(rt.model.switcher.selected_node(), selection);
}

#[test]
fn reordered_inventory_does_not_retarget_a_stationary_link_hover() {
    let (mut rt, _) = host_screen_with_new_session_link();
    let rect = rt.model.render_plan.view_links[0].1;
    update(
        &mut rt.model,
        Msg::Hover {
            col: rect.x,
            row: rect.y,
        },
    );
    assert!(rt.model.switcher.hover_targets().1.is_some());
    let selection = rt.model.switcher.selected_node();
    let mut sessions = rt
        .model
        .state
        .groups
        .iter()
        .find(|g| g.host == "web")
        .unwrap()
        .sessions
        .clone();
    sessions.push(crate::session::Session {
        host: "web".into(),
        name: "aaa".into(),
        ..Default::default()
    });
    update(
        &mut rt.model,
        Msg::ApplyInventory {
            host: "web".into(),
            sessions,
            live: true,
        },
    );
    assert_eq!(rt.model.switcher.hover_targets(), (None, None));
    assert_eq!(rt.model.switcher.selected_node(), selection);
}

#[test]
fn a_click_on_a_screen_link_opens_it() {
    let mut rt = hierarchy_rt();
    rt.handle_stdin_bytes(b"\x1b[B\x1b[1;5A", &Selection::default()); // the web host
    assert_eq!(selected(&rt), Some(crate::model::Node::Host("web".into())));
    rt.model
        .state
        .focus
        .set_view_focus(crate::app::focus::ViewFocus::Terminal);
    rt.model.switcher.sync_view_focus(true);
    sync_test_render_plan(&mut rt);
    let (_, rect) = rt
        .model
        .render_plan
        .view_links
        .iter()
        .find(|(i, _)| *i == 1)
        .copied()
        .expect("the second session's link is painted");
    let press = crate::display::mouse::MouseEvent {
        cb: 0,
        col: rect.x + 1,
        row: rect.y + 1,
        pressed: true,
    };
    rt.handle_mouse_event(
        &press,
        &Selection::default(),
        &mut false,
        &mut false,
        &mut false,
        &mut false,
    );
    assert_eq!(selected(&rt), session_node("web", "deploy"));
}

/// The web host's screen in terminal focus, and the index of its link that starts a new
/// session.
fn host_screen_with_new_session_link() -> (Runtime, usize) {
    use crate::model::keys::KeyCommand;
    use crate::model::ScreenAction;
    let mut rt = hierarchy_rt();
    rt.handle_stdin_bytes(b"\x1b[B\x1b[1;5A", &Selection::default()); // the web host
    rt.model
        .state
        .focus
        .set_view_focus(crate::app::focus::ViewFocus::Terminal);
    rt.model.switcher.sync_view_focus(true);
    sync_test_render_plan(&mut rt);
    let new = rt
        .model
        .switcher
        .shown_links(&rt.model.state)
        .iter()
        .position(|l| {
            l.target
                == crate::ui::chrome::LinkTarget::Action(ScreenAction::Command(
                    KeyCommand::NewSession,
                ))
        })
        .expect("the host screen offers a new session");
    (rt, new)
}

fn new_session_input_open(rt: &Runtime) -> bool {
    matches!(
        &rt.model.state.modal,
        Some(crate::state::Modal::Input(input)) if input.mode == crate::state::InputMode::New
    )
}

#[test]
fn enter_on_an_action_link_runs_what_its_key_runs() {
    let (mut rt, new) = host_screen_with_new_session_link();
    assert_eq!(new, 2, "after the host's two sessions");
    // Two steps down from the first session reach the action.
    rt.handle_stdin_bytes(b"\x1b[B\x1b[B", &Selection::default());
    assert_eq!(
        rt.model
            .switcher
            .link_selection_and_hover(&rt.model.state)
            .0,
        Some(new)
    );
    assert!(!new_session_input_open(&rt));
    rt.handle_stdin_bytes(b"\r", &Selection::default());
    assert!(
        new_session_input_open(&rt),
        "Enter opens the new session input, as prefix n does"
    );
    assert_eq!(
        selected(&rt),
        Some(crate::model::Node::Host("web".into())),
        "running an action moves no selection"
    );
}

#[test]
fn a_click_on_an_action_link_runs_what_its_key_runs() {
    let (mut rt, new) = host_screen_with_new_session_link();
    let (_, rect) = rt
        .model
        .render_plan
        .view_links
        .iter()
        .find(|(i, _)| *i == new)
        .copied()
        .expect("the action's link is painted");
    let press = crate::display::mouse::MouseEvent {
        cb: 0,
        col: rect.x + 1,
        row: rect.y + 1,
        pressed: true,
    };
    rt.handle_mouse_event(
        &press,
        &Selection::default(),
        &mut false,
        &mut false,
        &mut false,
        &mut false,
    );
    assert!(
        new_session_input_open(&rt),
        "a click runs the action as Enter does"
    );
}

/// `hierarchy_rt` as it stands at launch: the landing screen up in nav focus.
fn landing_rt() -> Runtime {
    let mut rt = hierarchy_rt();
    rt.model.switcher.open_landing();
    sync_test_render_plan(&mut rt);
    rt
}

fn click(rt: &mut Runtime, cb: u16, col: u16, row: u16) -> bool {
    rt.handle_mouse_event(
        &mouse(cb, col + 1, row + 1, true),
        &Selection::default(),
        &mut false,
        &mut false,
        &mut false,
        &mut false,
    )
}

#[test]
fn a_landing_link_takes_hover_and_a_click_from_the_navs_focus() {
    let mut rt = landing_rt();
    let deploy = rt
        .model
        .switcher
        .landing_links(&rt.model.state)
        .iter()
        .position(|l| l.node() == session_node("web", "deploy").as_ref())
        .unwrap();
    let (_, rect) = rt
        .model
        .render_plan
        .view_links
        .iter()
        .find(|(i, _)| *i == deploy)
        .copied()
        .expect("the landing paints the card's link");
    assert!(click(&mut rt, 35, rect.x, rect.y), "hover repaints");
    assert_eq!(rt.model.switcher.hover_targets().1, Some(deploy));
    assert!(
        rt.model.switcher.landing_open(),
        "hovering executes nothing"
    );

    click(&mut rt, 0, rect.x, rect.y);
    assert!(!rt.model.switcher.landing_open());
    assert_eq!(selected(&rt), session_node("web", "deploy"));
    assert_eq!(rt.model.switcher.terminal_view_target().target, "deploy");
    assert!(
        !rt.model.state.focus.view_is_nav(),
        "the click executes: the terminal view takes the focus"
    );
}

#[test]
fn a_click_off_the_landing_links_executes_nothing() {
    let mut rt = landing_rt();
    let area = rt.model.render_plan.regions.terminal;
    click(&mut rt, 0, area.right() - 2, area.bottom() - 2);
    assert!(rt.model.switcher.landing_open());
    assert!(rt.model.state.focus.is_nav_focused());
}

#[test]
fn enter_in_the_nav_executes_the_landing_selection() {
    let mut rt = landing_rt();
    rt.handle_stdin_bytes(b"\x1b[B", &Selection::default());
    assert!(rt.model.switcher.landing_open(), "an arrow only selects");
    assert_eq!(rt.model.switcher.terminal_view_target().target, "");
    rt.handle_stdin_bytes(b"\r", &Selection::default());
    assert!(!rt.model.switcher.landing_open());
    assert_eq!(rt.model.switcher.terminal_view_target().target, "api");
}

#[test]
fn every_key_a_screen_reads_is_in_the_key_table_and_every_screen_entry_is_read() {
    use crate::model::keys::{Keys, TABLE};
    use crate::state::Key;
    use ratatui::crossterm::event::KeyCode;
    let code_of = |key: Key| match key {
        Key::Char(c) => KeyCode::Char(c),
        Key::Enter => KeyCode::Enter,
        Key::Tab => KeyCode::Tab,
        Key::BackTab => KeyCode::BackTab,
        Key::Up => KeyCode::Up,
        Key::Down => KeyCode::Down,
        Key::Backspace => KeyCode::Backspace,
    };
    let listed: Vec<KeyCode> = TABLE
        .iter()
        .filter_map(|e| match e.keys {
            Keys::Screen(codes) => Some(codes),
            _ => None,
        })
        .flatten()
        .copied()
        .collect();
    let mut inputs: Vec<Vec<u8>> = (0x00u8..=0x7e).map(|b| vec![b]).collect();
    inputs.extend([b"\x1b[A", b"\x1b[B", b"\x1b[C", b"\x1b[D", b"\x1b[Z"].map(|s| s.to_vec()));
    let mut read = Vec::new();
    for bytes in &inputs {
        for key in crate::state::decode_keys(bytes) {
            for unreachable in [false, true] {
                if input::screen_msg(key, unreachable).is_some() {
                    assert!(
                        listed.contains(&code_of(key)),
                        "a screen reads {key:?} but the key table does not name it"
                    );
                    read.push(code_of(key));
                }
            }
        }
    }
    for code in &listed {
        assert!(
            read.contains(code),
            "the key table names {code:?} on a screen but no screen reads it"
        );
    }
}

/// Two remote hosts: `deb-1` serving abduco, whose display reattaches on every session
/// change, and `deb-2` serving tmux.
fn abduco_and_tmux_hosts() -> crate::model::Hosts {
    let mut hosts = crate::model::Hosts::default();
    for (alias, mux) in [("deb-1", "abduco"), ("deb-2", "tmux")] {
        hosts.insert(crate::model::Host::new(
            crate::transport::ssh(alias.into(), String::new(), "linux".into()),
            crate::mux::for_binary(mux).unwrap(),
        ));
    }
    hosts
}

fn logged(log: &std::sync::Arc<std::sync::Mutex<Vec<Vec<u8>>>>) -> Vec<u8> {
    log.lock().unwrap().concat()
}

/// Returning from another host to an abduco session keeps the other host's session
/// on screen until the fresh abduco attachment paints, and abduco repaints nothing on
/// attach. Keys typed meanwhile belong to the selected session: they wait while its
/// attachment spawns and reach it once it exists, never the session still on screen
/// and never the stale attachment under the same key.
#[tokio::test(flavor = "current_thread")]
async fn keys_typed_while_returning_to_a_reattaching_host_reach_the_selected_session() {
    let mut rt = test_rt(fake_env_with_machines(&[]));
    rt.hosts = abduco_and_tmux_hosts();
    let selected = Selection {
        host: "deb-1".into(),
        session: "abduco2".into(),
    };
    let shown = Selection {
        host: "deb-2".into(),
        session: "tmux1".into(),
    };
    let abduco_key = display_key(&rt.hosts, &selected);
    let tmux_key = display_key(&rt.hosts, &shown);
    let (stale, stale_log) = crate::display::attachment::fake_attachment_with_input_log(1);
    let (tmux, tmux_log) = crate::display::attachment::fake_attachment_with_input_log(2);
    rt.registry.insert(&abduco_key, stale);
    rt.registry.insert(&tmux_key, tmux);
    rt.model.state.displayed = shown;
    rt.model.state.selection = selected.clone();
    {
        let display = &mut rt.hosts.get_mut("deb-1").unwrap().display;
        display.set_shows(&abduco_key, "abduco2");
        display.mark_in_flight(&abduco_key, 1);
        display.mark_pending(3, &abduco_key);
    }

    rt.forward_input(b"whereami".to_vec());
    rt.flush_held_input();
    assert!(
        logged(&tmux_log).is_empty(),
        "the session on screen is not selected"
    );
    assert!(
        logged(&stale_log).is_empty(),
        "the stale attachment shows abduco1"
    );

    let (fresh, fresh_log) = crate::display::attachment::fake_attachment_with_input_log(3);
    rt.on_display_event(DisplayEvent::Ready {
        seq: 1,
        key: abduco_key.clone(),
        attachment: fresh,
    });
    rt.flush_held_input();
    rt.forward_input(b"\r".to_vec());
    assert_eq!(logged(&fresh_log), b"whereami\r");
    assert!(logged(&tmux_log).is_empty());
    assert!(logged(&stale_log).is_empty());
}

/// herdr drops every key it reads before it draws its first frame. Keys typed into a
/// fresh herdr attachment that has drawn nothing wait, and reach it in the order typed
/// once it has drawn.
#[tokio::test(flavor = "current_thread")]
async fn keys_typed_before_a_herdr_attachment_draws_reach_it_after_its_first_frame() {
    let mut rt = test_rt(fake_env_with_machines(&[]));
    let mut hosts = crate::model::Hosts::default();
    hosts.insert(crate::model::Host::new(
        crate::transport::ssh("deb-1".into(), String::new(), "linux".into()),
        crate::mux::for_binary("herdr").unwrap(),
    ));
    rt.hosts = hosts;
    let selected = Selection {
        host: "deb-1".into(),
        session: "herdr1".into(),
    };
    let key = display_key(&rt.hosts, &selected);
    let (fresh, log) = crate::display::attachment::fake_attachment_with_input_log(1);
    rt.registry.insert(&key, fresh);
    rt.model.state.displayed = selected.clone();
    rt.model.state.selection = selected;

    rt.forward_input(b"whereami".to_vec());
    rt.flush_held_input();
    assert!(logged(&log).is_empty(), "herdr has drawn nothing yet");

    rt.registry
        .get(&key)
        .unwrap()
        .mark_painted_for_test(std::time::Instant::now());
    rt.flush_held_input();
    rt.forward_input(b"\r".to_vec());
    assert_eq!(logged(&log), b"whereami\r");
}

/// Input held for a selection is dropped when the selection moves on before its
/// attachment exists, so it never reaches a session it was not typed for.
#[tokio::test(flavor = "current_thread")]
async fn input_held_for_a_selection_left_behind_is_dropped() {
    let mut rt = test_rt(fake_env_with_machines(&[]));
    rt.hosts = abduco_and_tmux_hosts();
    let shown = Selection {
        host: "deb-2".into(),
        session: "tmux1".into(),
    };
    let tmux_key = display_key(&rt.hosts, &shown);
    let (tmux, tmux_log) = crate::display::attachment::fake_attachment_with_input_log(2);
    rt.registry.insert(&tmux_key, tmux);
    rt.model.state.displayed = shown.clone();
    rt.model.state.selection = Selection {
        host: "deb-1".into(),
        session: "abduco2".into(),
    };
    rt.model.state.attach_pending = true;

    rt.forward_input(b"lost".to_vec());
    rt.model.state.selection = shown;
    rt.model.state.attach_pending = false;
    rt.forward_input(b"kept".to_vec());
    assert_eq!(logged(&tmux_log), b"kept");
}

const LONG_MACHINE: &str = "build-runner-07.internal.example.net";

/// A runtime of one machine `machine` sized `cols` by `rows` with the terminal view
/// focused: blocked on a refused login when `session` is empty, otherwise serving that
/// session on its mux, with the selection on the host.
fn headline_rt(machine: &str, session: &str, cols: u16, rows: u16) -> Runtime {
    let mut rt = test_rt(fake_env_with_machines(&[]));
    let host = if session.is_empty() {
        machine.to_string()
    } else {
        format!("{machine}:tmux")
    };
    let mut state = crate::state::State::from_hosts(vec![host.clone()]);
    let mut switcher = crate::ui::switcher::Switcher::from_hosts(&mut state);
    if session.is_empty() {
        switcher.apply_host_result(
            host,
            Vec::new(),
            Some(format!(
                "alice@{machine}: Permission denied (publickey,password)."
            )),
            &mut state,
        );
    } else {
        switcher.apply_host_result(
            host.clone(),
            vec![crate::session::Session {
                host: host.clone(),
                name: session.into(),
                mux: "tmux".into(),
                id: String::new(),
                windows: 1,
                clients: 0,
                stopped: false,
            }],
            None,
            &mut state,
        );
        switcher.handle_key(
            ratatui::crossterm::event::KeyEvent::new(
                ratatui::crossterm::event::KeyCode::Up,
                ratatui::crossterm::event::KeyModifiers::CONTROL,
            ),
            &mut state,
        );
    }
    state
        .focus
        .set_view_focus(crate::app::focus::ViewFocus::Terminal);
    rt.model.state = state;
    rt.model.switcher = switcher;
    rt.cols = cols;
    rt.body_rows = rows - 1;
    rt.last_draw = std::time::Instant::now() - std::time::Duration::from_secs(1);
    rt
}

/// The terminal view's rows of `rt` as drawn on `term`, each cut to the view's columns.
fn view_rows(rt: &Runtime, term: &ratatui::Terminal<ratatui::backend::TestBackend>) -> Vec<String> {
    let view = rt.model.render_plan.regions.terminal;
    drawn_text(term)
        .lines()
        .map(|l| {
            l.chars()
                .skip(view.x as usize)
                .take(view.width as usize)
                .collect()
        })
        .collect()
}

#[test]
fn a_headline_wider_than_the_view_continues_under_its_path() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    for (session, level) in [("", "machine "), ("train", "host ")] {
        for (cols, rows) in [(80u16, 24u16), (40, 14)] {
            let mut rt = headline_rt(LONG_MACHINE, session, cols, rows);
            let mut term = Terminal::new(TestBackend::new(cols, rows)).unwrap();
            rt.prepare_and_draw(&mut term);
            let view = view_rows(&rt, &term);
            let out = view.join("\n");
            let path_col = 1 + level.len();
            // The headline's rows: the first after the level word, the rest indented to
            // the path's first column.
            let mut headline = view[1][path_col..].trim_end().to_string();
            for row in &view[2..] {
                if !row.starts_with(&" ".repeat(path_col)) || row.trim().is_empty() {
                    break;
                }
                headline.push_str(row.trim());
            }
            let want = if session.is_empty() {
                LONG_MACHINE.to_string()
            } else {
                format!("{LONG_MACHINE}/tmux")
            };
            assert_eq!(headline, want, "{cols}x{rows}:\n{out}");
            assert!(view[1].starts_with(&format!(" {level}")), "{out}");
            if !session.is_empty() {
                // The machine half is the link up on every row it covers.
                let up = rt
                    .model
                    .switcher
                    .shown_links(&rt.model.state)
                    .iter()
                    .position(|l| matches!(l.node(), Some(crate::model::Node::Machine(_))))
                    .unwrap_or_else(|| panic!("{out}"));
                let link_rows: Vec<u16> = rt
                    .model
                    .render_plan
                    .view_links
                    .iter()
                    .filter(|(link, _)| *link == up)
                    .map(|(_, rect)| rect.y)
                    .collect();
                assert_eq!(link_rows.len(), 2, "{cols}x{rows}: {link_rows:?}\n{out}");
            }
        }
    }
}

#[test]
fn a_wrapped_screen_value_keeps_every_character_in_the_view() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let session = "a-long-training-session-name";
    for (cols, rows) in [(80u16, 24u16), (40, 30)] {
        let mut rt = headline_rt("gpu", session, cols, rows);
        let mut term = Terminal::new(TestBackend::new(cols, rows)).unwrap();
        rt.prepare_and_draw(&mut term);
        let view = view_rows(&rt, &term);
        let out = view.join("\n");
        let at = view
            .iter()
            .rposition(|l| l.trim_start().starts_with("sessions") && !l.contains("sessions     1"))
            .unwrap_or_else(|| panic!("{out}"));
        let value_col = view[at].find("a-long").unwrap_or_else(|| panic!("{out}"));
        let mut value = String::new();
        for row in &view[at..] {
            if row.trim().is_empty() {
                break;
            }
            value.push_str(row[value_col..].trim_end());
            value.push(' ');
        }
        assert!(
            value
                .replace(' ', "")
                .starts_with(&format!("{session}1window")),
            "{cols}x{rows}: {value}\n{out}"
        );
    }
}

/// What one palette execution left behind, as far as the user can see it: whether the
/// read quits xmux, which view holds the focus, which popup is open, and the nav layout.
fn palette_outcome(rt: &Runtime, out: &StdinOutcome) -> String {
    use crate::state::{InputMode, Modal};
    let modal = match &rt.model.state.modal {
        None => "none",
        Some(Modal::Help { .. }) => "help",
        Some(Modal::History { .. }) => "history",
        Some(Modal::Check { .. }) => "check",
        Some(Modal::Palette { .. }) => "palette",
        Some(Modal::Input(input)) => match input.mode {
            InputMode::Filter => "filter",
            InputMode::New => "new",
            InputMode::Logout => "logout",
            InputMode::LogoutKeys => "logout keys",
            InputMode::Jump => "jump",
        },
    };
    format!(
        "quit={} focus={:?} modal={modal} collapsed={} auto_hide={} position={:?}",
        out.quit,
        rt.model.state.focus,
        rt.model.nav_collapsed,
        rt.model.auto_hide_nav,
        rt.model.nav_position,
    )
}

/// A runtime with the palette open in the given view's focus, laid out as a frame
/// paints it so a click is hit-tested against the drawn popup.
fn palette_rt(focus: crate::model::FocusTarget) -> Runtime {
    let mut rt = test_rt(fake_env_with_machines(&["local"]));
    let _ = update(&mut rt.model, Msg::Focus(focus));
    let _ = update(&mut rt.model, Msg::TogglePalette);
    sync_test_render_plan(&mut rt);
    rt
}

#[tokio::test]
async fn a_click_on_every_palette_entry_does_what_enter_on_it_does() {
    use crate::model::FocusTarget;
    for focus in [FocusTarget::Nav, FocusTarget::Terminal] {
        let probe = palette_rt(focus);
        let entries = probe.model.switcher.palette_entries(&probe.model.state, "");
        assert!(entries.len() > 1, "the palette lists its commands");
        for (index, (name, _)) in entries.iter().enumerate() {
            let mut keyed = palette_rt(focus);
            let mut keys = b"\x1b[B".repeat(index);
            keys.push(b'\r');
            let enter = keyed.handle_stdin_bytes(&keys, &Selection::default());
            let enter = palette_outcome(&keyed, &enter);

            let mut clicked = palette_rt(focus);
            // The cell the click lands on is found by the same hit test the hover uses.
            let popup = clicked.model.render_plan.popup_rect;
            let cell = (popup.y..popup.bottom())
                .flat_map(|row| (popup.x..popup.right()).map(move |col| (col, row)))
                .find(|&(col, row)| {
                    let _ = update(&mut clicked.model, Msg::HoverPopup { col, row });
                    matches!(
                        clicked.model.state.modal,
                        Some(crate::state::Modal::Palette { hover: Some(h), .. }) if h == index
                    )
                })
                .unwrap_or_else(|| panic!("{name} is drawn in the palette"));
            let (col, row) = (cell.0 + 1, cell.1 + 1);
            let press = clicked.handle_stdin_bytes(
                format!("\x1b[<0;{col};{row}M").as_bytes(),
                &Selection::default(),
            );
            assert!(!press.quit, "{name}: a press alone runs nothing");
            let release = clicked.handle_stdin_bytes(
                format!("\x1b[<0;{col};{row}m").as_bytes(),
                &Selection::default(),
            );
            let click = palette_outcome(&clicked, &release);
            assert_eq!(click, enter, "{name} from {focus:?} focus");
        }
    }
}

/// A session in the terminal view with focus there, and the input its attachment reads.
fn rt_terminal_focus_with_attachment() -> (Runtime, std::sync::Arc<std::sync::Mutex<Vec<Vec<u8>>>>)
{
    let mut rt = rt_terminal_focus_with_session();
    let selection = rt.model.state.selection.clone();
    rt.model.state.displayed = selection.clone();
    let (att, log) = crate::display::attachment::fake_attachment_with_input_log(7);
    rt.registry.insert(&display_key(&rt.hosts, &selection), att);
    rt.on_stdin(b"x");
    assert_eq!(
        logged(&log),
        b"x",
        "precondition: typed keys reach the session"
    );
    log.lock().unwrap().clear();
    (rt, log)
}

#[test]
fn a_paste_reaches_a_session_that_did_not_ask_for_bracketed_paste_as_plain_text() {
    // The outer terminal wraps a paste because xmux asked it to; the session's client did
    // not ask, so it reads the pasted text alone, and a prefix byte inside it is text.
    let (mut rt, log) = rt_terminal_focus_with_attachment();
    rt.on_stdin(b"\x1b[200~echo a\recho \x07b\r\x1b[201~");
    assert_eq!(logged(&log), b"echo a\recho \x07b\r");
    assert!(!rt.prefix_active(), "a pasted prefix byte arms nothing");
}

#[test]
fn a_paste_reaches_a_session_that_asked_for_bracketed_paste_wrapped() {
    let (mut rt, log) = rt_terminal_focus_with_attachment();
    let key = display_key(&rt.hosts, &rt.model.state.selection);
    rt.registry
        .grid(&key)
        .unwrap()
        .lock()
        .unwrap()
        .feed(b"\x1b[?2004h$ ");
    // Split across reads, the paste still arrives once, between the markers.
    rt.on_stdin(b"\x1b[200~ls\r");
    assert!(logged(&log).is_empty(), "held until the paste ends");
    rt.on_stdin(b"pwd\r\x1b[201~q");
    assert_eq!(logged(&log), b"\x1b[200~ls\rpwd\r\x1b[201~q");
}

#[test]
fn a_paste_after_the_prefix_is_text_for_the_session_not_a_command() {
    let (mut rt, log) = rt_terminal_focus_with_attachment();
    rt.on_stdin(b"\x07");
    assert!(rt.prefix_active(), "the prefix waits for its key");
    let out = rt.on_stdin(b"\x1b[200~q\x1b[201~");
    assert!(!out, "a pasted q does not quit");
    assert!(!rt.prefix_active(), "the paste ends the waiting prefix");
    assert_eq!(logged(&log), b"q");
}

#[test]
fn a_paste_over_the_nav_moves_nothing_and_a_filter_takes_its_text() {
    use crate::state::{InputMode, Modal};
    let (mut rt, log) = rt_terminal_focus_with_attachment();
    rt.model
        .state
        .apply(crate::model::Action::Focus(crate::model::FocusTarget::Nav));
    let before = rt.model.state.selection.clone();
    rt.on_stdin(b"\x1b[200~kkjj\x07q\r\x1b[201~");
    assert_eq!(
        rt.model.state.selection, before,
        "no key in the paste moved it"
    );
    assert!(
        rt.model.state.focus.is_nav_focused(),
        "Enter in the paste is text"
    );
    assert!(rt.model.state.modal.is_none());
    rt.on_stdin(b"\x07/");
    rt.on_stdin(b"\x1b[200~ap\ri\x1b[201~");
    match &rt.model.state.modal {
        Some(Modal::Input(input)) => {
            assert!(matches!(input.mode, InputMode::Filter));
            assert_eq!(
                input.buffer, "api",
                "the line break is left out of the field"
            );
        }
        _ => panic!("the filter stays open"),
    }
    assert!(logged(&log).is_empty(), "nothing reached the session");
}

#[test]
fn a_focus_report_from_the_terminal_reaches_no_session_that_did_not_ask_for_one() {
    // xmux's terminal reports its own focus; a session that did not enable `?1004`
    // reads nothing of it, and a prefix waiting for its key still waits.
    let (mut rt, log) = rt_terminal_focus_with_attachment();
    rt.on_stdin(b"\x1b[O\x1b[I");
    assert!(logged(&log).is_empty());
    rt.on_stdin(b"\x07");
    rt.on_stdin(b"\x1b[O");
    assert!(rt.prefix_active(), "a focus report is not the prefix's key");
}

#[test]
fn a_session_that_asked_for_focus_events_hears_every_focus_move() {
    let (mut rt, log) = rt_terminal_focus_with_attachment();
    let key = display_key(&rt.hosts, &rt.model.state.selection);
    rt.registry
        .grid(&key)
        .unwrap()
        .lock()
        .unwrap()
        .feed(b"\x1b[?1004h");
    rt.sync_child_focus();
    assert_eq!(
        logged(&log),
        b"\x1b[I",
        "it holds the focus once it can hear it"
    );
    let heard = |rt: &mut Runtime, keys: &[u8]| {
        log.lock().unwrap().clear();
        rt.on_stdin(keys);
        rt.sync_child_focus();
        logged(&log)
    };
    assert_eq!(
        heard(&mut rt, b"\x07\x1b[D"),
        b"\x1b[O",
        "the nav takes the focus"
    );
    assert_eq!(
        heard(&mut rt, b"\r"),
        b"\x1b[I",
        "the terminal view takes it back"
    );
    assert_eq!(
        heard(&mut rt, b"\x1b[O"),
        b"\x1b[O",
        "xmux's window loses it"
    );
    assert_eq!(heard(&mut rt, b"\x1b[I"), b"\x1b[I", "and gets it back");
    assert_eq!(heard(&mut rt, b"\x07?"), b"\x1b[O", "a popup takes it");
    assert_eq!(
        heard(&mut rt, b"\x1b"),
        b"\x1b[I",
        "closing the popup returns it"
    );
    assert_eq!(heard(&mut rt, b"ab"), b"ab", "typing is no focus move");
}

#[test]
fn switching_the_terminal_view_to_another_session_moves_the_focus_between_them() {
    let (mut rt, log) = rt_terminal_focus_with_attachment();
    let key = display_key(&rt.hosts, &rt.model.state.selection);
    rt.registry
        .grid(&key)
        .unwrap()
        .lock()
        .unwrap()
        .feed(b"\x1b[?1004h");
    rt.sync_child_focus();
    log.lock().unwrap().clear();
    let other = Selection {
        host: "jup".into(),
        session: "web".into(),
    };
    let (att, other_log) = crate::display::attachment::fake_attachment_with_input_log(8);
    att.grid.lock().unwrap().feed(b"\x1b[?1004h");
    rt.registry.insert(&display_key(&rt.hosts, &other), att);
    rt.model.state.displayed = other;
    rt.sync_child_focus();
    assert_eq!(logged(&log), b"\x1b[O");
    assert_eq!(logged(&other_log), b"\x1b[I");
}

#[tokio::test(flavor = "current_thread")]
async fn alerts_reach_the_terminal_and_mark_a_session_not_on_screen() {
    use crate::display::grid::Alert;
    let mut rt = test_rt(fake_env_with_machines(&[]));
    rt.hosts = detach_test_hosts("jup");
    rt.registry.insert_fake("jup", 7);
    rt.hosts
        .get_mut("jup")
        .unwrap()
        .display
        .set_shows("jup", "work");
    let (_tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let records = |rt: &Runtime| rt.model.state.notify.history.len();
    let before = records(&rt);

    // Nothing is on screen: the bell reaches the terminal and marks the card.
    rt.on_pty_event(
        PtyEvent::Alert {
            id: 7,
            alert: Alert::Bell,
        },
        &mut rx,
    );
    rt.on_pty_event(
        PtyEvent::Alert {
            id: 7,
            alert: Alert::Bell,
        },
        &mut rx,
    );
    assert_eq!(
        rt.passthrough, b"\x07\x07",
        "every bell reaches the terminal"
    );
    assert!(rt.model.switcher.alerted("jup", "work"));
    assert_eq!(
        records(&rt),
        before + 1,
        "a run of bells is one history record"
    );
    let notify = Alert::Notify {
        text: "needs input".into(),
        seq: b"\x1b]9;needs input\x07".to_vec(),
    };
    rt.on_pty_event(
        PtyEvent::Alert {
            id: 7,
            alert: notify.clone(),
        },
        &mut rx,
    );
    assert!(rt.passthrough.ends_with(b"\x1b]9;needs input\x07"));
    let last = rt.model.state.notify.history.back().unwrap();
    assert!(
        format!("{last:?}").contains("notification: needs input"),
        "the history keeps the notification's words: {last:?}"
    );

    // Showing the session takes the mark off.
    rt.model.state.displayed = Selection {
        host: "jup".into(),
        session: "work".into(),
    };
    let _ = update(
        &mut rt.model,
        Msg::SyncFrame {
            spinner_frame: 0,
            animation_ms: 0,
            nav_border_hovered: false,
            prefix_active: false,
        },
    );
    assert!(!rt.model.switcher.alerted("jup", "work"));

    // The session on screen rings the terminal and leaves no mark or record.
    rt.passthrough.clear();
    let before = records(&rt);
    rt.on_pty_event(
        PtyEvent::Alert {
            id: 7,
            alert: notify,
        },
        &mut rx,
    );
    assert_eq!(rt.passthrough, b"\x1b]9;needs input\x07");
    assert!(!rt.model.switcher.alerted("jup", "work"));
    assert_eq!(records(&rt), before);

    // An attachment that is no longer live still reaches the terminal, and marks nothing.
    rt.passthrough.clear();
    rt.on_pty_event(
        PtyEvent::Alert {
            id: 99,
            alert: Alert::Bell,
        },
        &mut rx,
    );
    assert_eq!(rt.passthrough, b"\x07");
}

#[tokio::test(flavor = "current_thread")]
async fn the_terminal_title_follows_the_session_on_screen() {
    let mut rt = test_rt(fake_env_with_machines(&[]));
    rt.hosts = detach_test_hosts("jup");
    rt.registry.insert_fake("jup", 7);
    let feed = |rt: &Runtime, bytes: &[u8]| {
        rt.registry.grid("jup").unwrap().lock().unwrap().feed(bytes);
    };
    let synced = |rt: &mut Runtime| {
        rt.passthrough.clear();
        rt.sync_title();
        String::from_utf8(rt.passthrough.clone()).unwrap()
    };

    feed(&rt, b"\x1b]2;vim notes.md\x07");
    assert_eq!(
        synced(&mut rt),
        "",
        "no session on screen, no title written"
    );
    rt.model.state.displayed = Selection {
        host: "jup".into(),
        session: "work".into(),
    };
    assert_eq!(synced(&mut rt), "\x1b]2;vim notes.md\x07");
    assert_eq!(synced(&mut rt), "", "an unchanged title is written once");
    feed(&rt, b"\x1b]0;htop\x07");
    assert_eq!(synced(&mut rt), "\x1b]2;htop\x07");
    rt.model.state.displayed = Selection::default();
    assert_eq!(
        synced(&mut rt),
        "\x1b]2;xmux\x07",
        "a title no longer on screen gives way to xmux's own"
    );
    assert_eq!(synced(&mut rt), "");
}

/// The display attachments xmux holds are counted on the session each one is on while
/// the registry holds it, and stop counting once their client ends.
#[tokio::test(flavor = "current_thread")]
async fn live_display_attachments_are_counted_as_xmuxs_own_clients() {
    let mut rt = test_rt(fake_env_with_machines(&[]));
    rt.hosts = detach_test_hosts("jup");
    rt.hosts
        .get_mut("jup")
        .unwrap()
        .display
        .set_shows("jup", "api");
    assert!(
        !rt.sync_display_clients(),
        "a record with no live attachment counts nothing"
    );
    rt.registry.insert_fake("jup", 7);
    assert!(rt.sync_display_clients());
    let api = crate::session::Address::new("jup", "api");
    assert_eq!(rt.model.state.display_clients.get(&api), Some(&1));
    assert!(!rt.sync_display_clients(), "unchanged, so nothing to tell");
    rt.registry.reap(7);
    assert!(rt.sync_display_clients());
    assert!(rt.model.state.display_clients.is_empty());
}

#[test]
fn the_prefix_and_its_keys_work_in_the_kitty_keyboard_encoding() {
    // Keys as a terminal sends them once the session asked for the protocol: the
    // prefix and the key after it act, and their releases go nowhere.
    let (mut rt, log) = rt_terminal_focus_with_attachment();
    rt.on_stdin(b"\x1b[103;5u");
    assert!(rt.prefix_active(), "Ctrl+g arms the prefix");
    rt.on_stdin(b"\x1b[103;5:3u");
    assert!(rt.prefix_active(), "its release is not the key after it");
    rt.on_stdin(b"\x1b[47:63;2u\x1b[47:63;2:3u");
    assert!(
        rt.model.state.is_modal_popup_open(),
        "Shift+/ is `?`, the help"
    );
    assert!(
        logged(&log).is_empty(),
        "nothing xmux read reached the session"
    );
    rt.on_stdin(b"\x1b");
    rt.on_stdin(b"\x1b[13;2u\x1b[13;2:3u\x1b[97;5:3u");
    assert_eq!(
        logged(&log),
        b"\x1b[13;2u\x1b[13;2:3u\x1b[97;5:3u",
        "Shift+Enter and the releases xmux did not read reach the session unchanged"
    );
    log.lock().unwrap().clear();
    rt.on_stdin(b"\x1b[103;5u\x1b[103;5:3u\x1b[103;5u");
    assert_eq!(
        logged(&log),
        b"\x1b[103;5u",
        "a doubled prefix sends the prefix as it arrived"
    );
}

#[test]
fn xmux_sets_on_its_terminal_the_keyboard_flags_of_the_session_its_keys_reach() {
    let (mut rt, _log) = rt_terminal_focus_with_attachment();
    let key = display_key(&rt.hosts, &rt.model.state.selection);
    rt.registry
        .grid(&key)
        .unwrap()
        .lock()
        .unwrap()
        .feed(b"\x1b[>1u");
    assert!(
        rt.keyboard_update().is_empty(),
        "the terminal has not answered"
    );
    assert!(crate::display::keyboard::record_support(true));
    assert_eq!(rt.keyboard_update(), b"\x1b[>0u\x1b[=1;1u");
    assert!(rt.keyboard_update().is_empty(), "already in force");
    rt.on_stdin(b"\x07\x1b[D");
    assert_eq!(
        rt.keyboard_update(),
        b"\x1b[=0;1u",
        "the nav reads legacy keys"
    );
    rt.on_stdin(b"\r");
    assert_eq!(rt.keyboard_update(), b"\x1b[=1;1u");
    rt.registry
        .grid(&key)
        .unwrap()
        .lock()
        .unwrap()
        .feed(b"\x1b[<u");
    assert_eq!(
        rt.keyboard_update(),
        b"\x1b[=0;1u",
        "the session popped its flags"
    );
}

#[test]
fn a_terminal_without_the_keyboard_protocol_is_left_as_it_is() {
    let (mut rt, log) = rt_terminal_focus_with_attachment();
    let key = display_key(&rt.hosts, &rt.model.state.selection);
    let grid = rt.registry.grid(&key).unwrap();
    crate::display::keyboard::record_support(false);
    grid.lock().unwrap().feed(b"\x1b[>1u\x1b[?u");
    assert!(rt.keyboard_update().is_empty());
    assert!(
        grid.lock().unwrap().take_replies().is_empty(),
        "a session asking is told nothing, so it keeps legacy keys"
    );
    rt.on_stdin(b"a");
    assert_eq!(logged(&log), b"a");
}

#[test]
fn bare_ctrl_arrows_resize_only_in_the_visible_resize_mode() {
    let (mut rt, log) = rt_terminal_focus_with_attachment();
    let width = rt.model.nav_width_natural;
    rt.on_stdin(b"\x07\x1b[1;5C");
    assert!(
        rt.model.mouse_state.resizing,
        "a prefix resize starts the mode"
    );
    assert_eq!(rt.model.nav_width_natural, width + 1);
    // In the mode, a bare Ctrl+arrow and its release move the border, not the session.
    rt.on_stdin(b"\x1b[1;5D\x1b[1;5:3D");
    assert_eq!(rt.model.nav_width_natural, width);
    assert_eq!(logged(&log), b"");
    // Any other key ends the mode and reaches the session as typed.
    rt.on_stdin(b"x");
    assert!(!rt.model.mouse_state.resizing);
    assert!(!rt.prefix_active(), "the key list closes with the mode");
    rt.on_stdin(b"\x1b[1;5D");
    assert_eq!(
        logged(&log),
        b"x\x1b[1;5D",
        "outside the mode it is the session's"
    );
    assert_eq!(rt.model.nav_width_natural, width);
}

/// A runtime with the terminal view focused over a session whose client enabled
/// `modes`, laid out as a frame paints it, and the input that session reads.
fn rt_mouse_over_session(
    modes: &[u8],
) -> (
    Runtime,
    Selection,
    std::sync::Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
) {
    use crate::ui::switcher::{Scan, Switcher};
    let mut state = crate::state::State::from_scan(Scan { groups: vec![] });
    let switcher = Switcher::new(&mut state);
    let sel = Selection {
        host: "local".into(),
        session: "work".into(),
    };
    let (att, log) = crate::display::attachment::fake_attachment_with_input_log(42);
    att.grid.lock().unwrap().feed(modes);
    let mut rt = test_rt(fake_env_with_machines(&["local"]));
    rt.model.state = state;
    rt.model.switcher = switcher;
    rt.model
        .state
        .focus
        .set_view_focus(crate::app::focus::ViewFocus::Terminal);
    sync_test_render_plan(&mut rt);
    rt.registry.insert(&display_key(&rt.hosts, &sel), att);
    (rt, sel, log)
}

fn send_mouse(rt: &mut Runtime, sel: &Selection, cb: u16, col: u16, row: u16, pressed: bool) {
    let (mut ft, mut wheel) = (false, false);
    let ev = mouse(cb, col, row, pressed);
    rt.handle_mouse_event(&ev, sel, &mut ft, &mut wheel, &mut false, &mut false);
}

#[test]
fn a_drag_that_leaves_the_terminal_view_still_reaches_the_session_and_ends_there() {
    // A selection drag starts in the view and crosses into the nav: the session keeps
    // getting the motion at the view's edge and always gets the release.
    let (mut rt, sel, log) = rt_mouse_over_session(b"\x1b[?1002h\x1b[?1006h");
    let x0 = rt.model.render_plan.regions.terminal.x;
    send_mouse(&mut rt, &sel, 0, x0 + 6, 5, true);
    send_mouse(&mut rt, &sel, 32, x0 - 3, 6, true);
    send_mouse(&mut rt, &sel, 0, x0 - 3, 6, false);
    assert_eq!(
        logged(&log),
        b"\x1b[<0;6;5M\x1b[<32;1;6M\x1b[<0;1;6m",
        "press, then motion and release at the view's left edge"
    );
    assert!(!rt.model.mouse_state.view_drag, "the release ends the drag");
}

#[test]
fn a_session_gets_only_the_mouse_reports_its_client_asked_for() {
    let x0 = |rt: &Runtime| rt.model.render_plan.regions.terminal.x;
    // No mouse mode: nothing, not even a click.
    let (mut rt, sel, log) = rt_mouse_over_session(b"");
    let x = x0(&rt);
    send_mouse(&mut rt, &sel, 0, x + 2, 2, true);
    send_mouse(&mut rt, &sel, 0, x + 2, 2, false);
    assert!(logged(&log).is_empty());
    // `?1000` in the legacy form: the click as the client reads it, and no hover.
    let (mut rt, sel, log) = rt_mouse_over_session(b"\x1b[?1000h");
    send_mouse(&mut rt, &sel, 35, x + 2, 2, true);
    send_mouse(&mut rt, &sel, 0, x + 2, 2, true);
    send_mouse(&mut rt, &sel, 0, x + 2, 2, false);
    assert_eq!(logged(&log), b"\x1b[M\x20\x22\x22\x1b[M\x23\x22\x22");
    // `?1003` with SGR: hover too.
    let (mut rt, sel, log) = rt_mouse_over_session(b"\x1b[?1003h\x1b[?1006h");
    send_mouse(&mut rt, &sel, 35, x + 2, 2, true);
    assert_eq!(logged(&log), b"\x1b[<35;2;2M");
}

#[test]
fn a_frame_reaches_the_terminal_inside_one_synchronized_update() {
    use ratatui::backend::CrosstermBackend;
    use ratatui::layout::Rect;
    use ratatui::{Terminal, TerminalOptions, Viewport};
    /// Records what the backend writes, readable after the terminal took the writer.
    #[derive(Clone, Default)]
    struct Sink(std::rc::Rc<std::cell::RefCell<Vec<u8>>>);
    impl std::io::Write for Sink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let sink = Sink::default();
    let mut rt = login_pane_rt(40, 12);
    let mut term = Terminal::with_options(
        CrosstermBackend::new(sink.clone()),
        TerminalOptions {
            viewport: Viewport::Fixed(Rect::new(0, 0, 40, 12)),
        },
    )
    .unwrap();
    rt.prepare_and_draw(&mut term);
    let out = String::from_utf8_lossy(&sink.0.borrow()).into_owned();
    // The cursor lands at its final cell before the update ends, so the terminal never
    // shows it on a cell the frame only passed through.
    let cursor = out
        .rfind("\x1b[?25h")
        .or_else(|| out.rfind("\x1b[?25l"))
        .expect("the frame sets the cursor");
    assert!(out.starts_with("\x1b[?2026h"), "{out:?}");
    assert!(out.ends_with("\x1b[?2026l"), "{out:?}");
    assert!(cursor < out.len() - "\x1b[?2026l".len(), "{out:?}");
    assert_eq!(out.matches("\x1b[?2026h").count(), 1, "{out:?}");
}

#[tokio::test(flavor = "current_thread")]
async fn a_held_synchronized_update_keeps_the_frame_cadence_drawing() {
    // The session began an update and went quiet: the frame timer keeps redrawing so
    // its live screen appears when the hold runs out, and stops once nothing is held.
    let mut rt = a_settled_herdr_runtime();
    let grid = rt.registry.grid("local").expect("the displayed grid");
    grid.lock().unwrap().feed(b"\x1b[?2026h");
    nav_text(&mut rt);
    assert!(rt.display_sync_held);
    grid.lock().unwrap().feed(b"\x1b[?2026l");
    rt.dirty = true;
    nav_text(&mut rt);
    assert!(!rt.display_sync_held);
}

#[test]
fn unchanged_inventory_keeps_a_stationary_link_hover() {
    let (mut rt, _) = host_screen_with_new_session_link();
    let rect = rt.model.render_plan.view_links[0].1;
    update(
        &mut rt.model,
        Msg::Hover {
            col: rect.x,
            row: rect.y,
        },
    );
    let before = rt.model.switcher.hover_targets();
    let sessions = rt
        .model
        .state
        .groups
        .iter()
        .find(|g| g.host == "web")
        .unwrap()
        .sessions
        .clone();
    update(
        &mut rt.model,
        Msg::ApplyInventory {
            host: "web".into(),
            sessions,
            live: true,
        },
    );
    assert_eq!(rt.model.switcher.hover_targets(), before);
}

#[test]
fn resized_frame_clears_stale_hover_before_painting() {
    let (mut rt, _) = host_screen_with_new_session_link();
    let rect = rt.model.render_plan.view_links[0].1;
    update(
        &mut rt.model,
        Msg::Hover {
            col: rect.x,
            row: rect.y,
        },
    );
    let previous = rt.model.render_plan.clone();
    let nav = rt.model.nav_size();
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(70, 20)).unwrap();
    terminal
        .draw(|frame| {
            let plan = rt.model.prepare_render_plan(frame.area(), nav, &previous);
            assert_eq!(rt.model.switcher.hover_targets(), (None, None));
            rt.model
                .switcher
                .render(frame, None, true, &rt.model.state, &plan);
        })
        .unwrap();
    assert_eq!(rt.model.switcher.hover_targets(), (None, None));
}

struct SavingLoginOps {
    config: std::path::PathBuf,
}

#[async_trait::async_trait]
impl crate::model::Ops for SavingLoginOps {
    fn hosts(&self) -> Vec<String> {
        vec![]
    }
    async fn list_sessions(&self, _: &str) -> anyhow::Result<Vec<crate::session::Session>> {
        Ok(vec![])
    }
    async fn new_session(&self, _: &str, _: &str) -> anyhow::Result<crate::session::Session> {
        unreachable!()
    }
    async fn login_command(
        &self,
        host: &str,
        _: &crate::transport::Login,
        _: String,
    ) -> anyhow::Result<Option<crate::transport::CommandSpec>> {
        Ok(Some(crate::transport::Transport::exec_argv(
            &TestRemote(host.into()),
            false,
            &[],
        )))
    }
    fn write_login_stanza(
        &self,
        host: &str,
        login: &crate::transport::Login,
    ) -> Result<(), String> {
        let text = std::fs::read_to_string(&self.config).unwrap_or_default();
        std::fs::write(
            &self.config,
            crate::provision::config::upsert_managed_stanza(&text, host, login),
        )
        .map_err(|e| e.to_string())
    }
    async fn register_login_key(
        &self,
        _: &str,
        _: &crate::transport::Login,
        _: crate::model::KeyRegistration,
    ) -> crate::model::RegistrationOutcome {
        crate::model::RegistrationOutcome::Registered
    }
}

#[tokio::test]
async fn after_login_choices_persist_connection_for_restart() {
    use crate::model::{AfterLogin, RegistrationOutcome};
    for (choice, saves, registers) in [
        (AfterLogin::Nothing, false, false),
        (AfterLogin::SshConfig, true, false),
        (AfterLogin::RegisterKey, true, true),
    ] {
        let env = fake_env_with_machines(&["prod"]);
        let config = env.xmux_dir.join("ssh_config");
        let mut rt = test_rt(env);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        rt.op_tx = tx;
        rt.ops = Arc::new(SavingLoginOps {
            config: config.clone(),
        });
        let login = crate::transport::Login {
            address: Some("192.0.2.10".into()),
            port: Some(2222),
            user: Some("remoteuser".into()),
        };
        rt.execute_commands(vec![crate::model::Command::RunLogin {
            host: "prod".into(),
            login: login.clone(),
            password: "test-only-secret".into(),
            after_login: choice,
        }]);
        let progress = &rt.model.state.login_progress["prod"];
        assert_eq!(
            progress
                .steps
                .iter()
                .any(|row| row.step == crate::model::LoginStep::Save),
            saves,
        );
        assert_eq!(
            progress
                .steps
                .iter()
                .any(|row| row.step == crate::model::LoginStep::RegisterKey),
            registers,
        );
        let result = tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                if let crate::model::OpResult::Login { outcome, .. } = rx.recv().await.unwrap() {
                    break outcome;
                }
            }
        })
        .await
        .unwrap();
        assert!(result.connect.is_ok(), "{result:?}");
        assert_eq!(result.saved, saves.then_some(Ok(())), "{choice:?}");
        assert_eq!(
            result.registration,
            if registers {
                RegistrationOutcome::Registered
            } else {
                RegistrationOutcome::NotRequested
            }
        );
        assert_eq!(config.exists(), saves);
        if saves {
            let text = std::fs::read_to_string(&config).unwrap();
            let defaults = crate::provision::config::login_defaults("prod", None, None, &text);
            assert_eq!(defaults.ssh_effective, Some(login));
            assert!(!text.contains("test-only-secret"));
        }
    }
}

#[tokio::test]
async fn login_after_failed_rescan_refreshes_finished_poll() {
    let mut rt = test_rt(fake_env_with_machines(&["prod"]));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    rt.mgr = HostManager::new(tx);
    let mut host = crate::model::Host::new(
        crate::transport::local_as("prod".into(), None),
        crate::mux::for_kind("zellij", "xmux-missing-poll-test-binary").unwrap(),
    );
    host.detected = true;
    rt.hosts.insert(host);
    rt.mgr
        .ensure("prod", rt.hosts.get("prod").unwrap(), 80, 24)
        .unwrap();
    let first = tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(&first, HostEvent::Sessions { err: Some(_), .. }));
    rt.handle_host_event(first);
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while rt.mgr.is_live("prod") {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    rt.handle_host_event(HostEvent::MachineProbed {
        machine: "prod".into(),
        err: Some("Permission denied (publickey,password).".into()),
        shell: None,
        rescan: true,
        password_supplied: false,
        credential_rejection_generation: None,
        credential_held: false,
        credential_generation: 0,
        current_credential_generation: 0,
    });
    rt.execute_effects(vec![Effect::LoginApplied {
        host: "prod".into(),
        login: Default::default(),
    }]);
    let probe = tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
        .await
        .unwrap()
        .unwrap();
    rt.handle_host_event(probe);
    let refreshed = tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv()).await;
    let event = refreshed
        .expect("a successful login must enumerate the stopped poll host")
        .unwrap();
    assert!(matches!(&event, HostEvent::Sessions { host, .. } if host == "prod"));
    rt.handle_host_event(event);
    assert!(!rt.model.state.scanning.contains("prod"));
}

#[test]
fn newly_created_config_is_applied_without_a_second_edit() {
    let env = fake_env_with_machines(&[]);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = env.xmux_dir.join(format!("created-config-{stamp}.toml"));
    let mut last = None;
    assert!(super::handlers::poll_ui_config(&mut last, &path).is_none());
    std::fs::write(&path, "[ui]\nmax-fps = 60").unwrap();
    let ui = super::handlers::poll_ui_config(&mut last, &path)
        .expect("file creation reloads")
        .unwrap();
    assert_eq!(ui.max_fps, 60);
    std::fs::remove_file(&path).unwrap();
    assert!(super::handlers::poll_ui_config(&mut last, &path).is_none());
    std::fs::write(&path, "[ui]\nmax-fps = 90").unwrap();
    let ui = super::handlers::poll_ui_config(&mut last, &path)
        .expect("file replacement reloads")
        .unwrap();
    assert_eq!(ui.max_fps, 90);
}
