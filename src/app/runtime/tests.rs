use super::*;
use crate::config::Config;
use crate::source::Source;
use std::collections::HashMap;

fn fake_source(alias: &str) -> Source {
    Source {
        alias: alias.into(),
        binary: "cmd.exe".into(),
        kind: crate::machine::MachineKind::Local { socket: None },
        runner: None,
    }
}

fn fake_env_with_sources(aliases: &[&str]) -> Env {
    let srcs: Vec<Source> = aliases.iter().map(|a| fake_source(a)).collect();
    let by_alias: HashMap<String, Source> =
        srcs.iter().map(|s| (s.alias.clone(), s.clone())).collect();
    let ssh_aliases: Vec<String> = aliases
        .iter()
        .filter(|a| **a != crate::session::LOCAL_SOURCE)
        .map(|a| a.to_string())
        .collect();
    Env {
        cfg: Config::default(),
        cfg_warnings: Vec::new(),
        srcs,
        by_alias,
        local_bin: "cmd.exe".into(),
        ui_prefix: "C-g".into(),
        xmux_dir: std::path::PathBuf::from("."),
        ssh_aliases,
        local_socket: None,
    }
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
    // by mux behavior, read off the Host — never the transport's remote flag.
    let mut hosts = crate::model::Hosts::default();
    hosts.insert(crate::model::Host::new(
        crate::machine::ssh("jup".into(), String::new(), "linux".into()),
        crate::mux::for_binary("tmux"), // Shared
    ));
    hosts.insert(crate::model::Host::new(
        crate::machine::local(None),     // host id == "local"
        crate::mux::for_binary("psmux"), // PerSession
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
        crate::machine::local(None),
        crate::mux::for_binary("tmux"),
    ));

    apply_scan_result(
        &mut hosts,
        "local",
        Some(crate::mux::for_kind("psmux", "tmux")),
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
        crate::machine::local(None),
        crate::mux::for_binary("psmux"),
    ));

    apply_scan_result(
        &mut hosts,
        "local",
        Some(crate::mux::for_kind("tmux", "psmux")),
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
async fn connect_all_sources_connects_remote_hosts() {
    // Control-event (tmux) hosts get a control client at startup; poll hosts
    // enumerate off the loop (no control client). The gate is the host's
    // event_source, read off the Host — not the transport remote flag. The cmd.exe
    // binary is a spawnable stand-in for ssh that EOFs at once.
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<HostEvent>();
    let mut mgr = HostManager::new(tx);
    let mut hosts = crate::model::Hosts::default();
    let mut host = crate::model::Host::new(
        crate::machine::ssh("jupiter06".into(), String::new(), "linux".into()),
        crate::mux::for_binary("tmux"), // Control event source
    );
    host.detected = true;
    hosts.insert(host);
    let mut detecting = HashSet::new();
    connect_all_sources(
        &mut mgr,
        &hosts,
        &mut detecting,
        80,
        24,
        crate::ui::switcher::TREE_WIDTH,
    );
    assert!(
        mgr.get("jupiter06").is_some(),
        "control host got a control client from the registry alone"
    );
    mgr.teardown_all();
}

#[tokio::test]
async fn scan_or_dispatch_host_detects_from_hosts_without_env() {
    // An UNDETECTED host is routed to detection using ONLY the Hosts registry — no
    // Env/by_alias. The detection branch marks the source in `detecting`; the probe
    // clones the host's transport + mux rather than re-deriving from a Source.
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<HostEvent>();
    let mut mgr = HostManager::new(tx);
    let mut hosts = crate::model::Hosts::default();
    hosts.insert(crate::model::Host::new(
        crate::machine::local(None),
        crate::mux::for_kind("psmux", "psmux-no-such-binary"),
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
    assert_eq!(terminal_view_size(80, 23, 0, 0), (80, 24));
    // Shown tree: cols - tree_width - 1 (view border), height = body_rows (bottom row
    // reserved for the full-width hint_bar).
    assert_eq!(terminal_view_size(80, 23, 48, 0), (31, 23));
    // Degenerate widths clamp to at least 1.
    assert_eq!(terminal_view_size(0, 0, 0, 0), (1, 1));
}

#[test]
fn terminal_view_size_reserves_full_width_hint_row_when_tree_shown() {
    use crate::ui::switcher::TREE_WIDTH;
    // Tree hidden (sentinel 0): no hint_bar, terminal view spans the full height.
    let (_, full) = terminal_view_size(120, 39, 0, 0);
    assert_eq!(full, 40);
    // Tree shown: the full-width hint_bar owns the bottom row, so the terminal
    // view is exactly one row shorter.
    let (_, shown) = terminal_view_size(120, 39, TREE_WIDTH, 0);
    assert_eq!(
        shown, 39,
        "shown tree reserves one row for the full-width hint_bar"
    );
}

#[test]
fn reconciled_tree_width_hides_only_when_focused_and_enabled() {
    // Tree focused (terminal_focused = false): always the natural width.
    assert_eq!(reconciled_tree_width(false, true, 48), 48);
    assert_eq!(reconciled_tree_width(false, false, 48), 48);
    // Terminal view focused + setting on: hidden (0).
    assert_eq!(reconciled_tree_width(true, true, 48), 0);
    // Terminal view focused + setting off: stays shown at natural width.
    assert_eq!(reconciled_tree_width(true, false, 48), 48);
}

#[test]
fn apply_width_delta_is_write_free_and_reports_change() {
    let mut w = 48u16;
    assert!(apply_width_delta(1, &mut w), "a real delta reports changed");
    assert_eq!(w, 49);
    assert!(
        !apply_width_delta(0, &mut w),
        "a zero delta reports unchanged"
    );
    assert_eq!(w, 49);
    // Clamp at the max: a delta that cannot move the width reports unchanged.
    let mut hi = TREE_WIDTH_MAX;
    assert!(
        !apply_width_delta(10, &mut hi),
        "a clamped no-op reports unchanged"
    );
    assert_eq!(hi, TREE_WIDTH_MAX);
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
fn tree_width_adjust_clamps() {
    assert_eq!(adjust_tree_width(48, 1), 49);
    assert_eq!(adjust_tree_width(48, -1), 47);
    assert_eq!(adjust_tree_width(20, -1), 20, "clamped at min");
    assert_eq!(adjust_tree_width(100, 1), 100, "clamped at max");
}

#[test]
fn terminal_view_size_subtracts_tree_and_view_border() {
    use crate::ui::switcher::TREE_WIDTH;
    let (vc, vr) = terminal_view_size(143, 39, TREE_WIDTH, 0);
    assert_eq!(
        vc,
        143 - (TREE_WIDTH + 1),
        "cols minus tree minus view border"
    );
    // The full-width hint_bar owns the bottom row, so the terminal view drops one row
    // below the full terminal height (height == body_rows).
    assert_eq!(vr, 39, "height drops one row for the full-width hint_bar");
}

#[test]
fn terminal_view_size_clamps_to_at_least_one() {
    use crate::ui::switcher::TREE_WIDTH;
    // A 10-col terminal can't fit the 48-col tree beside it, so the layout goes Top and the
    // terminal keeps full width; a zero-row body still clamps the height up to 1. The
    // invariant this guards is that neither dimension is ever 0 (degenerate PTY size).
    let (vc, vr) = terminal_view_size(10, 0, TREE_WIDTH, 0);
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
    assert!(out.contains("empty"), "an empty host reads (empty):\n{out}");
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
    // the tree (no flash) but clears `connected`; a refresh + a failed reconnect (no
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
    // The reconnect fails with "no sessions": it must resolve scanning → (empty).
    note_host_exited(
        &mut switcher,
        &mut state,
        &mut connected,
        "jupiter06",
        Some("no sessions".into()),
    );
    let out = dump_screen(&mut switcher, None, 80, 24, &state);
    assert!(
        out.contains("empty"),
        "failed reconnect resolves to (empty):\n{out}"
    );
    assert!(
        !out.contains("scanning"),
        "scanning must clear, not load forever:\n{out}"
    );
}

#[test]
fn active_window_probe_moves_tree_selection() {
    // A resolved active-window probe (HostEvent::Focus) sets the cached active-window
    // marker; the loop-top select_active_window then moves the selection. Selection starts
    // on window 1's row; Focus to window 0 sets the marker, and select_active_window
    // (simulating the loop-top call) lands the selection on window 0.
    use crate::session::{Pane, Session, WindowPanes};
    use crate::ui::switcher::{Scan, Switcher};
    use crate::ui::tree::Group;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let mut panes = std::collections::HashMap::new();
    panes.insert(
        "jup/api".to_string(),
        vec![
            WindowPanes {
                index: 0,
                name: "w0".into(),
                active: true,
                panes: vec![Pane {
                    index: 0,
                    active: true,
                    command: "bash".into(),
                }],
            },
            WindowPanes {
                index: 1,
                name: "w1".into(),
                active: false,
                panes: vec![Pane {
                    index: 0,
                    active: true,
                    command: "bash".into(),
                }],
            },
        ],
    );
    let scan = Scan {
        groups: vec![Group {
            source: "jup".into(),
            err: None,
            sessions: vec![Session {
                source: "jup".into(),
                name: "api".into(),
                windows: 2,
                attached: false,
                last_attached: 100,
            }],
        }],
        panes,
    };
    let mut state = crate::state::State::from_scan(scan);
    let mut switcher = Switcher::new(&mut state);
    // host row -> (→ descend) api session -> (→ descend) window 0 -> (↓ sibling) window 1.
    switcher.handle_key(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    );
    switcher.handle_key(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    );
    switcher.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &mut state);
    assert_eq!(
        switcher.terminal_view_target().target,
        "api:1",
        "selection on window 1"
    );

    // Focus sets the cached active-window marker (window 0).
    let mut rt = test_rt(fake_env_with_sources(&[]));
    rt.hosts = crate::model::Hosts::default();
    rt.state = state;
    rt.switcher = switcher;
    let _ = rt.handle_host_event(HostEvent::Focus {
        host: "jup".into(),
        session: "api".into(),
        window: 0,
    });
    // The loop-top follow (simulated here) consumes the marker and moves the selection.
    rt.switcher.select_active_window(&mut rt.state);
    assert_eq!(
        rt.switcher.terminal_view_target().target,
        "api:0",
        "loop-top follow moved selection to active window 0"
    );
}

#[test]
fn focus_event_updates_marker_without_moving_cursor() {
    // handle_host_event(Focus) updates the active-window marker but never moves
    // the selection — selection follow is a loop-top concern. The selection is left wherever
    // the caller placed it (here, window 1) regardless of the Focus payload.
    use crate::session::{Pane, Session, WindowPanes};
    use crate::ui::switcher::{Scan, Switcher};
    use crate::ui::tree::Group;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let mut panes = std::collections::HashMap::new();
    panes.insert(
        "jup/api".to_string(),
        vec![
            WindowPanes {
                index: 0,
                name: "w0".into(),
                active: true,
                panes: vec![Pane {
                    index: 0,
                    active: true,
                    command: "bash".into(),
                }],
            },
            WindowPanes {
                index: 1,
                name: "w1".into(),
                active: false,
                panes: vec![Pane {
                    index: 0,
                    active: true,
                    command: "bash".into(),
                }],
            },
        ],
    );
    let scan = Scan {
        groups: vec![Group {
            source: "jup".into(),
            err: None,
            sessions: vec![Session {
                source: "jup".into(),
                name: "api".into(),
                windows: 2,
                attached: false,
                last_attached: 100,
            }],
        }],
        panes,
    };
    let mut state = crate::state::State::from_scan(scan);
    let mut switcher = Switcher::new(&mut state);
    switcher.handle_key(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    ); // → api (session): launch preselects the host row
    switcher.handle_key(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    ); // → window 0
    switcher.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &mut state); // ↓ → window 1
    assert_eq!(switcher.terminal_view_target().target, "api:1");

    let mut rt = test_rt(fake_env_with_sources(&[]));
    rt.hosts = crate::model::Hosts::default();
    rt.state = state;
    rt.switcher = switcher;
    let _ = rt.handle_host_event(HostEvent::Focus {
        host: "jup".into(),
        session: "api".into(),
        window: 0,
    });
    assert_eq!(
        rt.switcher.terminal_view_target().target,
        "api:1",
        "handler alone must not move the selection"
    );
}

#[test]
fn prefix_s_toggles_state() {
    use crate::app::focus::Focus;
    let mut focus = Focus::default();
    assert!(focus.is_tree_focused());
    focus.toggle();
    assert_eq!(focus, Focus::Terminal);
    focus.toggle();
    assert!(focus.is_tree_focused());
}

// Suppress unused warnings for the test-only env builder kept for future loop tests.
#[test]
fn fake_env_builder_constructs() {
    let env = fake_env_with_sources(&["local", "jupiter06"]);
    assert_eq!(env.srcs.len(), 2);
}

#[test]
fn apply_inventory_effect_folds_sessions_into_host_inventory() {
    // C1: the control reader carries its parsed sessions on the HostEvent; the
    // loop folds them into the single owner (`model::Host.inventory`) and applies
    // them to the tree. There is no shared `Arc<Mutex<HostInventory>>` to read.
    use crate::ui::switcher::{Scan, Switcher};
    use crate::ui::tree::Group;

    let scan = Scan {
        groups: vec![Group {
            source: "jup".into(),
            err: None,
            sessions: vec![],
        }],
        panes: Default::default(),
    };
    let mut state = crate::state::State::from_scan(scan);
    let switcher = Switcher::new(&mut state);
    let mut hosts = crate::model::Hosts::default();
    hosts.insert(crate::model::Host::new(
        crate::machine::ssh("jup".into(), String::new(), "linux".into()),
        crate::mux::for_binary("tmux"),
    ));
    let mut rt = test_rt(fake_env_with_sources(&[]));
    rt.mgr.insert_fake("jup"); // a control client so request_session_panes has a sink
    rt.hosts = hosts;
    rt.state = state;
    rt.switcher = switcher;

    let sessions = vec![crate::session::Session {
        source: "jup".into(),
        name: "api".into(),
        ..Default::default()
    }];
    let rearm = rt.run_event_effect(crate::model::EventEffect::ApplyInventory {
        host: "jup".into(),
        sessions: sessions.clone(),
    });
    assert!(!rearm, "ApplyInventory does not rearm detach recovery");
    // The single owner now holds the carried sessions — folded by the loop.
    let owned = &rt
        .hosts
        .get("jup")
        .expect("host present")
        .inventory
        .sessions;
    assert_eq!(owned.len(), 1, "sessions folded into model::Host.inventory");
    assert_eq!(owned[0].name, "api");
    // And the tree group reflects the same sessions.
    let group = rt
        .state
        .groups
        .iter()
        .find(|g| g.source == "jup")
        .expect("jup group");
    assert_eq!(group.sessions.len(), 1, "tree applied the carried sessions");
    assert_eq!(group.sessions[0].name, "api");
}

#[test]
fn r_rescan_reloads_control_host_panes() {
    // Regression (S4-M5 follow-up): the client-initiated `r` re-scan must not
    // strand a control host's window/pane subtrees on "loading…". `request_rescan`
    // clears every session's panes from `state.panes`, so the loop-local
    // `panes_requested` dedup must be cleared in lockstep — otherwise the re-list's
    // `ApplyInventory` skips `list-panes` for every already-requested address and
    // the panes never reload. `kick_rescan` (the single consumer of the rescan
    // kick) owns that clear.
    use crate::session::{Pane, Session, WindowPanes};
    use crate::ui::switcher::{Scan, Switcher};
    use crate::ui::tree::Group;

    let mut panes = std::collections::HashMap::new();
    panes.insert(
        "jup/api".to_string(),
        vec![WindowPanes {
            index: 0,
            name: "w0".into(),
            active: true,
            panes: vec![Pane {
                index: 0,
                active: true,
                command: "bash".into(),
            }],
        }],
    );
    let scan = Scan {
        groups: vec![Group {
            source: "jup".into(),
            err: None,
            sessions: vec![Session {
                source: "jup".into(),
                name: "api".into(),
                windows: 1,
                attached: false,
                last_attached: 100,
            }],
        }],
        panes,
    };
    let mut state = crate::state::State::from_scan(scan);
    let switcher = Switcher::new(&mut state);

    // A detected CONTROL (tmux) host with a live control client sink.
    let mut hosts = crate::model::Hosts::default();
    let mut host = crate::model::Host::new(
        crate::machine::ssh("jup".into(), String::new(), "linux".into()),
        crate::mux::for_binary("tmux"),
    );
    host.detected = true;
    hosts.insert(host);

    let mut rt = test_rt(fake_env_with_sources(&[]));
    rt.mgr.insert_fake("jup");
    rt.hosts = hosts;
    rt.state = state;
    rt.switcher = switcher;

    // Panes were already loaded + requested during the initial scan.
    rt.panes_requested.insert("jup/api".into());
    assert!(
        rt.state.panes.contains_key("jup/api"),
        "precondition: panes are loaded before the re-scan"
    );

    // The `r` re-scan resets the tree to its scanning skeleton and clears panes.
    rt.switcher.request_rescan(&mut rt.state);
    assert!(
        !rt.state.panes.contains_key("jup/api"),
        "request_rescan cleared the loaded panes"
    );

    // The loop consumes the kick and re-lists each host.
    kick_rescan(
        &mut rt.switcher,
        &rt.hosts,
        &mut rt.detecting,
        &mut rt.mgr,
        &mut rt.panes_requested,
        80,
        24,
    );
    // The dedup must no longer block re-requesting this session's panes; otherwise
    // the re-list below silently skips `list-panes` and the subtree stays "loading…".
    assert!(
        !rt.panes_requested.contains("jup/api"),
        "kick_rescan must clear the pane-request dedup so control-host panes reload"
    );

    // The re-list reply folds in via ApplyInventory, which re-requests each
    // session's panes — re-inserting the (now-cleared) address, i.e. issuing list-panes.
    let sessions = vec![Session {
        source: "jup".into(),
        name: "api".into(),
        windows: 1,
        attached: false,
        last_attached: 100,
    }];
    rt.run_event_effect(crate::model::EventEffect::ApplyInventory {
        host: "jup".into(),
        sessions,
    });
    assert!(
        rt.panes_requested.contains("jup/api"),
        "ApplyInventory re-requested the session's panes after the re-scan"
    );
}

#[test]
fn current_grid_returns_none_for_empty_displayed() {
    // An empty `displayed` (source "") misses `hosts.get`, so no driver is
    // built and no grid is produced — the blank-terminal case on first launch.
    let mut hosts = crate::model::Hosts::default();
    let mut registry = AttachRegistry::new();
    let (ptx, _prx) = tokio::sync::mpsc::unbounded_channel();
    let worker = crate::display::DisplayWorker::new(ptx);
    let (etx, _erx) = tokio::sync::mpsc::unbounded_channel::<crate::host::HostEvent>();
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
            tree_width: crate::ui::switcher::TREE_WIDTH,
            tree_height: 0,
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
        crate::machine::ssh("jup".into(), String::new(), "linux".into()),
        crate::mux::for_binary("tmux"),
    ));
    let (ptx, _prx) = tokio::sync::mpsc::unbounded_channel();
    let worker = crate::display::DisplayWorker::new(ptx);
    let mut registry = AttachRegistry::new();
    let mut attach_seq = 0u64;
    // No control client registered ⇒ select_attach falls back to the lowered-switch
    // path (this test exercises attach/in-flight latching, not the switch transport).
    let (etx, _erx) = tokio::sync::mpsc::unbounded_channel::<crate::host::HostEvent>();
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
            tree_width: crate::ui::switcher::TREE_WIDTH,
            tree_height: 0,
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
            tree_width: crate::ui::switcher::TREE_WIDTH,
            tree_height: 0,
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
        crate::machine::local(None),
        crate::mux::for_binary("psmux"),
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
            tree_width: crate::ui::switcher::TREE_WIDTH,
            tree_height: 0,
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
            tree_width: crate::ui::switcher::TREE_WIDTH,
            tree_height: 0,
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
        crate::machine::local(None),
        crate::mux::for_binary("psmux"),
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
            tree_width: crate::ui::switcher::TREE_WIDTH,
            tree_height: 0,
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
    let gate = |selection: &Selection, displayed: &Selection, key_live, in_flight| {
        let s = crate::state::State {
            selection: selection.clone(),
            displayed: displayed.clone(),
            ..crate::state::State::default()
        };
        s.should_attach(key_live, in_flight)
    };
    // Settled: displayed == selection, PTY live, nothing in flight → no attach.
    assert!(!gate(&a, &a, true, false));
    // Selection moved off the displayed session → attach.
    assert!(gate(&b, &a, true, false));
    // An attach for the key is already in flight → never re-fire (no storm).
    assert!(!gate(&b, &a, false, true));
    // PTY gone (exited / reaped) while displayed == selection → re-attach to recover.
    assert!(gate(&a, &a, false, false));
}

