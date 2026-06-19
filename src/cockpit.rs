//! The cockpit: a persistent supervisor that owns the terminal for the whole
//! session. It owns one [`HostManager`] (a lazily-spawned control-mode client per
//! host), the [`Switcher`], the [`App`] (Overlay/Passthrough), and the picker
//! control socket. One `select!` loop interleaves stdin, host events, the control
//! socket, terminal resize, and an animation tick. On a cursor move (real or via
//! the control socket) it ensures the cursor's host is connected and
//! `switch-client`s to its session (`select = attach`). ratatui owns stdout in
//! both states: Overlay draws the switcher; Passthrough draws the selected
//! session's live grid fullscreen plus a one-line status bar.

use std::path::PathBuf;
use std::sync::Arc;

use crate::attach;
use crate::env::Env;
use crate::host::{HostManager, HostEvent};
use crate::ui::switcher::TerminalViewTarget;

/// Ensures `tgt`'s host is connected (spawning its control client lazily) and
/// queues a `switch-client` to its session on that client. Returns `true` when a
/// switch was queued, `false` when the source is unknown or the client could not
/// be spawned. Extracted from the loop so the `select = attach` decision is
/// unit-testable without a real terminal.
fn select_attach(
    mgr: &mut HostManager,
    env: &Env,
    tgt: &TerminalViewTarget,
    cols: u16,
    rows: u16,
) -> bool {
    let Some(src) = env.by_alias.get(&tgt.source) else {
        return false;
    };
    if mgr.ensure(&tgt.source, src, cols, rows).is_err() {
        return false;
    }
    match mgr.get(&tgt.source) {
        Some(client) => {
            client.switch_client(tgt.target.clone());
            true
        }
        None => false,
    }
}

/// Connects the host the cursor is on (if not already), so its control-mode
/// client's `list-sessions` streams that host's tree in. A control-mode client is
/// the only source of a host's session list, so a host whose client is never
/// spawned shows an empty skeleton; ensuring on cursor focus populates it lazily.
fn ensure_current_host(
    mgr: &mut HostManager,
    env: &Env,
    switcher: &crate::ui::switcher::Switcher,
    cols: u16,
    rows: u16,
) {
    if let Some(host) = switcher.current_host() {
        if let Some(src) = env.by_alias.get(&host) {
            let _ = mgr.ensure(&host, src, cols, rows);
        }
    }
}

/// Connects EVERY source's control client at startup so each one's
/// `list-sessions` streams its host's tree in without waiting for a cursor move.
/// A control-mode client is the only source of a host's session list, so without
/// this every host sits on "scanning…" until the user happens to focus it. The
/// bound is the host count (one connection per host); each connect runs its
/// handshake on its own reader/writer threads, so this returns at once and the
/// trees fill in asynchronously as each host responds.
fn connect_all_sources(mgr: &mut HostManager, env: &Env, cols: u16, rows: u16) {
    for src in &env.srcs {
        let _ = mgr.ensure(&src.alias, src, cols, rows);
    }
}

/// Requests `list-panes` for each of a host's sessions whose panes have not been
/// requested yet, so every session's window/pane subtree loads instead of sitting
/// on the "loading…" placeholder. The control client never volunteers pane data —
/// it must be asked, once per session (`requested` dedupes repeat Inventory
/// events). The resolved reply emits an Inventory event that paints the subtree.
fn request_session_panes(
    client: &crate::host::HostClient,
    sessions: &[crate::session::Session],
    requested: &mut std::collections::HashSet<String>,
) {
    for s in sessions {
        let addr = s.address();
        if requested.insert(addr.clone()) {
            client.list_panes(&s.name, addr);
        }
    }
}

/// Handles a host's control client dying. If the host never reached a connected
/// state, marks it unreachable in the switcher so it renders "⚠ unreachable"
/// instead of spinning on "scanning…" forever; a host that had connected keeps
/// its last-known tree (the reap leaves the switcher state intact). Returns
/// `true` when it marked the host unreachable.
fn note_host_exited(
    switcher: &mut crate::ui::switcher::Switcher,
    connected: &std::collections::HashSet<String>,
    host: &str,
    reason: Option<String>,
) -> bool {
    if connected.contains(host) {
        return false;
    }
    let msg = reason.unwrap_or_else(|| "connection closed".into());
    switcher.apply_source_result(host.to_string(), Vec::new(), Some(msg));
    true
}

