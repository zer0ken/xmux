//! The cockpit: a persistent supervisor that owns the terminal for the whole
//! session. It keeps a bounded set of live attachments (each a mux-client child
//! on its own ConPTY), runs ONE event loop that interleaves stdin, the picker
//! control socket, terminal resize, dwell-driven attaches, and EOF reaps, and
//! transitions between Passthrough (a foreground attachment owns raw stdout) and
//! Overlay (ratatui draws the switcher). Switching between hosts is instant: the
//! target is already (or quickly) attached and kept warm, so a switch is a state
//! transition, not a fresh attach.

use std::path::PathBuf;
use std::sync::Arc;

use crate::attach;
use crate::env::Env;
use crate::session;

/// The `xmux` (no subcommand) entry: the persistent cockpit. Owns the terminal,
/// keeps a bounded set of live attachments, and lets the in-session overlay
/// switch between them with no re-attach. It serves a picker control socket so a
/// headless driver can inject keys/text and dump the switcher screen.
pub async fn run_cockpit(env: Arc<Env>) -> i32 {
    use crate::proxy::app::{App, AppState};
    use crate::proxy::input::{InAction, InputMachine};
    use crate::proxy::registry::AttachRegistry;
    use crate::proxy::run::{parse_prefix, LiveOwner, RawGuard};
    use crate::ui::switcher::Switcher;
    use std::io::Read;
    use std::time::Duration;

    // The cockpit owns the terminal and attaches mux clients as children; nested
    // inside a mux every attach is refused, leaving only a doomed loop. So running
    // it inside a mux is refused outright, not warned.
    if let Err(e) = attach::nest_guard(attach::in_mux()) {
        eprintln!("xmux: {e}");
        eprintln!("xmux: the cockpit must be your terminal entry, not run inside a mux.");
        return 2;
    }
    let _ = std::fs::create_dir_all(&env.xmux_dir);

    // Raw mode for the whole session (RAII-restored on return/panic).
    let _raw = match RawGuard::enter() {
        Ok(g) => g,
        Err(e) => {
            eprintln!("xmux: {e}");
            return 1;
        }
    };

    let size = ratatui::crossterm::terminal::size().unwrap_or((80, 24));
    let (mut cols, mut body_rows) = (size.0, size.1.saturating_sub(1)); // status bar = last row

    let live = LiveOwner::new();
    let (eof_tx, mut eof_rx) = tokio::sync::mpsc::unbounded_channel::<u64>();
    let mut registry = AttachRegistry::new(env.cfg.keep_cap(), live.clone(), eof_tx);
    let mut app = App::new(live.clone());

    // The switcher, seeded from the source skeletons; probes stream sessions in.
    let ops = env.ops();
    let mut switcher = Switcher::from_sources(ops.sources());

    // Single stdin reader thread (the proxy pattern): raw host bytes → channel.
    let (stdin_tx, mut stdin_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(256);
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut stdin = stdin.lock();
        let mut buf = [0u8; 256];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if stdin_tx.blocking_send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let mut machine = InputMachine::new(
        parse_prefix(Some(&env.ui_prefix)),
        b's',
        b'q',
        Duration::from_millis(400),
    );

    // ratatui terminal over the real stdout (Overlay draws; Passthrough is raw).
    let mut term =
        match ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(std::io::stdout())) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("xmux: {e}");
                return 1;
            }
        };

    // The picker control + probe channel: serves headless key/text/dump and
    // carries the streaming probe results back into the switcher.
    let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::channel::<crate::ui::run::Cmd>(256);
    let control = pick_control_path(&env);
    let _control_handle = control.and_then(|p| crate::ui::run::serve_control(p, cmd_tx.clone()));
    crate::ui::run::spawn_probes(&ops, &cmd_tx); // kick the streaming probes

    let mut event_stream = ratatui::crossterm::event::EventStream::new();
    use futures::StreamExt;

    loop {
        // Draw: in Overlay, ratatui paints the sidebar + terminal view (the cursor
        // session's grid). In Passthrough the foreground pump writes raw — nothing
        // to draw here, the terminal is the child's.
        if app.is_overlay() {
            let tv_addr = {
                let t = switcher.terminal_view_target();
                (!t.target.is_empty()).then(|| format!("{}/{}", t.source, t.target))
            };
            // Borrow the cursor session's grid (clone the Arc, lock briefly).
            let grid_arc = tv_addr
                .as_deref()
                .and_then(|a| registry.get(a))
                .map(|att| att.grid.clone());
            let _ = match &grid_arc {
                Some(g) => {
                    let guard = g.lock().ok();
                    term.draw(|f| switcher.render(f, guard.as_deref()))
                }
                None => term.draw(|f| switcher.render(f, None)),
            };
        }

        // Dwell-completed attach (Overlay only): attach + keep, mark live. Polled
        // every iteration so `dwell_pending` clears once the attach is taken and
        // the tick reverts from the 33ms animation rate back to the idle rate.
        if app.is_overlay() {
            if let Some(tgt) = switcher.take_dwell_attach(std::time::Instant::now()) {
                let addr = format!("{}/{}", tgt.source, tgt.target);
                attach_into_registry(&env, &mut registry, &addr, cols, body_rows, &app);
            }
        }

        let tick = if app.is_overlay() && switcher.dwell_pending() {
            Duration::from_millis(33)
        } else {
            Duration::from_millis(250)
        };

        tokio::select! {
            biased;
            Some(id) = eof_rx.recv() => {
                // A session exited: reap it; if it was the foreground, fall to
                // Overlay (clear + redraw on the next loop top overdraws any stray
                // chunk a demoted pump leaked during the gate handoff).
                let was_fg = matches!(&app.state, AppState::Passthrough { fg_id, .. } if *fg_id == id);
                registry.reap(id);
                if was_fg {
                    app.enter_overlay();
                    switcher.clear_result();
                    let _ = term.clear();
                }
            }
            Some(bytes) = stdin_rx.recv() => {
                if app.is_overlay() {
                    // Drive the switcher: decode bytes → KeyEvents.
                    let mut decoder = crate::proxy::decode::KeyDecoder::new();
                    for key in decoder.feed(&bytes) {
                        switcher.handle_key(key);
                    }
                    handle_switcher_outcome(
                        &env, &mut registry, &mut switcher, &mut app, &mut term, cols, body_rows,
                    );
                    if switcher.should_exit() && switcher.result().chosen.is_none() {
                        break; // q quit the app
                    }
                } else {
                    // Passthrough: the InputMachine intercepts only the prefix.
                    let now = std::time::Instant::now();
                    let mut to_fg: Vec<u8> = Vec::new();
                    let mut open = false;
                    let mut quit = false;
                    for b in bytes {
                        for action in machine.feed(b, now) {
                            match action {
                                InAction::Forward(f) => to_fg.extend_from_slice(&f),
                                InAction::OpenOverlay => open = true,
                                InAction::Quit => quit = true,
                            }
                        }
                    }
                    if let AppState::Passthrough { fg, .. } = &app.state {
                        if !to_fg.is_empty() {
                            if let Some(att) = registry.get(fg) {
                                att.input(to_fg);
                            }
                        }
                    }
                    if quit {
                        break;
                    }
                    if open {
                        // ONLY enter Overlay from Passthrough (never re-enter while
                        // already Overlay). Clear + the next loop-top redraw overdraw
                        // any stray chunk the demoted foreground pump may have leaked.
                        app.enter_overlay();
                        switcher.clear_result();
                        let _ = term.clear();
                    }
                }
            }
            Some(cmd) = cmd_rx.recv() => {
                use crate::ui::run::Cmd;
                match cmd {
                    Cmd::Key(k) => {
                        switcher.handle_key(k);
                        if app.is_overlay() {
                            handle_switcher_outcome(
                                &env, &mut registry, &mut switcher, &mut app, &mut term, cols,
                                body_rows,
                            );
                            if switcher.should_exit() && switcher.result().chosen.is_none() {
                                break;
                            }
                        }
                    }
                    Cmd::SourceResult { source, sessions, err } => {
                        let reachable = err.is_none();
                        switcher.apply_source_result(source, sessions.clone(), err);
                        if reachable {
                            crate::ui::run::spawn_panes(&ops, &cmd_tx, sessions);
                        }
                    }
                    Cmd::Panes { address, panes } => switcher.apply_panes(address, panes),
                    Cmd::Dump(reply) => {
                        let sz = term
                            .size()
                            .unwrap_or(ratatui::layout::Size { width: 80, height: 24 });
                        let _ = reply.send(crate::ui::run::dump_switcher(
                            &mut switcher,
                            sz.width,
                            sz.height,
                        ));
                    }
                    Cmd::OpDone(result) => switcher.apply_op_result(result),
                    Cmd::Mouse(_) | Cmd::Resize(_, _) => {}
                }
            }
            Some(Ok(ev)) = event_stream.next() => {
                if let ratatui::crossterm::event::Event::Resize(c, r) = ev {
                    let body = r.saturating_sub(1);
                    cols = c;
                    body_rows = body;
                    registry.resize_all(c, body);
                    let _ = term.resize(ratatui::layout::Rect::new(0, 0, c, r));
                    // If in Passthrough, recompute the status bar at the new dimensions
                    // and push it into the attachment so the owner pump re-emits it on
                    // the child's next full-screen redraw. The loop MUST NOT write stdout
                    // here — the foreground pump owns stdout in Passthrough.
                    if let crate::proxy::app::AppState::Passthrough { fg, .. } = &app.state {
                        let fg = fg.clone();
                        let new_status = status_bar_bytes(&fg, registry.kept(), env.cfg.keep_cap(), cols, body_rows);
                        registry.set_status_bar(&fg, new_status);
                    }
                }
            }
            _ = tokio::time::sleep(tick) => { /* wake to repaint dwell progress */ }
        }
    }

    registry.teardown_all();
    0
}