#[tokio::test(flavor = "current_thread")]
async fn psmux_select_attach_supersedes_in_flight_attach() {
    let mut hosts = crate::model::Hosts::default();
    hosts.insert(crate::model::Host::new(
        crate::machine::local(None),
        crate::mux::for_binary("psmux"),
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
            tree_width: crate::ui::switcher::TREE_WIDTH,
            tree_height: 0,
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
    let hosts = crate::model::Hosts::build(
        &env.cfg,
        &env.ssh_aliases,
        "windows",
        &env.xmux_dir,
        env.local_socket.clone(),
    );
    let mut state = crate::state::State::from_sources(hosts.ids().to_vec());
    let switcher = crate::ui::switcher::Switcher::from_sources(&mut state);
    let ops = env.ops();
    let (op_tx, _op_rx) = tokio::sync::mpsc::unbounded_channel();
    let (border_tx, _border_rx) = tokio::sync::mpsc::unbounded_channel();
    let prefix = crate::display::term::parse_prefix(Some(&env.ui_prefix));
    Runtime {
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
        tree_width: crate::ui::switcher::TREE_WIDTH,
        tree_width_natural: crate::ui::switcher::TREE_WIDTH,
        tree_height: 0,
        applied_tree_height: u16::MAX,
        auto_hide_tree: false,
        mouse_state: MouseState::default(),
        term_input: crate::display::input::TermInput::new(prefix),
        tree_decoder: crate::display::decode::KeyDecoder::new(),
        prefix,
        connected: HashSet::new(),
        panes_requested: HashSet::new(),
        detecting: HashSet::new(),
        draw_observer: DrawObserver::default(),
        spinner_start: std::time::Instant::now(),
        dirty: true,
        last_draw: std::time::Instant::now(),
        width_dirty: false,
        width_flush_at: None,
        border_tx,
        border_cache: Default::default(),
        border_inflight: Default::default(),
        border_applied: None,
    }
}

fn detach_test_hosts(alias: &str) -> crate::model::Hosts {
    let mut hosts = crate::model::Hosts::default();
    hosts.insert(crate::model::Host::new(
        crate::machine::ssh(alias.to_string(), String::new(), "linux".into()),
        crate::mux::for_binary("tmux"),
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

// =========================================================================
// HUMAN VISUAL-GATE CHECKLIST (run in a REAL terminal — never headless):
// 1. Launch `xmux`. Confirm it enters the alternate screen cleanly and starts in
//    Focus::Tree: the Host·Session·Window·Pane tree on the left, the live REAL
//    terminal of the selection's session on the right (a true attached mux client).
// 2. Move the selection between sessions. Confirm the terminal view shows each session's
//    real attached terminal instantly (it is pre-attached + kept alive), with a
//    spinner while a session's attach is still establishing.
// 3. Select a WINDOW row — confirm the attached client switches to that window.
// 4. Press Enter (or C-g → / C-g Tab) — focus the terminal (Focus::Terminal); the split
//    is unchanged (view border turns green) and keystrokes reach the real attached pane.
//    C-g ← / C-g Esc / C-g Tab return focus to the tree. Confirm no blank/flash.
// 5. Create / kill a window or session inside a pane — confirm the tree view
//    syncs (remote via control events, local within the poll interval) and the
//    PTY set follows (new session attaches, killed session's PTY is reaped).
// 6. C-g then `q` — clean quit, terminal restored.
// 7. NEVER attach the session that owns xmux (xmux refuses to run inside a mux,
//    so in normal use no session mirrors the UI).
// 8. Mouse: dragging never selects native terminal text (the app captures the
//    mouse). A LEFT-button press in the UNFOCUSED view switches focus to it (focus
//    only — the click is not delivered); right-click never moves focus (it opens the
//    tree context menu). Once the terminal view is focused, clicks/scroll/
//    right-click reach the mux (status-bar click, pane select, scroll, context menu).
//    Mux mouse forwarding requires the mux to have `mouse on` (`set -g mouse on`);
//    xmux only forwards. (Windows: capture needs ENABLE_VIRTUAL_TERMINAL_INPUT +
//    the SGR DECSET that crossterm's WinAPI path omits — see display::term.)
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
                    source: "jup".into(),
                    name: "api".into(),
                    windows: 1,
                    attached: false,
                    last_attached: 200,
                },
                Session {
                    source: "jup".into(),
                    name: "db".into(),
                    windows: 1,
                    attached: false,
                    last_attached: 100,
                },
            ],
        }],
        panes: Default::default(),
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
    // Focus(Terminal) leaves tree focus → terminal focus.
    assert!(state.focus.is_tree_focused());
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
    // Focus(Tree) returns to tree focus.
    dispatch_action(
        Action::Focus(FocusTarget::Tree),
        &mut sw,
        &mut state,
        &mut natural,
        &mut hide,
        &dir,
        (&ops, &op_tx),
    );
    assert_eq!(state.focus, Focus::Tree);
    // TreeWidth adjusts the natural width and signals width_changed; Quit signals quit.
    assert_eq!(
        dispatch_action(
            Action::TreeWidth(1),
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
                source: "jup".into(),
                name: "api".into(),
                windows: 1,
                attached: false,
                last_attached: 1,
            }],
        }],
        panes: Default::default(),
    };
    let mut state = crate::state::State::from_scan(scan);
    let sw = Switcher::new(&mut state);
    // Tab-separated so a cwd containing spaces survives; cwd/tty are injected so
    // the assertion stays deterministic (no real env read).
    assert_eq!(
        status_line(&sw, true, "/tmp/x", "-"),
        "focus=tree\ttarget=api\tcwd=/tmp/x\ttty=-"
    );
    assert_eq!(
        status_line(&sw, false, "/tmp/x", "/dev/pts/3"),
        "focus=terminal\ttarget=api\tcwd=/tmp/x\ttty=/dev/pts/3"
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
                    source: "jup".into(),
                    name: "api".into(),
                    windows: 1,
                    attached: false,
                    last_attached: 1,
                },
                Session {
                    source: "jup".into(),
                    name: "db".into(),
                    windows: 1,
                    attached: false,
                    last_attached: 2,
                },
            ],
        }],
        panes: Default::default(),
    };
    let mut state = crate::state::State::from_scan(scan);
    let mut sw = Switcher::new(&mut state);
    let mut natural = 48u16;
    let mut hide = false;
    let ops = crate::ui::switcher::tests_support::noop_ops();
    let (op_tx, _op_rx) = tokio::sync::mpsc::unbounded_channel();
    let dir = std::env::temp_dir().join(format!("xmux-ctl-switch-sync-{}", std::process::id()));

    sync_selection_from_switcher(&mut state, &sw);
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
    // apply(Select) — selection becomes jup/db and the attach is marked pending
    // (the deadline is armed by the next Tick, not here).
    assert!(sync_selection_from_switcher(&mut state, &sw));
    assert_eq!(state.selection.source, "jup");
    assert_eq!(state.selection.session, "db");
    assert!(state.attach_pending, "Select marks the attach pending");
    assert!(
        state.attach_deadline.is_none(),
        "Select arms no deadline — the trailing Tick does"
    );
}