/// The `xmux` (no subcommand) entry: the persistent cockpit. Owns the terminal,
/// keeps a lazily-spawned control client per host, and lets the in-session overlay
/// switch between them with no re-attach. It serves a picker control socket so a
/// headless driver can inject keys/text and dump the switcher screen.
pub async fn run_cockpit(env: Arc<Env>) -> i32 {
    use crate::proxy::app::App;
    use crate::proxy::decode::KeyDecoder;
    use crate::proxy::input::{InAction, InputMachine};
    use crate::proxy::term::{parse_prefix, TermGuard};
    use crate::ui::run::{dump_overlay, serve_control, AppStateKind, Cmd};
    use crate::ui::switcher::Switcher;
    use std::collections::HashSet;
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

    // Raw mode + alternate screen for the whole session (RAII-restored on
    // return/panic). On a failed enter, report and bail before the first draw.
    let _term_guard = match TermGuard::enter() {
        Ok(g) => g,
        Err(e) => {
            eprintln!("xmux: {e}");
            return 1;
        }
    };

    let size = ratatui::crossterm::terminal::size().unwrap_or((80, 24));
    let (mut cols, mut body_rows) = (size.0, size.1.saturating_sub(1)); // status bar = last row

    // The host manager: one control client per host, spawned lazily on first
    // select. Every client emits onto the one shared event sink.
    let (host_tx, mut host_rx) = tokio::sync::mpsc::unbounded_channel::<HostEvent>();
    let mut mgr = HostManager::new(host_tx);

    // The switcher, seeded from the source skeletons; host events stream the tree in.
    let mut switcher = Switcher::from_sources(env.srcs.iter().map(|s| s.alias.clone()).collect());
    let mut app = App::new();

    // The live mutate ops (create/rename/kill) only — NOT tree probing (that comes
    // from the control clients' inventory).
    let ops = env.ops();

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

    // ratatui terminal over the real stdout (draws every frame in BOTH states).
    let mut term =
        match ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(std::io::stdout())) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("xmux: {e}");
                return 1;
            }
        };
    let _ = term.clear(); // #1: clear the alt screen before the first draw

    // The picker control socket: serves headless key/text/dump.
    let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::channel::<Cmd>(256);
    let control = pick_control_path(&env);
    let _control_handle = control.and_then(|p| serve_control(p, cmd_tx));

    // Off-loop create/rename/kill results fold back through this cockpit-local
    // channel (replacing the removed Cmd::OpDone arm).
    let (op_tx, mut op_rx) = tokio::sync::mpsc::unbounded_channel();

    // Connect EVERY source up front so each control client's list-sessions
    // streams its host's tree in without waiting for a cursor move. `connected`
    // tracks the hosts that have reached a connected state, so a connection that
    // dies before it ever connected is marked unreachable instead of spinning on
    // "scanning…" forever.
    let mut connected: HashSet<String> = HashSet::new();
    // Session addresses whose list-panes has been requested, so a session's
    // subtree is asked for exactly once (repeat Inventory events don't re-issue).
    let mut panes_requested: HashSet<String> = HashSet::new();
    connect_all_sources(&mut mgr, &env, cols, body_rows);

    loop {
        // Draw every frame: in Overlay paint the switcher (tree + the cursor
        // session's grid); in Passthrough draw that grid fullscreen + status bar.
        let tgt = switcher.terminal_view_target();
        let grid_arc = (!tgt.target.is_empty())
            .then(|| mgr.get(&tgt.source).map(|c| c.grid.clone()))
            .flatten();
        if app.is_overlay() {
            let _ = match &grid_arc {
                Some(g) => {
                    let guard = g.lock().ok();
                    term.draw(|f| switcher.render(f, guard.as_deref()))
                }
                None => term.draw(|f| switcher.render(f, None)),
            };
        } else {
            let _ = term.draw(|f| {
                let area = f.area();
                let body = ratatui::layout::Rect::new(
                    area.x,
                    area.y,
                    area.width,
                    area.height.saturating_sub(1),
                );
                if let Some(g) = &grid_arc {
                    if let Ok(guard) = g.lock() {
                        guard.render_into(f.buffer_mut(), body);
                    }
                }
                draw_status_bar(f, &mgr, &tgt);
            });
        }

        // The renderer has now caught up, so it is safe to resume any panes the
        // server paused for flow control. Snapshot + clear the set under a brief
        // lock, then resume each outside the lock (never across an `.await`).
        if let Some(client) = mgr.get(&tgt.source) {
            let paused: Vec<String> = {
                let mut inv = client.inventory.lock().unwrap();
                inv.paused_panes.drain().collect()
            };
            for pane in &paused {
                client.resume_pane(pane);
            }
        }

        let tick = Duration::from_millis(250);

        tokio::select! {
            biased;
            Some(ev) = host_rx.recv() => {
                match ev {
                    HostEvent::Connected { host } | HostEvent::Inventory { host } => {
                        connected.insert(host.clone());
                        // Rebuild this host's tree from its live inventory, then ask
                        // for each session's panes (once) so its subtree loads.
                        if let Some(client) = mgr.get(&host) {
                            let sessions = {
                                let inv = client.inventory.lock().unwrap();
                                switcher.apply_source_result(host.clone(), inv.sessions.clone(), None);
                                for (addr, windows) in inv.panes.iter() {
                                    switcher.apply_panes(addr.clone(), windows.clone());
                                }
                                inv.sessions.clone()
                            };
                            request_session_panes(client, &sessions, &mut panes_requested);
                        }
                    }
                    HostEvent::Output { .. } => { /* redraw on the next loop top */ }
                    HostEvent::Attached { .. } => {
                        // A confirmed switch: the live grid is now feeding, so the
                        // spinner clears. The next animation tick recomputes the
                        // connecting set from scratch.
                        switcher.set_spinner(HashSet::new());
                    }
                    HostEvent::Exited { host, reason } => {
                        // A client that dies before it ever connected is marked
                        // unreachable so its host stops spinning on "scanning…".
                        note_host_exited(&mut switcher, &connected, &host, reason);
                        mgr.reap(&host);
                    }
                }
            }
            Some(bytes) = stdin_rx.recv() => {
                if app.is_overlay() {
                    let mut decoder = KeyDecoder::new();
                    for key in decoder.feed(&bytes) {
                        switcher.handle_key(key);
                    }
                    dispatch_pending_op(&mut switcher, &ops, &op_tx);
                    ensure_current_host(&mut mgr, &env, &switcher, cols, body_rows);
                    if let Some(tgt) = switcher.current_attach_target() {
                        select_attach(&mut mgr, &env, &tgt, cols, body_rows);
                    }
                    if switcher.wants_quit() {
                        break;
                    }
                } else {
                    // Passthrough: the InputMachine intercepts only the prefix; all
                    // other bytes forward to the selected session's active pane.
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
                    if !to_fg.is_empty() {
                        let tv = switcher.terminal_view_target();
                        if let Some(client) = mgr.get(&tv.source) {
                            let pane = client
                                .inventory
                                .lock()
                                .unwrap()
                                .active_pane
                                .clone();
                            if let Some(pane) = pane {
                                client.send_keys(pane, to_fg);
                            }
                        }
                    }
                    if quit {
                        break;
                    }
                    if open {
                        app.toggle();
                        let _ = term.clear();
                    }
                }
            }
            Some(cmd) = cmd_rx.recv() => {
                match cmd {
                    Cmd::Key(k) => {
                        // The control channel drives the switcher (Overlay path) so
                        // `xmux ctl key …` navigates + attaches headlessly.
                        switcher.handle_key(k);
                        dispatch_pending_op(&mut switcher, &ops, &op_tx);
                        ensure_current_host(&mut mgr, &env, &switcher, cols, body_rows);
                        if let Some(tgt) = switcher.current_attach_target() {
                            select_attach(&mut mgr, &env, &tgt, cols, body_rows);
                        }
                        if switcher.wants_quit() {
                            break;
                        }
                    }
                    Cmd::Dump(reply) => {
                        // Render the SAME Overlay content the main draw produces —
                        // the switcher plus the cursor host's live grid — so a
                        // headless `dump` reflects the live screen.
                        let sz = term
                            .size()
                            .unwrap_or(ratatui::layout::Size { width: 80, height: 24 });
                        let tgt = switcher.terminal_view_target();
                        let grid_arc = (!tgt.target.is_empty())
                            .then(|| mgr.get(&tgt.source).map(|c| c.grid.clone()))
                            .flatten();
                        let dump = match &grid_arc {
                            Some(g) => {
                                let guard = g.lock().ok();
                                dump_overlay(&mut switcher, guard.as_deref(), sz.width, sz.height)
                            }
                            None => dump_overlay(&mut switcher, None, sz.width, sz.height),
                        };
                        let _ = reply.send(dump);
                    }
                    Cmd::SetState(kind) => {
                        app.state = match kind {
                            AppStateKind::Overlay => crate::proxy::app::AppState::Overlay,
                            AppStateKind::Passthrough => crate::proxy::app::AppState::Passthrough,
                        };
                    }
                    Cmd::Keys(bytes) => {
                        // Run the bytes through the same Passthrough forwarding path
                        // as stdin bytes in Passthrough state: feed through the input
                        // machine so the prefix is intercepted, then send any Forward
                        // output to the active pane.
                        let now = std::time::Instant::now();
                        let mut to_fg: Vec<u8> = Vec::new();
                        for b in bytes {
                            for action in machine.feed(b, now) {
                                if let InAction::Forward(f) = action {
                                    to_fg.extend_from_slice(&f);
                                }
                            }
                        }
                        if !to_fg.is_empty() {
                            let tv = switcher.terminal_view_target();
                            if let Some(client) = mgr.get(&tv.source) {
                                let pane = client
                                    .inventory
                                    .lock()
                                    .unwrap()
                                    .active_pane
                                    .clone();
                                if let Some(pane) = pane {
                                    client.send_keys(pane, to_fg);
                                }
                            }
                        }
                    }
                }
            }
            Some(result) = op_rx.recv() => {
                switcher.apply_op_result(result);
            }
            _ = tokio::time::sleep(tick) => {
                // The dedicated stdin thread is the sole fd-0 reader; resize is
                // detected here by polling the console size (an ioctl/Win32
                // console-info call, NOT a stdin read — no fd contention). On a
                // change push the new size to the hosts and let ratatui pick it up
                // on the next draw.
                if let Ok((c, r)) = ratatui::crossterm::terminal::size() {
                    if (c, r) != (cols, body_rows + 1) {
                        let body = r.saturating_sub(1);
                        cols = c;
                        body_rows = body;
                        mgr.resize_all(c, body);
                        let _ = term.autoresize();
                    }
                }
                // Animation tick: advance the braille spinner and rebuild the
                // spinner set from the hosts still connecting (the cursor's target).
                switcher.tick_spinner();
                let mut sp = HashSet::new();
                let tv = switcher.terminal_view_target();
                if !tv.target.is_empty() {
                    let connecting = match mgr.get(&tv.source) {
                        Some(c) => c.connecting.load(std::sync::atomic::Ordering::Acquire),
                        None => true,
                    };
                    if connecting {
                        sp.insert(spinner_key(&tv));
                    }
                }
                switcher.set_spinner(sp);
            }
        }
    }

    mgr.teardown_all();
    0
}

