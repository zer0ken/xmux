//! The cockpit: a persistent supervisor that owns the terminal for the whole
//! session. It keeps ONE real attached mux client per session — a `tmux attach` /
//! `psmux attach` running inside a `portable-pty` PTY ([`AttachRegistry`]) — alive
//! across selections, and renders the SELECTED session's live `Grid` on the right.
//! A separate control-mode client per remote host ([`HostManager`]) supplies the
//! sidebar inventory, mux-side change events, and programmatic `select-window`;
//! local psmux is enumerated/polled with plain commands (it is one-server-per-
//! session, so a host-level control client cannot see across its sessions).
//!
//! State is explicit: [`Selection`] (the canonical `source`/`session`/`window`) is
//! the single source of truth the display reads — the `Switcher` owns only the tree
//! and cursor. One `select!` loop interleaves stdin, host events, PTY events, the
//! control socket, terminal resize, and an animation tick. ratatui owns stdout in
//! both states: Overlay draws the switcher + the selected PTY grid; Passthrough
//! draws that grid fullscreen plus a status bar.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use ratatui::crossterm::event::KeyCode;

use crate::attach;
use crate::display::{DisplayEnsure, DisplayEvent, DisplayWorker};
use crate::env::Env;
use crate::host::{HostEvent, HostManager};
use crate::proxy::registry::AttachRegistry;
use crate::proxy::run::PtyEvent;
use crate::ui::switcher::TerminalViewTarget;

/// Milliseconds per braille-spinner frame. The frame index is derived from
/// elapsed wall-clock time (see [`spinner_frame_at`]), not a per-tick counter, so
/// the spinner animates on every render and never freezes when the animation tick
/// starves under a PTY-output flood.
const SPINNER_FRAME_MS: u64 = 120;

/// Max events (host or PTY) drained into one redraw before the loop yields back to
/// `select!`. Coalesces an output burst without letting a sustained flood
/// monopolize the single thread.
const EVENT_DRAIN_BUDGET: usize = 512;

/// How often local psmux sources are re-enumerated (no host-level control stream
/// exists for one-server-per-session psmux, so new/removed sessions and window/pane
/// structure changes are discovered by polling). Local commands are instant, so a
/// brisk cadence keeps the sidebar in sync without meaningful cost.
const LOCAL_POLL_MS: u64 = 1500;

/// Minimum interval between redraws. Drawing is decoupled from events and capped
/// to this frame rate: rapid input (or a busy PTY) sets a `dirty` flag, and the
/// loop redraws at most once per frame — so no navigation pattern can flood the
/// terminal with full-screen repaints and stall the single-threaded loop. A frame
/// timer at this cadence flushes a pending dirty draw promptly even with no input.
const FRAME_MS: u64 = 33;

/// Debounce before a cursor move attaches/switches its session+window. Rapid
/// navigation must NOT switch-client / select-window per step: each switch makes the
/// remote mux send a full-screen repaint, and a storm of repaints floods the draw —
/// the single-threaded loop then spends all its time redrawing, which IS the freeze.
/// Deferring the attach until the cursor settles keeps per-step redraws to a cheap
/// tree-only diff. Re-checked on the spinner tick, so it fires even with no further input.
const ATTACH_DEBOUNCE_MS: u64 = 90;

/// How often the reconnect sweep runs: re-ensures a died remote control client and
/// re-attaches the selected session's PTY if it dropped. Doubles as the retry
/// backoff so a genuinely-down host is retried at this cadence, never hot-looped.
const RECONNECT_MS: u64 = 2000;

/// Appends a diagnostic line to `<xmux_dir>/debug.log` when `XMUX_DEBUG` is set in
/// the environment. A no-op otherwise, so it costs nothing in normal runs. Used to
/// trace the live attach/selection/inventory flow that headless tests cannot reach.
fn dbg_log(dir: &std::path::Path, msg: &str) {
    if std::env::var_os("XMUX_DEBUG").is_none() {
        return;
    }
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("debug.log"))
    {
        let _ = writeln!(f, "{msg}");
    }
}

/// Formats one slow-step debug line: `SLOW <label> <ms>ms`. Kept separate from
/// [`dbg_ms`] so the line format is unit-testable without timing or filesystem I/O.
fn slow_log_line(label: &str, ms: u128) -> String {
    format!("SLOW {label} {ms}ms")
}

/// Logs `label` to the debug log when a synchronous step took at least 10ms — used
/// to locate what stalls the single-threaded event loop during rapid navigation
/// (a no-op unless `XMUX_DEBUG` is set, like [`dbg_log`]).
fn dbg_ms(dir: &std::path::Path, label: &str, start: std::time::Instant) {
    let ms = start.elapsed().as_millis();
    if ms >= 10 {
        dbg_log(dir, &slow_log_line(label, ms));
    }
}

/// The canonical selection — the single source of truth the display reads. The
/// `Switcher` owns the tree + cursor; the cockpit commits the cursor's target into
/// this struct, and the render, input routing, and spinner all key off it. `window`
/// is `Some` only for a window-row selection.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Selection {
    pub source: String,
    /// Empty ⇒ no terminal view (cursor on a host/loading row).
    pub session: String,
    pub window: Option<i64>,
}

impl Selection {
    /// Derives the selection from the switcher's current terminal-view target. The
    /// target is either `session` or `session:window`; the session part keys the
    /// PTY attachment, the optional window part drives `select-window`.
    fn from_target(t: &TerminalViewTarget) -> Self {
        if t.target.is_empty() {
            return Selection::default();
        }
        let session = crate::mux::session_name(&t.target).to_string();
        let window = t.target.split(':').nth(1).and_then(|w| w.parse::<i64>().ok());
        Selection {
            source: t.source.clone(),
            session,
            window,
        }
    }

    /// The `AttachRegistry` key — `source/session`, matching `Session::address()`.
    pub fn address(&self) -> String {
        format!("{}/{}", self.source, self.session)
    }

    pub fn is_empty(&self) -> bool {
        self.session.is_empty()
    }
}

/// The size to give a PTY attachment: the terminal-view pane (right of the tree +
/// divider), NOT the whole terminal. Sizing a session to the full terminal makes
/// the remote wrap at a width wider than the visible pane, so a line overflows the
/// right edge (and a double-width char straddles the clip boundary). The view width
/// is `cols - TREE_WIDTH - 1` (tree + the single divider rule); the view height is
/// the body height. Both clamp to at least 1.
fn terminal_view_size(cols: u16, body_rows: u16) -> (u16, u16) {
    let view_cols = cols
        .saturating_sub(crate::ui::switcher::TREE_WIDTH + 1)
        .max(1);
    (view_cols, body_rows.max(1))
}