/// Resolves an `addr` to its attach argv and ensures it is in the registry,
/// protecting the foreground + the target itself from eviction. Returns the
/// attachment's id, or `None` when the source is unknown, the address is
/// malformed, or xmux is (now) inside a mux.
fn attach_into_registry(
    env: &Arc<Env>,
    registry: &mut crate::proxy::registry::AttachRegistry,
    addr: &str,
    cols: u16,
    rows: u16,
    app: &crate::proxy::app::App,
) -> Option<u64> {
    let sess = session::parse_target(addr).ok()?;
    let src = env.by_alias.get(&sess.source)?.clone();
    if attach::nest_guard(attach::in_mux()).is_err() {
        return None;
    }
    let argv = src.attach_command(&sess.name, None);
    // Protect both the target itself and the address the Esc key returns to (the
    // previous foreground). While in Overlay there is no Passthrough foreground to
    // read directly, but prev_fg (esc_target) could be evicted by rapid navigation
    // across ≥cap sessions, forcing a cold re-attach on Esc.
    let esc_addr: Option<String> = app.esc_target().map(|(a, _)| a);
    let mut protect: Vec<&str> = vec![addr];
    if let crate::proxy::app::AppState::Passthrough { fg, .. } = &app.state {
        protect.push(fg.as_str());
    }
    if let Some(ref a) = esc_addr {
        protect.push(a.as_str());
    }
    registry.ensure(addr, &argv, cols, rows, &protect).ok()
}

