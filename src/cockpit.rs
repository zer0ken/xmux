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
//! control socket, terminal resize, and an animation tick. ratatui owns stdout and
//! draws the SAME split (tree + selected PTY grid) in both focus states — Overlay
//! (tree focused) and Passthrough (terminal focused) differ only in the divider
//! colour and where keys go, so toggling focus needs no screen clear.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use ratatui::crossterm::event::{KeyCode, KeyModifiers};

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

const TREE_WIDTH_MIN: u16 = 20;
const TREE_WIDTH_MAX: u16 = 100;

/// How long the resize-repeat window stays open after a prefix-driven tree resize:
/// during it a bare Ctrl+←/→ (no prefix) keeps resizing and refreshes the window —
/// tmux's `bind -r` repeat applied to the tree width. Each repeat resets the window.
const RESIZE_REPEAT_MS: u64 = 400;

fn adjust_tree_width(w: u16, delta: i32) -> u16 {
    (w as i32 + delta).clamp(TREE_WIDTH_MIN as i32, TREE_WIDTH_MAX as i32) as u16
}

/// Applies a tree-width delta to the natural width: clamps to the allowed range and
/// persists it. A no-op for a zero delta. Shared by the tree- and mux-focus resize
/// paths so the adjust + save lives in one place.
fn apply_width_delta(wd: i32, natural: &mut u16, xmux_dir: &std::path::Path) {
    if wd == 0 {
        return;
    }
    *natural = adjust_tree_width(*natural, wd);
    crate::state::save_tree_width(xmux_dir, *natural);
}

/// Flips the auto-hide-tree mode and persists it, so the next launch restores it.
/// Shared by the tree- and mux-focus `prefix t` paths. The effective tree width is
/// reconciled at the next loop top (`reconciled_tree_width`); the caller marks dirty.
fn toggle_auto_hide(mode: &mut bool, xmux_dir: &std::path::Path) {
    *mode = !*mode;
    crate::state::save_auto_hide_tree(xmux_dir, *mode);
}

/// The tree width a divider drag to 1-based screen column `col` sets: the dragged
/// column becomes the divider position (= the tree width), clamped to the allowed range.
fn divider_drag_width(col: u16) -> u16 {
    col.saturating_sub(1).clamp(TREE_WIDTH_MIN, TREE_WIDTH_MAX)
}

/// If `bytes` STARTS with a Ctrl+←/→ (`ESC [ 1 ; 5 D/C`), the resize delta and the
/// 6-byte length it consumed; else `None`. Peeling leading Ctrl-arrows (rather than
/// matching the whole read) lets a coalesced autorepeat burst — several presses
/// delivered in one stdin read — keep resizing instead of ending the repeat window.
/// Restricted to Ctrl-arrows (not bare arrows or h/l) so it never hijacks navigation
/// or typed pane input outside the window.
fn leading_ctrl_arrow(bytes: &[u8]) -> Option<(i32, usize)> {
    if bytes.len() >= 6 && bytes[0] == 0x1b && bytes[1] == b'[' && &bytes[2..5] == b"1;5" {
        match bytes[5] {
            b'C' => return Some((1, 6)),
            b'D' => return Some((-1, 6)),
            _ => {}
        }
    }
    None
}

/// The EFFECTIVE tree width to render and size the mux against. Hidden (0, mux
/// full width) only while the mux is focused AND auto-hide-tree mode is on;
/// otherwise the tree's natural width. Pure so the focus/mode interaction is
/// unit-testable; the loop owns the natural width and the PTY resize on change.
fn reconciled_tree_width(terminal_focused: bool, auto_hide_tree: bool, natural: u16) -> u16 {
    if terminal_focused && auto_hide_tree {
        0
    } else {
        natural
    }
}

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
/// is `cols - tree_width - 1` (tree + the single divider rule), except `tree_width == 0`
/// (the tree-hidden sentinel) gives the full `cols` with no divider; the view HEIGHT is
/// the full terminal height (`body_rows + 1`) because the footer and input occupy
/// the tree column, leaving the terminal column the full height. Both clamp to at least 1.
fn terminal_view_size(cols: u16, body_rows: u16, tree_width: u16) -> (u16, u16) {
    // tree_width == 0 is the "tree hidden" sentinel: the mux takes the full width
    // with no divider column. Otherwise subtract the tree column + the 1-col divider.
    let view_cols = if tree_width == 0 {
        cols.max(1)
    } else {
        cols.saturating_sub(tree_width + 1).max(1)
    };
    (view_cols, (body_rows + 1).max(1))
}