/// The `AttachRegistry` key for a selection. REMOTE tmux keeps ONE PTY per HOST
/// (keyed by the source) and moves it between the host's sessions with
/// `switch-client`; LOCAL psmux is one-server-per-session, so it keeps one PTY per
/// SESSION (keyed by `source/session`).
fn display_key(env: &Env, sel: &Selection) -> String {
    match env.by_alias.get(&sel.source) {
        Some(s) if s.remote => sel.source.clone(),
        _ => sel.address(),
    }
}

/// Issues an OFF-LOOP attach for `key`: allocates the attachment id, records the request's
/// seq in `in_flight`, and asks the worker to spawn. The worker's `Ready` reply (handled in
/// the cockpit loop) inserts the finished attachment into the registry.
#[allow(clippy::too_many_arguments)]
fn request_attach(
    registry: &mut AttachRegistry,
    worker: &DisplayWorker,
    in_flight: &mut std::collections::HashMap<String, u64>,
    attach_seq: &mut u64,
    key: &str,
    argv: Vec<String>,
    cols: u16,
    rows: u16,
) {
    let id = registry.alloc_id();
    *attach_seq += 1;
    in_flight.insert(key.to_string(), *attach_seq);
    worker.ensure(DisplayEnsure {
        seq: *attach_seq,
        key: key.to_string(),
        argv,
        cols,
        rows,
        id,
    });
}

/// Makes the SELECTED session live in its host's display terminal and lands it on
/// the selected window. Returns `true` when the selection has a session to show.
///
/// REMOTE tmux: one kept PTY per host (`tmux attach`). The first attach lands on the
/// selected session; selecting a DIFFERENT session of the same host `switch-client`s
/// that one client over to it (`host_session` tracks where it is, to skip redundant
/// switches). LOCAL psmux: one kept PTY per session (one-server-per-session). In both
/// cases a window-row selection moves the session's active window server-side, which
/// the real attached client follows.
#[allow(clippy::too_many_arguments)]
fn select_attach(
    registry: &mut AttachRegistry,
    mgr: &mut HostManager,
    env: &Env,
    sel: &Selection,
    host_session: &mut std::collections::HashMap<String, String>,
    worker: &DisplayWorker,
    in_flight: &mut std::collections::HashMap<String, u64>,
    attach_seq: &mut u64,
    cols: u16,
    rows: u16,
) -> bool {
    if sel.is_empty() {
        return false;
    }
    let (cols, rows) = terminal_view_size(cols, rows);
    let Some(src) = env.by_alias.get(&sel.source) else {
        return false;
    };
    let key = display_key(env, sel);
    let already = registry.contains(&key);

    if src.remote {
        if !already {
            // Off-loop: request the spawn on the worker thread (skip if one is already in
            // flight for this key). The Ready reply inserts the finished attachment.
            if !in_flight.contains_key(&key) {
                let argv = src.attach_command(&sel.session, sel.window);
                request_attach(registry, worker, in_flight, attach_seq, &key, argv, cols, rows);
            }
            host_session.insert(sel.source.clone(), sel.session.clone());
        } else if host_session.get(&sel.source) != Some(&sel.session) {
            // The host's PTY is on a different session — switch that one client over.
            // Wipe the grid first so the previous session's cells do not linger as
            // residue: switch-client triggers a FULL client redraw, which refills the
            // cleared grid with the new session's content (a brief blank, not stale
            // colours/glyphs). The per-host PTY reuses ONE grid across sessions, so
            // without this the old session's uncovered cells stay on screen.
            let t = std::time::Instant::now();
            registry.clear_grid(&key);
            dbg_ms(&env.xmux_dir, "clear_grid", t);
            let cmd = src.switch_client_remote_cmd(&sel.session);
            let src2 = src.clone();
            tokio::spawn(async move {
                let _ = src2.run_raw(&cmd).await;
            });
            host_session.insert(sel.source.clone(), sel.session.clone());
        }
    } else {
        // LOCAL psmux: one PTY per session — off-loop attach, deduped by in-flight + contains.
        if !already && !in_flight.contains_key(&key) {
            let argv = src.attach_command(&sel.session, None);
            request_attach(registry, worker, in_flight, attach_seq, &key, argv, cols, rows);
        }
    }
    dbg_log(
        &env.xmux_dir,
        &format!("select_attach key={key} already={already} remote={} sess={}", src.remote, sel.session),
    );

    // Window-row selection → move the session's active window. The fresh remote
    // attach above already folded the window in; otherwise switch it server-side.
    if let Some(win) = sel.window {
        let folded_into_attach = !already && src.remote;
        if !folded_into_attach {
            let target = crate::mux::window_target(&sel.session, win);
            if src.remote {
                if mgr.ensure(&sel.source, src, cols, rows).is_ok() {
                    if let Some(client) = mgr.get(&sel.source) {
                        client.select_window_on(&target);
                    }
                }
            } else {
                let src2 = src.clone();
                let argv = crate::mux::select_window(&src2.binary, &target);
                tokio::spawn(async move {
                    let _ = src2.run(&argv).await;
                });
            }
        }
    }
    true
}

/// Keeps a source's display terminals in sync with its sessions. REMOTE tmux keeps
/// ONE PTY per host: warm it on the first session (later session selections
/// `switch-client` it), and reap it when the host has no sessions left. LOCAL psmux
/// (one-server-per-session) keeps one PTY per session: ensure each, reap the closed.
/// Called whenever a source's inventory updates (a remote `%`-event refresh or a
/// local poll), so a new session is reachable and a killed one is torn down (#5).
fn sync_source_terminals(
    registry: &mut AttachRegistry,
    env: &Env,
    source: &str,
    sessions: &[crate::session::Session],
    host_session: &mut std::collections::HashMap<String, String>,
    cols: u16,
    rows: u16,
) {
    let Some(src) = env.by_alias.get(source) else {
        return;
    };
    let (cols, rows) = terminal_view_size(cols, rows);
    if src.remote {
        // One PTY per host. Warm it on the first session if not yet attached; reap it
        // (and forget its session) when the host has no sessions.
        match sessions.first() {
            Some(first) if !registry.contains(source) => {
                let t = std::time::Instant::now();
                let _ = registry.ensure(source, &src.attach_command(&first.name, None), cols, rows);
                dbg_ms(&env.xmux_dir, "sync.ensure", t);
                host_session.insert(source.to_string(), first.name.clone());
            }
            None => {
                registry.remove(source);
                host_session.remove(source);
            }
            _ => {}
        }
        return;
    }
    // LOCAL psmux: one PTY per session; ensure each + reap closed.
    let mut desired: HashSet<String> = HashSet::new();
    for s in sessions {
        let addr = s.address();
        desired.insert(addr.clone());
        let t = std::time::Instant::now();
        let _ = registry.ensure(&addr, &src.attach_command(&s.name, None), cols, rows);
        dbg_ms(&env.xmux_dir, "sync.ensure", t);
    }
    for addr in addresses_to_reap(&registry.addresses(), &desired, source) {
        registry.remove(&addr);
    }
}