/// The spinner key for a terminal-view target, matching `Session::address()`
/// (`source/name`). A Window-row target carries `name:window`, so its session
/// part is everything before the first `:`; a session-row target has no `:` and
/// is the name verbatim. Without this the spinner key would never match the
/// switcher's session addresses and the spinner would never show.
fn spinner_key(tgt: &TerminalViewTarget) -> String {
    let name = tgt.target.split(':').next().unwrap_or(&tgt.target);
    format!("{}/{}", tgt.source, name)
}

/// Draws the Passthrough status bar on the last physical row: `host/session` plus
/// the active window/pane, in reverse video. Info only — no shortcuts.
fn draw_status_bar(
    f: &mut ratatui::Frame,
    mgr: &HostManager,
    tgt: &TerminalViewTarget,
) {
    use ratatui::style::{Modifier, Style};
    use ratatui::widgets::Paragraph;

    let area = f.area();
    if area.height == 0 {
        return;
    }
    let row = ratatui::layout::Rect::new(area.x, area.y + area.height - 1, area.width, 1);
    let active = mgr
        .get(&tgt.source)
        .and_then(|c| c.inventory.lock().unwrap().active_pane.clone());
    let text = match active {
        Some(pane) => format!(" {}/{}  ·  pane {pane} ", tgt.source, tgt.target),
        None => format!(" {}/{} ", tgt.source, tgt.target),
    };
    let bar = Paragraph::new(text).style(Style::default().add_modifier(Modifier::REVERSED));
    f.render_widget(bar, row);
}