/// Maps a 1-based SGR mouse cell to 1-based grid-local coords if it falls inside
/// `area` (a 0-based screen Rect), else None. SGR uses 1-based coordinates; ratatui
/// Rects use 0-based screen positions. The result is 1-based so it can be directly
/// re-encoded in a new SGR sequence forwarded to the mux.
fn to_grid_local(area: ratatui::layout::Rect, col: u16, row: u16) -> Option<(u16, u16)> {
    let c0 = col.checked_sub(1)?; // SGR 1-based → 0-based screen cell
    let r0 = row.checked_sub(1)?;
    if c0 >= area.x && c0 < area.x + area.width && r0 >= area.y && r0 < area.y + area.height {
        Some((c0 - area.x + 1, r0 - area.y + 1)) // back to 1-based, grid-local
    } else {
        None
    }
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
    tree_width: u16,
) -> bool {
    if sel.is_empty() {
        return false;
    }
    let (cols, rows) = terminal_view_size(cols, rows, tree_width);
    let Some(src) = env.by_alias.get(&sel.source) else {
        return false;
    };
    let key = display_key(env, sel);
    let already = registry.contains(&key);

    if src.remote {
        if !already {
            // Off-loop first-attach: request the spawn ONLY if one is not already in flight.
            // Do NOT overwrite host_session while an attach is in flight — the in-flight attach
            // lands on its ORIGINAL target session, and the post-Ready re-evaluation (see the
            // Ready arm) issues a switch-client to the current selection. Overwriting it here
            // would make the switch-client guard think the PTY is already on the new session.
            if !in_flight.contains_key(&key) {
                let argv = src.attach_command(&sel.session, sel.window);
                request_attach(registry, worker, in_flight, attach_seq, &key, argv, cols, rows);
                host_session.insert(sel.source.clone(), sel.session.clone());
            }
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
#[allow(clippy::too_many_arguments)]
fn sync_source_terminals(
    registry: &mut AttachRegistry,
    env: &Env,
    source: &str,
    sessions: &[crate::session::Session],
    host_session: &mut std::collections::HashMap<String, String>,
    worker: &DisplayWorker,
    in_flight: &mut std::collections::HashMap<String, u64>,
    attach_seq: &mut u64,
    cols: u16,
    rows: u16,
    tree_width: u16,
) {
    let Some(src) = env.by_alias.get(source) else {
        return;
    };
    let (cols, rows) = terminal_view_size(cols, rows, tree_width);
    if src.remote {
        // One PTY per host. Warm it on the first session if not yet attached; reap it
        // (and forget its session) when the host has no sessions.
        match sessions.first() {
            Some(first) if !registry.contains(source) && !in_flight.contains_key(source) => {
                request_attach(
                    registry, worker, in_flight, attach_seq,
                    source, src.attach_command(&first.name, None), cols, rows,
                );
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
    // LOCAL psmux: one PTY per session; request each off-loop + reap closed.
    let mut desired: HashSet<String> = HashSet::new();
    for s in sessions {
        let addr = s.address();
        desired.insert(addr.clone());
        if !registry.contains(&addr) && !in_flight.contains_key(&addr) {
            request_attach(
                registry, worker, in_flight, attach_seq,
                &addr, src.attach_command(&s.name, None), cols, rows,
            );
        }
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
    tree_width: u16,
) {
    let (cols, rows) = terminal_view_size(cols, rows, tree_width);
    if let Some(host) = switcher.current_host() {
        if let Some(src) = env.by_alias.get(&host) {
            if src.remote {
                let _ = mgr.ensure(&host, src, cols, rows);
            }
        }
    }
}

/// Consumes a pending re-scan kick (set by `r` or a menu "reconnect"): re-lists every
/// remote host and re-enumerates every local source — the same probes as first launch.
/// A no-op when no kick is pending. Shared by the key and context-menu paths.
fn kick_rescan(
    switcher: &mut crate::ui::switcher::Switcher,
    env: &Env,
    mgr: &HostManager,
    enum_tx: &tokio::sync::mpsc::UnboundedSender<LocalEnum>,
) {
    if switcher.take_rescan_kick() {
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
    tree_width: u16,
    enum_tx: &tokio::sync::mpsc::UnboundedSender<LocalEnum>,
) {
    let (cols, rows) = terminal_view_size(cols, rows, tree_width);
    for src in &env.srcs {
        if src.remote {
            let _ = mgr.ensure(&src.alias, src, cols, rows);
        } else {
            spawn_local_enumeration(src.clone(), enum_tx.clone());
        }
    }
}

/// The single key that moves focus from the tree into the terminal pane.
/// (Arrows navigate the tree; the prefix-Esc path returns focus — see TermInput.)
fn is_focus_in(code: KeyCode) -> bool {
    matches!(code, KeyCode::Enter)
}

/// Processes a batch of TREE-focus input bytes through ONE path — used for both real
/// stdin and bytes replayed after a terminal→tree switch. Handles prefix arming
/// (`C-g` then `q` → quit, `h`/`Ctrl+←` → shrink tree, `l`/`Ctrl+→` → grow tree),
/// Enter → focus terminal (unless an inline input is open),
/// ←/→ navigate the tree; then the off-loop op dispatch, ensure-current-host, and
/// the `r` re-scan. Returns `(focus_terminal, quit, width_delta)`. The selection is
/// committed at the loop top, so this only drives navigation + metadata, not the display.
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
    tree_width: u16,
) -> (bool, bool, i32, bool) {
    let mut focus_terminal = false;
    let mut quit = false;
    let mut width_delta = 0i32;
    let mut toggle_auto_hide = false;
    for key in tree_decoder.feed(bytes) {
        if *tree_armed {
            *tree_armed = false;
            let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
            match key.code {
                KeyCode::Char('q') => quit = true,
                KeyCode::Left if ctrl => width_delta = -1,
                KeyCode::Right if ctrl => width_delta = 1,
                KeyCode::Char('h') => width_delta = -1,
                KeyCode::Char('l') => width_delta = 1,
                KeyCode::Char('t') => toggle_auto_hide = true,
                KeyCode::Char('?') => switcher.toggle_help(),
                // prefix →/Tab: focus the mux pane (prefix ←/Esc: focus the tree, where
                // we already are — a no-op that falls through).
                KeyCode::Right | KeyCode::Tab => focus_terminal = true,
                _ => {}
            }
            continue;
        }
        if !switcher.is_inputting() && key.code == KeyCode::Char(prefix as char) {
            *tree_armed = true;
            continue;
        }
        // Enter focuses the terminal pane. ←/→ navigate the tree (→ descends to a child,
        // ← ascends to the parent) in `switcher.handle_key`.
        if !switcher.is_inputting() && is_focus_in(key.code) {
            focus_terminal = true;
            continue;
        }
        switcher.handle_key(key);
    }
    dispatch_pending_op(switcher, ops, op_tx);
    ensure_current_host(mgr, env, switcher, cols, rows, tree_width);
    kick_rescan(switcher, env, mgr, enum_tx);
    (focus_terminal, quit, width_delta, toggle_auto_hide)
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
    worker: &DisplayWorker,
    in_flight: &mut std::collections::HashMap<String, u64>,
    attach_seq: &mut u64,
    cols: u16,
    rows: u16,
    tree_width: u16,
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
                sync_source_terminals(registry, env, &host, &sessions, host_session, worker, in_flight, attach_seq, cols, rows, tree_width);
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
            // The active-window probe resolved. Updates the bold+italic marker so the
            // sidebar reflects the mux's current active window. Cursor follow in
            // passthrough happens at the loop top (single unified call), not here.
            switcher.set_active_window(&host, &session, window);
        }
        HostEvent::Exited { host, reason } => {
            note_host_exited(switcher, connected, &host, reason);
            mgr.reap(&host);
        }
    }
}

/// Handles a remote host's control client dying. A host that had connected keeps its
/// last-known tree. A never-connected host that died with "no sessions" / "no server
/// running" is REACHABLE but has no mux server — it renders "(empty)" (and a session
/// can be created there), NOT "⚠". Any other never-connected death is a real
/// transport failure and renders "⚠". Returns `true` only when it marked the host
/// unreachable.
/// Locks focus to the bottom input pane while one is open, restoring the prior focus when
/// it closes. While `inputting`: capture the focus once (so it can be returned to) and force
/// `Overlay` so keystrokes route to the input; forcing it every tick also overrides any focus
/// toggle that slipped through mid-iteration. On the close edge: restore the captured focus.
fn reconcile_input_focus(
    inputting: bool,
    state: &mut crate::proxy::app::AppState,
    saved: &mut Option<crate::proxy::app::AppState>,
) {
    if inputting {
        if saved.is_none() {
            *saved = Some(*state);
        }
        *state = crate::proxy::app::AppState::Overlay;
    } else if let Some(prev) = saved.take() {
        *state = prev;
    }
}

fn note_host_exited(
    switcher: &mut crate::ui::switcher::Switcher,
    connected: &HashSet<String>,
    host: &str,
    reason: Option<String>,
) -> bool {
    if connected.contains(host) {
        return false;
    }
    if reason.as_deref().is_some_and(crate::source::reason_is_no_sessions) {
        switcher.apply_source_result(host.to_string(), Vec::new(), None);
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

    // Route panic output to a file instead of stderr so a panic never corrupts the
    // alternate screen. Worker threads (PTY pumps) catch+recover their own panics
    // (see Grid::feed); TermGuard restores the screen on a main-thread unwind.
    {
        let log = env.xmux_dir.join("panic.log");
        std::panic::set_hook(Box::new(move |info| {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&log) {
                let _ = writeln!(f, "{info}");
            }
        }));
    }

    let size = ratatui::crossterm::terminal::size().unwrap_or((80, 24));
    let (mut cols, mut body_rows) = (size.0, size.1.saturating_sub(1)); // status bar = last row
    // `tree_width` is the EFFECTIVE width (0 = tree hidden, mux full width); every
    // sizing/render site reads it. `tree_width_natural` holds the tree's natural
    // width (what prefix h·l adjusts, restored when the tree is shown again).
    // Restore the natural width the user last set (persisted across runs); clamp a
    // stale out-of-range value, and fall back to the default when none is saved.
    let mut tree_width_natural = adjust_tree_width(
        crate::state::load_tree_width(&env.xmux_dir).unwrap_or(crate::ui::switcher::TREE_WIDTH),
        0,
    );
    let mut tree_width = tree_width_natural;
    // Auto-hide-tree mode: the live `prefix t` toggle, restored from its persisted
    // state if set, else the `auto-hide-tree` config default. The loop-top reconcile
    // reads it to size the tree (0 = hidden, mux full width) on focus changes.
    let mut auto_hide_tree = crate::state::load_auto_hide_tree(&env.xmux_dir)
        .unwrap_or_else(|| env.cfg.ui_auto_hide_tree());
    // The resize-repeat window: set when a prefix-driven resize fires, it lets a bare
    // Ctrl+←/→ keep resizing (no re-prefix) until it lapses (see RESIZE_REPEAT_MS).
    let mut repeat_until: Option<std::time::Instant> = None;
    // True while the left button is dragging the tree/mux divider rule to resize.
    let mut dragging_divider = false;
    // True while the mouse is hovering the divider rule (no button) — lights it up as
    // a drag-resize grab cue. Fed to the switcher each draw via set_divider_hovered.
    let mut hovered_divider = false;

    // The control-mode metadata clients: one per remote host.
    let (host_tx, mut host_rx) = tokio::sync::mpsc::unbounded_channel::<HostEvent>();
    let mut mgr = HostManager::new(host_tx);

    // The live PTY attachments: one real attached mux client per session.
    let (pty_tx, mut pty_rx) = tokio::sync::mpsc::unbounded_channel::<PtyEvent>();
    let mut worker = DisplayWorker::new(pty_tx);
    let mut registry = AttachRegistry::new();

    // The switcher, seeded from the source skeletons; events stream the tree in.
    let mut switcher = Switcher::from_sources(env.srcs.iter().map(|s| s.alias.clone()).collect());
    // Feed the switcher the ssh config so an unreachable host's info pane can show its
    // Host/Match stanza. Read once; a missing file just yields no stanza.
    switcher.set_ssh_config_text(
        std::fs::read_to_string(crate::env::ssh_config_path()).unwrap_or_default(),
    );
    // Divider colours from config's tmux-style pane-border options (tmux defaults
    // otherwise), so the tree|mux rule matches the user's tmux pane-border experience.
    switcher.set_divider_colors(crate::ui::switcher::DividerColors {
        active: crate::ui::switcher::map_color(&env.cfg.ui.pane_active_border_style),
        inactive: crate::ui::switcher::map_color(&env.cfg.ui.pane_border_style),
        hover: crate::ui::switcher::map_color(&env.cfg.ui.pane_border_hover_style),
    });
    // Restore the session the user last had selected (persisted across runs), so the
    // preselect lands there once its host streams in instead of guessing from the
    // unreliable cross-host `session_last_attached` (#1).
    switcher.set_preferred(crate::state::load_last_session(&env.xmux_dir));
    let mut app = App::new();
    // Focus belongs to the bottom input pane while one is open: capture the focus it was
    // opened from, force tree focus so keystrokes reach the input, and restore the captured
    // focus when the input closes (Esc cancel or Enter confirm). Reconciled at the loop top
    // so a mid-iteration focus toggle (e.g. a mouse click) cannot stick.
    let mut focus_before_input: Option<crate::proxy::app::AppState> = None;

    // The canonical selection; committed from the switcher's cursor at the loop top.
    let mut selection = Selection::default();
    // Debounced attach: the selection last actually attached/switched to, and the
    // deadline after which a settled selection is attached (see ATTACH_DEBOUNCE_MS).
    let mut last_attached_sel = Selection::default();
    let mut attach_deadline: Option<std::time::Instant> = None;
    // The session address last persisted as the user's last-selected (#1), so it is
    // not rewritten on every window step within the same session.
    let mut last_saved_session = String::new();
    // Off-loop attach: keys with a spawn request in flight (key → latest request seq), so the
    // spinner shows "(attaching…)" before the Ready reply and a duplicate request is skipped.
    let mut in_flight: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    let mut attach_seq: u64 = 0;
    // Ids whose Exited arrived BEFORE their off-loop Ready insert: the pump already ended,
    // so the Ready arm must tear the dead attachment down instead of registering an
    // unreapable, permanently-frozen pane.
    let mut reaped_ids: std::collections::HashSet<u64> = std::collections::HashSet::new();

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
    connect_all_sources(&mut mgr, &env, cols, body_rows, tree_width, &enum_tx);

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
    // DIAG (temp): detect if the parent console's raw mode gets reset out from under us.
    let mut prev_raw = ratatui::crossterm::terminal::is_raw_mode_enabled().unwrap_or(true);

    loop {
        // Advance the spinner from wall-clock so it animates regardless of which arm
        // fired, then commit the cursor's target into the canonical selection. A
        // changed selection ensures its PTY + (for a window row) switches the window.
        switcher.set_spinner_frame(spinner_frame_at(spinner_start.elapsed()));
        switcher.set_divider_hovered(hovered_divider);
        // Lock focus to the input pane while it is shown; restore the prior focus on close.
        reconcile_input_focus(switcher.is_inputting(), &mut app.state, &mut focus_before_input);
        // The single owner of the effective tree width: reconcile it to the current
        // focus + the hide setting, and to any natural-width change from prefix h/l.
        // On a change (focus toggled, hide flips the width, or h/l resized the tree),
        // resize the PTYs to the new mux view size so the mux reflows, and mark dirty.
        // ponytail: resize_all touches every live attachment on each change. It is
        // gated to fire once per actual change (not per loop), matching the existing
        // h/l and console-resize paths; debounce only if toggle-spam proves costly.
        let want_tree_width = reconciled_tree_width(!app.is_overlay(), auto_hide_tree, tree_width_natural);
        if want_tree_width != tree_width {
            // Crossing the hidden sentinel (0) flips the column TOPOLOGY: full-width mux
            // <-> tree+divider+mux. A stale wide-char (CJK) cell at the new tree/divider
            // boundary can survive ratatui's incremental diff, leaving background residue,
            // so force a full repaint on that transition. A plain h/l resize keeps the
            // same topology and needs no clear (the per-frame Clear widget handles it).
            let crossed_hidden = (want_tree_width == 0) != (tree_width == 0);
            tree_width = want_tree_width;
            let (vc, vr) = terminal_view_size(cols, body_rows, tree_width);
            registry.resize_all(vc, vr);
            mgr.resize_all(vc, vr);
            if crossed_hidden {
                let _ = term.clear();
            }
            dirty = true;
        }
        // DIAG (temp): log the exact iteration where raw mode flips off (terminal goes
        // canonical → keys echo on screen, xmux stops receiving input). The line just
        // before this in debug.log shows what activity (attach/reap/draw) preceded it.
        {
            let raw_now = ratatui::crossterm::terminal::is_raw_mode_enabled().unwrap_or(true);
            if prev_raw && !raw_now {
                dbg_log(&env.xmux_dir, "DIAG RAW_LOST (raw mode flipped off this iteration)");
            } else if !prev_raw && raw_now {
                dbg_log(&env.xmux_dir, "DIAG RAW_REGAINED");
            }
            prev_raw = raw_now;
        }
        // A portable-pty child spawn clears ENABLE_MOUSE_INPUT on the parent CONIN,
        // killing mouse capture; re-assert it whenever it drifts off.
        crate::proxy::term::ensure_mouse_capture();
        // In passthrough the user no longer drives the tree cursor (stdin goes to the
        // PTY), so the tree selection tracks the displayed session's active window — always.
        // select_active_window is idempotent (no move when already on the active window or
        // when the session's panes are unknown), so calling it each iteration is cheap.
        if !app.is_overlay() {
            switcher.select_active_window();
        }
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
                // Persist the settled session as the user's last-selected, so the next
                // launch restores it (#1). Only on an address change, so stepping
                // between windows of one session does not rewrite it every settle.
                let addr = selection.address();
                if addr != last_saved_session {
                    crate::state::save_last_session(&env.xmux_dir, &addr);
                    last_saved_session = addr;
                }
                let key = display_key(&env, &selection);
                if selection != last_attached_sel
                    || (!registry.contains(&key) && !in_flight.contains_key(&key))
                {
                    let t = std::time::Instant::now();
                    if select_attach(
                        &mut registry, &mut mgr, &env, &selection, &mut host_session,
                        &worker, &mut in_flight, &mut attach_seq, cols, body_rows, tree_width,
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
            // The divider glyph reflects auto-hide-tree mode (║ on, │ off).
            switcher.set_auto_hide(auto_hide_tree);
            let t_draw = std::time::Instant::now();
            // Split the draw cost: `render` is the in-memory buffer build (tree +
            // grid → cells); the remainder of `draw` is crossterm's diff + console
            // flush. On Windows the flush (writing changed cells to the console) is
            // the suspected dominant stall during a remote repaint flood — the split
            // tells render-cost from flush-cost so the off-loop-draw fix targets the
            // right half (#2). XMUX_DEBUG-gated like the other probes.
            let xmux_dir = env.xmux_dir.clone();
            let _ = match &grid_arc {
                Some(g) => {
                    let t_lock = std::time::Instant::now();
                    let guard = g.lock().ok();
                    dbg_ms(&env.xmux_dir, "grid_lock", t_lock);
                    term.draw(|f| {
                        let t_render = std::time::Instant::now();
                        switcher.render(f, guard.as_deref(), terminal_focused, tree_width);
                        dbg_ms(&xmux_dir, "render", t_render);
                    })
                }
                None => term.draw(|f| {
                    let t_render = std::time::Instant::now();
                    switcher.render(f, None, terminal_focused, tree_width);
                    dbg_ms(&xmux_dir, "render", t_render);
                }),
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
                handle_host_event(ev, &mut mgr, &mut registry, &mut switcher, &env, &mut connected, &mut panes_requested, &mut host_session, &worker, &mut in_flight, &mut attach_seq, cols, body_rows, tree_width);
                let mut budget = EVENT_DRAIN_BUDGET;
                while budget > 0 {
                    match host_rx.try_recv() {
                        Ok(ev) => {
                            handle_host_event(ev, &mut mgr, &mut registry, &mut switcher, &env, &mut connected, &mut panes_requested, &mut host_session, &worker, &mut in_flight, &mut attach_seq, cols, body_rows, tree_width);
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
                // Detach-to-quit: if the session the user is VIEWING (mux focused) exits —
                // a `detach` inside it, or it being killed — there is nothing left to show,
                // so quit xmux rather than strand a dead pane the reconnect sweep would keep
                // trying to revive. Capture its id BEFORE any reap removes it; a background
                // session dropping (tree focus, or a non-displayed attach) does NOT quit.
                let displayed_attach_id = (!app.is_overlay() && !selection.is_empty())
                    .then(|| registry.get(&display_key(&env, &selection)).map(|a| a.id()))
                    .flatten();
                let mut detached = false;
                if let PtyEvent::Exited { id } = ev {
                    if Some(id) == displayed_attach_id {
                        detached = true;
                    }
                    if !registry.reap(id) {
                        reaped_ids.insert(id);
                    }
                }
                let mut budget = EVENT_DRAIN_BUDGET;
                while budget > 0 {
                    match pty_rx.try_recv() {
                        Ok(PtyEvent::Exited { id }) => {
                            if Some(id) == displayed_attach_id {
                                detached = true;
                            }
                            if !registry.reap(id) {
                                reaped_ids.insert(id);
                            }
                            budget -= 1;
                        }
                        Ok(PtyEvent::Output { .. }) => { budget -= 1; }
                        Err(_) => break,
                    }
                }
                if detached {
                    break; // the viewed session detached/exited → quit the cockpit
                }
            }
            Some(ev) = worker.recv() => {
                match ev {
                    DisplayEvent::Ready { seq, key, attachment } => {
                        if reaped_ids.remove(&attachment.id()) {
                            // Exited raced ahead of this Ready: the child already died and its
                            // pump (one Exited at EOF) has ended. Inserting now would leave a
                            // dead, never-reaped pane that contains() refuses to re-attach.
                            // Tear it down and clear in-flight so the next settle re-requests.
                            in_flight.remove(&key);
                            attachment.teardown();
                        } else if attach_reply_is_current(&in_flight, &key, seq) {
                            in_flight.remove(&key);
                            registry.insert(&key, attachment);
                            // DIAG (temp): was raw mode already off right after an attach landed?
                            // If RAW off here, the ConPTY spawn on the worker thread reset it.
                            if !ratatui::crossterm::terminal::is_raw_mode_enabled().unwrap_or(true) {
                                dbg_log(&env.xmux_dir, &format!("DIAG raw OFF right after attach insert key={key}"));
                            }
                            // The selection may have moved to another session of the same host
                            // while this first-attach was in flight; clearing the latch makes the
                            // next pass re-run select_attach, which (now that the host PTY exists)
                            // issues the deferred switch-client to the current selection.
                            last_attached_sel = Selection::default();
                        } else {
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
                // Scan for SGR mouse sequences BEFORE routing to Overlay/Passthrough branches.
                // Mouse capture is global, so mouse bytes arrive in both states; scanning here
                // prevents them from reaching handle_tree_bytes (which would mis-decode them)
                // or TermInput's prefix logic. Split into: mouse events + non-mouse byte stream.
                // Edge case: a sequence split across reads parses as None and falls into
                // non_mouse — rare in practice; no cross-read buffering in v1.
                let (vw, vh) = terminal_view_size(cols, body_rows, tree_width);
                let term_x = if tree_width == 0 { 0 } else { tree_width + 1 };
                let term_area = ratatui::layout::Rect::new(term_x, 0, vw, vh);
                let mut non_mouse: Vec<u8> = Vec::with_capacity(bytes.len());
                let mut mouse_focus_toggle = false;
                let mut wheel_scrolled = false;
                {
                    let mut i = 0;
                    while i < bytes.len() {
                        if let Some((ev, len)) = crate::proxy::mouse::parse_sgr_mouse(&bytes[i..]) {
                            let in_mux = to_grid_local(term_area, ev.col, ev.row);
                            // A LEFT-button press in the UNFOCUSED pane switches focus to that
                            // pane — focus only; the click is not delivered. Right-click is
                            // reserved for the tree context menu, so it never moves focus.
                            // Within the focused mux pane, the click forwards.
                            let is_press = ev.pressed && (ev.cb & 0x60) == 0;
                            // Wheel events carry the 0x40 bit (cb 64=up, 65=down; +16=Ctrl).
                            let is_wheel = ev.pressed && (ev.cb & 0x40) != 0;
                            // Divider drag: grab the divider rule (the column at the effective
                            // tree width, only when the tree is shown) with the left button and
                            // drag to resize. Once grabbed it owns every mouse event until the
                            // button is released. Sets the NATURAL width; the loop-top reconcile
                            // applies it and resizes the PTYs (same path as prefix h/l).
                            let col0 = ev.col.saturating_sub(1); // 1-based SGR → 0-based screen col
                            // A context menu owns every mouse event until the right
                            // button is released (press-hold-release), exactly like the
                            // divider drag below. Motion sets the hovered item; button-up
                            // acts on it (or cancels if released off-menu).
                            if switcher.menu_active() {
                                if !ev.pressed {
                                    match switcher.menu_release() {
                                        crate::ui::switcher::MenuOutcome::FocusTerminal => {
                                            // Connect the target's host (mirrors the left-click
                                            // select path) so its control client streams, then
                                            // focus the mux on the now-selected session.
                                            ensure_current_host(&mut mgr, &env, &switcher, cols, body_rows, tree_width);
                                            if app.is_overlay() {
                                                app.toggle();
                                            }
                                        }
                                        crate::ui::switcher::MenuOutcome::Handled => {
                                            // A menu item may queue an op (split) or a
                                            // re-scan (reconnect); dispatch them here so it
                                            // works in either focus state, not only overlay.
                                            dispatch_pending_op(&mut switcher, &ops, &op_tx);
                                            kick_rescan(&mut switcher, &env, &mgr, &enum_tx);
                                            ensure_current_host(&mut mgr, &env, &switcher, cols, body_rows, tree_width);
                                        }
                                        crate::ui::switcher::MenuOutcome::None => {}
                                    }
                                    dirty = true;
                                } else if !is_wheel {
                                    switcher.menu_hover(col0, ev.row.saturating_sub(1));
                                    dirty = true;
                                }
                                i += len;
                                continue;
                            }
                            if dragging_divider {
                                if !ev.pressed {
                                    // Button up ends the drag; persist the final width once
                                    // (motion resizes live but does not write per cell).
                                    dragging_divider = false;
                                    crate::state::save_tree_width(&env.xmux_dir, tree_width_natural);
                                } else if !is_wheel {
                                    let target = divider_drag_width(ev.col);
                                    if target != tree_width_natural {
                                        tree_width_natural = target;
                                        dirty = true;
                                    }
                                }
                                i += len;
                                continue;
                            }
                            let is_left_press = is_press && (ev.cb & 0x03) == 0;
                            if is_left_press && tree_width > 0 && col0 == tree_width {
                                dragging_divider = true; // grabbed the divider
                                i += len;
                                continue;
                            }
                            // Idle motion (motion bit set, no button held) — reported only
                            // because any-motion tracking (1003h) is on. Over the divider it
                            // lights the hover cue and is consumed (nothing under it to forward).
                            // Elsewhere it falls through to the routing below, so a hover over the
                            // mux pane IS forwarded to the child (the inner app gets hover); over
                            // the tree it is harmlessly dropped.
                            if ev.pressed && (ev.cb & 0x23) == 0x23 {
                                let over_divider = tree_width > 0 && col0 == tree_width;
                                if over_divider != hovered_divider {
                                    hovered_divider = over_divider;
                                    dirty = true;
                                }
                                if over_divider {
                                    i += len;
                                    continue;
                                }
                            }
                            // Right-button press over the tree opens that row's context
                            // menu (press-hold-release). Tree-only: a press over the mux
                            // pane falls through and forwards to the child as before.
                            let is_right_press = is_press && (ev.cb & 0x03) == 2;
                            if is_right_press && in_mux.is_none() && switcher.menu_open(col0, ev.row.saturating_sub(1)) {
                                // The menu's actions (rename input, kill confirm) are driven by
                                // the tree's keyboard path, and a kill confirmed in the mux-focus
                                // state would quit the cockpit when the killed PTY exits. So a
                                // menu always operates in tree focus: take it now if the mux had
                                // it (the tree is necessarily visible here — a hidden tree has no
                                // column to right-click, so menu_open would have returned false).
                                if !app.is_overlay() {
                                    app.toggle();
                                }
                                dirty = true;
                                i += len;
                                continue;
                            }
                            if is_wheel && app.is_overlay() {
                                let down = (ev.cb & 0x01) != 0;
                                if (ev.cb & 0x10) != 0 {
                                    // Ctrl+wheel → change level (← ascend / → descend); inject the
                                    // arrow so the tree path (decode → handle_key → ensure) drives it.
                                    non_mouse.extend_from_slice(if down { b"\x1b[C" } else { b"\x1b[D" });
                                } else {
                                    // Plain wheel → scroll the cursor LINEARLY through every row
                                    // (move_selection), like any list. NOT sibling-cycle: arrows do
                                    // that (move_sibling), but it wraps within a level, so a 2-sibling
                                    // level just bounces — the "two notches per move" report.
                                    switcher.mouse_scroll(down);
                                    wheel_scrolled = true;
                                    dirty = true;
                                }
                            } else if is_left_press && app.is_overlay() && in_mux.is_some() {
                                app.toggle(); // tree → mux focus
                                mouse_focus_toggle = true;
                            } else if is_left_press && app.is_overlay() && in_mux.is_none() {
                                // Left-click a tree row → move the cursor to it (select). The
                                // loop top commits the new selection (attach); ensure the
                                // clicked row's host connects so its subtree streams in.
                                switcher.mouse_select(col0, ev.row.saturating_sub(1));
                                ensure_current_host(&mut mgr, &env, &switcher, cols, body_rows, tree_width);
                                dirty = true;
                            } else if is_left_press && !app.is_overlay() && in_mux.is_none() {
                                app.toggle(); // mux → tree focus
                                mouse_focus_toggle = true;
                            } else if !app.is_overlay() {
                                if let Some((gc, gr)) = in_mux {
                                    registry.input(
                                        &display_key(&env, &selection),
                                        crate::proxy::mouse::encode_sgr_mouse(&ev, gc, gr),
                                    );
                                }
                            }
                            // Overlay + a tree-column click → focus only (no select) → dropped.
                            i += len;
                        } else {
                            non_mouse.push(bytes[i]);
                            i += 1;
                        }
                    }
                }
                // Watchdog: a divider drag is normally ended by the button-up event, but a
                // release can be lost (split across reads, released off-window, or a terminal
                // that omits it) — which would strand `dragging_divider` and eat all later
                // mouse input. Any non-mouse byte (a keystroke, or the split release's own
                // leftover bytes) ends the drag and persists the final width, so the user is
                // never trapped past the next input.
                if dragging_divider && !non_mouse.is_empty() {
                    dragging_divider = false;
                    crate::state::save_tree_width(&env.xmux_dir, tree_width_natural);
                }
                // Watchdog: a keystroke (or any non-mouse byte) during a held menu ends
                // the gesture without acting — mirrors the divider-drag watchdog, so a
                // missed button-up can't strand the menu and eat later input.
                if switcher.menu_active() && !non_mouse.is_empty() {
                    switcher.menu_cancel();
                    dirty = true;
                }
                if mouse_focus_toggle {
                    dirty = true;
                }
                if wheel_scrolled {
                    // The plain-wheel scroll moved the cursor; connect the host it landed on
                    // so its subtree streams in (mirrors handle_tree_bytes's ensure step).
                    ensure_current_host(&mut mgr, &env, &switcher, cols, body_rows, tree_width);
                }
                // Resize-repeat: while the window from a prefix-driven resize is open, a
                // bare Ctrl+←/→ (no prefix, in either focus) keeps resizing and refreshes
                // the window. Gated on NOT being mid-prefix (an armed prefix's next key is
                // a command, not a repeat — else skipping the input path would leave the
                // prefix armed and mis-read the following key). A pure-mouse read (empty
                // non_mouse) leaves the window untouched. Leading Ctrl-arrows are peeled off
                // (handles a coalesced autorepeat burst); any remaining bytes end the window
                // and fall through to the normal tree/mux routing below.
                let mut consumed_by_repeat = false;
                if repeat_until.is_some_and(|d| std::time::Instant::now() < d)
                    && !tree_armed
                    && !term_input.is_armed()
                    && !non_mouse.is_empty()
                {
                    let mut n = 0;
                    while let Some((d, len)) = leading_ctrl_arrow(&non_mouse[n..]) {
                        apply_width_delta(d, &mut tree_width_natural, &env.xmux_dir);
                        n += len;
                    }
                    if n > 0 {
                        non_mouse.drain(0..n);
                        dirty = true;
                        if non_mouse.is_empty() {
                            repeat_until = Some(
                                std::time::Instant::now() + Duration::from_millis(RESIZE_REPEAT_MS),
                            );
                            consumed_by_repeat = true;
                        } else {
                            repeat_until = None; // trailing non-arrow bytes end + route below
                        }
                    } else {
                        repeat_until = None; // first key isn't a Ctrl-arrow → end the window
                    }
                }
                if !consumed_by_repeat && !non_mouse.is_empty() && switcher.feed_help_key(&non_mouse) {
                    // The help overlay is modal (tmux view-mode style): while open it
                    // captures every key in EITHER focus — q/Esc closes it, the rest are
                    // swallowed — so nothing leaks to the tree or the mux pane. Above the
                    // tree/mux split so the behavior is identical regardless of focus.
                    dirty = true;
                } else if !consumed_by_repeat && app.is_overlay() {
                    let (ft, q, wd, th) = handle_tree_bytes(
                        &non_mouse, &mut tree_decoder, &mut tree_armed, prefix, &mut switcher,
                        &mut mgr, &env, &ops, &op_tx, &enum_tx, cols, body_rows, tree_width,
                    );
                    focus_terminal = ft;
                    quit = q;
                    if wd != 0 {
                        // A prefix-driven resize: apply it and open the repeat window so the
                        // next bare Ctrl+←/→ keeps resizing without re-pressing the prefix.
                        apply_width_delta(wd, &mut tree_width_natural, &env.xmux_dir);
                        repeat_until =
                            Some(std::time::Instant::now() + Duration::from_millis(RESIZE_REPEAT_MS));
                    }
                    if th {
                        toggle_auto_hide(&mut auto_hide_tree, &env.xmux_dir);
                        dirty = true;
                    }
                } else if !consumed_by_repeat {
                    // TERMINAL focus: forward raw bytes to the selected session's PTY;
                    // TermInput intercepts the prefix (→ tree / quit / help / resize / literal).
                    for action in term_input.feed(&non_mouse) {
                        match action {
                            TermAction::Forward(f) => registry.input(&display_key(&env, &selection), f),
                            TermAction::FocusTree(rest) => {
                                focus_tree = true;
                                tree_replay = rest;
                            }
                            TermAction::Quit => quit = true,
                            TermAction::ShowHelp => {
                                switcher.toggle_help();
                                dirty = true;
                            }
                            TermAction::Width(d) => {
                                // Same resize + repeat-window as the tree path, so a resize
                                // started from the mux pane chains with bare Ctrl+←/→ too.
                                apply_width_delta(d, &mut tree_width_natural, &env.xmux_dir);
                                repeat_until = Some(
                                    std::time::Instant::now() + Duration::from_millis(RESIZE_REPEAT_MS),
                                );
                            }
                            TermAction::ToggleAutoHide => {
                                toggle_auto_hide(&mut auto_hide_tree, &env.xmux_dir);
                                dirty = true;
                            }
                        }
                    }
                }
                if focus_terminal && app.is_overlay() {
                    app.toggle();
                    // No term.clear(): both states draw the SAME split layout (only the
                    // divider colour changes), so clearing would blank the screen and
                    // force a full repaint for nothing.
                }
                if focus_tree && !app.is_overlay() {
                    app.toggle();
                    if !tree_replay.is_empty() {
                        let (ft, q, wd, th) = handle_tree_bytes(
                            &tree_replay, &mut tree_decoder, &mut tree_armed, prefix,
                            &mut switcher, &mut mgr, &env, &ops, &op_tx, &enum_tx, cols, body_rows, tree_width,
                        );
                        if ft && app.is_overlay() {
                            app.toggle();
                        }
                        quit = quit || q;
                        if wd != 0 {
                            // A prefix-driven resize on the replayed bytes: apply + open the
                            // repeat window, same as the direct tree-focus path above.
                            apply_width_delta(wd, &mut tree_width_natural, &env.xmux_dir);
                            repeat_until =
                                Some(std::time::Instant::now() + Duration::from_millis(RESIZE_REPEAT_MS));
                        }
                        if th {
                            toggle_auto_hide(&mut auto_hide_tree, &env.xmux_dir);
                            dirty = true;
                        }
                    }

                }
                if quit {
                    break;
                }
            }
            Some(cmd) = cmd_rx.recv() => {
                match cmd {
                    Cmd::Key(k) => {
                        switcher.handle_key(k);
                        dispatch_pending_op(&mut switcher, &ops, &op_tx);
                        ensure_current_host(&mut mgr, &env, &switcher, cols, body_rows, tree_width);
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
                            sync_source_terminals(&mut registry, &env, &source, &sessions, &mut host_session, &worker, &mut in_flight, &mut attach_seq, cols, body_rows, tree_width);
                        }
                    }
                    LocalEnum::Panes { address, panes } => {
                        switcher.apply_panes(address, panes);
                        // Local psmux has no `%`-event stream, so window changes inside
                        // the displayed session are only seen on the poll.
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
                        let (vc, vr) = terminal_view_size(c, body, tree_width);
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
                let (vc, vr) = terminal_view_size(cols, body_rows, tree_width);
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
                        if !in_flight.contains_key(&src.alias) {
                            request_attach(
                                &mut registry, &worker, &mut in_flight, &mut attach_seq,
                                &src.alias, src.attach_command(&s.name, None), vc, vr,
                            );
                            host_session.insert(src.alias.clone(), s.name.clone());
                        }
                    }
                }
                // Re-attach the selected session's display terminal if it dropped.
                if !selection.is_empty() {
                    let key = display_key(&env, &selection);
                    if !registry.contains(&key) && !in_flight.contains_key(&key) {
                        select_attach(
                            &mut registry, &mut mgr, &env, &selection, &mut host_session,
                            &worker, &mut in_flight, &mut attach_seq, cols, body_rows, tree_width,
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
    fn enter_focuses_terminal_tab_does_not() {
        assert!(is_focus_in(KeyCode::Enter));
        assert!(!is_focus_in(KeyCode::Char('\t')));
        assert!(!is_focus_in(KeyCode::Right));
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
        connect_all_sources(&mut mgr, &env, 80, 24, crate::ui::switcher::TREE_WIDTH, &enum_tx);
        assert!(mgr.get("jupiter06").is_some(), "remote host got a control client");
        mgr.teardown_all();
    }

    #[test]
    fn terminal_view_size_zero_tree_is_full_width() {
        // Hidden tree (sentinel 0): full cols, no divider subtracted.
        assert_eq!(terminal_view_size(80, 23, 0), (80, 24));
        // Shown tree: cols - tree_width - 1 (divider), height = body_rows + 1.
        assert_eq!(terminal_view_size(80, 23, 48), (31, 24));
        // Degenerate widths clamp to at least 1.
        assert_eq!(terminal_view_size(0, 0, 0), (1, 1));
    }

    #[test]
    fn reconciled_tree_width_hides_only_when_focused_and_enabled() {
        // Tree focused (terminal_focused = false): always the natural width.
        assert_eq!(reconciled_tree_width(false, true, 48), 48);
        assert_eq!(reconciled_tree_width(false, false, 48), 48);
        // Mux focused + setting on: hidden (0).
        assert_eq!(reconciled_tree_width(true, true, 48), 0);
        // Mux focused + setting off: stays shown at natural width.
        assert_eq!(reconciled_tree_width(true, false, 48), 48);
    }

    #[test]
    fn leading_ctrl_arrow_peels_one_and_ignores_others() {
        assert_eq!(leading_ctrl_arrow(b"\x1b[1;5C"), Some((1, 6)), "Ctrl-Right widens");
        assert_eq!(leading_ctrl_arrow(b"\x1b[1;5D"), Some((-1, 6)), "Ctrl-Left narrows");
        // A LEADING Ctrl-arrow is peeled even with trailing bytes (the caller loops /
        // routes the remainder) — this is what makes a coalesced autorepeat keep going.
        assert_eq!(leading_ctrl_arrow(b"\x1b[1;5C\x1b[1;5C"), Some((1, 6)), "peels the first of a burst");
        assert_eq!(leading_ctrl_arrow(b"\x1b[1;5Cx"), Some((1, 6)), "peels past trailing input");
        // Bare arrows and h/l are not repeat keys.
        assert_eq!(leading_ctrl_arrow(b"\x1b[C"), None, "bare arrow is not a repeat key");
        assert_eq!(leading_ctrl_arrow(b"l"), None, "h/l are not repeat keys");
        assert_eq!(leading_ctrl_arrow(b""), None, "empty is not a repeat key");
    }

    #[test]
    fn apply_width_delta_clamps_and_ignores_zero() {
        let dir = std::env::temp_dir().join(format!("xmux-awd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut w = 48u16;
        apply_width_delta(1, &mut w, &dir);
        assert_eq!(w, 49);
        apply_width_delta(0, &mut w, &dir);
        assert_eq!(w, 49, "zero delta is a no-op");
        let mut hi = TREE_WIDTH_MAX;
        apply_width_delta(10, &mut hi, &dir);
        assert_eq!(hi, TREE_WIDTH_MAX, "clamps at the max");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn divider_drag_width_clamps_to_range() {
        // The dragged 1-based column becomes the 0-based tree width, clamped to range.
        assert_eq!(divider_drag_width(51), 50);
        assert_eq!(divider_drag_width(5), TREE_WIDTH_MIN, "too far left clamps to min");
        assert_eq!(divider_drag_width(500), TREE_WIDTH_MAX, "too far right clamps to max");
    }

    #[test]
    fn spinner_frame_advances_with_wall_clock() {
        use std::time::Duration;
        assert_eq!(spinner_frame_at(Duration::from_millis(0)), 0);
        assert_eq!(spinner_frame_at(Duration::from_millis(SPINNER_FRAME_MS)), 1);
        assert_eq!(spinner_frame_at(Duration::from_millis(SPINNER_FRAME_MS * 3 + 10)), 3);
    }

    #[test]
    fn tree_width_adjust_clamps() {
        assert_eq!(adjust_tree_width(48, 1), 49);
        assert_eq!(adjust_tree_width(48, -1), 47);
        assert_eq!(adjust_tree_width(20, -1), 20, "clamped at min");
        assert_eq!(adjust_tree_width(100, 1), 100, "clamped at max");
    }

    #[test]
    fn terminal_view_size_subtracts_tree_and_divider() {
        use crate::ui::switcher::TREE_WIDTH;
        let (vc, vr) = terminal_view_size(143, 39, TREE_WIDTH);
        assert_eq!(vc, 143 - (TREE_WIDTH + 1), "cols minus tree minus divider");
        // The footer and input live in the tree column, so the terminal column
        // spans the full terminal height (body_rows + 1).
        assert_eq!(vr, 40, "height is the full terminal height (body_rows + 1)");
    }

    #[test]
    fn terminal_view_size_clamps_to_at_least_one() {
        use crate::ui::switcher::TREE_WIDTH;
        let (vc, vr) = terminal_view_size(10, 0, TREE_WIDTH);
        assert_eq!(vc, 1);
        // (0 + 1).max(1) = 1: clamping still holds for zero body rows.
        assert_eq!(vr, 1);
    }

    #[test]
    fn to_grid_local_inside_area_maps_correctly() {
        // Terminal area starts at screen col 50 (x=49 0-based), row 0, size 80×24.
        // SGR cell (52,3) = 0-based (51,2) which is inside (49..129, 0..24).
        // grid-local = (51-49+1, 2-0+1) = (3, 3) in 1-based.
        let area = ratatui::layout::Rect::new(49, 0, 80, 24);
        assert_eq!(to_grid_local(area, 52, 3), Some((3, 3)));
    }

    #[test]
    fn to_grid_local_in_tree_column_returns_none() {
        // Terminal area starts at screen col 50 (0-based). SGR col 10 is in the tree.
        let area = ratatui::layout::Rect::new(49, 0, 80, 24);
        assert_eq!(to_grid_local(area, 10, 5), None);
    }

    #[test]
    fn to_grid_local_boundary_cells() {
        // area (49,0,80,24): valid cols 49..129, valid rows 0..24 (0-based).
        // Top-left corner: SGR (50,1) → 0-based (49,0) → grid-local (1,1).
        let area = ratatui::layout::Rect::new(49, 0, 80, 24);
        assert_eq!(to_grid_local(area, 50, 1), Some((1, 1)));
        // Bottom-right corner: SGR (129,24) → 0-based (128,23) → grid-local (80,24).
        assert_eq!(to_grid_local(area, 129, 24), Some((80, 24)));
        // One past the right edge: 0-based col 129 >= 49+80=129 → None.
        assert_eq!(to_grid_local(area, 130, 1), None);
        // One past the bottom: 0-based row 24 >= 0+24=24 → None.
        assert_eq!(to_grid_local(area, 50, 25), None);
    }

    #[test]
    fn to_grid_local_zero_col_or_row_returns_none() {
        let area = ratatui::layout::Rect::new(0, 0, 80, 24);
        assert_eq!(to_grid_local(area, 0, 5), None, "col=0 triggers checked_sub None");
        assert_eq!(to_grid_local(area, 5, 0), None, "row=0 triggers checked_sub None");
    }

    #[test]
    fn to_grid_local_full_width_area_maps_left_edge() {
        // Tree hidden (auto-hide-tree): the mux owns the whole screen, so the
        // input handler builds term_area at x=0. The top-left cell SGR (1,1) must map
        // to grid-local (1,1) rather than being rejected as it would in the tree column.
        let area = ratatui::layout::Rect::new(0, 0, 80, 24);
        assert_eq!(to_grid_local(area, 1, 1), Some((1, 1)));
        assert_eq!(to_grid_local(area, 80, 24), Some((80, 24)));
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
    async fn host_exited_with_no_sessions_marks_empty_not_unreachable() {
        use crate::ui::run::dump_overlay;
        use crate::ui::switcher::Switcher;
        let mut switcher = Switcher::from_sources(vec!["jupiter06".into()]);
        let connected: HashSet<String> = HashSet::new();
        // A reachable host whose mux has no server: "no sessions" → (empty), not ⚠.
        assert!(
            !note_host_exited(&mut switcher, &connected, "jupiter06", Some("no sessions".into())),
            "an empty mux is reachable, not unreachable"
        );
        let out = dump_overlay(&mut switcher, None, 80, 24);
        assert!(out.contains("empty"), "an empty host reads (empty):\n{out}");
        assert!(!out.contains("unreachable"), "must NOT read unreachable:\n{out}");
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
    fn active_window_probe_moves_sidebar_cursor() {
        // A resolved active-window probe (HostEvent::Focus) sets the cached active-window
        // marker; the loop-top select_active_window then moves the cursor. Cursor starts
        // on window 1's row; Focus to window 0 sets the marker, and select_active_window
        // (simulating the loop-top call) lands the cursor on window 0.
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
        // session row -> (→ descend) window 0 -> (↓ sibling) window 1.
        switcher.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        switcher.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(switcher.terminal_view_target().target, "api:1", "cursor on window 1");

        let (htx, _hrx) = tokio::sync::mpsc::unbounded_channel::<HostEvent>();
        let mut mgr = HostManager::new(htx);
        let (ptx, _prx) = tokio::sync::mpsc::unbounded_channel::<PtyEvent>();
        let mut registry = AttachRegistry::new();
        let worker = DisplayWorker::new(ptx);
        let mut in_flight = HashMap::new();
        let mut attach_seq = 0u64;
        let env = fake_env_with_sources(&["jup"]);
        let mut connected = HashSet::new();
        let mut panes_requested = HashSet::new();
        let mut host_session = HashMap::new();
        // Focus sets the cached active-window marker (window 0).
        handle_host_event(
            HostEvent::Focus { host: "jup".into(), session: "api".into(), window: 0 },
            &mut mgr, &mut registry, &mut switcher, &env,
            &mut connected, &mut panes_requested, &mut host_session,
            &worker, &mut in_flight, &mut attach_seq, 80, 24, crate::ui::switcher::TREE_WIDTH,
        );
        // The loop-top follow (simulated here) consumes the marker and moves the cursor.
        switcher.select_active_window();
        assert_eq!(switcher.terminal_view_target().target, "api:0", "loop-top follow moved cursor to active window 0");
    }

    #[test]
    fn focus_event_updates_marker_without_moving_cursor() {
        // handle_host_event(Focus) updates the active-window marker but never moves
        // the cursor — cursor follow is a loop-top concern. The cursor is left wherever
        // the caller placed it (here, window 1) regardless of the Focus payload.
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
        switcher.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE)); // → window 0
        switcher.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)); // ↓ → window 1
        assert_eq!(switcher.terminal_view_target().target, "api:1");

        let (htx, _hrx) = tokio::sync::mpsc::unbounded_channel::<HostEvent>();
        let mut mgr = HostManager::new(htx);
        let (ptx, _prx) = tokio::sync::mpsc::unbounded_channel::<PtyEvent>();
        let mut registry = AttachRegistry::new();
        let worker = DisplayWorker::new(ptx);
        let mut in_flight = HashMap::new();
        let mut attach_seq = 0u64;
        let env = fake_env_with_sources(&["jup"]);
        let mut connected = HashSet::new();
        let mut panes_requested = HashSet::new();
        let mut host_session = HashMap::new();
        handle_host_event(
            HostEvent::Focus { host: "jup".into(), session: "api".into(), window: 0 },
            &mut mgr, &mut registry, &mut switcher, &env,
            &mut connected, &mut panes_requested, &mut host_session,
            &worker, &mut in_flight, &mut attach_seq, 80, 24, crate::ui::switcher::TREE_WIDTH,
        );
        assert_eq!(switcher.terminal_view_target().target, "api:1", "handler alone must not move the cursor");
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

    #[test]
    fn input_focus_locks_to_overlay_and_restores_prior_focus() {
        use crate::proxy::app::AppState;
        // Opened from mux focus: capture Passthrough once, force Overlay so keys reach the input.
        let mut state = AppState::Passthrough;
        let mut saved = None;
        reconcile_input_focus(true, &mut state, &mut saved);
        assert_eq!(state, AppState::Overlay, "input forces tree focus");
        assert_eq!(saved, Some(AppState::Passthrough), "prior focus captured once");
        // A stray toggle mid-input is overridden back to Overlay; the capture is not clobbered.
        state = AppState::Passthrough;
        reconcile_input_focus(true, &mut state, &mut saved);
        assert_eq!(state, AppState::Overlay, "focus stays locked while the input is open");
        assert_eq!(saved, Some(AppState::Passthrough), "capture survives re-locks");
        // On close, restore the captured focus and clear it.
        reconcile_input_focus(false, &mut state, &mut saved);
        assert_eq!(state, AppState::Passthrough, "prior focus restored when the input closes");
        assert_eq!(saved, None, "capture cleared after restore");
        // With nothing captured, a no-input tick leaves focus untouched.
        reconcile_input_focus(false, &mut state, &mut saved);
        assert_eq!(state, AppState::Passthrough);
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

    #[test]
    fn remote_second_session_in_flight_keeps_original_host_session() {
        use std::collections::HashMap;
        let mut remote = fake_source("jup");
        remote.remote = true;
        let by_alias: HashMap<String, Source> =
            [("jup".to_string(), remote.clone())].into_iter().collect();
        let env = Env {
            cfg: Config::default(),
            cfg_warnings: Vec::new(),
            srcs: vec![remote],
            by_alias,
            local_bin: "cmd.exe".into(),
            ui_prefix: "C-g".into(),
            xmux_dir: std::path::PathBuf::from("."),
        };
        let (ptx, _prx) = tokio::sync::mpsc::unbounded_channel();
        let worker = crate::display::DisplayWorker::new(ptx);
        let mut registry = AttachRegistry::new();
        let (htx, _hrx) = tokio::sync::mpsc::unbounded_channel::<HostEvent>();
        let mut mgr = HostManager::new(htx);
        let mut host_session: HashMap<String, String> = HashMap::new();
        let mut in_flight: HashMap<String, u64> = HashMap::new();
        let mut attach_seq = 0u64;

        let sel_a = Selection { source: "jup".into(), session: "a".into(), window: None };
        let sel_b = Selection { source: "jup".into(), session: "b".into(), window: None };

        // First attach (session a): requests off-loop, latches host_session=a, marks in-flight.
        assert!(select_attach(&mut registry, &mut mgr, &env, &sel_a, &mut host_session,
            &worker, &mut in_flight, &mut attach_seq, 80, 24, crate::ui::switcher::TREE_WIDTH));
        assert_eq!(host_session.get("jup"), Some(&"a".to_string()));
        assert!(in_flight.contains_key("jup"), "first attach is in flight");

        // Select session b of the SAME host before a's Ready arrives: must NOT overwrite host_session
        // (else the switch-client to b after a lands would never fire).
        assert!(select_attach(&mut registry, &mut mgr, &env, &sel_b, &mut host_session,
            &worker, &mut in_flight, &mut attach_seq, 80, 24, crate::ui::switcher::TREE_WIDTH));
        assert_eq!(host_session.get("jup"), Some(&"a".to_string()),
            "an in-flight attach must not latch host_session to the new target");

        mgr.teardown_all();
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
    // 4. Press Enter (or C-g → / C-g Tab) — focus the terminal (Passthrough); the split
    //    is unchanged (divider turns green) and keystrokes reach the real attached pane.
    //    C-g ← / C-g Esc / C-g Tab return focus to the tree. Confirm no blank/flash.
    // 5. Create / kill a window or session inside a pane — confirm the sidebar tree
    //    syncs (remote via control events, local within the poll interval) and the
    //    PTY set follows (new session attaches, killed session's PTY is reaped).
    // 6. C-g then `q` — clean quit, terminal restored.
    // 7. NEVER attach the session that owns xmux (xmux refuses to run inside a mux,
    //    so in normal use no session mirrors the UI).
    // 8. Mouse: dragging never selects native terminal text (the cockpit captures the
    //    mouse). A LEFT-button press in the UNFOCUSED pane switches focus to it (focus
    //    only — the click is not delivered); right-click never moves focus (it opens the
    //    tree context menu). Once the mux pane is focused, clicks/scroll/
    //    right-click reach the mux (status-bar click, pane select, scroll, context menu).
    //    Mux mouse forwarding requires the mux to have `mouse on` (`set -g mouse on`);
    //    xmux only forwards. (Windows: capture needs ENABLE_VIRTUAL_TERMINAL_INPUT +
    //    the SGR DECSET that crossterm's WinAPI path omits — see proxy::term.)
    // =========================================================================
}