#[test]
fn handle_stdin_bytes_quit_on_prefix_q_in_tree_focus() {
    use crate::ui::switcher::{Scan, Switcher};
    // prefix is Ctrl-G (0x07) in the default config; prefix then 'q' = quit.
    let scan = Scan {
        groups: vec![],
        panes: Default::default(),
    };
    let mut state = crate::state::State::from_scan(scan); // tree focus
    let switcher = Switcher::new(&mut state);
    // The default fake env's prefix is "C-g" (0x07), matching this test's `\x07q`.
    let mut rt = test_rt(fake_env_with_sources(&["local"]));
    rt.hosts = crate::model::Hosts::default();
    rt.state = state;
    rt.switcher = switcher;
    let out = rt.handle_stdin_bytes(b"\x07q", &Selection::default());
    assert!(out.quit, "prefix+q in tree focus quits");
}

/// Builds a `Runtime` with one reachable session on source `jup`, focused on the
/// TERMINAL view — the setup the focus-independent tree-action tests share.
fn rt_terminal_focus_with_session() -> Runtime {
    use crate::session::Session;
    use crate::ui::switcher::{Scan, Switcher};
    use crate::ui::tree::Group;
    let scan = Scan {
        groups: vec![Group {
            source: "jup".into(),
            err: None,
            sessions: vec![Session {
                source: "jup".into(),
                name: "api".into(),
                windows: 1,
                attached: false,
                last_attached: 1,
            }],
        }],
        panes: Default::default(),
    };
    let mut state = crate::state::State::from_scan(scan); // launches in tree focus
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
        !rt.state.focus.is_tree_focused() && !rt.state.focus.is_modal(),
        "precondition: the terminal view holds focus (not tree, not modal)"
    );
    rt
}