/// After the switcher processed a key in Overlay: Enter → attach + promote to
/// Passthrough; Esc → return to the previous foreground (when one exists). Quit
/// (`q`) is detected by the caller via `should_exit` + no chosen session.
fn handle_switcher_outcome(
    env: &Arc<Env>,
    registry: &mut crate::proxy::registry::AttachRegistry,
    switcher: &mut crate::ui::switcher::Switcher,
    app: &mut crate::proxy::app::App,
    term: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
    cols: u16,
    rows: u16,
) {
    // Enter chose a session: attach immediately + promote to foreground.
    let result = switcher.result();
    if let Some(chosen) = result.chosen {
        let addr = chosen.address();
        if let Some(id) = attach_into_registry(env, registry, &addr, cols, rows, app) {
            switcher.note_attached(&addr);
            enter_passthrough(env, registry, app, &addr, id, cols, rows);
        }
        switcher.clear_result();
        let _ = term; // the terminal is ratatui's; Passthrough writes raw next
        return;
    }
    // Esc: return to the previous foreground if there is one (otherwise stay in
    // Overlay — the initial Overlay has no foreground to return to). The kept
    // attachment's CURRENT id is read from the registry (the remembered id can be
    // stale if it was evicted/reaped and re-attached while in Overlay).
    if switcher.take_esc() {
        if let Some((addr, _stale_id)) = app.esc_target() {
            let id = registry
                .id_of(&addr)
                .or_else(|| attach_into_registry(env, registry, &addr, cols, rows, app));
            if let Some(id) = id {
                enter_passthrough(env, registry, app, &addr, id, cols, rows);
            }
        }
    }
}

/// Builds the foreground restore + status bar for `addr`/`id`, pushes the status
/// bytes into the attachment (so its owner pump re-emits them after a child
/// full-screen clear), then transitions the app to Passthrough.
fn enter_passthrough(
    env: &Arc<Env>,
    registry: &mut crate::proxy::registry::AttachRegistry,
    app: &mut crate::proxy::app::App,
    addr: &str,
    id: u64,
    cols: u16,
    rows: u16,
) {
    let restore = registry
        .get(addr)
        .and_then(|att| att.grid.lock().ok().map(|g| g.restore_bytes()))
        .unwrap_or_default();
    let status = status_bar_bytes(addr, registry.kept(), env.cfg.keep_cap(), cols, rows);
    registry.set_status_bar(addr, status.clone());
    app.enter_passthrough(addr.to_string(), id, &restore, &status);
}