/// The attached addresses belonging to `source` that are no longer in `desired`
/// (their sessions closed). Pure so the reap selection is unit-testable. An address
/// is `source/session`; it belongs to `source` iff it starts with `source/`.
fn addresses_to_reap(existing: &[String], desired: &HashSet<String>, source: &str) -> Vec<String> {
    let prefix = format!("{source}/");
    existing
        .iter()
        .filter(|a| a.starts_with(&prefix) && !desired.contains(*a))
        .cloned()
        .collect()
}

/// True when a worker `Ready`/`Failed` reply is still the latest in-flight request for its
/// key. A stale reply (the key was re-requested after a reap, so a newer seq is in flight, or
/// the key is no longer in flight at all) must not register or clear state.
fn attach_reply_is_current(in_flight: &std::collections::HashMap<String, u64>, key: &str, seq: u64) -> bool {
    in_flight.get(key) == Some(&seq)
}

/// Connects the host the cursor is on (if not already), so its control-mode
/// client's `list-sessions` streams that host's tree in. A control-mode client is
/// the only source of a REMOTE host's session list; local psmux enumerates via
/// plain commands and has no host-level connection to ensure.
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
            if src.remote {
                let _ = mgr.ensure(&host, src, cols, rows);
            }
        }
    }
}

/// Connects EVERY remote source's control client and kicks off EVERY local source's
/// enumeration at startup, so each host's tree streams in without waiting for a
/// cursor move. Remote control clients connect on their own reader/writer threads
/// (this returns at once); local sources enumerate off the loop via the enum
/// channel. PTYs are attached as each source's sessions arrive (see
/// [`sync_source_terminals`]).
fn connect_all_sources(
    mgr: &mut HostManager,
    env: &Env,
    cols: u16,
    rows: u16,
    enum_tx: &tokio::sync::mpsc::UnboundedSender<LocalEnum>,
) {
    let (cols, rows) = terminal_view_size(cols, rows);
    for src in &env.srcs {
        if src.remote {
            let _ = mgr.ensure(&src.alias, src, cols, rows);
        } else {
            spawn_local_enumeration(src.clone(), enum_tx.clone());
        }
    }
}

/// Processes a batch of TREE-focus input bytes through ONE path — used for both real
/// stdin and bytes replayed after a terminal→tree switch. Handles prefix arming
/// (`C-g` then Tab/←/→ → focus terminal, `q` → quit), plain →/Enter/Tab → focus
/// terminal (unless an inline input is open), otherwise switcher navigation; then
/// the off-loop op dispatch, ensure-current-host, and the `r` re-scan. Returns
/// `(focus_terminal, quit)`. The selection is committed at the loop top, so this
/// only drives navigation + metadata, not the display.
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
    cols: u16,
    rows: u16,
) -> (bool, bool) {
    let mut focus_terminal = false;
    let mut quit = false;
    for key in tree_decoder.feed(bytes) {
        if *tree_armed {
            *tree_armed = false;
            // After the prefix: Tab focuses the terminal, q quits. Arrows have NO
            // function after the prefix (←/→ are tree navigation, handled below).
            match key.code {
                KeyCode::Char('\t') => focus_terminal = true,
                KeyCode::Char('q') => quit = true,
                _ => {}
            }
            continue;
        }
        if !switcher.is_inputting() && key.code == KeyCode::Char(prefix as char) {
            *tree_armed = true;
            continue;
        }
        // Enter/Tab focus the terminal pane. ←/→ are NOT focus keys: they navigate the
        // tree (→ descend to a child, ← ascend to the parent) in `switcher.handle_key`.
        if !switcher.is_inputting() && matches!(key.code, KeyCode::Enter | KeyCode::Char('\t')) {
            focus_terminal = true;
            continue;
        }
        switcher.handle_key(key);
    }
    dispatch_pending_op(switcher, ops, op_tx);
    ensure_current_host(mgr, env, switcher, cols, rows);
    if switcher.take_rescan_kick() {
        // 'r' re-scan: re-enumerate local sources and re-list remote ones.
        for src in &env.srcs {
            if src.remote {
                if let Some(c) = mgr.get(&src.alias) {
                    c.list_sessions();
                }
            } else {
                spawn_local_enumeration(src.clone(), enum_tx.clone());
            }
        }
    }
    (focus_terminal, quit)
}

/// A plain-enumeration result for a local psmux source: its tree is filled from a
/// plain `list-sessions` + per-session `list-panes` run off the loop and folded
/// back through this channel (a one-server-per-session mux has no host-level
/// control stream to enumerate or push changes).
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

/// Enumerates a local source off the loop (plain `list-sessions` then `list-panes`
/// per session) and streams the results back through `tx`.
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
/// it must be asked, once per session (`requested` dedupes repeat Inventory events).
fn request_session_panes(
    client: &crate::host::HostClient,
    sessions: &[crate::session::Session],
    requested: &mut HashSet<String>,
) {
    for s in sessions {
        let addr = s.address();
        if requested.insert(addr.clone()) {
            client.list_panes(&s.name, addr);
        }
    }
}

/// Refetches a host's inventory after a `%`-change notification: clears its
/// pane-request dedup so every session re-lists, then re-runs list-sessions — its
/// reply (Connected/Inventory) re-applies the tree, re-requests panes, and re-syncs
/// the PTY set (a new session attaches, a closed one is reaped). #5 sidebar sync.
fn refetch_host(mgr: &HostManager, panes_requested: &mut HashSet<String>, host: &str) {
    if let Some(client) = mgr.get(host) {
        let prefix = format!("{host}/");
        panes_requested.retain(|a| !a.starts_with(&prefix));
        client.list_sessions();
    }
}

