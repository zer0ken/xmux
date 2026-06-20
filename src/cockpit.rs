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

use ratatui::crossterm::event::KeyCode;

use crate::attach;
use crate::env::Env;
use crate::host::{HostManager, HostEvent};
use crate::ui::switcher::TerminalViewTarget;

/// Milliseconds per braille-spinner frame. The frame index is derived from
/// elapsed wall-clock time (see [`spinner_frame_at`]), not a per-tick counter, so
/// the spinner animates on every render and never freezes when the animation tick
/// starves under a `%output` flood.
const SPINNER_FRAME_MS: u64 = 120;

/// Max host events drained into one redraw before the loop yields back to
/// `select!` (servicing stdin, the control socket, ops, and the tick). Coalesces
/// a %output burst without letting a sustained flood monopolize the single thread.
const HOST_EVENT_DRAIN_BUDGET: usize = 512;

/// The braille-spinner frame index for `elapsed` since the cockpit started. Render
/// wraps it modulo the glyph count, so the raw count growing without bound is fine.
fn spinner_frame_at(elapsed: std::time::Duration) -> usize {
    (elapsed.as_millis() / SPINNER_FRAME_MS as u128) as usize
}

/// The size to give a host's grid + `refresh-client -C`: the terminal-view pane
/// (right of the tree + divider), NOT the whole terminal. Sizing a host to the
/// full terminal makes the remote wrap at a width wider than the visible pane, so
/// a line overflows the right edge (and a double-width char straddles the clip
/// boundary, wrapping to col 0 of the next line), and the bottom status row falls
/// outside the pane. The view width is `cols - TREE_WIDTH - 1` (tree + the single
/// divider rule); the view height is the body height. Both clamp to at least 1.
fn terminal_view_size(cols: u16, body_rows: u16) -> (u16, u16) {
    let view_cols = cols
        .saturating_sub(crate::ui::switcher::TREE_WIDTH + 1)
        .max(1);
    (view_cols, body_rows.max(1))
}