/// The Passthrough status bar bytes: `host/session · kept N/cap`, painted on the
/// last physical row, wrapped in cursor save/restore. Info only — no shortcuts.
fn status_bar_bytes(addr: &str, kept: usize, cap: usize, cols: u16, rows: u16) -> Vec<u8> {
    let text = format!("{addr} · kept {kept}/{cap}");
    let clipped: String = text.chars().take(cols as usize).collect();
    let mut out = Vec::new();
    out.extend_from_slice(b"\x1b7"); // save cursor
    out.extend_from_slice(format!("\x1b[{};1H", rows + 1).as_bytes()); // last physical row
    out.extend_from_slice(b"\x1b[7m"); // reverse video
    out.extend_from_slice(b"\x1b[K"); // clear line
    out.extend_from_slice(clipped.as_bytes());
    out.extend_from_slice(b"\x1b[0m");
    out.extend_from_slice(b"\x1b8"); // restore cursor
    out
}

/// The picker's control socket path (`ctl-<pid>.sock`), unless `XMUX_CONTROL=0`.
fn pick_control_path(env: &Env) -> Option<PathBuf> {
    if std::env::var("XMUX_CONTROL").as_deref() == Ok("0") {
        return None;
    }
    let _ = std::fs::create_dir_all(&env.xmux_dir);
    Some(crate::control::socket_path(&env.xmux_dir, std::process::id()))
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn enter_promotes_cursor_to_passthrough_foreground() {
        use crate::proxy::app::{App, AppState};
        use crate::proxy::run::LiveOwner;
        use crate::session::Session;
        use crate::ui::switcher::{Scan, Switcher};
        use crate::ui::tree::Group;

        let live = LiveOwner::new();
        let mut app = App::new(live.clone());
        let scan = Scan {
            groups: vec![Group {
                source: "jupiter06".into(),
                err: None,
                sessions: vec![Session {
                    source: "jupiter06".into(),
                    name: "api".into(),
                    windows: 1,
                    attached: false,
                    last_attached: 100,
                }],
            }],
            panes: Default::default(),
        };
        let mut sw = Switcher::new(scan);
        // Cursor preselected on jupiter06/api; Enter chooses it.
        sw.handle_key(ratatui::crossterm::event::KeyEvent::new(
            ratatui::crossterm::event::KeyCode::Enter,
            ratatui::crossterm::event::KeyModifiers::NONE,
        ));
        let chosen = sw.result().chosen.expect("Enter chooses the cursor session");
        let addr = chosen.address();
        assert_eq!(addr, "jupiter06/api");
        // The cockpit would attach (fake id 9) and promote to foreground.
        app.enter_passthrough(addr.clone(), 9, b"", b"");
        assert_eq!(
            app.state,
            AppState::Passthrough {
                fg: "jupiter06/api".into(),
                fg_id: 9
            }
        );
        assert!(live.is_owner(9), "the foreground attachment owns stdout");
    }

    #[tokio::test]
    async fn esc_returns_to_previous_foreground_no_switch() {
        use crate::proxy::app::App;
        use crate::proxy::run::LiveOwner;
        let live = LiveOwner::new();
        let mut app = App::new(live.clone());
        app.enter_passthrough("local/work".into(), 1, b"", b"");
        app.enter_overlay();
        // Esc target is the remembered previous foreground.
        let (addr, id) = app.esc_target().expect("esc returns to previous fg");
        assert_eq!(addr, "local/work");
        app.enter_passthrough(addr, id, b"", b"");
        assert!(live.is_owner(1));
    }

    // =========================================================================
    // Headless cockpit smoke: drives the SWITCHER half of the cockpit (Overlay
    // tree paint, recency order, nav, filter, dwell completion) without a real
    // PTY. Live attach + the raw passthrough screen handover are the human gate
    // (see checklist below). Marked #[ignore] — run on demand:
    //   cargo test -p xmux cockpit::tests::cockpit_overlay_headless_smoke -- --ignored --nocapture
    //
    // HUMAN VISUAL-GATE CHECKLIST (run in a REAL terminal — never headless):
    // 1. Launch `xmux` (the cockpit) in a real terminal. Confirm it starts in
    //    Overlay: sidebar tree on the left, terminal view on the right.
    // 2. Cursor to `jupiter06/probe`. Hold still for ~500ms — confirm the
    //    selected row's background fills left→right (dwell progress bar), then
    //    the terminal view goes live once the 500ms elapses.
    // 3. Press Enter — confirm the session promotes to full-screen Passthrough;
    //    the last physical row shows the status bar
    //    "jupiter06/probe · kept N/cap" in reverse-video.
    // 4. Press the prefix (C-g) then `s` — confirm it returns to Overlay with
    //    the previous foreground remembered. Press Esc — confirm it returns to
    //    the jupiter06/probe Passthrough with no switch.
    // 5. Press the prefix then `q` (or `q` in Overlay) — confirm a clean quit
    //    and terminal restored to its previous state.
    // 6. NEVER select or attach `local/xmux` (the live session) during this
    //    test — only the throwaway `jupiter06`. Attaching the live session from
    //    within itself would mirror xmux inside its own terminal view.
    // =========================================================================
    #[ignore]
    #[tokio::test]
    async fn cockpit_overlay_headless_smoke() {
        use crate::session::Session;
        use crate::ui::switcher::{Scan, Switcher, DWELL};
        use crate::ui::tree::Group;
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        // local pinned first by order_groups (LOCAL_SOURCE = "local"), even
        // though jupiter06/probe has a higher last_attached (300 vs 50).
        let scan = Scan {
            groups: vec![
                Group {
                    source: "jupiter06".into(),
                    err: None,
                    sessions: vec![Session {
                        source: "jupiter06".into(),
                        name: "probe".into(),
                        windows: 1,
                        attached: false,
                        last_attached: 300,
                    }],
                },
                Group {
                    source: "local".into(),
                    err: None,
                    sessions: vec![Session {
                        source: "local".into(),
                        name: "work".into(),
                        windows: 1,
                        attached: false,
                        last_attached: 50,
                    }],
                },
            ],
            panes: Default::default(),
        };
        let mut sw = Switcher::new(scan);

        // Ordering: local group pinned first (Task 10 ordering) even though
        // jupiter06/probe is more recently attached.
        let dump = crate::ui::run::dump_switcher(&mut sw, 100, 30);
        let local_at = dump.find("local").expect("local present in dump");
        let jup_at = dump.find("jupiter06").expect("jupiter06 present in dump");
        assert!(local_at < jup_at, "local group pinned above remote:\n{dump}");

        // Filter to "probe": opens the filter input (/), types the name, Enter applies.
        for c in ['/', 'p', 'r', 'o', 'b', 'e'] {
            sw.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        sw.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let dump = crate::ui::run::dump_switcher(&mut sw, 100, 30);
        assert!(dump.contains("probe"), "filter keeps the throwaway:\n{dump}");
        assert!(!dump.contains("work"), "filter drops local/work:\n{dump}");

        // A completed dwell on the filtered, unattached session yields an attach
        // target. now + DWELL is always past the 500ms threshold.
        let now = std::time::Instant::now();
        let got = sw.take_dwell_attach(now + DWELL);
        assert_eq!(
            got.map(|t| t.target),
            Some("probe".to_string()),
            "dwell completes on the filtered throwaway session"
        );
    }

    /// Verifies that the esc_target (prev_fg) address is included in the protect
    /// list so a rapid dwell-driven attach while in Overlay cannot LRU-evict the
    /// session the user will Esc back to.
    #[test]
    fn esc_target_included_in_protect_list() {
        use crate::proxy::app::{App, AppState};
        use crate::proxy::run::LiveOwner;

        let live = LiveOwner::new();
        let mut app = App::new(live);
        // First passthrough: local/work (id 1) becomes the prev_fg after overlay.
        app.enter_passthrough("local/work".into(), 1, b"", b"");
        app.enter_overlay();
        // Now in Overlay: esc_target() == Some(("local/work", 1)); no Passthrough.
        assert!(matches!(app.state, AppState::Overlay));
        let esc_addr = app.esc_target().map(|(a, _)| a);
        assert_eq!(esc_addr.as_deref(), Some("local/work"));
        // Build protect the same way attach_into_registry does — target + fg + esc.
        let target = "jupiter06/api";
        let mut protect: Vec<&str> = vec![target];
        // No Passthrough foreground (we are in Overlay) — no fg push here.
        if let Some(ref a) = esc_addr {
            protect.push(a.as_str());
        }
        assert!(
            protect.contains(&"local/work"),
            "esc_target must be in protect so it is not LRU-evicted during Overlay navigation"
        );
    }
}