/// Applies one [`HostEvent`] to the cockpit state (tree, connected set, PTY sync,
/// reap). Extracted so a burst of events can be drained in one loop iteration.
#[allow(clippy::too_many_arguments)]
fn handle_host_event(
    ev: HostEvent,
    mgr: &mut HostManager,
    registry: &mut AttachRegistry,
    switcher: &mut crate::ui::switcher::Switcher,
    env: &Env,
    connected: &mut HashSet<String>,
    panes_requested: &mut HashSet<String>,
    host_session: &mut std::collections::HashMap<String, String>,
    cols: u16,
    rows: u16,
    overlay: bool,
) {
    match ev {
        HostEvent::Connected { host } | HostEvent::Inventory { host } => {
            connected.insert(host.clone());
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
                dbg_log(
                    &env.xmux_dir,
                    &format!(
                        "host event host={host} n={} names={:?}",
                        sessions.len(),
                        sessions.iter().map(|s| &s.name).collect::<Vec<_>>()
                    ),
                );
                // Sync this host's display terminal(s) (per-host for remote tmux).
                sync_source_terminals(registry, env, &host, &sessions, host_session, cols, rows);
            }
        }
        HostEvent::Changed { host } => {
            // The server's session/window structure changed (a `%`-notification).
            // Refetch so the tree, panes, and PTY set resync (#5 sidebar sync).
            refetch_host(mgr, panes_requested, &host);
        }
        HostEvent::WindowChanged { host } => {
            // A session's ACTIVE WINDOW switched — the structure did NOT change, so do
            // NOT refetch the whole inventory: a full list-sessions + per-session
            // list-panes per change storms the single-threaded loop and freezes the UI
            // during rapid window navigation (each tree step issues select-window,
            // which echoes back as this notification). Instead probe ONLY the displayed
            // session's new active window; the reply (Focus) updates the marker and
            // follows the cursor without any refetch.
            if let (Some(client), Some(displayed)) = (mgr.get(&host), host_session.get(&host)) {
                client.probe_active_window(displayed);
            }
        }
        HostEvent::Focus { host, session, window } => {
            // The active-window probe resolved. ALWAYS move the bold+italic marker to
            // the new active window. Follow the CURSOR only when the terminal pane has
            // focus (an external change, e.g. prefix-n in the pane): in TREE focus the
            // user is driving the cursor, and this notification is the echo of their
            // own select-window navigation — a (possibly stale/lagged) reply would
            // yank the cursor backward and fight their navigation.
            switcher.set_active_window(&host, &session, window);
            if !overlay {
                switcher.select_window(&host, &session, window);
            }
        }
        HostEvent::Exited { host, reason } => {
            note_host_exited(switcher, connected, &host, reason);
            mgr.reap(&host);
        }
    }
}