/// After a key was handled in Overlay: take any queued create/rename/kill and run
/// it OFF the loop in a detached task, folding its result back through `op_tx`, so
/// a slow ssh round-trip never freezes rendering, host streaming, or the control
/// socket.
fn dispatch_pending_op(
    switcher: &mut crate::ui::switcher::Switcher,
    ops: &Arc<dyn crate::ui::switcher::Ops>,
    op_tx: &tokio::sync::mpsc::UnboundedSender<crate::ui::switcher::OpResult>,
) {
    if let Some(op) = switcher.take_pending_op() {
        let ops = ops.clone();
        let tx = op_tx.clone();
        tokio::spawn(async move {
            let result = crate::ui::switcher::run_op(&op, ops.as_ref()).await;
            let _ = tx.send(result);
        });
    }
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
    use super::*;
    use crate::config::Config;
    use crate::source::Source;
    use std::collections::HashMap;

    /// A LOCAL `Source` whose `control_argv()` is valid (`cmd.exe`), so `ensure`
    /// can spawn a throwaway control child that EOFs at once.
    fn fake_source(alias: &str) -> Source {
        Source {
            alias: alias.into(),
            binary: "cmd.exe".into(),
            remote: false,
            control_path: String::new(),
            os: "windows".into(),
            socket: None,
            runner: None,
        }
    }

    fn fake_env_with_source(alias: &str) -> Env {
        fake_env_with_sources(&[alias])
    }

    fn fake_env_with_sources(aliases: &[&str]) -> Env {
        let srcs: Vec<Source> = aliases.iter().map(|a| fake_source(a)).collect();
        let by_alias: HashMap<String, Source> = srcs
            .iter()
            .map(|s| (s.alias.clone(), s.clone()))
            .collect();
        Env {
            cfg: Config::default(),
            cfg_warnings: Vec::new(),
            srcs,
            by_alias,
            local_bin: "cmd.exe".into(),
            ui_prefix: "C-g".into(),
            xmux_dir: std::path::PathBuf::from("."),
        }
    }

    #[tokio::test]
    async fn select_attach_ensures_then_switches() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<HostEvent>();
        let mut mgr = HostManager::new(tx);
        mgr.insert_fake("jupiter06"); // pretend connected
        let env = fake_env_with_source("jupiter06");
        let tgt = TerminalViewTarget {
            source: "jupiter06".into(),
            target: "api".into(),
        };
        assert!(
            select_attach(&mut mgr, &env, &tgt, 80, 24),
            "select on a connected host queues a switch-client"
        );
        mgr.teardown_all();
    }

    #[tokio::test]
    async fn connect_all_sources_connects_every_host() {
        // Startup must connect EVERY source so each host's list-sessions streams
        // its tree in without a cursor move — the fix for hosts stuck on
        // "scanning…" until the user happened to focus them.
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<HostEvent>();
        let mut mgr = HostManager::new(tx);
        let env = fake_env_with_sources(&["jupiter06", "jupiter00", "local"]);
        connect_all_sources(&mut mgr, &env, 80, 24);
        assert!(mgr.get("jupiter06").is_some(), "jupiter06 connected at startup");
        assert!(mgr.get("jupiter00").is_some(), "jupiter00 connected at startup");
        assert!(mgr.get("local").is_some(), "local connected at startup");
        mgr.teardown_all();
    }

    #[tokio::test]
    async fn host_exited_before_connect_marks_unreachable() {
        use crate::ui::run::dump_overlay;
        use crate::ui::switcher::Switcher;
        use std::collections::HashSet;
        // A control client that dies before it ever connected must flip its host
        // from "scanning…" to "⚠ unreachable" so it does not spin forever.
        let mut switcher = Switcher::from_sources(vec!["jupiter00".into()]);
        let connected: HashSet<String> = HashSet::new();
        assert!(
            note_host_exited(&mut switcher, &connected, "jupiter00", Some("no route to host".into())),
            "a never-connected host is marked unreachable on exit"
        );
        let out = dump_overlay(&mut switcher, None, 80, 24);
        assert!(out.contains("unreachable"), "host reads unreachable:\n{out}");
        assert!(out.contains("no route to host"), "shows the exit reason:\n{out}");
        assert!(!out.contains("scanning"), "no longer scanning:\n{out}");
    }

    #[tokio::test]
    async fn host_exited_after_connect_keeps_tree() {
        use crate::ui::switcher::Switcher;
        use std::collections::HashSet;
        // A host that had connected keeps its last-known tree on a later exit —
        // it is NOT reset to unreachable.
        let mut switcher = Switcher::from_sources(vec!["jupiter06".into()]);
        let mut connected: HashSet<String> = HashSet::new();
        connected.insert("jupiter06".into());
        assert!(
            !note_host_exited(&mut switcher, &connected, "jupiter06", None),
            "an already-connected host is not marked unreachable on exit"
        );
    }

    #[test]
    fn spinner_key_is_the_session_address_for_a_window_target() {
        // A Window-row target carries `name:window`; its spinner key must be the
        // session address `source/name` (matching `Session::address()`), not
        // `source/name:window`, else the spinner never matches and never shows.
        let win_tgt = TerminalViewTarget {
            source: "jupiter06".into(),
            target: "api:2".into(),
        };
        assert_eq!(spinner_key(&win_tgt), "jupiter06/api");
        // A session-row target has no `:window` suffix — the name passes through.
        let sess_tgt = TerminalViewTarget {
            source: "jupiter06".into(),
            target: "api".into(),
        };
        assert_eq!(spinner_key(&sess_tgt), "jupiter06/api");
    }

    #[test]
    fn prefix_s_toggles_state() {
        use crate::proxy::app::{App, AppState};
        let mut app = App::new();
        assert!(app.is_overlay());
        app.toggle(); // C-g s from Overlay → Passthrough
        assert_eq!(app.state, AppState::Passthrough);
        app.toggle(); // C-g s from Passthrough → Overlay
        assert!(app.is_overlay());
    }

    // =========================================================================
    // HUMAN VISUAL-GATE CHECKLIST (run in a REAL terminal — never headless):
    // 1. Launch `xmux` in a real terminal. Confirm it enters the alternate screen
    //    cleanly (no pre-launch shell output bleeds under the UI) and starts in
    //    Overlay: the Host·Session·Window·Pane tree on the left, the live terminal
    //    view of the cursor's session on the right.
    // 2. Move the cursor between sessions. Confirm the right pane follows the
    //    cursor and goes live for the selected session (select = attach), with a
    //    braille spinner right of a session's name while its host is connecting.
    // 3. Press the prefix (C-g) then `s` — confirm it switches to fullscreen
    //    Passthrough: the selected session's screen fills the terminal and the last
    //    physical row shows a reverse-video status bar `host/session · pane %N`.
    // 4. Type into the session — confirm keystrokes reach the live pane.
    // 5. Press C-g then `s` again — confirm it returns to Overlay on the SAME
    //    session (the cursor and live view are unchanged).
    // 6. Press C-g then `q` (or `q` in Overlay) — confirm a clean quit and the
    //    terminal restored to its pre-launch screen.
    // 7. NEVER select or attach `local/xmux` (the live session) during this test —
    //    only the throwaway `jupiter06`. Attaching the live session from within
    //    itself would mirror xmux inside its own terminal view.
    // =========================================================================
}