#[test]
fn prefix_x_in_terminal_focus_arms_active_pane_kill() {
    // prefix x is focus-AWARE: from the terminal view it arms a kill confirm for the
    // ACTIVE pane of the DISPLAYED session (tmux prefix-x parity), not the tree
    // selection. (TermInput → KillActivePane → Switcher::arm_kill_active_pane, which
    // reads state.displayed + its cached active window.)
    let mut rt = rt_terminal_focus_with_session();
    // The terminal view shows jup/api; give it an active window so the active pane resolves.
    rt.state.panes.insert(
        "jup/api".into(),
        vec![crate::session::WindowPanes {
            index: 0,
            name: "w".into(),
            active: true,
            panes: vec![crate::session::Pane {
                index: 0,
                active: true,
                command: "bash".into(),
            }],
        }],
    );
    rt.state.displayed = Selection {
        source: "jup".into(),
        session: "api".into(),
        window: None,
    };
    rt.handle_stdin_bytes(b"\x07x", &Selection::default());
    assert!(
        matches!(
            rt.state.modal,
            Some(crate::ui::modal::Modal::Kill(
                crate::ui::modal::PendingKill::Pane { .. }
            ))
        ),
        "prefix x in terminal focus arms a kill confirm for the displayed session's active pane"
    );
}

