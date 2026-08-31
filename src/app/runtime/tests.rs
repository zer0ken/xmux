use super::*;
use crate::model::source::Source;

fn fake_source(alias: &str) -> Source {
    Source {
        alias: alias.into(),
        binary: "tmux".into(),
        kind: crate::transport::MachineKind::Local {
            id: String::new(),
            socket: None,
        },
        runner: None,
    }
}

fn fake_roster(aliases: &[&str]) -> crate::provision::env::Roster {
    crate::provision::env::Roster {
        sources: aliases.iter().map(|a| fake_source(a)).collect(),
        local_muxes: vec!["tmux".into()],
        ssh_aliases: aliases
            .iter()
            .filter(|a| **a != crate::session::LOCAL_SOURCE)
            .map(|a| a.to_string())
            .collect(),
        ..Default::default()
    }
}

fn fake_env_with_sources(aliases: &[&str]) -> Env {
    // A real throwaway dir, not `.`: tests that exercise pref persistence (e.g.
    // resize_axis saving nav_height) write `<xmux_dir>/<file>`, and `.` would
    // pollute the repository root with stray pref files.
    let xmux_dir = std::env::temp_dir().join(format!("xmux-test-env-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&xmux_dir);
    Env::new(fake_roster(aliases), "C-g".into(), xmux_dir, None, None)
}

#[test]
fn selection_from_session_row_target() {
    let t = TerminalViewTarget {
        source: "jupiter06".into(),
        target: "api".into(),
    };
    let sel = selection_from_target(&t);
    assert_eq!(sel.source, "jupiter06");
    assert_eq!(sel.session, "api");
    assert_eq!(sel.window, None);
    assert_eq!(sel.address(), "jupiter06/api");
    assert!(!sel.is_empty());
}

#[test]
fn selection_from_window_row_target() {
    // A window-row target `session:window` keeps the session as the PTY key and
    // carries the window index for select-window.
    let t = TerminalViewTarget {
        source: "jupiter06".into(),
        target: "api:2".into(),
    };
    let sel = selection_from_target(&t);
    assert_eq!(sel.session, "api");
    assert_eq!(sel.window, Some(2));
    assert_eq!(
        sel.address(),
        "jupiter06/api",
        "address is source/session, not the window"
    );
}

#[test]
fn selection_from_empty_target_is_empty() {
    let sel = selection_from_target(&TerminalViewTarget::default());
    assert!(sel.is_empty());
    assert_eq!(sel.window, None);
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
        source: "jup".into(),
        session: "api".into(),
        window: None,
    };
    assert_eq!(display_key(&hosts, &rsel), "jup", "shared → per-host key");
    let lsel = Selection {
        source: "local".into(),
        session: "work".into(),
        window: None,
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
        crate::model::EventSource::Poll { .. }
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

#[tokio::test]
async fn dispatch_detected_host_connects_remote_hosts() {
    // Control-event (tmux) hosts get a control client at startup; poll hosts
    // enumerate off the loop (no control client). The gate is the host's
    // event_source, read off the Host - not the transport remote flag. The
    // control child spawns and dies at once on a host that is not there.
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<HostEvent>();
    let mut mgr = HostManager::new(tx);
    let mut hosts = crate::model::Hosts::default();
    let mut host = crate::model::Host::new(
        crate::transport::ssh("jupiter06".into(), String::new(), "linux".into()),
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
    // Env/by_alias. The detection branch marks the source in `detecting`; the probe
    // clones the host's transport + mux rather than re-deriving from a Source.
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<HostEvent>();
    let mut mgr = HostManager::new(tx);
    let mut hosts = crate::model::Hosts::default();
    hosts.insert(crate::model::Host::new(
        crate::transport::local(None),
        crate::mux::for_kind("psmux", "psmux-no-such-binary").unwrap(),
    )); // Host::new leaves it undetected
    let mut detecting = HashSet::new();
    scan_or_dispatch_host(&mut mgr, &hosts, &mut detecting, "local", 80, 24);
    assert!(
        detecting.contains("local"),
        "an undetected host is queued for detection straight from the registry"
    );
}

#[test]
fn terminal_view_size_zero_tree_is_full_width() {
    // Hidden tree (sentinel 0): full cols, no view border subtracted.
    assert_eq!(
        terminal_view_size(
            80,
            23,
            crate::ui::switcher::NavSize::hidden(crate::ui::switcher::NAV_WIDTH)
        ),
        (80, 24)
    );
    // Shown tree: cols - nav_width - 1 (view border). The hint bar lives inside the nav
    // column, so the terminal view keeps every row. Wide enough to STAY in Side: a row is
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
    // Tree shown in Side: the hint bar is the NAV column's bottom row, not a full-width
    // strip, so the terminal view costs nothing in height.
    // 220 wide keeps the side column at 40 rows (171 against 80); at 120 the column would
    // leave a terminal squarer than it looks, and the band would take over.
    let (_, shown) = terminal_view_size(220, 39, crate::ui::switcher::NavSize::visible(NAV_WIDTH));
    assert_eq!(
        shown, 40,
        "the nav-local hint bar costs the terminal view no rows"
    );
}

#[test]
fn reconciled_nav_width_hides_only_when_focused_and_enabled_and_no_prefix() {
    // Tree focused (terminal_focused = false): always the natural width.
    assert_eq!(reconciled_nav_width(false, true, false, 48), 48);
    assert_eq!(reconciled_nav_width(false, false, true, 48), 48);
    // Terminal view focused + setting on + no prefix interaction: hidden (0).
    assert_eq!(reconciled_nav_width(true, true, false, 48), 0);
    // Terminal view focused + setting on + prefix active: shown.
    assert_eq!(reconciled_nav_width(true, true, true, 48), 48);
    // Terminal view focused + setting off: stays shown regardless.
    assert_eq!(reconciled_nav_width(true, false, false, 48), 48);
    assert_eq!(reconciled_nav_width(true, false, true, 48), 48);
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
    // The floor is the resting prefix "C-g" (3 cells) plus a one-cell gap each side.
    let min = nav_width_min("C-g");
    assert_eq!(adjust_nav_width(48, 1, "C-g"), 49);
    assert_eq!(adjust_nav_width(48, -1, "C-g"), 47);
    assert_eq!(
        adjust_nav_width(min, -1, "C-g"),
        min,
        "clamped at the prefix floor"
    );
    assert_eq!(
        adjust_nav_width(NAV_WIDTH_MAX, 1, "C-g"),
        NAV_WIDTH_MAX,
        "clamped at max"
    );
    assert_eq!(
        nav_width_min("C-Space"),
        9,
        "a wider prefix raises the floor"
    );
}

#[test]
fn terminal_view_size_subtracts_tree_and_view_border() {
    use crate::ui::switcher::NAV_WIDTH;
    let (vc, vr) = terminal_view_size(143, 39, crate::ui::switcher::NavSize::visible(NAV_WIDTH));
    assert_eq!(
        vc,
        143 - (NAV_WIDTH + 1),
        "cols minus tree minus view border"
    );
    // The hint bar sits inside the nav column, so the terminal view keeps the full
    // terminal height (body_rows + 1).
    assert_eq!(vr, 40, "the nav-local hint bar costs no terminal rows");
}

#[test]
fn terminal_view_size_clamps_to_at_least_one() {
    use crate::ui::switcher::NAV_WIDTH;
    // A 10-col terminal can't fit the 48-col tree beside it, so the layout goes Top and the
    // terminal keeps full width; a zero-row body still clamps the height up to 1. The
    // invariant this guards is that neither dimension is ever 0 (degenerate PTY size).
    let (vc, vr) = terminal_view_size(10, 0, crate::ui::switcher::NavSize::visible(NAV_WIDTH));
    assert!(vc >= 1, "width never zero, got {vc}");
    assert_eq!(vr, 1, "0.max(1) = 1: height clamps up for a zero-row body");
}

#[tokio::test]
async fn host_exited_before_connect_marks_unreachable() {
    use crate::ui::run::dump_screen;
    use crate::ui::switcher::Switcher;
    let mut state = crate::state::State::from_sources(vec!["jupiter00".into()]);
    let mut switcher = Switcher::from_sources(&mut state);
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
    let out = dump_screen(&mut switcher, None, 80, 24, &state);
    assert!(
        out.contains("unreachable"),
        "host reads unreachable:\n{out}"
    );
    assert!(
        out.contains("no route to host"),
        "shows the exit reason:\n{out}"
    );
}

#[tokio::test]
async fn host_exited_with_no_sessions_marks_empty_not_unreachable() {
    use crate::ui::run::dump_screen;
    use crate::ui::switcher::Switcher;
    let mut state = crate::state::State::from_sources(vec!["jupiter06".into()]);
    let mut switcher = Switcher::from_sources(&mut state);
    let mut connected: HashSet<String> = HashSet::new();
    // A reachable host whose mux has no server: "no sessions" → (empty), not ⚠.
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
    let out = dump_screen(&mut switcher, None, 80, 24, &state);
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
    let mut state = crate::state::State::from_sources(vec!["jupiter06".into()]);
    let mut switcher = Switcher::from_sources(&mut state);
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
    let mut state = crate::state::State::from_sources(vec!["jupiter06".into()]);
    let mut switcher = Switcher::from_sources(&mut state);
    let mut connected: HashSet<String> = HashSet::new();
    connected.insert("jupiter06".into());
    // First drop of the connected host: keeps last-known tree, clears connected.
    note_host_exited(&mut switcher, &mut state, &mut connected, "jupiter06", None);
    // User hits refresh → the host goes back to a scanning skeleton.
    switcher.request_rescan(&mut state);
    assert!(
        dump_screen(&mut switcher, None, 80, 24, &state).contains("scanning"),
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
    let out = dump_screen(&mut switcher, None, 80, 24, &state);
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
    let env = fake_env_with_sources(&["local", "jupiter06"]);
    assert_eq!(env.source_list().len(), 2);
}

#[test]
fn apply_inventory_effect_folds_sessions_into_host_inventory() {
    // C1: the control reader carries its parsed sessions on the HostEvent; the
    // loop folds them into the single owner (`model::Host.inventory`) and applies
    // them to the nav. There is no shared `Arc<Mutex<HostInventory>>` to read.
    use crate::ui::switcher::{Scan, Switcher};
    use crate::ui::tree::Group;

    let scan = Scan {
        groups: vec![Group {
            source: "jup".into(),
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
    let mut rt = test_rt(fake_env_with_sources(&[]));
    rt.mgr.insert_fake("jup"); // a control client so the display attach has a sink
    rt.hosts = hosts;
    rt.state = state;
    rt.switcher = switcher;

    let sessions = vec![crate::session::Session {
        mux: String::new(),
        source: "jup".into(),
        name: "api".into(),
        ..Default::default()
    }];
    let rearm = rt.run_event_effect(crate::model::EventEffect::ApplyInventory {
        host: "jup".into(),
        sessions: sessions.clone(),
    });
    assert!(!rearm, "ApplyInventory does not rearm detach recovery");
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
        .state
        .groups
        .iter()
        .find(|g| g.source == "jup")
        .expect("jup group");
    assert_eq!(group.sessions.len(), 1, "tree applied the carried sessions");
    assert_eq!(group.sessions[0].name, "api");
}

// A re-scan starts the roster re-resolution off the loop, so the harness needs the
// runtime the real loop always runs inside.
#[tokio::test]
async fn r_rescan_rebuilds_nav_and_kicks_discovery() {
    // The client-initiated `r` re-scan resets the nav to its scanning skeleton
    // (clears every group's sessions) and raises the rescan kick; the loop consumes
    // the kick in kick_rescan, which re-lists each host. No session survives into
    // the skeleton, so the subtree must stream back exactly as on first launch.
    use crate::session::Session;
    use crate::ui::switcher::{Scan, Switcher};
    use crate::ui::tree::Group;

    let scan = Scan {
        groups: vec![Group {
            source: "jup".into(),
            err: None,
            sessions: vec![Session {
                mux: String::new(),
                source: "jup".into(),
                name: "api".into(),
                windows: 1,
                attached: false,
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

    let mut rt = test_rt(fake_env_with_sources(&[]));
    rt.mgr.insert_fake("jup");
    rt.hosts = hosts;
    rt.state = state;
    rt.switcher = switcher;

    // The `r` re-scan resets the nav to its scanning skeleton and clears sessions.
    rt.switcher.request_rescan(&mut rt.state);
    assert!(
        rt.state.groups.iter().all(|g| g.sessions.is_empty()),
        "request_rescan cleared the loaded sessions"
    );
    assert!(
        rt.switcher.take_rescan_kick(),
        "request_rescan raised the rescan kick"
    );

    // The loop consumes the kick and re-lists each host.
    kick_rescan(
        &rt.env,
        &mut rt.switcher,
        &rt.hosts,
        &mut rt.detecting,
        &mut rt.mgr,
        (80, 24),
    );
    assert!(
        !rt.switcher.take_rescan_kick(),
        "kick_rescan consumed the rescan kick"
    );
}

#[test]
fn current_grid_returns_none_for_empty_displayed() {
    // An empty `displayed` (source "") misses `hosts.get`, so no driver is
    // built and no grid is produced - the blank-terminal case on first launch.
    let mut hosts = crate::model::Hosts::default();
    let mut registry = AttachRegistry::new();
    let (ptx, _prx) = tokio::sync::mpsc::unbounded_channel();
    let worker = crate::display::DisplayWorker::new(ptx);
    let (etx, _erx) = tokio::sync::mpsc::unbounded_channel::<crate::link::HostEvent>();
    let mgr = HostManager::new(etx);
    let (pty_tx, _pty_rx) = tokio::sync::mpsc::unbounded_channel::<PtyEvent>();
    let mut attach_seq = 0u64;
    let displayed = Selection::default();
    let grid = current_grid(
        &displayed,
        &crate::driver::DriverCtx {
            registry: &mut registry,
            hosts: &mut hosts,
            worker: &worker,
            mgr: &mgr,
            pty_tx: &pty_tx,
            attach_seq: &mut attach_seq,
            cols: 80,
            body_rows: 24,
            nav: crate::ui::switcher::NavSize::visible(crate::ui::switcher::NAV_WIDTH),
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
    let (etx, _erx) = tokio::sync::mpsc::unbounded_channel::<crate::link::HostEvent>();
    let mgr = HostManager::new(etx);
    let (pty_tx, _ptx_rx) = tokio::sync::mpsc::unbounded_channel::<PtyEvent>();

    let sel_a = Selection {
        source: "jup".into(),
        session: "a".into(),
        window: None,
    };
    let sel_b = Selection {
        source: "jup".into(),
        session: "b".into(),
        window: None,
    };

    // First attach (session a): requests off-loop, latches display.current[jup]=a, marks in-flight.
    assert!(select_attach(
        &sel_a,
        &mut crate::driver::DriverCtx {
            registry: &mut registry,
            hosts: &mut hosts,
            worker: &worker,
            mgr: &mgr,
            pty_tx: &pty_tx,
            attach_seq: &mut attach_seq,
            cols: 80,
            body_rows: 24,
            nav: crate::ui::switcher::NavSize::visible(crate::ui::switcher::NAV_WIDTH),
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
            worker: &worker,
            mgr: &mgr,
            pty_tx: &pty_tx,
            attach_seq: &mut attach_seq,
            cols: 80,
            body_rows: 24,
            nav: crate::ui::switcher::NavSize::visible(crate::ui::switcher::NAV_WIDTH),
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
    let mgr = empty_manager();
    let (pty_tx, _ptx_rx) = tokio::sync::mpsc::unbounded_channel::<PtyEvent>();

    let sel_test2 = Selection {
        source: "local".into(),
        session: "test2".into(),
        window: None,
    };
    let sel_test = Selection {
        source: "local".into(),
        session: "test".into(),
        window: None,
    };

    assert!(select_attach(
        &sel_test2,
        &mut crate::driver::DriverCtx {
            registry: &mut registry,
            hosts: &mut hosts,
            worker: &worker,
            mgr: &mgr,
            pty_tx: &pty_tx,
            attach_seq: &mut attach_seq,
            cols: 80,
            body_rows: 24,
            nav: crate::ui::switcher::NavSize::visible(crate::ui::switcher::NAV_WIDTH),
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
                h.display.resolve_ready(&key, seq, id),
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
            worker: &worker,
            mgr: &mgr,
            pty_tx: &pty_tx,
            attach_seq: &mut attach_seq,
            cols: 80,
            body_rows: 24,
            nav: crate::ui::switcher::NavSize::visible(crate::ui::switcher::NAV_WIDTH),
        }
    ));

    let h = hosts.get("local").unwrap();
    assert_eq!(h.display.shows("local"), Some("test"));
    assert!(h.display.in_flight_contains("local"));
    assert!(
        registry.contains("local"),
        "old psmux display attach is HELD on screen until the reattach is ready \
             (stale-while-revalidate); DisplayReady swaps it in and tears the old down"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn psmux_select_attach_does_not_trust_stale_display_bookkeeping() {
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
    let mgr = empty_manager();
    let (pty_tx, _ptx_rx) = tokio::sync::mpsc::unbounded_channel::<PtyEvent>();

    let sel = Selection {
        source: "local".into(),
        session: "target".into(),
        window: None,
    };

    assert!(select_attach(
        &sel,
        &mut crate::driver::DriverCtx {
            registry: &mut registry,
            hosts: &mut hosts,
            worker: &worker,
            mgr: &mgr,
            pty_tx: &pty_tx,
            attach_seq: &mut attach_seq,
            cols: 80,
            body_rows: 24,
            nav: crate::ui::switcher::NavSize::visible(crate::ui::switcher::NAV_WIDTH),
        }
    ));

    let h = hosts.get("local").unwrap();
    assert!(h.display.in_flight_contains("local"));
    assert!(
        registry.contains("local"),
        "psmux select_attach requests a reattach even when bookkeeping is stale, but \
             HOLDS the prior grid on screen until DisplayReady swaps in the fresh one"
    );
}

#[test]
fn should_attach_fires_on_change_and_recovery_never_storms_in_flight() {
    let a = Selection {
        source: "h".into(),
        session: "api".into(),
        window: None,
    };
    let b = Selection {
        session: "db".into(),
        ..a.clone()
    };
    let gate = |selection: &Selection, displayed: &Selection, key_live, in_flight, astray| {
        let s = crate::state::State {
            selection: selection.clone(),
            displayed: displayed.clone(),
            ..crate::state::State::default()
        };
        s.should_attach(key_live, in_flight, astray)
    };
    // Settled: displayed == selection, PTY live, nothing in flight → no attach.
    assert!(!gate(&a, &a, true, false, false));
    // Selection moved off the displayed session → attach.
    assert!(gate(&b, &a, true, false, false));
    // An attach for the key is already in flight → never re-fire (no storm).
    assert!(!gate(&b, &a, false, true, false));
    // PTY gone (exited / reaped) while displayed == selection → re-attach to recover.
    assert!(gate(&a, &a, false, false, false));
    // Everything xmux itself recorded agrees - the selection is what it last put on
    // screen and that PTY is alive - and the client is on another session anyway. Only
    // the astray leg can see it, and this is the split it closes.
    assert!(gate(&a, &a, true, false, true));
    // Even then, not on top of an attach already carrying the display there.
    assert!(!gate(&a, &a, true, true, true));
}

#[tokio::test(flavor = "current_thread")]
async fn psmux_select_attach_supersedes_in_flight_attach() {
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
    let mgr = empty_manager();
    let (pty_tx, _ptx_rx) = tokio::sync::mpsc::unbounded_channel::<PtyEvent>();

    let sel = Selection {
        source: "local".into(),
        session: "target".into(),
        window: None,
    };

    assert!(select_attach(
        &sel,
        &mut crate::driver::DriverCtx {
            registry: &mut registry,
            hosts: &mut hosts,
            worker: &worker,
            mgr: &mgr,
            pty_tx: &pty_tx,
            attach_seq: &mut attach_seq,
            cols: 80,
            body_rows: 24,
            nav: crate::ui::switcher::NavSize::visible(crate::ui::switcher::NAV_WIDTH),
        }
    ));

    let h = hosts.get("local").unwrap();
    assert_eq!(h.display.in_flight_seq("local"), Some(8));
}

fn empty_manager() -> HostManager {
    HostManager::new(tokio::sync::mpsc::unbounded_channel().0)
}

/// A headless `Runtime` for exercising the `&mut self` arm/effect methods: a fake
/// attach worker (no real PTYs), dropped receiver halves, hosts built from `env`.
/// A test overrides the fields it cares about (`rt.hosts`, `rt.state`, ...).
#[tokio::test]
async fn a_re_scan_roster_adds_a_machine_it_now_names() {
    // The point of re-resolving on a re-scan: a machine that was not reachable at launch
    // (a tailnet peer that has since come online, a host the user just wrote into the
    // config) turns into a card without a restart.
    let mut rt = test_rt(fake_env_with_sources(&["prod"]));
    assert!(rt.hosts.get("stage").is_none(), "nothing knows stage yet");
    rt.run_event_effect(crate::model::EventEffect::ApplyRoster {
        roster: Box::new(fake_roster(&["prod", "stage"])),
    });
    assert!(
        rt.hosts.get("stage").is_some(),
        "the loop's registry has it"
    );
    assert!(
        rt.env.source("stage").is_some(),
        "and so do the off-loop ops, which resolve a source through Env"
    );
    assert!(
        rt.state.groups.iter().any(|g| g.source == "stage"),
        "and it has a card"
    );
    assert!(
        rt.hosts.get("prod").is_some(),
        "a machine that was already there is untouched"
    );
}

#[tokio::test]
async fn a_re_scan_roster_drops_a_machine_it_stopped_naming() {
    // The mirror case: the config turned a provider off, or a peer went offline. All
    // three registries have to let go, or the nav paints a card nothing can reach.
    let mut rt = test_rt(fake_env_with_sources(&["prod", "stage"]));
    assert!(rt.hosts.get("stage").is_some(), "precondition");
    rt.run_event_effect(crate::model::EventEffect::ApplyRoster {
        roster: Box::new(fake_roster(&["prod"])),
    });
    assert!(rt.hosts.get("stage").is_none(), "the registry let go");
    assert!(rt.env.source("stage").is_none(), "the off-loop ops let go");
    assert!(
        !rt.state.groups.iter().any(|g| g.source == "stage"),
        "and the card is gone"
    );
    assert!(rt.hosts.get("prod").is_some(), "prod is still named");
}

#[tokio::test]
async fn a_discovered_mux_becomes_a_source_on_the_spot() {
    // The whole point of discovering asynchronously: the machine's answer arrives after
    // the app is up, and the mux nobody wrote down turns into a card RIGHT THEN.
    let mut rt = test_rt(fake_env_with_sources(&["prod"]));
    assert!(
        rt.hosts.get("prod:zellij").is_none(),
        "nothing knows about zellij yet"
    );
    rt.run_event_effect(crate::model::EventEffect::AddDiscoveredSources {
        machine: "prod".into(),
        muxes: vec!["tmux".into(), "zellij".into()],
    });
    // tmux is what `prod` was already painted as, so it is left exactly as it is: its
    // BARE id is what the frozen order, the saved selection, and anything the user typed
    // are keyed to.
    assert!(rt.hosts.get("prod").is_some(), "the bare id is untouched");
    assert!(
        rt.hosts.get("prod:tmux").is_none(),
        "the mux it already serves is not added a second time"
    );
    // zellij is new, so it becomes its own source under a qualified id, scanning.
    let h = rt.hosts.get("prod:zellij").expect("the discovered source");
    assert_eq!(h.mux.kind(), "zellij");
    assert_eq!(h.transport.host_id(), "prod:zellij", "it answers as itself");
    // And the OFF-LOOP ops resolve it: they look a source up in `Env`, so a discovered
    // source missing from that list scans and paints but refuses `prefix n` with
    // `unknown source`.
    let src = rt
        .env
        .source("prod:zellij")
        .expect("the off-loop ops know the discovered source");
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
        rt.state.groups.iter().any(|g| g.source == "prod:zellij"),
        "and it has a card: {:?}",
        rt.state
            .groups
            .iter()
            .map(|g| &g.source)
            .collect::<Vec<_>>()
    );
    assert!(
        rt.state.scanning.contains("prod:zellij"),
        "the card reads scanning until its first result"
    );
    // Idempotent: the same answer twice adds nothing.
    let before = rt.state.groups.len();
    rt.run_event_effect(crate::model::EventEffect::AddDiscoveredSources {
        machine: "prod".into(),
        muxes: vec!["tmux".into(), "zellij".into()],
    });
    assert_eq!(rt.state.groups.len(), before, "no duplicate card");
}

#[tokio::test]
async fn a_discovered_source_sorts_into_place_and_leaves_the_selection_put() {
    // A card the user is looking at must not move because another machine answered:
    // the discovered card sorts into its name position, and the selection stays put.
    let mut rt = test_rt(fake_env_with_sources(&["prod", "db"]));
    let selected = {
        let t = rt.switcher.terminal_view_target();
        (t.source, t.target)
    };
    rt.run_event_effect(crate::model::EventEffect::AddDiscoveredSources {
        machine: "db".into(),
        muxes: vec!["zellij".into()],
    });
    let after: Vec<String> = rt.state.groups.iter().map(|g| g.source.clone()).collect();
    assert_eq!(
        after,
        vec!["local", "db", "db:zellij", "prod"],
        "the discovered card sorts into name order"
    );
    let now = rt.switcher.terminal_view_target();
    assert_eq!(
        (now.source, now.target),
        selected,
        "the selection stays put"
    );
}

fn test_rt(env: Env) -> Runtime {
    let env = std::sync::Arc::new(env);
    let (host_tx, _host_rx) = tokio::sync::mpsc::unbounded_channel();
    let mgr = HostManager::new(host_tx);
    let (wtx, _wrx) = tokio::sync::mpsc::unbounded_channel::<PtyEvent>();
    let worker = DisplayWorker::with_spawner(
        wtx,
        Box::new(|_argv, _cols, _rows, id, _events, _env_clear| {
            Ok(crate::display::attachment::fake_attachment(id))
        }),
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
    let mut state = crate::state::State::from_sources(hosts.ids().to_vec());
    let switcher = crate::ui::switcher::Switcher::from_sources(&mut state);
    let ops = env.ops();
    let (op_tx, _op_rx) = tokio::sync::mpsc::unbounded_channel();
    let prefix = crate::display::term::parse_prefix(Some(&env.ui_prefix));
    Runtime {
        instance_name: "test".into(),
        env,
        ops,
        hosts,
        mgr,
        registry: AttachRegistry::new(),
        worker,
        switcher,
        state,
        attach_seq: 0,
        driver_pty_tx: pty_tx,
        op_tx,
        cols: 80,
        body_rows: 24,
        nav_width: crate::ui::switcher::NAV_WIDTH,
        nav_width_natural: crate::ui::switcher::NAV_WIDTH,
        nav_height: 0,
        applied_nav_height: u16::MAX,
        auto_hide_nav: false,
        mouse_state: MouseState::default(),
        term_input: crate::display::input::TermInput::new(prefix),
        nav_decoder: crate::display::decode::KeyDecoder::new(),
        prefix,
        connected: HashSet::new(),
        detecting: HashSet::new(),
        draw_observer: DrawObserver::default(),
        spinner_start: std::time::Instant::now(),
        dirty: true,
        last_draw: std::time::Instant::now(),
        config_last_mtime: None,
        width_dirty: false,
        width_flush_at: None,
    }
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
    let mut state = crate::state::State::from_sources(vec!["jup".into()]);
    let switcher = crate::ui::switcher::Switcher::from_sources(&mut state);
    let mut rt = test_rt(fake_env_with_sources(&[]));
    rt.hosts = detach_test_hosts("jup");
    rt.state = state;
    rt.switcher = switcher;

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
    let mut state = crate::state::State::from_sources(vec!["jup".into()]);
    let switcher = crate::ui::switcher::Switcher::from_sources(&mut state);
    let mut rt = test_rt(fake_env_with_sources(&[]));
    rt.hosts = detach_test_hosts("jup");
    rt.state = state;
    rt.switcher = switcher;

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

/// A host `jup` with two loaded sessions: `api` and `db`.
/// Lets a follow test assert the selection lands on the mux-moved session's card.
fn two_session_scan() -> crate::ui::switcher::Scan {
    use crate::session::Session;
    use crate::ui::switcher::Scan;
    use crate::ui::tree::Group;
    let sess = |name: &str, windows: i64| Session {
        mux: String::new(),
        source: "jup".into(),
        name: name.into(),
        windows,
        attached: false,
    };
    Scan {
        groups: vec![Group {
            source: "jup".into(),
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
    sync_selection_from_switcher(&mut rt.state, &rt.switcher);
    rt.state.displayed = rt.state.selection.clone();
    rt.state.attach_pending = false;
    rt.state.attach_deadline = None;
}

/// One pass of the loop top, minus the drawing a headless test has no terminal for:
/// the two regions are reconciled against each other, the selection settles, and the
/// attach beat runs at `now`. Passing the SAME `now` to several passes holds the clock
/// still, so the debounce cannot elapse between them.
fn one_pass(rt: &mut Runtime, now: std::time::Instant) {
    rt.follow_selection_to_display();
    sync_selection_from_switcher(&mut rt.state, &rt.switcher);
    rt.drive_attach_beat(now);
}

#[tokio::test(flavor = "current_thread")]
async fn a_mux_side_switch_in_terminal_focus_moves_the_nav_to_that_session() {
    // With the terminal focused the user is driving the mux (prefix+s), so the session
    // they moved to is where they want to be: the NAV goes to its card, and the client
    // stays where it is.
    let mut state = crate::state::State::from_scan(two_session_scan());
    let mut switcher = crate::ui::switcher::Switcher::new(&mut state);
    switcher.select_address("jup/api", &state); // deterministic start (ignore any last_session)
    let mut rt = test_rt(fake_env_with_sources(&[]));
    rt.hosts = detach_test_hosts("jup");
    rt.hosts.get_mut("jup").unwrap().display_tty =
        crate::model::DisplayTty(Some("/dev/pts/3".into()));
    rt.hosts
        .get_mut("jup")
        .unwrap()
        .display
        .set_shows("jup", "api");
    rt.registry.insert_fake("jup", 7);
    rt.state = state;
    rt.switcher = switcher;
    rt.state
        .focus
        .set_view_focus(crate::app::focus::ViewFocus::Terminal);
    settled(&mut rt);
    let t0 = std::time::Instant::now();
    assert_eq!(
        rt.switcher.terminal_view_target().target,
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
        rt.switcher.terminal_view_target().target,
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
            source: "jup".into(),
            name: (*name).into(),
            windows: 1,
            attached: false,
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
    switcher.select_address("jup/api", &state);
    let mut rt = test_rt(fake_env_with_sources(&[]));
    rt.hosts = detach_test_hosts("jup");
    rt.hosts.get_mut("jup").unwrap().display_tty =
        crate::model::DisplayTty(Some("/dev/pts/3".into()));
    rt.hosts
        .get_mut("jup")
        .unwrap()
        .display
        .set_shows("jup", "api");
    rt.registry.insert_fake("jup", 7);
    rt.state = state;
    rt.switcher = switcher;
    rt.state
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
        rt.switcher.terminal_view_target().target,
        "api",
        "there is no ops card to move to yet"
    );

    rt.handle_host_event(HostEvent::Sessions {
        source: "jup".into(),
        sessions: jup_sessions(&["api", "db", "ops"]),
        err: None,
    });
    one_pass(&mut rt, t0);
    assert_eq!(
        rt.switcher.terminal_view_target().target,
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
    switcher.select_address("jup/api", &state);
    let mut rt = test_rt(fake_env_with_sources(&[]));
    rt.hosts = detach_test_hosts("jup");
    rt.hosts.get_mut("jup").unwrap().display_tty =
        crate::model::DisplayTty(Some("/dev/pts/3".into()));
    rt.hosts
        .get_mut("jup")
        .unwrap()
        .display
        .set_shows("jup", "api");
    rt.registry.insert_fake("jup", 7);
    rt.state = state;
    rt.switcher = switcher;
    rt.state
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
        rt.switcher.terminal_view_target().target,
        "db",
        "the nav is on the session the client ended up on"
    );

    rt.handle_host_event(HostEvent::Sessions {
        source: "jup".into(),
        sessions: jup_sessions(&["api", "db", "ops"]),
        err: None,
    });
    one_pass(&mut rt, t0);
    assert_eq!(
        rt.switcher.terminal_view_target().target,
        "db",
        "the ops card appearing moves nothing: the client left ops"
    );
}

/// The id of the psmux client xmux itself spawned, so a test can say whether a pass kept
/// it or replaced it.
const OWN_CLIENT: u64 = 42;

/// A local psmux source holding sessions `a` and `b`.
fn psmux_scan() -> crate::ui::switcher::Scan {
    use crate::session::Session;
    use crate::ui::tree::Group;
    let sess = |name: &str| Session {
        mux: String::new(),
        source: "local".into(),
        name: name.into(),
        windows: 1,
        attached: false,
    };
    crate::ui::switcher::Scan {
        groups: vec![Group {
            source: "local".into(),
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
    switcher.select_address("local/a", &state);
    let mut rt = test_rt(fake_env_with_sources(&[]));
    let mut hosts = crate::model::Hosts::default();
    hosts.insert(crate::model::Host::new(
        crate::transport::local(None),
        crate::mux::for_binary("psmux").unwrap(),
    ));
    rt.hosts = hosts;
    rt.state = state;
    rt.switcher = switcher;
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

#[tokio::test(flavor = "current_thread")]
async fn a_mux_side_switch_in_terminal_focus_keeps_the_client_the_user_moved() {
    // The other half of the same rule, on the mux that has to be READ for a switch. The
    // nav follows the client to `b`, which makes the selection differ from what is
    // confirmed on screen, so an attach fires for `b` - and a per-session mux reaches a
    // session by reattaching, which would kill the very client the user just moved. It is
    // held instead, on the client's own report that it is already there.
    let mut rt = a_settled_psmux_runtime();
    rt.state
        .focus
        .set_view_focus(crate::app::focus::ViewFocus::Terminal);
    let t0 = std::time::Instant::now();

    the_client_reports(&mut rt, OWN_CLIENT, "b");
    rt.observe_display_session();
    one_pass(&mut rt, t0);
    one_pass(&mut rt, t0 + std::time::Duration::from_millis(200));

    assert_eq!(
        rt.switcher.terminal_view_target().target,
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
    assert!(!rt.state.focus.is_terminal_focused(), "starts in nav focus");

    the_client_reports(&mut rt, OWN_CLIENT, "b");
    assert!(
        rt.observe_display_session(),
        "the beat reads the client's own report of where it went"
    );

    let t0 = std::time::Instant::now();
    one_pass(&mut rt, t0);
    one_pass(&mut rt, t0 + std::time::Duration::from_millis(200));

    assert_eq!(
        rt.switcher.terminal_view_target().target,
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
            rt.switcher.terminal_view_target().target.as_str(),
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
    rt.state
        .focus
        .set_view_focus(crate::app::focus::ViewFocus::Terminal);
    let t0 = std::time::Instant::now();

    // What a ctl `switch local/b` dispatches to.
    rt.switcher.select_address("local/b", &rt.state);
    one_pass(&mut rt, t0);
    one_pass(&mut rt, t0 + std::time::Duration::from_millis(50));
    assert_eq!(
        rt.switcher.terminal_view_target().target,
        "b",
        "the selection stays where the switch put it while its attach is owed"
    );

    one_pass(&mut rt, t0 + std::time::Duration::from_millis(200));
    assert_eq!(
        rt.switcher.terminal_view_target().target,
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
    rt.state
        .focus
        .set_view_focus(crate::app::focus::ViewFocus::Terminal);
    let t0 = std::time::Instant::now();
    one_pass(&mut rt, t0);

    // `focus nav`
    rt.state
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
    rt.switcher.select_address("local/a", &rt.state);

    // `focus terminal`
    rt.state
        .focus
        .set_view_focus(crate::app::focus::ViewFocus::Terminal);
    for ms in [400, 3_000, 11_000] {
        rt.observe_display_session();
        one_pass(&mut rt, t0 + std::time::Duration::from_millis(ms));
    }

    assert_eq!(
        rt.switcher.terminal_view_target().target,
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
// 3. Select a WINDOW row - confirm the attached client switches to that window.
// 4. Press Enter (or C-g → / C-g Tab) - focus the terminal (Focus::Terminal); the split
//    is unchanged (view border turns green) and keystrokes reach the real attached pane.
//    C-g ← / C-g Esc / C-g Tab return focus to the nav. Confirm no blank/flash.
// 5. Create / kill a window or session inside a pane - confirm the nav view
//    syncs (remote via control events, local within the poll interval) and the
//    PTY set follows (new session attaches, killed session's PTY is reaped).
// 6. C-g then `q` - clean quit, terminal restored.
// 7. NEVER attach the session that owns xmux (xmux refuses to run inside a mux,
//    so in normal use no session mirrors the UI).
// 8. Mouse: dragging never selects native terminal text (the app captures the
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
    use crate::ui::switcher::{Scan, Switcher};
    use crate::ui::tree::Group;
    let scan = Scan {
        groups: vec![Group {
            source: "jup".into(),
            err: None,
            sessions: vec![
                Session {
                    mux: String::new(),
                    source: "jup".into(),
                    name: "api".into(),
                    windows: 1,
                    attached: false,
                },
                Session {
                    mux: String::new(),
                    source: "jup".into(),
                    name: "db".into(),
                    windows: 1,
                    attached: false,
                },
            ],
        }],
    };
    let mut state = crate::state::State::from_scan(scan);
    let mut sw = Switcher::new(&mut state);
    let mut natural = 48u16;
    let mut hide = false;
    let ops = crate::ui::switcher::tests_support::noop_ops();
    let (op_tx, _op_rx) = tokio::sync::mpsc::unbounded_channel();
    let dir = std::env::temp_dir().join(format!("xmux-apply-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    // Switch addr → selection lands on db; returns (quit=false, width_changed=false).
    assert_eq!(
        dispatch_action(
            Action::Switch {
                address: "jup/db".into()
            },
            &mut sw,
            &mut state,
            &mut natural,
            &mut hide,
            &dir,
            (&ops, &op_tx),
        ),
        (false, false)
    );
    assert_eq!(sw.terminal_view_target().target, "db");
    // Focus(Terminal) leaves nav focus → terminal focus.
    assert!(state.focus.is_nav_focused());
    dispatch_action(
        Action::Focus(FocusTarget::Terminal),
        &mut sw,
        &mut state,
        &mut natural,
        &mut hide,
        &dir,
        (&ops, &op_tx),
    );
    assert_eq!(state.focus, Focus::Terminal);
    // Focus(Tree) returns to nav focus.
    dispatch_action(
        Action::Focus(FocusTarget::Nav),
        &mut sw,
        &mut state,
        &mut natural,
        &mut hide,
        &dir,
        (&ops, &op_tx),
    );
    assert_eq!(state.focus, Focus::Nav);
    // NavWidth adjusts the natural width and signals width_changed; Quit signals quit.
    assert_eq!(
        dispatch_action(
            Action::NavWidth(1),
            &mut sw,
            &mut state,
            &mut natural,
            &mut hide,
            &dir,
            (&ops, &op_tx),
        ),
        (false, true)
    );
    assert_eq!(natural, 49);
    assert_eq!(
        dispatch_action(
            Action::Quit,
            &mut sw,
            &mut state,
            &mut natural,
            &mut hide,
            &dir,
            (&ops, &op_tx),
        ),
        (true, false),
        "Quit signals quit"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn status_line_reports_focus_and_address() {
    use crate::session::Session;
    use crate::ui::switcher::{Scan, Switcher};
    use crate::ui::tree::Group;
    let scan = Scan {
        groups: vec![Group {
            source: "jup".into(),
            err: None,
            sessions: vec![Session {
                mux: String::new(),
                source: "jup".into(),
                name: "api".into(),
                windows: 1,
                attached: false,
            }],
        }],
    };
    let mut state = crate::state::State::from_scan(scan);
    let sw = Switcher::new(&mut state);
    // Tab-separated so a cwd containing spaces survives; cwd/tty are injected so
    // the assertion stays deterministic (no real env read).
    let pid = std::process::id();
    assert_eq!(
        status_line(&sw, "amber-otter", true, "/tmp/x", "-"),
        format!("name=amber-otter\tpid={pid}\tfocus=nav\ttarget=api\tcwd=/tmp/x\ttty=-")
    );
    assert_eq!(
        status_line(&sw, "amber-otter", false, "/tmp/x", "/dev/pts/3"),
        format!(
            "name=amber-otter\tpid={pid}\tfocus=terminal\ttarget=api\tcwd=/tmp/x\ttty=/dev/pts/3"
        )
    );
}

#[test]
fn ctl_switch_syncs_canonical_selection_immediately() {
    use crate::model::Action;
    use crate::session::Session;
    use crate::ui::switcher::{Scan, Switcher};
    use crate::ui::tree::Group;

    let scan = Scan {
        groups: vec![Group {
            source: "jup".into(),
            err: None,
            sessions: vec![
                Session {
                    mux: String::new(),
                    source: "jup".into(),
                    name: "api".into(),
                    windows: 1,
                    attached: false,
                },
                Session {
                    mux: String::new(),
                    source: "jup".into(),
                    name: "db".into(),
                    windows: 1,
                    attached: false,
                },
            ],
        }],
    };
    let mut state = crate::state::State::from_scan(scan);
    let mut sw = Switcher::new(&mut state);
    let mut natural = 48u16;
    let mut hide = false;
    let ops = crate::ui::switcher::tests_support::noop_ops();
    let (op_tx, _op_rx) = tokio::sync::mpsc::unbounded_channel();
    let dir = std::env::temp_dir().join(format!("xmux-ctl-switch-sync-{}", std::process::id()));

    sync_selection_from_switcher(&mut state, &sw);
    // api (name order) is the preselected top card, so switch to db to exercise a real
    // selection move.
    dispatch_action(
        Action::Switch {
            address: "jup/db".into(),
        },
        &mut sw,
        &mut state,
        &mut natural,
        &mut hide,
        &dir,
        (&ops, &op_tx),
    );

    // The switch moved the selection to db; the loop-top derive routes it through
    // apply(Select) - selection becomes jup/db and the attach is marked pending
    // (the deadline is armed by the next Tick, not here).
    assert!(sync_selection_from_switcher(&mut state, &sw));
    assert_eq!(state.selection.source, "jup");
    assert_eq!(state.selection.session, "db");
    assert!(state.attach_pending, "Select marks the attach pending");
    assert!(
        state.attach_deadline.is_none(),
        "Select arms no deadline - the trailing Tick does"
    );
}

#[test]
fn handle_stdin_bytes_quit_on_prefix_q_in_tree_focus() {
    use crate::ui::switcher::{Scan, Switcher};
    // prefix is Ctrl-G (0x07) in the default config; prefix then 'q' = quit.
    let scan = Scan { groups: vec![] };
    let mut state = crate::state::State::from_scan(scan); // nav focus
    let switcher = Switcher::new(&mut state);
    // The default fake env's prefix is "C-g" (0x07), matching this test's `\x07q`.
    let mut rt = test_rt(fake_env_with_sources(&["local"]));
    rt.hosts = crate::model::Hosts::default();
    rt.state = state;
    rt.switcher = switcher;
    let out = rt.handle_stdin_bytes(b"\x07q", &Selection::default());
    assert!(out.quit, "prefix+q in nav focus quits");
}

#[test]
fn arming_the_prefix_marks_the_frame_dirty_so_the_hint_bar_swaps() {
    use crate::ui::switcher::{Scan, Switcher};
    // The hint bar shows the prefix at rest and its keys once armed, so the bare prefix
    // read is a VISIBLE change even though it moves no selection and runs no action. If
    // it did not mark the frame dirty the cheatsheet would only appear on the next
    // unrelated redraw (a poll tick), which reads as the prefix doing nothing.
    let scan = Scan { groups: vec![] };
    let mut state = crate::state::State::from_scan(scan); // nav focus
    let switcher = Switcher::new(&mut state);
    let mut rt = test_rt(fake_env_with_sources(&["local"]));
    rt.hosts = crate::model::Hosts::default();
    rt.state = state;
    rt.switcher = switcher;
    assert!(!rt.prefix_active(), "starts unarmed");
    let out = rt.handle_stdin_bytes(b"\x07", &Selection::default());
    assert!(rt.prefix_active(), "the bare prefix arms");
    assert!(out.dirty, "arming redraws, so the cheatsheet shows at once");
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

/// Builds a `Runtime` with one reachable session on source `jup`, focused on the
/// TERMINAL view - the setup the focus-independent tree-action tests share.
fn rt_terminal_focus_with_session() -> Runtime {
    use crate::session::Session;
    use crate::ui::switcher::{Scan, Switcher};
    use crate::ui::tree::Group;
    let scan = Scan {
        groups: vec![Group {
            source: "jup".into(),
            err: None,
            sessions: vec![Session {
                mux: String::new(),
                source: "jup".into(),
                name: "api".into(),
                windows: 1,
                attached: false,
            }],
        }],
    };
    let mut state = crate::state::State::from_scan(scan); // launches in nav focus
    let switcher = Switcher::new(&mut state);
    let mut rt = test_rt(fake_env_with_sources(&["jup"]));
    rt.hosts = crate::model::Hosts::default();
    rt.state = state;
    rt.switcher = switcher;
    // Descend to the api session so it is the selection, then focus the terminal view.
    rt.handle_stdin_bytes(b"l", &Selection::default());
    rt.state.apply(crate::model::Action::Focus(
        crate::model::FocusTarget::Terminal,
    ));
    assert!(
        !rt.state.focus.is_nav_focused() && !rt.state.focus.is_modal(),
        "precondition: the terminal view holds focus (not tree, not modal)"
    );
    rt
}

// A re-scan starts the roster re-resolution off the loop, so the harness needs the
// runtime the real loop always runs inside.
#[tokio::test]
async fn prefix_r_in_terminal_focus_kicks_rescan() {
    // prefix r is focus-independent: from the terminal view it re-scans every host. The
    // re-scan clears each group's sessions and re-arms scanning - and kick_rescan must
    // run for it to fire, which the terminal arm now does.
    let mut rt = rt_terminal_focus_with_session();
    assert!(
        !rt.state.groups[0].sessions.is_empty(),
        "precondition: a session exists before the re-scan"
    );
    rt.handle_stdin_bytes(b"\x07r", &Selection::default());
    assert!(
        rt.state.groups[0].sessions.is_empty(),
        "prefix r in terminal focus cleared sessions for a re-scan"
    );
    assert!(
        rt.state.scanning.contains("jup"),
        "and re-armed scanning for the source"
    );
}

#[test]
fn repeated_prefix_bytes_keep_the_nav_steady_in_nav_focus() {
    use crate::ui::switcher::{Scan, Switcher};
    // In nav focus there is no pane to send a literal to, so arming is idempotent: a
    // held prefix's autorepeat neither toggles nor consumes ready, and the hint bar
    // and the auto-hide nav show stay put until a command key consumes it.
    let mut state = crate::state::State::from_scan(Scan { groups: vec![] });
    let switcher = Switcher::new(&mut state);
    let mut rt = test_rt(fake_env_with_sources(&["local"]));
    rt.state = state;
    rt.switcher = switcher;
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
fn a_resize_keeps_the_prefix_live_for_its_repeat_window() {
    // A prefix is consumed when its FUNCTION ends. A resize opens the bare-Ctrl-arrow
    // repeat window, so the function is still running: the bar and the auto-hide nav
    // show stay up across the whole burst instead of dropping on the first arrow.
    let mut rt = rt_terminal_focus_with_session();
    rt.handle_stdin_bytes(b"\x07", &Selection::default()); // prefix: ready
    assert!(rt.prefix_active(), "the bar shows while ready");
    rt.handle_stdin_bytes(b"\x1b[1;5C", &Selection::default()); // Ctrl+Right resizes
    assert!(
        rt.prefix_active(),
        "the resize opened the repeat window, so the function has not ended"
    );
    rt.handle_stdin_bytes(b"\x1b[1;5C", &Selection::default()); // bare Ctrl+Right
    assert!(rt.prefix_active(), "each repeat refreshes the window");
    // A key that is not a repeat key ends the window at once.
    rt.handle_stdin_bytes(b"z", &Selection::default());
    assert!(
        !rt.prefix_active(),
        "a non-repeat key closes the window, ending the function"
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
fn an_open_input_row_keeps_the_prefix_live_until_it_closes() {
    // A command that opens an input row owns the prefix until the row closes: the
    // hint bar hosts the input, so it must stay expanded while the user types.
    let mut rt = rt_terminal_focus_with_session();
    rt.handle_stdin_bytes(b"\x07", &Selection::default());
    rt.handle_stdin_bytes(b"n", &Selection::default()); // new session: opens the input row
    assert!(rt.state.is_inputting(), "the input row is open");
    assert!(
        rt.prefix_active(),
        "the function has not ended, so the prefix is still live"
    );
    // The loop top hands the modal its focus dimension before the next read; without
    // it the Esc would route to the pane instead of the row.
    let kind = rt.state.modal_kind();
    rt.state.focus.sync_modal(kind);
    rt.handle_stdin_bytes(b"\x1b", &Selection::default()); // Esc cancels
    assert!(!rt.state.is_inputting(), "Esc closes the row");
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
        !rt.state.focus.is_nav_focused(),
        "precondition: terminal focus"
    );
    // Terminal → nav: a held prefix chord ends when prefix Left hands focus over.
    rt.handle_stdin_bytes(b"\x07", &Selection::default()); // prefix down: +ready
    assert!(rt.prefix_active());
    rt.handle_stdin_bytes(b"\x1b[D", &Selection::default()); // prefix Left → nav
    assert!(rt.state.focus.is_nav_focused(), "focus moved to the nav");
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
        !rt.state.focus.is_nav_focused(),
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
    // without an explicit disarm the chord stays half-open: its cheatsheet keeps floating
    // over the window, and the next key it swallows is one meant for the pane.
    let ev = |cb: u16, pressed: bool| crate::display::mouse::MouseEvent {
        cb,
        col: 10,
        row: 3,
        pressed,
    };
    let nav_width = crate::ui::switcher::NAV_WIDTH;
    let (vw, vh) = terminal_view_size(80, 24, crate::ui::switcher::NavSize::visible(nav_width));
    let term_area = ratatui::layout::Rect::new(nav_width + 1, 0, vw, vh);
    // cb 0 = left press, cb 64 = wheel up, cb 0 with pressed=false = release.
    for (cb, pressed, what) in [
        (0u16, true, "a click"),
        (0, false, "a release"),
        (64, true, "a wheel"),
        (32, true, "a drag"), // motion WITH a button held
    ] {
        let mut state = crate::state::State::from_scan(Scan { groups: vec![] });
        let switcher = Switcher::new(&mut state);
        let mut rt = test_rt(fake_env_with_sources(&["local"]));
        rt.state = state;
        rt.switcher = switcher;
        rt.mouse_state.nav_armed = true;
        let dirty = rt.handle_mouse_event(
            &ev(cb, pressed),
            &Selection::default(),
            &mut false,
            &mut false,
            term_area,
        );
        assert!(!rt.prefix_active(), "{what} disarms the prefix");
        assert!(dirty, "{what} redraws, so the cheatsheet goes at once");
    }
    // Bare hover is the pointer sitting there, not an action: it must not break a chord
    // the user is still typing. cb 35 = motion bit with no button held.
    let mut state = crate::state::State::from_scan(Scan { groups: vec![] });
    let switcher = Switcher::new(&mut state);
    let mut rt = test_rt(fake_env_with_sources(&["local"]));
    rt.state = state;
    rt.switcher = switcher;
    rt.mouse_state.nav_armed = true;
    rt.handle_mouse_event(
        &ev(35, true),
        &Selection::default(),
        &mut false,
        &mut false,
        term_area,
    );
    assert!(rt.prefix_active(), "a hover leaves the chord alone");
}

#[test]
fn handle_mouse_event_view_border_grab_sets_dragging() {
    use crate::ui::switcher::{Scan, Switcher};
    // A left-press exactly on the view border column sets dragging_view_border, as the
    // inline gate did (is_left_press && nav_width > 0 && col0 == nav_width).
    let scan = Scan { groups: vec![] };
    let mut state = crate::state::State::from_scan(scan);
    let switcher = Switcher::new(&mut state);
    let sel = Selection::default();
    let nav_width = crate::ui::switcher::NAV_WIDTH;
    // 0-based col0 = ev.col - 1 must equal nav_width to grab the view border rule.
    let view_border_col = nav_width + 1; // 1-based SGR column of the view border
                                         // cb=0 → left button, press, no wheel/motion → is_left_press is true.
    let ev = crate::display::mouse::MouseEvent {
        cb: 0,
        col: view_border_col,
        row: 3,
        pressed: true,
    };
    // Landscape enough to keep the side column, whose border this test grabs.
    let (vw, vh) = terminal_view_size(200, 24, crate::ui::switcher::NavSize::visible(nav_width));
    let term_area = ratatui::layout::Rect::new(nav_width + 1, 0, vw, vh);
    let mut focus_toggle = false;
    let mut wheel = false;
    let mut rt = test_rt(fake_env_with_sources(&["local"]));
    rt.state = state;
    rt.switcher = switcher;
    // The handler cuts its own regions from the runtime's size, so the runtime has to be
    // landscape too or the border it looks for is a horizontal rule under the band.
    rt.cols = 200;
    rt.body_rows = 23;
    rt.handle_mouse_event(&ev, &sel, &mut focus_toggle, &mut wheel, term_area);
    assert!(
        rt.mouse_state.dragging_view_border,
        "left-press on the view border column grabs it"
    );
}

#[test]
fn handle_mouse_event_top_layout_border_drag_resizes_height() {
    use crate::ui::switcher::{Scan, Switcher};
    // In the portrait Top layout the view border is a HORIZONTAL rule; a left-press on that
    // row grabs it and a drag sets the nav HEIGHT (not width). 40x60 → Top; the nav band
    // carries its own hint bar, so its auto height is ~40% of the whole 60-row area = 24,
    // putting the border at row 24 (0-based) = SGR row 25.
    let mut state = crate::state::State::from_scan(Scan { groups: vec![] });
    let switcher = Switcher::new(&mut state);
    let sel = Selection::default();
    let mut rt = test_rt(fake_env_with_sources(&["local"]));
    rt.state = state;
    rt.switcher = switcher;
    rt.cols = 40;
    rt.body_rows = 59;
    rt.nav_height = 0; // auto

    let press = crate::display::mouse::MouseEvent {
        cb: 0,
        col: 5,
        row: 25,
        pressed: true,
    };
    let (mut ft, mut wheel) = (false, false);
    let area = ratatui::layout::Rect::default();
    rt.handle_mouse_event(&press, &sel, &mut ft, &mut wheel, area);
    assert!(
        rt.mouse_state.dragging_view_border,
        "left-press on the horizontal Top border grabs it"
    );

    // Drag DOWN to SGR row 30 (motion bit 0x20, left button held) → nav height = 30-1 = 29.
    let drag = crate::display::mouse::MouseEvent {
        cb: 0x20,
        col: 5,
        row: 30,
        pressed: true,
    };
    rt.handle_mouse_event(&drag, &sel, &mut ft, &mut wheel, area);
    assert_eq!(
        rt.nav_height, 29,
        "dragging the horizontal border sets the nav HEIGHT to the dragged row"
    );
}

#[test]
fn resize_keys_adjust_height_in_top_layout() {
    use crate::ui::switcher::{Scan, Switcher, ViewLayout, NAV_WIDTH};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    // In the portrait Top layout the nav-resize keys (prefix h/l · Ctrl+←/→) adjust the
    // HEIGHT, not the width - seeded from the auto height the first time.
    let mut state = crate::state::State::from_scan(Scan { groups: vec![] });
    let switcher = Switcher::new(&mut state);
    let mut rt = test_rt(fake_env_with_sources(&["local"]));
    rt.state = state;
    rt.switcher = switcher;
    rt.cols = 40;
    rt.body_rows = 59;
    rt.nav_height = 0; // auto
                       // Render once into a portrait backend so the switcher caches layout = Top.
    let mut term = Terminal::new(TestBackend::new(40, 60)).unwrap();
    {
        let sw = &mut rt.switcher;
        let st = &rt.state;
        term.draw(|f| {
            sw.render(
                f,
                None,
                false,
                crate::ui::switcher::NavSize::visible(NAV_WIDTH),
                st,
            )
        })
        .unwrap();
    }
    assert_eq!(rt.switcher.layout(), ViewLayout::Top, "portrait → Top");

    let auto = crate::ui::switcher::default_nav_height(59);
    // Vertical axis (Ctrl+↓ = grow) resizes HEIGHT in Top; horizontal (Ctrl+→) is a no-op here.
    assert!(
        !rt.resize_axis(true, 1),
        "horizontal resize is a no-op in Top"
    );
    assert!(rt.resize_axis(false, 1), "grow changes the height");
    assert_eq!(
        rt.nav_height,
        auto + 1,
        "a resize key grows the Top nav height from the auto seed"
    );
    assert!(rt.resize_axis(false, -1), "shrink changes the height");
    assert_eq!(rt.nav_height, auto, "and shrinks it back");
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
    let mut rt = test_rt(fake_env_with_sources(&[]));
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
            rt.state.displayed.source.as_str(),
            rt.state.displayed.session.as_str()
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
async fn an_attach_for_the_selected_host_takes_the_terminal_view() {
    // The floor under the test above: the attach the selection IS displayed through
    // confirms as the view, so the rule that keeps a warm attach out cannot keep the
    // real one out too.
    let mut rt = a_settled_psmux_runtime();
    rt.hosts
        .get_mut("local")
        .unwrap()
        .display
        .set_shows("local", "b");
    the_reattach_lands_for(&mut rt, 11, "b");
    assert_eq!(
        rt.state.displayed.session, "b",
        "the selected host's landed attach is the view"
    );
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
    let line = super::handlers::shell_line(&argv);
    assert_eq!(line, r"tmux list-sessions -F 'a\tb' 'two words'");
}

#[test]
fn a_sources_reach_names_its_mux_and_the_machine_it_is_asked_over() {
    // What the unreachable screen states about a source, resolved from that source's own
    // config: the binary asked for, how the machine is addressed, and the listing command
    // itself.
    let s = Source {
        alias: "prod".into(),
        binary: "tmux".into(),
        kind: crate::transport::MachineKind::Ssh {
            id: String::new(),
            alias: "prod".into(),
            control_path: "/tmp/cm-prod.sock".into(),
            os: "linux".into(),
        },
        runner: None,
    };
    let reach = super::handlers::source_reach(&s);
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
    // The live config watch is driven by mtime: the first sight is a baseline (the
    // startup apply already ran), and only a real change reloads the [ui] section. A
    // malformed edit keeps the last good config rather than blanking the UI.
    let dir = std::env::temp_dir().join(format!("xmux-poll-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.toml");
    std::fs::write(&path, "[ui]\ntheme = \"auto-dark\"\n").unwrap();
    let mut last = None;
    // First sight = baseline; the same file again = no change.
    assert!(super::handlers::poll_ui_config(&mut last, &path).is_none());
    assert!(super::handlers::poll_ui_config(&mut last, &path).is_none());
    // A real edit reloads the [ui] section.
    std::thread::sleep(std::time::Duration::from_millis(30));
    std::fs::write(&path, "[ui]\ntheme = \"auto-light\"\n").unwrap();
    let ui = super::handlers::poll_ui_config(&mut last, &path).expect("a real change reloads");
    assert_eq!(ui.theme, "auto-light");
    // A malformed edit keeps the last good config (None) but is still recorded.
    std::thread::sleep(std::time::Duration::from_millis(30));
    std::fs::write(&path, "not [[ valid toml").unwrap();
    assert!(super::handlers::poll_ui_config(&mut last, &path).is_none());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn config_poll_ignores_a_missing_file() {
    // A deletion (or an editor's atomic-rename mid-save) is not a reload: record the
    // absence and wait. Only a file that comes back AND changes again reloads.
    let dir = std::env::temp_dir().join(format!("xmux-poll-missing-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.toml");
    std::fs::write(&path, "[ui]\ntheme = \"auto-dark\"\n").unwrap();
    let mut last = None;
    assert!(super::handlers::poll_ui_config(&mut last, &path).is_none()); // baseline
    std::fs::remove_file(&path).unwrap();
    assert!(super::handlers::poll_ui_config(&mut last, &path).is_none()); // gone: no reload
    std::fs::remove_dir_all(&dir).ok();
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