/// Handles a remote host's control client dying. If the host never reached a
/// connected state, marks it unreachable in the switcher so it renders
/// "⚠ unreachable" instead of spinning on "scanning…"; a host that had connected
/// keeps its last-known tree. Returns `true` when it marked the host unreachable.
fn note_host_exited(
    switcher: &mut crate::ui::switcher::Switcher,
    connected: &HashSet<String>,
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

/// The `xmux` (no subcommand) entry: the persistent cockpit. Keeps one real attached
/// mux client per session alive and renders the selected one, with a control-mode
/// client per remote host for inventory/events/window-switch. It serves a picker
/// control socket so a headless driver can inject keys/text and dump the screen.
pub async fn run_cockpit(env: Arc<Env>) -> i32 {
    use crate::proxy::app::App;
    use crate::proxy::decode::KeyDecoder;
    use crate::proxy::input::{TermAction, TermInput};
    use crate::proxy::term::{parse_prefix, TermGuard};
    use crate::ui::run::{dump_overlay, serve_control, AppStateKind, Cmd};
    use crate::ui::switcher::Switcher;
    use std::io::Read;
    use std::time::Duration;

    // The cockpit owns the terminal and attaches mux clients as PTY children; nested
    // inside a mux every attach is refused. So running it inside a mux is refused.
    if let Err(e) = attach::nest_guard(attach::in_mux()) {
        eprintln!("xmux: {e}");
        eprintln!("xmux: the cockpit must be your terminal entry, not run inside a mux.");
        return 2;
    }
    let _ = std::fs::create_dir_all(&env.xmux_dir);

    let _term_guard = match TermGuard::enter() {
        Ok(g) => g,
        Err(e) => {
            eprintln!("xmux: {e}");
            return 1;
        }
    };

    let size = ratatui::crossterm::terminal::size().unwrap_or((80, 24));
    let (mut cols, mut body_rows) = (size.0, size.1.saturating_sub(1)); // status bar = last row

    // The control-mode metadata clients: one per remote host.
    let (host_tx, mut host_rx) = tokio::sync::mpsc::unbounded_channel::<HostEvent>();
    let mut mgr = HostManager::new(host_tx);

    // The live PTY attachments: one real attached mux client per session.
    let (pty_tx, mut pty_rx) = tokio::sync::mpsc::unbounded_channel::<PtyEvent>();
    let mut worker = DisplayWorker::new(pty_tx.clone());
    let mut registry = AttachRegistry::new(pty_tx);

    // The switcher, seeded from the source skeletons; events stream the tree in.
    let mut switcher = Switcher::from_sources(env.srcs.iter().map(|s| s.alias.clone()).collect());
    let mut app = App::new();

    // The canonical selection; committed from the switcher's cursor at the loop top.
    let mut selection = Selection::default();
    // Debounced attach: the selection last actually attached/switched to, and the
    // deadline after which a settled selection is attached (see ATTACH_DEBOUNCE_MS).
    let mut last_attached_sel = Selection::default();
    let mut attach_deadline: Option<std::time::Instant> = None;
    // Off-loop attach: keys with a spawn request in flight (key → latest request seq), so the
    // spinner shows "(attaching…)" before the Ready reply and a duplicate request is skipped.
    let mut in_flight: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    let mut attach_seq: u64 = 0;

    // The live mutate ops (create/rename/kill) — NOT tree probing (that comes from
    // the control clients' inventory + local enumeration).
    let ops = env.ops();

    // Single stdin reader thread: raw host bytes → channel.
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

    let prefix = parse_prefix(Some(&env.ui_prefix));
    let mut term_input = TermInput::new(prefix);
    let mut tree_decoder = KeyDecoder::new();
    let mut tree_armed = false;

    let mut term =
        match ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(std::io::stdout())) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("xmux: {e}");
                return 1;
            }
        };
    let _ = term.clear();

    // The picker control socket: serves headless key/text/dump.
    let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::channel::<Cmd>(256);
    let control = pick_control_path(&env);
    let _control_handle = control.and_then(|p| serve_control(p, cmd_tx));

    let (op_tx, mut op_rx) = tokio::sync::mpsc::unbounded_channel();
    let (enum_tx, mut enum_rx) = tokio::sync::mpsc::unbounded_channel::<LocalEnum>();

    let mut connected: HashSet<String> = HashSet::new();
    let mut panes_requested: HashSet<String> = HashSet::new();
    // The session each remote host's per-host PTY is currently switched to, so a
    // re-select of the same session skips a redundant switch-client.
    let mut host_session: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    connect_all_sources(&mut mgr, &env, cols, body_rows, &enum_tx);

    let spinner_start = std::time::Instant::now();
    let mut tick = tokio::time::interval(Duration::from_millis(SPINNER_FRAME_MS));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Periodic local re-enumeration (one-server-per-session psmux has no event push).
    // `interval_at(+period)` so the FIRST poll fires after a period, not at t=0 (the
    // startup `connect_all_sources` already kicked the initial enumeration).
    let poll_start = tokio::time::Instant::now() + Duration::from_millis(LOCAL_POLL_MS);
    let mut local_poll = tokio::time::interval_at(poll_start, Duration::from_millis(LOCAL_POLL_MS));
    local_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Periodic reconnect sweep: re-ensure any died remote control client (so #5
    // metadata sync self-heals) and re-attach the selected session's PTY if it
    // dropped (so a transient remote disconnect does not leave the right pane stuck
    // on "(attaching…)"). The sweep interval doubles as the retry backoff.
    let reconnect_start = tokio::time::Instant::now() + Duration::from_millis(RECONNECT_MS);
    let mut reconnect = tokio::time::interval_at(reconnect_start, Duration::from_millis(RECONNECT_MS));
    reconnect.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Frame timer: wakes the loop at the redraw cadence so a pending `dirty` draw is
    // flushed promptly even when no other event arrives. Redraws are gated on `dirty`
    // + elapsed ≥ FRAME_MS, so rapid input coalesces into ≤30fps instead of one
    // full-screen repaint per keystroke (the navigation freeze).
    let mut frame = tokio::time::interval(Duration::from_millis(FRAME_MS));
    frame.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut dirty = true;
    let mut last_draw = std::time::Instant::now() - Duration::from_millis(FRAME_MS);

    loop {
        // Advance the spinner from wall-clock so it animates regardless of which arm
        // fired, then commit the cursor's target into the canonical selection. A
        // changed selection ensures its PTY + (for a window row) switches the window.
        switcher.set_spinner_frame(spinner_frame_at(spinner_start.elapsed()));
        let new_sel = Selection::from_target(&switcher.terminal_view_target());
        if new_sel != selection {
            selection = new_sel;
            // Arm the debounce — do NOT attach yet. Rapid navigation keeps pushing the
            // deadline, so only the settled selection attaches (one switch, not a storm).
            attach_deadline = Some(std::time::Instant::now() + Duration::from_millis(ATTACH_DEBOUNCE_MS));
            dirty = true; // the cursor moved → the tree needs a redraw
        }
        // Apply the debounced attach once the selection has settled. The frame timer
        // re-enters the loop, so this fires even with no further input.
        if attach_deadline.is_some_and(|d| std::time::Instant::now() >= d) {
            attach_deadline = None;
            // Attach when the settled selection changed OR its display PTY is gone
            // (it may have exited / been reaped while the cursor was elsewhere — then
            // re-selecting the same session must re-attach NOW, not wait for the 2s
            // reconnect sweep, or the pane stays blank). Record it only on a real
            // attach, so a failed attach does not latch and suppress the retry.
            if !selection.is_empty() {
                let key = display_key(&env, &selection);
                if selection != last_attached_sel
                    || (!registry.contains(&key) && !in_flight.contains_key(&key))
                {
                    let t = std::time::Instant::now();
                    if select_attach(
                        &mut registry, &mut mgr, &env, &selection, &mut host_session,
                        &worker, &mut in_flight, &mut attach_seq, cols, body_rows,
                    ) {
                        last_attached_sel = selection.clone();
                    }
                    dbg_ms(&env.xmux_dir, "select_attach", t);
                    dirty = true;
                    dbg_log(
                        &env.xmux_dir,
                        &format!("selection -> key={} sess={}", display_key(&env, &selection), selection.session),
                    );
                }
            }
        }

        // Draw the split: the tree (left) and the selected session's live PTY grid
        // (right). GATED — redraw only when something changed (`dirty`) AND at most
        // once per frame, so rapid navigation / a busy PTY cannot flood the terminal
        // with full-screen repaints and stall the loop. The display key is the HOST
        // for remote tmux (one PTY per host) or `source/session` for local psmux.
        if dirty && last_draw.elapsed() >= Duration::from_millis(FRAME_MS) {
            let grid_arc = (!selection.is_empty())
                .then(|| registry.grid(&display_key(&env, &selection)))
                .flatten();
            let terminal_focused = !app.is_overlay();
            let t_draw = std::time::Instant::now();
            let _ = match &grid_arc {
                Some(g) => {
                    let t_lock = std::time::Instant::now();
                    let guard = g.lock().ok();
                    dbg_ms(&env.xmux_dir, "grid_lock", t_lock);
                    term.draw(|f| switcher.render(f, guard.as_deref(), terminal_focused))
                }
                None => term.draw(|f| switcher.render(f, None, terminal_focused)),
            };
            dbg_ms(&env.xmux_dir, "draw", t_draw);
            // The grids are now on screen — clear every attachment's output-coalescing
            // flag so each pump may signal its next chunk (bounds the PTY event channel).
            registry.clear_all_pending();
            dirty = false;
            last_draw = std::time::Instant::now();
        }

        // NOT biased: a biased select polls host_rx first every iteration, so a
        // sustained output flood would starve stdin, the control socket, ops,
        // enumeration, and the tick. Unbiased select gives every branch a fair share.
        //
        // Every arm EXCEPT the bare frame timer represents a real state change, so it
        // marks the UI dirty (drawn on the next gated pass); the frame timer only wakes
        // the loop to flush an already-pending dirty draw, so it must NOT set dirty.
        let mut from_frame = false;
        tokio::select! {
            Some(ev) = host_rx.recv() => {
                let t = std::time::Instant::now();
                let overlay = app.is_overlay();
                handle_host_event(ev, &mut mgr, &mut registry, &mut switcher, &env, &mut connected, &mut panes_requested, &mut host_session, cols, body_rows, overlay);
                let mut budget = EVENT_DRAIN_BUDGET;
                while budget > 0 {
                    match host_rx.try_recv() {
                        Ok(ev) => {
                            handle_host_event(ev, &mut mgr, &mut registry, &mut switcher, &env, &mut connected, &mut panes_requested, &mut host_session, cols, body_rows, overlay);
                            budget -= 1;
                        }
                        Err(_) => break,
                    }
                }
                dbg_ms(&env.xmux_dir, "host_drain", t);
            }
            Some(ev) = pty_rx.recv() => {
                // A kept attachment's pump fed its grid (Output → redraw on the next
                // loop top) or its master hit EOF (Exited → reap). Coalesce a burst
                // into one redraw so a busy session cannot monopolize the thread.
                if let PtyEvent::Exited { id } = ev { registry.reap(id); }
                let mut budget = EVENT_DRAIN_BUDGET;
                while budget > 0 {
                    match pty_rx.try_recv() {
                        Ok(PtyEvent::Exited { id }) => { registry.reap(id); budget -= 1; }
                        Ok(PtyEvent::Output { .. }) => { budget -= 1; }
                        Err(_) => break,
                    }
                }
            }
            Some(ev) = worker.recv() => {
                match ev {
                    DisplayEvent::Ready { seq, key, attachment } => {
                        if attach_reply_is_current(&in_flight, &key, seq) {
                            in_flight.remove(&key);
                            registry.insert(&key, attachment);
                        } else {
                            // Stale (the cursor moved on / the key was reaped): kill the child
                            // we no longer want rather than leaking it.
                            attachment.teardown();
                        }
                    }
                    DisplayEvent::Failed { seq, key, message } => {
                        if attach_reply_is_current(&in_flight, &key, seq) {
                            in_flight.remove(&key);
                        }
                        dbg_log(&env.xmux_dir, &format!("attach failed key={key}: {message}"));
                    }
                }
            }
            Some(bytes) = stdin_rx.recv() => {
                let mut quit = false;
                let mut focus_terminal = false;
                let mut focus_tree = false;
                let mut tree_replay: Vec<u8> = Vec::new();
                if app.is_overlay() {
                    let (ft, q) = handle_tree_bytes(
                        &bytes, &mut tree_decoder, &mut tree_armed, prefix, &mut switcher,
                        &mut mgr, &env, &ops, &op_tx, &enum_tx, cols, body_rows,
                    );
                    focus_terminal = ft;
                    quit = q;
                } else {
                    // TERMINAL focus: forward raw bytes to the selected session's PTY;
                    // TermInput intercepts the prefix (→ tree / quit / literal prefix).
                    for action in term_input.feed(&bytes) {
                        match action {
                            TermAction::Forward(f) => registry.input(&display_key(&env, &selection), f),
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
                    if !tree_replay.is_empty() {
                        let (ft, q) = handle_tree_bytes(
                            &tree_replay, &mut tree_decoder, &mut tree_armed, prefix,
                            &mut switcher, &mut mgr, &env, &ops, &op_tx, &enum_tx, cols, body_rows,
                        );
                        if ft && app.is_overlay() {
                            app.toggle();
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
                        switcher.handle_key(k);
                        dispatch_pending_op(&mut switcher, &ops, &op_tx);
                        ensure_current_host(&mut mgr, &env, &switcher, cols, body_rows);
                        if switcher.wants_quit() {
                            break;
                        }
                    }
                    Cmd::Dump(reply) => {
                        let sz = term
                            .size()
                            .unwrap_or(ratatui::layout::Size { width: 80, height: 24 });
                        let grid_arc = (!selection.is_empty())
                            .then(|| registry.grid(&display_key(&env, &selection)))
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
                        if !bytes.is_empty() {
                            registry.input(&display_key(&env, &selection), bytes);
                        }
                    }
                }
            }
            Some(result) = op_rx.recv() => {
                switcher.apply_op_result(result);
            }
            Some(le) = enum_rx.recv() => {
                match le {
                    LocalEnum::Sessions { source, sessions, err } => {
                        let had_err = err.is_some();
                        dbg_log(
                            &env.xmux_dir,
                            &format!(
                                "local enum source={source} n={} names={:?} err={:?}",
                                sessions.len(),
                                sessions.iter().map(|s| &s.name).collect::<Vec<_>>(),
                                err
                            ),
                        );
                        switcher.apply_source_result(source.clone(), sessions.clone(), err);
                        // Only sync the PTY set on a SUCCESSFUL enumeration. A transient
                        // poll failure returns an empty list with an error; reaping on
                        // that would tear down every live attachment for a source whose
                        // mux is actually fine (the keep-alive guarantee). Show the error
                        // in the tree, but leave the attachments intact.
                        if !had_err {
                            sync_source_terminals(&mut registry, &env, &source, &sessions, &mut host_session, cols, body_rows);
                        }
                    }
                    LocalEnum::Panes { address, panes } => {
                        switcher.apply_panes(address, panes);
                    }
                }
            }
            _ = tick.tick() => {
                // Resize detection: poll the console size (an ioctl, not a stdin
                // read). On a change push the new size to the PTYs + control clients.
                if let Ok((c, r)) = ratatui::crossterm::terminal::size() {
                    if (c, r) != (cols, body_rows + 1) {
                        let body = r.saturating_sub(1);
                        cols = c;
                        body_rows = body;
                        let (vc, vr) = terminal_view_size(c, body);
                        registry.resize_all(vc, vr);
                        mgr.resize_all(vc, vr);
                        let _ = term.autoresize();
                    }
                }
                // Spinner set = the selected session if its PTY is still connecting.
                let mut sp = HashSet::new();
                if !selection.is_empty() {
                    let key = display_key(&env, &selection);
                    if in_flight.contains_key(&key) || registry.connecting(&key) {
                        sp.insert(selection.address());
                    }
                }
                switcher.set_spinner(sp);
            }
            _ = local_poll.tick() => {
                // Re-enumerate local sources so new/removed sessions + window/pane
                // changes sync into the sidebar (no event push for one-server psmux).
                for src in &env.srcs {
                    if !src.remote {
                        spawn_local_enumeration(src.clone(), enum_tx.clone());
                    }
                }
            }
            _ = reconnect.tick() => {
                let (vc, vr) = terminal_view_size(cols, body_rows);
                // Re-ensure each remote control client (a no-op when alive; respawns
                // a died one so its list-sessions re-streams and #5 sync resumes).
                for src in &env.srcs {
                    if src.remote {
                        let _ = mgr.ensure(&src.alias, src, vc, vr);
                    }
                }
                // Re-warm each remote host's per-host PTY if it dropped (ENSURE-ONLY,
                // never reap: a just-respawned control client has an empty inventory
                // until its list-sessions resolves; reaping on that would tear down a
                // live PTY. Closed-host reaping is owned by the Inventory/Changed path).
                for src in &env.srcs {
                    if !src.remote || registry.contains(&src.alias) {
                        continue;
                    }
                    let first = match mgr.get(&src.alias) {
                        Some(client) => client.inventory.lock().unwrap().sessions.first().cloned(),
                        None => continue,
                    };
                    if let Some(s) = first {
                        let t = std::time::Instant::now();
                        let _ = registry.ensure(&src.alias, &src.attach_command(&s.name, None), vc, vr);
                        dbg_ms(&env.xmux_dir, "reconnect.ensure", t);
                        host_session.insert(src.alias.clone(), s.name.clone());
                    }
                }
                // Re-attach the selected session's display terminal if it dropped.
                if !selection.is_empty() {
                    let key = display_key(&env, &selection);
                    if !registry.contains(&key) && !in_flight.contains_key(&key) {
                        select_attach(
                            &mut registry, &mut mgr, &env, &selection, &mut host_session,
                            &worker, &mut in_flight, &mut attach_seq, cols, body_rows,
                        );
                    }
                }
            }
            _ = frame.tick() => {
                // Bare wake-up: flush a pending dirty draw at the frame cadence. No
                // state change, so it must NOT mark the UI dirty (else idle = 30fps
                // redraws forever).
                from_frame = true;
            }
        }
        // Any real event (not the bare frame wake) means the UI may have changed.
        if !from_frame {
            dirty = true;
        }
    }

    registry.teardown_all();
    mgr.teardown_all();
    0
}

/// After a key was handled in Overlay: take any queued create/rename/kill and run it
/// OFF the loop in a detached task, folding its result back through `op_tx`, so a
/// slow ssh round-trip never freezes rendering, host streaming, or the control socket.
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

/// The braille-spinner frame index for `elapsed` since the cockpit started.
fn spinner_frame_at(elapsed: std::time::Duration) -> usize {
    (elapsed.as_millis() / SPINNER_FRAME_MS as u128) as usize
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

    #[test]
    fn slow_log_line_includes_label_and_elapsed_ms() {
        let line = slow_log_line("registry.ensure", 42);
        assert_eq!(line, "SLOW registry.ensure 42ms");
    }

    #[test]
    fn selection_from_session_row_target() {
        let t = TerminalViewTarget { source: "jupiter06".into(), target: "api".into() };
        let sel = Selection::from_target(&t);
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
        let t = TerminalViewTarget { source: "jupiter06".into(), target: "api:2".into() };
        let sel = Selection::from_target(&t);
        assert_eq!(sel.session, "api");
        assert_eq!(sel.window, Some(2));
        assert_eq!(sel.address(), "jupiter06/api", "address is source/session, not the window");
    }

    #[test]
    fn selection_from_empty_target_is_empty() {
        let sel = Selection::from_target(&TerminalViewTarget::default());
        assert!(sel.is_empty());
        assert_eq!(sel.window, None);
    }

    #[test]
    fn display_key_is_per_host_remote_per_session_local() {
        // REMOTE tmux → one PTY per HOST (key = source); LOCAL psmux → one PTY per
        // SESSION (key = source/session).
        let mut remote = fake_source("jup");
        remote.remote = true;
        let local = fake_source("local"); // remote = false
        let by_alias: HashMap<String, Source> =
            [("jup".to_string(), remote.clone()), ("local".to_string(), local.clone())]
                .into_iter()
                .collect();
        let env = Env {
            cfg: Config::default(),
            cfg_warnings: Vec::new(),
            srcs: vec![remote, local],
            by_alias,
            local_bin: "psmux".into(),
            ui_prefix: "C-g".into(),
            xmux_dir: std::path::PathBuf::from("."),
        };
        let rsel = Selection { source: "jup".into(), session: "api".into(), window: None };
        assert_eq!(display_key(&env, &rsel), "jup", "remote → per-host key");
        let lsel = Selection { source: "local".into(), session: "work".into(), window: None };
        assert_eq!(display_key(&env, &lsel), "local/work", "local → per-session key");
    }

    #[test]
    fn addresses_to_reap_picks_this_sources_closed_sessions() {
        // Only addresses of `source` that are not in the desired set are reaped; a
        // different source's attachments and still-present sessions are protected.
        let existing = vec![
            "local/a".to_string(),
            "local/b".to_string(),
            "jupiter06/a".to_string(),
        ];
        let mut desired = HashSet::new();
        desired.insert("local/a".to_string());
        let reap = addresses_to_reap(&existing, &desired, "local");
        assert_eq!(reap, vec!["local/b".to_string()], "only local/b (closed, this source) is reaped");
    }

    #[test]
    fn addresses_to_reap_empty_desired_reaps_all_of_source() {
        let existing = vec!["local/a".to_string(), "jupiter06/x".to_string()];
        let reap = addresses_to_reap(&existing, &HashSet::new(), "local");
        assert_eq!(reap, vec!["local/a".to_string()], "every local session gone → reap local/a only");
    }

    #[tokio::test]
    async fn connect_all_sources_connects_remote_hosts() {
        // Remote sources get a control client at startup; local sources enumerate
        // off the loop (no control client). Here cmd.exe sources are LOCAL, so none
        // get a control client — they enumerate. Use a remote source to prove connect.
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<HostEvent>();
        let mut mgr = HostManager::new(tx);
        let (enum_tx, _enum_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut src = fake_source("jupiter06");
        src.remote = true;
        src.binary = "cmd.exe".into(); // a spawnable stand-in for ssh that EOFs at once
        let by_alias: HashMap<String, Source> =
            [("jupiter06".to_string(), src.clone())].into_iter().collect();
        let env = Env {
            cfg: Config::default(),
            cfg_warnings: Vec::new(),
            srcs: vec![src],
            by_alias,
            local_bin: "cmd.exe".into(),
            ui_prefix: "C-g".into(),
            xmux_dir: std::path::PathBuf::from("."),
        };
        connect_all_sources(&mut mgr, &env, 80, 24, &enum_tx);
        assert!(mgr.get("jupiter06").is_some(), "remote host got a control client");
        mgr.teardown_all();
    }

    #[test]
    fn spinner_frame_advances_with_wall_clock() {
        use std::time::Duration;
        assert_eq!(spinner_frame_at(Duration::from_millis(0)), 0);
        assert_eq!(spinner_frame_at(Duration::from_millis(SPINNER_FRAME_MS)), 1);
        assert_eq!(spinner_frame_at(Duration::from_millis(SPINNER_FRAME_MS * 3 + 10)), 3);
    }

    #[test]
    fn terminal_view_size_subtracts_tree_and_divider() {
        use crate::ui::switcher::TREE_WIDTH;
        let (vc, vr) = terminal_view_size(143, 39);
        assert_eq!(vc, 143 - (TREE_WIDTH + 1), "cols minus tree minus divider");
        assert_eq!(vr, 39, "rows pass through (the body height)");
    }

    #[test]
    fn terminal_view_size_clamps_to_at_least_one() {
        let (vc, vr) = terminal_view_size(10, 0);
        assert_eq!(vc, 1);
        assert_eq!(vr, 1);
    }

    #[tokio::test]
    async fn host_exited_before_connect_marks_unreachable() {
        use crate::ui::run::dump_overlay;
        use crate::ui::switcher::Switcher;
        let mut switcher = Switcher::from_sources(vec!["jupiter00".into()]);
        let connected: HashSet<String> = HashSet::new();
        assert!(
            note_host_exited(&mut switcher, &connected, "jupiter00", Some("no route to host".into())),
            "a never-connected host is marked unreachable on exit"
        );
        let out = dump_overlay(&mut switcher, None, 80, 24);
        assert!(out.contains("unreachable"), "host reads unreachable:\n{out}");
        assert!(out.contains("no route to host"), "shows the exit reason:\n{out}");
    }

    #[tokio::test]
    async fn host_exited_after_connect_keeps_tree() {
        use crate::ui::switcher::Switcher;
        let mut switcher = Switcher::from_sources(vec!["jupiter06".into()]);
        let mut connected: HashSet<String> = HashSet::new();
        connected.insert("jupiter06".into());
        assert!(
            !note_host_exited(&mut switcher, &connected, "jupiter06", None),
            "an already-connected host is not marked unreachable on exit"
        );
    }

    #[test]
    fn focus_event_follows_cursor_to_active_window() {
        // A resolved active-window probe (HostEvent::Focus) moves the sidebar cursor
        // to the new active window of the displayed session (#2). Cursor starts on
        // window 1's row; Focus to window 0 must land it there.
        use crate::session::{Pane, Session, WindowPanes};
        use crate::ui::switcher::{Scan, Switcher};
        use crate::ui::tree::Group;
        use ratatui::crossterm::event::{KeyEvent, KeyModifiers};

        let mut panes = std::collections::HashMap::new();
        panes.insert(
            "jup/api".to_string(),
            vec![
                WindowPanes { index: 0, name: "w0".into(), active: true, panes: vec![Pane { index: 0, active: true, command: "bash".into() }] },
                WindowPanes { index: 1, name: "w1".into(), active: false, panes: vec![Pane { index: 0, active: true, command: "bash".into() }] },
            ],
        );
        let scan = Scan {
            groups: vec![Group {
                source: "jup".into(),
                err: None,
                sessions: vec![Session { source: "jup".into(), name: "api".into(), windows: 2, attached: false, last_attached: 100 }],
            }],
            panes,
        };
        let mut switcher = Switcher::new(scan);
        // session row -> window 0 -> window 1.
        switcher.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        switcher.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(switcher.terminal_view_target().target, "api:1", "cursor on window 1");

        let (htx, _hrx) = tokio::sync::mpsc::unbounded_channel::<HostEvent>();
        let mut mgr = HostManager::new(htx);
        let (ptx, _prx) = tokio::sync::mpsc::unbounded_channel::<PtyEvent>();
        let mut registry = AttachRegistry::new(ptx);
        let env = fake_env_with_sources(&["jup"]);
        let mut connected = HashSet::new();
        let mut panes_requested = HashSet::new();
        let mut host_session = HashMap::new();
        // Terminal-focused (overlay=false): the probe-resolved Focus follows the cursor.
        handle_host_event(
            HostEvent::Focus { host: "jup".into(), session: "api".into(), window: 0 },
            &mut mgr, &mut registry, &mut switcher, &env,
            &mut connected, &mut panes_requested, &mut host_session, 80, 24, false,
        );
        assert_eq!(switcher.terminal_view_target().target, "api:0", "Focus followed the cursor to window 0");
    }

    #[test]
    fn focus_event_does_not_follow_cursor_while_navigating_the_tree() {
        // While the TREE has focus the user drives the cursor; a probe-resolved Focus
        // (echoed from the user's own select-window navigation, possibly stale) must
        // NOT yank the cursor — otherwise a lagged reply drags it backward and the
        // user fights the sidebar. The marker still updates; only the cursor is left.
        use crate::session::{Pane, Session, WindowPanes};
        use crate::ui::switcher::{Scan, Switcher};
        use crate::ui::tree::Group;
        use ratatui::crossterm::event::{KeyEvent, KeyModifiers};

        let mut panes = std::collections::HashMap::new();
        panes.insert(
            "jup/api".to_string(),
            vec![
                WindowPanes { index: 0, name: "w0".into(), active: true, panes: vec![Pane { index: 0, active: true, command: "bash".into() }] },
                WindowPanes { index: 1, name: "w1".into(), active: false, panes: vec![Pane { index: 0, active: true, command: "bash".into() }] },
            ],
        );
        let scan = Scan {
            groups: vec![Group {
                source: "jup".into(),
                err: None,
                sessions: vec![Session { source: "jup".into(), name: "api".into(), windows: 2, attached: false, last_attached: 100 }],
            }],
            panes,
        };
        let mut switcher = Switcher::new(scan);
        switcher.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        switcher.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)); // cursor on window 1
        assert_eq!(switcher.terminal_view_target().target, "api:1");

        let (htx, _hrx) = tokio::sync::mpsc::unbounded_channel::<HostEvent>();
        let mut mgr = HostManager::new(htx);
        let (ptx, _prx) = tokio::sync::mpsc::unbounded_channel::<PtyEvent>();
        let mut registry = AttachRegistry::new(ptx);
        let env = fake_env_with_sources(&["jup"]);
        let mut connected = HashSet::new();
        let mut panes_requested = HashSet::new();
        let mut host_session = HashMap::new();
        handle_host_event(
            HostEvent::Focus { host: "jup".into(), session: "api".into(), window: 0 },
            &mut mgr, &mut registry, &mut switcher, &env,
            &mut connected, &mut panes_requested, &mut host_session, 80, 24, true, // overlay = tree focus
        );
        assert_eq!(switcher.terminal_view_target().target, "api:1", "tree-focus Focus must NOT yank the cursor");
    }

    #[test]
    fn prefix_s_toggles_state() {
        use crate::proxy::app::{App, AppState};
        let mut app = App::new();
        assert!(app.is_overlay());
        app.toggle();
        assert_eq!(app.state, AppState::Passthrough);
        app.toggle();
        assert!(app.is_overlay());
    }

    // Suppress unused warnings for the test-only env builder kept for future loop tests.
    #[test]
    fn fake_env_builder_constructs() {
        let env = fake_env_with_sources(&["local", "jupiter06"]);
        assert_eq!(env.srcs.len(), 2);
    }

    #[test]
    fn attach_reply_is_current_only_for_latest_seq() {
        let mut f = std::collections::HashMap::new();
        f.insert("k".to_string(), 5u64);
        assert!(attach_reply_is_current(&f, "k", 5));
        assert!(!attach_reply_is_current(&f, "k", 4), "older seq is stale");
        assert!(!attach_reply_is_current(&f, "absent", 5), "no in-flight request → stale");
    }

    // =========================================================================
    // HUMAN VISUAL-GATE CHECKLIST (run in a REAL terminal — never headless):
    // 1. Launch `xmux`. Confirm it enters the alternate screen cleanly and starts in
    //    Overlay: the Host·Session·Window·Pane tree on the left, the live REAL
    //    terminal of the cursor's session on the right (a true attached mux client).
    // 2. Move the cursor between sessions. Confirm the right pane shows each session's
    //    real attached terminal instantly (it is pre-attached + kept alive), with a
    //    spinner while a session's attach is still establishing.
    // 3. Select a WINDOW row — confirm the attached client switches to that window.
    // 4. Press C-g then `s` — fullscreen Passthrough of the selected session; type
    //    and confirm keystrokes reach the real attached pane.
    // 5. Create / kill a window or session inside a pane — confirm the sidebar tree
    //    syncs (remote via control events, local within the poll interval) and the
    //    PTY set follows (new session attaches, killed session's PTY is reaped).
    // 6. C-g then `q` — clean quit, terminal restored.
    // 7. NEVER attach the session that owns xmux (xmux refuses to run inside a mux,
    //    so in normal use no session mirrors the UI).
    // =========================================================================
}