#[test]
fn prefix_r_in_terminal_focus_kicks_rescan() {
    // prefix r is focus-independent: from the terminal view it re-scans every host. The
    // re-scan clears each group's sessions and re-arms scanning — and kick_rescan must
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
fn killing_the_displayed_session_tears_down_the_host_attach_for_reattach() {
    // Regression (psmux "kill the shown session → host blanks forever"): killing the
    // session a host's display client currently shows invalidates that client (its
    // per-session server is gone) but the PTY need not EOF, so no reap fires and the
    // registry entry lingers. Without teardown the next show() trusts that stale `live`
    // and switch-client's a dead client — a silent no-op that blanks the whole host.
    // on_op_result must tear the attach down + rearm attach so show() reattaches fresh.
    use crate::session::Session;
    use crate::ui::switcher::{Scan, Switcher};
    use crate::ui::tree::Group;
    let sess = |n: &str, r: i64| Session {
        source: "local".into(),
        name: n.into(),
        windows: 1,
        attached: false,
        last_attached: r,
    };
    let scan = Scan {
        groups: vec![Group {
            source: "local".into(),
            err: None,
            sessions: vec![sess("A", 2), sess("B", 1)],
        }],
        panes: Default::default(),
    };
    let mut state = crate::state::State::from_scan(scan);
    let switcher = Switcher::new(&mut state);
    let mut rt = test_rt(fake_env_with_sources(&["local"]));
    rt.state = state;
    rt.switcher = switcher;
    // A live display attach showing session A (key = host id = "local").
    rt.registry.insert_fake("local", 1);
    rt.hosts
        .get_mut("local")
        .unwrap()
        .display
        .set_shows("local", "A");
    assert!(
        rt.registry.contains("local"),
        "precondition: the host has a live display attach"
    );

    // Kill the DISPLAYED session A.
    rt.on_op_result(crate::ui::switcher::OpResult::Killed {
        address: "local/A".into(),
    });

    assert!(
        !rt.registry.contains("local"),
        "the stale attach that showed the killed session is torn down"
    );
    assert!(
        rt.state.attach_deadline.is_some(),
        "a reattach is armed so the next show() reattaches the new selection"
    );
}

#[test]
fn killing_a_background_session_keeps_the_displayed_attach() {
    // The teardown is scoped: killing a session the display client is NOT showing must
    // leave the live attach intact (no needless blank/reattach).
    use crate::session::Session;
    use crate::ui::switcher::{Scan, Switcher};
    use crate::ui::tree::Group;
    let sess = |n: &str, r: i64| Session {
        source: "local".into(),
        name: n.into(),
        windows: 1,
        attached: false,
        last_attached: r,
    };
    let scan = Scan {
        groups: vec![Group {
            source: "local".into(),
            err: None,
            sessions: vec![sess("A", 2), sess("B", 1)],
        }],
        panes: Default::default(),
    };
    let mut state = crate::state::State::from_scan(scan);
    let switcher = Switcher::new(&mut state);
    let mut rt = test_rt(fake_env_with_sources(&["local"]));
    rt.state = state;
    rt.switcher = switcher;
    rt.registry.insert_fake("local", 1);
    rt.hosts
        .get_mut("local")
        .unwrap()
        .display
        .set_shows("local", "A"); // showing A

    // Kill the BACKGROUND session B (not the one on screen).
    rt.on_op_result(crate::ui::switcher::OpResult::Killed {
        address: "local/B".into(),
    });

    assert!(
        rt.registry.contains("local"),
        "killing a background session leaves the displayed attach alone"
    );
}

#[test]
fn kill_confirm_owns_keys_so_prefix_q_and_enter_do_not_quit_or_focus_mux() {
    // A kill-confirm is a modal popup, so it OWNS every key. With the resolver gated
    // on is_modal_popup_open (true for a confirm, where is_inputting is false),
    // `prefix q` reaches the switcher instead of arming the prefix, and Enter reaches
    // it instead of resolving to FocusTerminal — so a confirm can neither quit the app
    // nor focus the terminal out from under itself. (The first swallowed key cancels the
    // confirm, tmux confirm-before style; the point is the key does not quit/focus.)
    use crate::app::focus::{Focus, ViewFocus};
    use crate::session::Session;
    use crate::ui::switcher::{Scan, Switcher};
    use crate::ui::tree::Group;
    let scan = Scan {
        groups: vec![Group {
            source: "jup".into(),
            err: None,
            sessions: vec![Session {
                source: "jup".into(),
                name: "api".into(),
                windows: 1,
                attached: false,
                last_attached: 1,
            }],
        }],
        panes: Default::default(),
    };
    let mut state = crate::state::State::from_scan(scan); // tree focus
    let switcher = Switcher::new(&mut state);
    let mut rt = test_rt(fake_env_with_sources(&["jup"]));
    rt.hosts = crate::model::Hosts::default();
    rt.state = state;
    rt.switcher = switcher;
    macro_rules! feed {
        ($bytes:expr) => {
            rt.handle_stdin_bytes($bytes, &Selection::default())
        };
    }
    // Launch preselects the host row; `l` (== →) descends to the api session row.
    feed!(b"l");
    // `prefix x` on the session row arms the y/n confirm (a modal popup, not an inline input).
    feed!(b"\x07x");
    assert!(
        rt.state.is_modal_popup_open(),
        "x armed the kill-confirm popup"
    );
    assert!(
        !rt.state.is_inputting(),
        "a kill-confirm is NOT an inline input"
    );
    // The loop-top reconciler makes Focus a modal carrying the prior pane.
    {
        let mk = rt.state.modal_kind();
        rt.state.focus.sync_modal(mk);
    }
    assert_eq!(
        rt.state.focus,
        Focus::Popup {
            prior: ViewFocus::Tree
        }
    );
    // prefix q with the confirm armed: routed to the switcher, NOT a quit.
    let out = feed!(b"\x07q");
    assert!(
        !out.quit,
        "prefix q is owned by the kill-confirm, does not quit"
    );
    assert_eq!(
        rt.state.focus,
        Focus::Popup {
            prior: ViewFocus::Tree
        },
        "pane focus unchanged"
    );
    // Re-arm and feed Enter: routed to the switcher, NOT a terminal-view focus.
    feed!(b"\x07x");
    {
        let mk = rt.state.modal_kind();
        rt.state.focus.sync_modal(mk);
    }
    assert_eq!(
        rt.state.focus,
        Focus::Popup {
            prior: ViewFocus::Tree
        },
        "confirm re-armed"
    );
    let out = feed!(b"\r");
    assert!(!out.quit);
    assert_eq!(
        rt.state.focus,
        Focus::Popup {
            prior: ViewFocus::Tree
        },
        "Enter did not focus the terminal"
    );
}

#[test]
fn menu_keyboard_input_is_consumed_without_changing_restore_pane_or_writing_pty() {
    use crate::app::focus::{Focus, ViewFocus};
    use crate::session::Session;
    use crate::ui::switcher::{Scan, Switcher};
    use crate::ui::tree::Group;
    use ratatui::{backend::TestBackend, Terminal};

    fn run_case(bytes: &[u8]) -> (StdinOutcome, Focus, Focus, usize) {
        let scan = Scan {
            groups: vec![Group {
                source: "local".into(),
                err: None,
                sessions: vec![Session {
                    source: "local".into(),
                    name: "api".into(),
                    windows: 1,
                    attached: false,
                    last_attached: 1,
                }],
            }],
            panes: Default::default(),
        };
        let mut state = crate::state::State::from_scan(scan);
        let mut switcher = Switcher::new(&mut state);
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| switcher.render(f, None, false, crate::ui::switcher::TREE_WIDTH, 0, &state))
            .unwrap();
        let opened = (0..10).any(|row| switcher.menu_open(1, row, &mut state));
        assert!(opened, "menu opens over a rendered tree row");

        {
            let mk = state.modal_kind();
            state.focus.sync_modal(mk);
        }
        assert_eq!(
            state.focus,
            Focus::Menu {
                prior: ViewFocus::Tree
            }
        );

        let (att, input_log) = crate::display::attachment::fake_attachment_with_input_log(1);
        let selection = Selection {
            source: "local".into(),
            session: "api".into(),
            window: None,
        };
        let mut rt = test_rt(fake_env_with_sources(&["local"]));
        rt.hosts = crate::model::Hosts::default();
        rt.hosts.insert(crate::model::Host::new(
            crate::machine::local(None),
            crate::mux::for_binary("psmux"),
        ));
        rt.registry.insert("local/api", att);
        rt.state = state;
        rt.switcher = switcher;

        let out = rt.handle_stdin_bytes(bytes, &selection);
        let during = rt.state.focus;
        {
            let mk = rt.state.modal_kind();
            rt.state.focus.sync_modal(mk);
        }
        let restored = rt.state.focus;
        let writes = input_log.lock().unwrap().len();
        (out, during, restored, writes)
    }

    for (bytes, label) in [
        (b"\r".as_slice(), "Enter"),
        (b"\x07\t".as_slice(), "prefix Tab"),
    ] {
        let (out, during, restored, writes) = run_case(bytes);
        assert!(!out.quit, "{label} over a menu does not quit");
        assert!(
            !out.focus_terminal,
            "{label} over a menu does not request terminal-view focus"
        );
        assert_eq!(
            during,
            Focus::Menu {
                prior: ViewFocus::Tree
            },
            "{label} preserves the menu restore view"
        );
        assert_eq!(
            restored,
            Focus::Tree,
            "{label} closes the menu back to the prior tree pane"
        );
        assert_eq!(writes, 0, "{label} over a menu is not forwarded to the PTY");
    }
}