/// Ensures `tgt`'s host is connected (spawning its control client lazily) and
/// queues a `switch-client` to its session on that client. Returns `true` when a
/// switch was queued, `false` when the source is unknown or the client could not
/// be spawned. Extracted from the loop so the `select = attach` decision is
/// unit-testable without a real terminal. `cols`/`rows` are the terminal body
/// size; the host is sized to the terminal-view pane via [`terminal_view_size`].
fn select_attach(
    mgr: &mut HostManager,
    env: &Env,
    tgt: &TerminalViewTarget,
    cols: u16,
    rows: u16,
) -> bool {
    let (cols, rows) = terminal_view_size(cols, rows);
    let Some(src) = env.by_alias.get(&tgt.source) else {
        return false;
    };
    if src.control_per_session() {
        // Local psmux: one connection per session (PSMUX_SESSION_NAME), no
        // control-mode switch-client (psmux rejects it). The connection IS the
        // session; seed the grid with its current screen.
        let session = target_session(&tgt.target);
        let key = format!("{}/{}", tgt.source, session);
        let fresh = match mgr.ensure_session(&key, src, session, cols, rows) {
            Ok(fresh) => fresh,
            Err(_) => return false,
        };
        match mgr.get(&key) {
            Some(client) => {
                client.capture_screen(&tgt.target);
                // psmux has no control-mode switch-client, so the active pane is
                // never resolved by an attach probe. Resolve it once on connect so
                // terminal input has a pane to forward to (issue #6).
                if fresh {
                    client.probe_active_pane(session);
                }
                true
            }
            None => false,
        }
    } else {
        if mgr.ensure(&tgt.source, src, cols, rows).is_err() {
            return false;
        }
        match mgr.get(&tgt.source) {
            Some(client) => {
                client.switch_client(tgt.target.clone());
                // Seed the grid with this target's current screen: switch-client
                // alone does not repaint in control mode, so a static session would
                // stay blank (or show the previous session's content) without this.
                client.capture_screen(&tgt.target);
                true
            }
            None => false,
        }
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
    let (cols, rows) = terminal_view_size(cols, rows);
    if let Some(host) = switcher.current_host() {
        if let Some(src) = env.by_alias.get(&host) {
            // Per-session sources (local psmux) enumerate via plain commands and
            // open their control connections per session on select — there is no
            // useful host-level connection to ensure here.
            if !src.control_per_session() {
                let _ = mgr.ensure(&host, src, cols, rows);
            }
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
/// Processes a batch of TREE-focus input bytes through ONE path — used for both
/// real stdin and bytes replayed after a terminal→tree switch, so replayed input
/// behaves identically to input arriving in a later read. Handles prefix arming
/// (`C-g` then Tab/←/→ → focus terminal, `q` → quit), plain →/Enter/Tab → focus
/// terminal (unless an inline input is open), otherwise switcher navigation; then
/// the off-loop op dispatch, ensure/select, and the `r` re-scan. Returns
/// `(focus_terminal, quit)`.
#[allow(clippy::too_many_arguments)]
fn handle_tree_bytes(
    bytes: &[u8],
    tree_decoder: &mut crate::proxy::decode::KeyDecoder,
    tree_armed: &mut bool,
    prefix: u8,
    switcher: &mut crate::ui::switcher::Switcher,
    mgr: &mut HostManager,
    env: &Env,
    ops: &Arc<dyn crate::ui::switcher::Ops>,
    op_tx: &tokio::sync::mpsc::UnboundedSender<crate::ui::switcher::OpResult>,
    enum_tx: &tokio::sync::mpsc::UnboundedSender<LocalEnum>,
    panes_requested: &mut std::collections::HashSet<String>,
    cols: u16,
    rows: u16,
) -> (bool, bool) {
    let mut focus_terminal = false;
    let mut quit = false;
    for key in tree_decoder.feed(bytes) {
        if *tree_armed {
            *tree_armed = false;
            match key.code {
                KeyCode::Char('\t') | KeyCode::Left | KeyCode::Right => focus_terminal = true,
                KeyCode::Char('q') => quit = true,
                _ => {}
            }
            continue;
        }
        if !switcher.is_inputting() && key.code == KeyCode::Char(prefix as char) {
            *tree_armed = true;
            continue;
        }
        if !switcher.is_inputting()
            && matches!(key.code, KeyCode::Right | KeyCode::Enter | KeyCode::Char('\t'))
        {
            focus_terminal = true;
            continue;
        }
        switcher.handle_key(key);
    }
    dispatch_pending_op(switcher, ops, op_tx);
    ensure_current_host(mgr, env, switcher, cols, rows);
    if let Some(tgt) = switcher.current_attach_target() {
        select_attach(mgr, env, &tgt, cols, rows);
    }
    if switcher.take_rescan_kick() {
        // 'r' re-scan: re-enumerate per-session (local) sources and re-list
        // host-level (remote) ones; let panes re-request.
        panes_requested.clear();
        for src in &env.srcs {
            if src.control_per_session() {
                spawn_local_enumeration(src.clone(), enum_tx.clone());
            } else if let Some(c) = mgr.get(&src.alias) {
                c.list_sessions();
            }
        }
    }
    (focus_terminal, quit)
}

fn connect_all_sources(
    mgr: &mut HostManager,
    env: &Env,
    cols: u16,
    rows: u16,
    enum_tx: &tokio::sync::mpsc::UnboundedSender<LocalEnum>,
) {
    let (cols, rows) = terminal_view_size(cols, rows);
    for src in &env.srcs {
        if src.control_per_session() {
            // Per-session (local psmux): enumerate the tree with plain commands;
            // its control connections are opened per session on selection.
            spawn_local_enumeration(src.clone(), enum_tx.clone());
        } else {
            let _ = mgr.ensure(&src.alias, src, cols, rows);
        }
    }
}

/// A plain-enumeration result for a `control_per_session` source (local psmux):
/// its control connections are per-session and cannot enumerate the host, so the
/// tree is filled from a plain `list-sessions` + per-session `list-panes` run off
/// the loop and folded back through this channel.
enum LocalEnum {
    Sessions {
        source: String,
        sessions: Vec<crate::session::Session>,
        err: Option<String>,
    },
    Panes {
        address: String,
        panes: Vec<crate::session::WindowPanes>,
    },
}

/// The strip of a terminal-view target before any `:window` suffix — the session
/// name, which keys a per-session local connection and `capture-pane`.
fn target_session(target: &str) -> &str {
    target.split(':').next().unwrap_or(target)
}

/// The HostManager key for `source`'s connection to `session`: the source alias
/// for a host-level (tmux) connection, or `source/session` for a per-session
/// (local psmux) connection.
fn conn_key(env: &Env, source: &str, session: &str) -> String {
    match env.by_alias.get(source) {
        Some(s) if s.control_per_session() => format!("{source}/{session}"),
        _ => source.to_string(),
    }
}

/// The connection key for the cursor's current terminal-view target.
fn target_conn_key(env: &Env, tgt: &TerminalViewTarget) -> String {
    conn_key(env, &tgt.source, target_session(&tgt.target))
}

/// Enumerates a `control_per_session` source off the loop (plain `list-sessions`
/// then `list-panes` per session) and streams the results back through `tx`. A
/// control connection per session would each see only its own session, so the
/// tree must come from the plain, server-aggregating commands instead.
fn spawn_local_enumeration(
    src: crate::source::Source,
    tx: tokio::sync::mpsc::UnboundedSender<LocalEnum>,
) {
    let source = src.alias.clone();
    tokio::spawn(async move {
        let (sessions, err) = match src.list_sessions().await {
            Ok(s) => (s, None),
            Err(e) => (Vec::new(), Some(e.to_string())),
        };
        let addrs: Vec<(String, String)> = sessions
            .iter()
            .map(|s| (s.name.clone(), s.address()))
            .collect();
        let _ = tx.send(LocalEnum::Sessions { source, sessions, err });
        for (name, address) in addrs {
            if let Ok(panes) = crate::manage::panes(&src, &name).await {
                let _ = tx.send(LocalEnum::Panes { address, panes });
            }
        }
    });
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

/// Applies one [`HostEvent`] to the cockpit state (tree, connected set, spinner,
/// reap). Extracted so a burst of events can be drained in one loop iteration
/// (one redraw per burst), which keeps a `%output` flood from monopolizing the
/// single-threaded runtime and starving spawned ops / timers / the reactor.
fn handle_host_event(
    ev: HostEvent,
    mgr: &mut HostManager,
    switcher: &mut crate::ui::switcher::Switcher,
    connected: &mut std::collections::HashSet<String>,
    panes_requested: &mut std::collections::HashSet<String>,
) {
    match ev {
        HostEvent::Connected { host } | HostEvent::Inventory { host } => {
            connected.insert(host.clone());
            // A per-session local client (key `source/session`) only knows its OWN
            // session, so it must NOT rebuild the tree (the plain enumeration owns
            // it) — its events only feed the grid, redrawn on the next loop top. A
            // host-level (tmux) client owns its host's tree.
            if !host.contains('/') {
                if let Some(client) = mgr.get(&host) {
                    let sessions = {
                        let inv = client.inventory.lock().unwrap();
                        switcher.apply_source_result(host.clone(), inv.sessions.clone(), None);
                        for (addr, windows) in inv.panes.iter() {
                            switcher.apply_panes(addr.clone(), windows.clone());
                        }
                        inv.sessions.clone()
                    };
                    request_session_panes(client, &sessions, panes_requested);
                }
            }
        }
        HostEvent::Output { .. } => { /* redraw on the next loop top */ }
        HostEvent::Attached { .. } => {
            // A confirmed switch: the live grid is now feeding, so the spinner
            // clears. The next animation tick recomputes the connecting set.
            switcher.set_spinner(std::collections::HashSet::new());
        }
        HostEvent::WindowChanged { host } => {
            // The attached session's active window changed (another client switched
            // it). Probe the new active window index on a HOST-level (tmux) client;
            // the resulting Focus syncs the sidebar cursor. (Per-session local rows
            // come from the plain enumeration, not this client — skip them.)
            if !host.contains('/') {
                if let Some(client) = mgr.get(&host) {
                    let attached = client.inventory.lock().unwrap().attached_session.clone();
                    if let Some(session) = attached {
                        // Refresh the (active) window marker in the tree, then probe
                        // the new active window so the Focus event syncs the cursor.
                        let address = format!("{host}/{session}");
                        client.list_panes(&session, address);
                        client.probe_active_pane(session);
                    }
                }
            }
        }
        HostEvent::Focus { host, session, window } => {
            // Sync the sidebar cursor to the active window of the session the probe
            // was VALIDATED for (carried in the event — not re-read here, which
            // could have changed). select_window only moves when the cursor is on a
            // window row of that session, so it never yanks a user browsing
            // elsewhere or syncs the wrong session.
            if !host.contains('/') {
                if let Some(idx) = window {
                    switcher.select_window(&host, &session, idx);
                }
            }
        }
        HostEvent::Exited { host, reason } => {
            // A HOST-level client that dies before it ever connected is marked
            // unreachable so its host stops spinning on "scanning…". A per-session
            // local client (key `source/session`) is NOT a host — its tree is owned
            // by the plain enumeration, so marking it would inject a phantom row;
            // just reap it.
            if !host.contains('/') {
                note_host_exited(switcher, connected, &host, reason);
            }
            mgr.reap(&host);
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
    use crate::proxy::input::{TermAction, TermInput};
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

    // The prefix byte (default C-g). In tree focus, plain →/Enter/Tab move focus to
    // the terminal and C-g then Tab/←/→ toggles; in terminal focus, TermInput
    // intercepts C-g + a command key (Tab/←/→/Esc → tree, q → quit).
    let prefix = parse_prefix(Some(&env.ui_prefix));
    let mut term_input = TermInput::new(prefix);
    // Persistent across reads: the tree-focus key decoder and its prefix-arm flag,
    // so a split escape sequence or a C-g prefix sequence survives a read boundary.
    let mut tree_decoder = KeyDecoder::new();
    let mut tree_armed = false;

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

    // Plain-enumeration results for per-session (local psmux) sources.
    let (enum_tx, mut enum_rx) = tokio::sync::mpsc::unbounded_channel::<LocalEnum>();

    // Connect EVERY source up front so each control client's list-sessions
    // streams its host's tree in without waiting for a cursor move. `connected`
    // tracks the hosts that have reached a connected state, so a connection that
    // dies before it ever connected is marked unreachable instead of spinning on
    // "scanning…" forever.
    let mut connected: HashSet<String> = HashSet::new();
    // Session addresses whose list-panes has been requested, so a session's
    // subtree is asked for exactly once (repeat Inventory events don't re-issue).
    let mut panes_requested: HashSet<String> = HashSet::new();
    connect_all_sources(&mut mgr, &env, cols, body_rows, &enum_tx);

    // The braille spinner advances by wall-clock elapsed, not a per-tick counter,
    // so it animates on every render even when the animation tick starves under a
    // %output flood. A PERSISTENT interval (not a `sleep` recreated each loop
    // iteration, whose deadline would reset every time the biased select lets
    // host_rx win) drives idle redraws + resize polling on an absolute schedule.
    let spinner_start = std::time::Instant::now();
    let mut tick = tokio::time::interval(Duration::from_millis(SPINNER_FRAME_MS));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        // Advance the spinner from wall-clock so it animates regardless of which
        // select arm fired.
        switcher.set_spinner_frame(spinner_frame_at(spinner_start.elapsed()));
        // Always draw the split: the tree (left) and the cursor session's live
        // grid (right), with the divider colored to mark the focused side. The
        // app state selects focus + key routing, not what is drawn.
        let tgt = switcher.terminal_view_target();
        let tgt_key = target_conn_key(&env, &tgt);
        let grid_arc = (!tgt.target.is_empty())
            .then(|| mgr.get(&tgt_key).map(|c| c.grid.clone()))
            .flatten();
        let terminal_focused = !app.is_overlay();
        let _ = match &grid_arc {
            Some(g) => {
                let guard = g.lock().ok();
                term.draw(|f| switcher.render(f, guard.as_deref(), terminal_focused))
            }
            None => term.draw(|f| switcher.render(f, None, terminal_focused)),
        };

        // The renderer has now caught up, so it is safe to resume any panes the
        // server paused for flow control. Snapshot + clear the set under a brief
        // lock, then resume each outside the lock (never across an `.await`).
        if let Some(client) = mgr.get(&tgt_key) {
            let paused: Vec<String> = {
                let mut inv = client.inventory.lock().unwrap();
                inv.paused_panes.drain().collect()
            };
            for pane in &paused {
                client.resume_pane(pane);
            }
        }

        tokio::select! {
            biased;
            Some(ev) = host_rx.recv() => {
                handle_host_event(ev, &mut mgr, &mut switcher, &mut connected, &mut panes_requested);
                // Coalesce the rest of an in-flight burst into ONE redraw: a busy
                // session can flood %output, and a redraw per event would make the
                // single-threaded runtime never yield, starving spawned ops (the 6s
                // new-window timeout), the animation tick, and resize polling. The
                // drain is BUDGET-BOUNDED, not `while empty`: the reader OS threads
                // can keep an unbounded channel non-empty under a sustained flood, so
                // an unbounded drain would itself block stdin/ctl/the tick. After the
                // budget we redraw and re-enter select, then resume draining.
                let mut budget = HOST_EVENT_DRAIN_BUDGET;
                while budget > 0 {
                    match host_rx.try_recv() {
                        Ok(ev) => {
                            handle_host_event(ev, &mut mgr, &mut switcher, &mut connected, &mut panes_requested);
                            budget -= 1;
                        }
                        Err(_) => break,
                    }
                }
            }
            Some(bytes) = stdin_rx.recv() => {
                let mut quit = false;
                let mut focus_terminal = false;
                let mut focus_tree = false;
                // Bytes that followed a terminal→tree switch command in the same
                // read; replayed into the tree after focus flips (not lost).
                let mut tree_replay: Vec<u8> = Vec::new();
                if app.is_overlay() {
                    // TREE focus: one shared path (also used by the replay below).
                    let (ft, q) = handle_tree_bytes(
                        &bytes, &mut tree_decoder, &mut tree_armed, prefix, &mut switcher,
                        &mut mgr, &env, &ops, &op_tx, &enum_tx, &mut panes_requested,
                        cols, body_rows,
                    );
                    focus_terminal = ft;
                    quit = q;
                } else {
                    // TERMINAL focus: forward raw bytes to the session's active pane;
                    // TermInput intercepts the prefix (→ tree / quit / literal prefix).
                    for action in term_input.feed(&bytes) {
                        match action {
                            TermAction::Forward(f) => {
                                let tv = switcher.terminal_view_target();
                                if let Some(client) = mgr.get(&target_conn_key(&env, &tv)) {
                                    let pane = client.inventory.lock().unwrap().active_pane.clone();
                                    if let Some(pane) = pane {
                                        client.send_keys(pane, f);
                                    }
                                }
                            }
                            TermAction::FocusTree(rest) => {
                                focus_tree = true;
                                tree_replay = rest;
                            }
                            TermAction::Quit => quit = true,
                        }
                    }
                }
                if focus_terminal && app.is_overlay() {
                    app.toggle();
                    let _ = term.clear();
                }
                if focus_tree && !app.is_overlay() {
                    app.toggle();
                    let _ = term.clear();
                    // Replay the bytes that trailed the switch command through the
                    // SAME tree path, so they behave exactly as a later read would
                    // (prefix arming, plain →/Enter/Tab → terminal, etc.).
                    if !tree_replay.is_empty() {
                        let (ft, q) = handle_tree_bytes(
                            &tree_replay, &mut tree_decoder, &mut tree_armed, prefix,
                            &mut switcher, &mut mgr, &env, &ops, &op_tx, &enum_tx,
                            &mut panes_requested, cols, body_rows,
                        );
                        if ft && app.is_overlay() {
                            app.toggle(); // a replayed →/Enter/Tab flips straight back
                            let _ = term.clear();
                        }
                        quit = quit || q;
                    }
                }
                if quit || switcher.wants_quit() {
                    break;
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
                            .then(|| mgr.get(&target_conn_key(&env, &tgt)).map(|c| c.grid.clone()))
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
                        // Headless key injection: forward the bytes straight to the
                        // selected session's active pane (the automation path doesn't
                        // intercept the prefix).
                        if !bytes.is_empty() {
                            let tv = switcher.terminal_view_target();
                            if let Some(client) = mgr.get(&target_conn_key(&env, &tv)) {
                                let pane = client
                                    .inventory
                                    .lock()
                                    .unwrap()
                                    .active_pane
                                    .clone();
                                if let Some(pane) = pane {
                                    client.send_keys(pane, bytes);
                                }
                            }
                        }
                    }
                }
            }
            Some(result) = op_rx.recv() => {
                switcher.apply_op_result(result);
            }
            Some(le) = enum_rx.recv() => {
                // Plain-enumeration results for a per-session (local psmux) source
                // fill its tree (its per-session control clients can't enumerate).
                match le {
                    LocalEnum::Sessions { source, sessions, err } => {
                        switcher.apply_source_result(source, sessions, err);
                    }
                    LocalEnum::Panes { address, panes } => {
                        switcher.apply_panes(address, panes);
                    }
                }
                // A host-level client auto-attaches its session on connect, so its
                // grid fills without help; a per-session (local) target has no client
                // until selected, so once enumeration preselects one, attach it now —
                // otherwise the preselected local session's pane would read
                // "(attaching…)" until the first cursor move.
                if let Some(tgt) = switcher.current_attach_target() {
                    if mgr.get(&target_conn_key(&env, &tgt)).is_none() {
                        select_attach(&mut mgr, &env, &tgt, cols, body_rows);
                    }
                }
            }
            _ = tick.tick() => {
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
                        let (vc, vr) = terminal_view_size(c, body);
                        mgr.resize_all(vc, vr);
                        let _ = term.autoresize();
                    }
                }
                // Rebuild the spinner set from the hosts still connecting (the
                // cursor's target); the frame itself advances from wall-clock at the
                // loop top, so the spinner animates even between ticks.
                let mut sp = HashSet::new();
                let tv = switcher.terminal_view_target();
                if !tv.target.is_empty() {
                    let connecting = match mgr.get(&target_conn_key(&env, &tv)) {
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
        // fake sources use the cmd.exe binary (not psmux), so they take the
        // host-level connect path; the enum channel is unused here.
        let (enum_tx, _enum_rx) = tokio::sync::mpsc::unbounded_channel();
        let env = fake_env_with_sources(&["jupiter06", "jupiter00", "local"]);
        connect_all_sources(&mut mgr, &env, 80, 24, &enum_tx);
        assert!(mgr.get("jupiter06").is_some(), "jupiter06 connected at startup");
        assert!(mgr.get("jupiter00").is_some(), "jupiter00 connected at startup");
        assert!(mgr.get("local").is_some(), "local connected at startup");
        mgr.teardown_all();
    }

    #[test]
    fn spinner_frame_advances_with_wall_clock() {
        use std::time::Duration;
        // The spinner frame is derived from elapsed wall-clock time, NOT a per-tick
        // counter, so it animates smoothly on every render even when the 250ms tick
        // arm starves under a %output flood (the freeze/stutter bug).
        assert_eq!(spinner_frame_at(Duration::from_millis(0)), 0);
        assert_eq!(spinner_frame_at(Duration::from_millis(SPINNER_FRAME_MS)), 1);
        assert_eq!(spinner_frame_at(Duration::from_millis(SPINNER_FRAME_MS * 3 + 10)), 3);
    }

    #[test]
    fn terminal_view_size_subtracts_tree_and_divider() {
        use crate::ui::switcher::TREE_WIDTH;
        // The host grid + refresh-client must be sized to the terminal-view pane
        // (right of the tree + divider), not the full terminal — else the remote
        // wraps at the wrong width and a wide char straddles the clip boundary.
        let (vc, vr) = terminal_view_size(143, 39);
        assert_eq!(vc, 143 - (TREE_WIDTH + 1), "cols minus tree minus divider");
        assert_eq!(vr, 39, "rows pass through (the body height)");
    }

    #[test]
    fn terminal_view_size_clamps_to_at_least_one() {
        // A terminal narrower/shorter than the tree must never size a host to 0.
        let (vc, vr) = terminal_view_size(10, 0);
        assert_eq!(vc, 1);
        assert_eq!(vr, 1);
    }

    #[test]
    fn target_session_strips_window_suffix() {
        assert_eq!(target_session("api"), "api");
        assert_eq!(target_session("api:2"), "api");
    }

    #[test]
    fn conn_key_is_per_session_for_psmux_else_host() {
        let psmux = Source {
            alias: "local".into(),
            binary: "psmux".into(),
            remote: false,
            control_path: String::new(),
            os: "windows".into(),
            socket: None,
            runner: None,
        };
        let tmux = Source {
            alias: "jup".into(),
            binary: "tmux".into(),
            remote: true,
            control_path: String::new(),
            os: "linux".into(),
            socket: None,
            runner: None,
        };
        let by_alias: HashMap<String, Source> = [
            ("local".to_string(), psmux.clone()),
            ("jup".to_string(), tmux.clone()),
        ]
        .into_iter()
        .collect();
        let env = Env {
            cfg: Config::default(),
            cfg_warnings: Vec::new(),
            srcs: vec![psmux, tmux],
            by_alias,
            local_bin: "psmux".into(),
            ui_prefix: "C-g".into(),
            xmux_dir: std::path::PathBuf::from("."),
        };
        // Local psmux → one connection per session; remote tmux → one per host.
        assert_eq!(conn_key(&env, "local", "0"), "local/0");
        assert_eq!(conn_key(&env, "jup", "api"), "jup");
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