#[test]
fn handle_mouse_event_view_border_grab_sets_dragging() {
    use crate::ui::switcher::{Scan, Switcher};
    // A left-press exactly on the view border column sets dragging_view_border, as the
    // inline gate did (is_left_press && tree_width > 0 && col0 == tree_width).
    let scan = Scan {
        groups: vec![],
        panes: Default::default(),
    };
    let mut state = crate::state::State::from_scan(scan);
    let switcher = Switcher::new(&mut state);
    let sel = Selection::default();
    let tree_width = crate::ui::switcher::TREE_WIDTH;
    // 0-based col0 = ev.col - 1 must equal tree_width to grab the view border rule.
    let view_border_col = tree_width + 1; // 1-based SGR column of the view border
                                          // cb=0 → left button, press, no wheel/motion → is_left_press is true.
    let ev = crate::display::mouse::MouseEvent {
        cb: 0,
        col: view_border_col,
        row: 3,
        pressed: true,
    };
    let (vw, vh) = terminal_view_size(80, 24, tree_width, 0);
    let term_area = ratatui::layout::Rect::new(tree_width + 1, 0, vw, vh);
    let mut non_mouse: Vec<u8> = Vec::new();
    let mut focus_toggle = false;
    let mut wheel = false;
    let mut rt = test_rt(fake_env_with_sources(&["local"]));
    rt.state = state;
    rt.switcher = switcher;
    rt.handle_mouse_event(
        &ev,
        &sel,
        &mut non_mouse,
        &mut focus_toggle,
        &mut wheel,
        term_area,
    );
    assert!(
        rt.mouse_state.dragging_view_border,
        "left-press on the view border column grabs it"
    );
}

#[test]
fn handle_mouse_event_top_layout_border_drag_resizes_height() {
    use crate::ui::switcher::{Scan, Switcher};
    // In the portrait Top layout the view border is a HORIZONTAL rule; a left-press on that
    // row grabs it and a drag sets the tree HEIGHT (not width). 40x60 → Top; auto height is
    // ~40% of the 59-row body = 23, so the border sits at row 23 (0-based) = SGR row 24.
    let mut state = crate::state::State::from_scan(Scan {
        groups: vec![],
        panes: Default::default(),
    });
    let switcher = Switcher::new(&mut state);
    let sel = Selection::default();
    let mut rt = test_rt(fake_env_with_sources(&["local"]));
    rt.state = state;
    rt.switcher = switcher;
    rt.cols = 40;
    rt.body_rows = 59;
    rt.tree_height = 0; // auto

    let press = crate::display::mouse::MouseEvent {
        cb: 0,
        col: 5,
        row: 24,
        pressed: true,
    };
    let mut non_mouse: Vec<u8> = Vec::new();
    let (mut ft, mut wheel) = (false, false);
    let area = ratatui::layout::Rect::default();
    rt.handle_mouse_event(&press, &sel, &mut non_mouse, &mut ft, &mut wheel, area);
    assert!(
        rt.mouse_state.dragging_view_border,
        "left-press on the horizontal Top border grabs it"
    );

    // Drag DOWN to SGR row 30 (motion bit 0x20, left button held) → tree height = 30-1 = 29.
    let drag = crate::display::mouse::MouseEvent {
        cb: 0x20,
        col: 5,
        row: 30,
        pressed: true,
    };
    rt.handle_mouse_event(&drag, &sel, &mut non_mouse, &mut ft, &mut wheel, area);
    assert_eq!(
        rt.tree_height, 29,
        "dragging the horizontal border sets the tree HEIGHT to the dragged row"
    );
}

#[test]
fn resize_keys_adjust_height_in_top_layout() {
    use crate::ui::switcher::{Scan, Switcher, ViewLayout, TREE_WIDTH};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    // In the portrait Top layout the tree-resize keys (prefix h/l · Ctrl+←/→) adjust the
    // HEIGHT, not the width — seeded from the auto height the first time.
    let mut state = crate::state::State::from_scan(Scan {
        groups: vec![],
        panes: Default::default(),
    });
    let switcher = Switcher::new(&mut state);
    let mut rt = test_rt(fake_env_with_sources(&["local"]));
    rt.state = state;
    rt.switcher = switcher;
    rt.cols = 40;
    rt.body_rows = 59;
    rt.tree_height = 0; // auto
                        // Render once into a portrait backend so the switcher caches layout = Top.
    let mut term = Terminal::new(TestBackend::new(40, 60)).unwrap();
    {
        let sw = &mut rt.switcher;
        let st = &rt.state;
        term.draw(|f| sw.render(f, None, false, TREE_WIDTH, 0, st))
            .unwrap();
    }
    assert_eq!(rt.switcher.layout(), ViewLayout::Top, "portrait → Top");

    let auto = crate::ui::switcher::default_tree_height(59);
    // Vertical axis (Ctrl+↓ = grow) resizes HEIGHT in Top; horizontal (Ctrl+→) is a no-op here.
    assert!(
        !rt.resize_axis(true, 1),
        "horizontal resize is a no-op in Top"
    );
    assert!(rt.resize_axis(false, 1), "grow changes the height");
    assert_eq!(
        rt.tree_height,
        auto + 1,
        "a resize key grows the Top tree height from the auto seed"
    );
    assert!(rt.resize_axis(false, -1), "shrink changes the height");
    assert_eq!(rt.tree_height, auto, "and shrinks it back");
}
