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
//! draws the SAME split (tree + selected PTY grid) in both focus states — Focus::Tree
//! (tree focused) and Focus::Terminal (terminal focused) differ only in the divider
//! colour and where keys go, so toggling focus needs no screen clear.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use ratatui::crossterm::event::{KeyCode, KeyModifiers};

use crate::attach;
use crate::backend::SelectOutcome;
use crate::display::{DisplayEnsure, DisplayEvent, DisplayWorker};
use crate::env::Env;
use crate::host::{HostEvent, HostManager};
use crate::proxy::dispatch::Action;
use crate::proxy::registry::AttachRegistry;
use crate::proxy::run::PtyEvent;
/// The settled-attach debounce (the freeze fix). Owned by `state` (the `apply(Tick)`
/// re-arm uses it); the host-event re-arm paths reference the same constant so the
/// value can never drift between the two.
use crate::state::ATTACH_DEBOUNCE_MS;
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

/// Minimum interval between redraws. Drawing is decoupled from events and capped
/// to this frame rate: rapid input (or a busy PTY) sets a `dirty` flag, and the
/// loop redraws at most once per frame — so no navigation pattern can flood the
/// terminal with full-screen repaints and stall the single-threaded loop. A frame
/// timer at this cadence flushes a pending dirty draw promptly even with no input.
const FRAME_MS: u64 = 33;

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

/// How long after the last resize tick before the debounced tree-width persist fires.
/// Longer than `RESIZE_REPEAT_MS` so a held Ctrl-arrow autorepeat burst persists once
/// at the end, not per tick.
const WIDTH_FLUSH_MS: u64 = 400;

fn adjust_tree_width(w: u16, delta: i32) -> u16 {
    (w as i32 + delta).clamp(TREE_WIDTH_MIN as i32, TREE_WIDTH_MAX as i32) as u16
}

/// Adjusts the natural tree width by `wd`, clamped to the allowed range. Returns
/// true if the width actually changed (so the loop can schedule a debounced
/// persist). A zero delta or a clamp-noop returns false. Write-free: the loop
/// owns the single persist.
fn apply_width_delta(wd: i32, natural: &mut u16) -> bool {
    if wd == 0 {
        return false;
    }
    let next = adjust_tree_width(*natural, wd);
    if next == *natural {
        return false;
    }
    *natural = next;
    true
}

/// Flips the auto-hide-tree mode and persists it, so the next launch restores it.
/// Shared by the tree- and mux-focus `prefix t` paths. The effective tree width is
/// reconciled at the next loop top (`reconciled_tree_width`); the caller marks dirty.
fn toggle_auto_hide(mode: &mut bool, xmux_dir: &std::path::Path) {
    *mode = !*mode;
    crate::prefs::save_auto_hide_tree(xmux_dir, *mode);
}

/// Folds ONE domain [`Action`] in at the single mutation site ([`State::apply`]) and
/// runs the [`Command`]s it returns — the site both a keypress (via
/// `proxy::dispatch::Action::as_action`) and a ctl command resolve through, so the two
/// surfaces can never take divergent effect. Returns `(quit, width_changed)`: `quit`
/// signals the loop to exit; `width_changed` signals the loop to schedule the debounced
/// tree-width persist. `Switch` only moves the cursor (a `SelectAddress` command); the
/// loop-top `Tick`/`select_attach` commits the attach on a later pass.
///
/// Only the synchronous, registry-free commands arise here — `Attach`/
/// `PersistLastSession` come exclusively from `Action::Tick`, which the run loop drives
/// with full registry access. `Action::Quit` is the only quit path through this dispatcher.
///
/// [`Action`]: crate::model::Action
/// [`Command`]: crate::model::Command
/// [`State::apply`]: crate::state::State::apply
#[allow(clippy::too_many_arguments)]
fn dispatch_action(
    action: crate::model::Action,
    switcher: &mut crate::ui::switcher::Switcher,
    state: &mut crate::state::State,
    tree_width_natural: &mut u16,
    auto_hide_tree: &mut bool,
    xmux_dir: &std::path::Path,
    ops: &Arc<dyn crate::ui::switcher::Ops>,
    op_tx: &tokio::sync::mpsc::UnboundedSender<crate::ui::switcher::OpResult>,
) -> (bool, bool) {
    dispatch_commands(
        state.apply(action),
        switcher,
        state,
        tree_width_natural,
        auto_hide_tree,
        xmux_dir,
        ops,
        op_tx,
    )
}

/// Runs the [`Command`]s an [`Action`] produced — the sole dispatcher of the
/// synchronous, registry-free effects. `SelectAddress`/`Rescan`/`AdjustTreeWidth`/
/// `ToggleAutoHide`/`Quit` act on the switcher/width/loop here; `RunOp` is spawned
/// off-loop against the live mux (its `OpResult` folds back through `op_tx`, the
/// existing channel). `Attach`/`PersistLastSession` arise only from `Action::Tick`,
/// dispatched by the run loop with full registry access — never here.
///
/// [`Action`]: crate::model::Action
/// [`Command`]: crate::model::Command
#[allow(clippy::too_many_arguments)]
fn dispatch_commands(
    cmds: Vec<crate::model::Command>,
    switcher: &mut crate::ui::switcher::Switcher,
    state: &mut crate::state::State,
    tree_width_natural: &mut u16,
    auto_hide_tree: &mut bool,
    xmux_dir: &std::path::Path,
    ops: &Arc<dyn crate::ui::switcher::Ops>,
    op_tx: &tokio::sync::mpsc::UnboundedSender<crate::ui::switcher::OpResult>,
) -> (bool, bool) {
    use crate::model::Command;
    let mut quit = false;
    let mut width_changed = false;
    for cmd in cmds {
        match cmd {
            Command::SelectAddress(address) => {
                switcher.select_address(&address, state);
            }
            Command::Rescan => {
                switcher.request_rescan(state);
            }
            Command::AdjustTreeWidth(d) => {
                if apply_width_delta(d, tree_width_natural) {
                    width_changed = true;
                }
            }
            Command::ToggleAutoHide => toggle_auto_hide(auto_hide_tree, xmux_dir),
            Command::Quit => quit = true,
            Command::RunOp(op) => spawn_op(op, ops, op_tx),
            // Settled-selection effects come only from Action::Tick, dispatched by the
            // run loop with registry/host access — never from a key/ctl action here.
            Command::PersistLastSession(_) | Command::Attach(_) => {}
        }
    }
    (quit, width_changed)
}

/// The `status` verb reply: the current focus side + the cursor's session target.
/// A flat, parseable line an agent reads to confirm a `switch`/`focus` landed.
fn status_line(switcher: &crate::ui::switcher::Switcher, tree_focused: bool) -> String {
    format!(
        "focus={} target={}",
        if tree_focused { "tree" } else { "terminal" },
        switcher.terminal_view_target().target,
    )
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
pub(crate) fn dbg_log(dir: &std::path::Path, msg: &str) {
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
#[derive(Clone, Debug, Default, PartialEq, Eq)]
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
        let window = t
            .target
            .split(':')
            .nth(1)
            .and_then(|w| w.parse::<i64>().ok());
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

/// Derives the selection from the switcher cursor and, if it moved, routes it through
/// the single mutation site as [`Action::Select`] — which records the new selection
/// and marks the attach pending. It arms NO deadline; the trailing [`Action::Tick`]
/// arms the debounce (re-armed on every move, so rapid navigation coalesces into one
/// trailing attach). Returns true when the selection changed (the tree needs a redraw).
///
/// In 5.4d-i the switcher cursor is still the selection authority; this only routes
/// the derived value through `apply` instead of mutating `state` directly. (The
/// authority inversion — `state.selection` authoritative, cursor following — is 5.5.)
///
/// [`Action::Select`]: crate::model::Action::Select
/// [`Action::Tick`]: crate::model::Action::Tick
fn sync_selection_from_switcher(
    state: &mut crate::state::State,
    switcher: &crate::ui::switcher::Switcher,
) -> bool {
    let new_sel = Selection::from_target(&switcher.terminal_view_target());
    if new_sel == state.selection {
        return false;
    }
    state.apply(crate::model::Action::Select(new_sel));
    true
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

/// The `AttachRegistry` key for a selection.
pub(crate) fn display_key(hosts: &crate::model::Hosts, sel: &Selection) -> String {
    hosts
        .get(&sel.source)
        .map(host_selection_key)
        .unwrap_or_else(|| sel.address())
}

fn host_selection_key(host: &crate::model::Host) -> String {
    match host.mux.select() {
        SelectOutcome::SharedSwitch | SelectOutcome::PerSessionReattach => host.id().to_string(),
    }
}

/// The host id owning a display key: Shared keys ARE the host id; PerSession keys are
/// `host/session`, so the host id is the part before the first '/'.
fn host_of_key(key: &str) -> &str {
    key.split_once('/').map_or(key, |(h, _)| h)
}

/// The runtime attach facts the debounce gate needs, fed to [`State::apply`] as DATA
/// on [`Action::Tick`](crate::model::Action::Tick): whether the selected session's
/// display PTY is live, and whether an attach for its key is already in flight. The
/// gate (`should_attach`) lives in `State`; these facts (registry + host bookkeeping)
/// do not, so the loop computes them just before the Tick. An empty selection yields
/// `(false, false)` — the gate short-circuits on emptiness anyway.
///
/// [`State::apply`]: crate::state::State::apply
fn selection_attach_facts(
    registry: &AttachRegistry,
    hosts: &crate::model::Hosts,
    selection: &Selection,
) -> (bool, bool) {
    if selection.is_empty() {
        return (false, false);
    }
    let key = display_key(hosts, selection);
    let key_live = registry.contains(&key);
    let in_flight = hosts
        .get(&selection.source)
        .map(|h| h.display.in_flight.contains_key(&key))
        .unwrap_or(false);
    (key_live, in_flight)
}

/// Issues an OFF-LOOP attach for `key`: allocates the attachment id, records the request's
/// seq in the owning host's `display.in_flight` + the id→key in `display.pending`, and asks
/// the worker to spawn. The worker's `Ready` reply (handled in the cockpit loop) inserts the
/// finished attachment into the registry. `display` MUST be the host that owns `key`.
#[allow(clippy::too_many_arguments)]
fn request_attach(
    registry: &mut AttachRegistry,
    worker: &DisplayWorker,
    display: &mut crate::model::HostDisplay,
    attach_seq: &mut u64,
    key: &str,
    argv: Vec<String>,
    cols: u16,
    rows: u16,
) {
    let id = registry.alloc_id();
    *attach_seq += 1;
    display.in_flight.insert(key.to_string(), *attach_seq);
    display.pending.insert(id, key.to_string());
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
/// Shared (tmux): one kept PTY per host. The first attach lands on the selected
/// session; selecting a different session of the same host lowers a `SwitchPlan`
/// via `Transport::lower_switch`, targeting the in-memory `host.display_tty`.
/// PerSessionReattach (psmux): one PTY per host, reattached when the session changes.
/// In both cases a window-row selection moves the session's active window server-side,
/// which the real attached client follows. The bookkeeping (current session per key +
/// what spawn is in flight) lives on the owning `host.display`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn select_attach(
    registry: &mut AttachRegistry,
    hosts: &mut crate::model::Hosts,
    sel: &Selection,
    worker: &DisplayWorker,
    attach_seq: &mut u64,
    cols: u16,
    rows: u16,
    tree_width: u16,
    mgr: &HostManager,
) -> bool {
    if sel.is_empty() {
        return false;
    }
    let (cols, rows) = terminal_view_size(cols, rows, tree_width);
    // The host's open `-CC` control connection, if any. switch-client/select-window
    // ride it instead of a fresh `ssh` per switch (the slow path on Windows, which
    // has no ssh ControlMaster — each exec re-handshakes, ~0.5s; see #2).
    let control = mgr.get(&sel.source);
    let Some(host) = hosts.get_mut(&sel.source) else {
        return false;
    };
    let key = host_selection_key(host);
    let already = registry.contains(&key);

    match host.mux.select() {
        SelectOutcome::SharedSwitch => {
            if !already {
                // Off-loop first-attach: request the spawn ONLY if one is not already in flight.
                // Do NOT overwrite display.current while an attach is in flight — the in-flight
                // attach lands on its ORIGINAL target session, and the post-Ready re-evaluation
                // (see the Ready arm) issues a switch-client to the current selection. Overwriting
                // it here would make the switch-client guard think the PTY is already on the new
                // session.
                if !host.display.in_flight.contains_key(&key) {
                    // Build the argv (immutable mux/transport reads) BEFORE taking &mut display.
                    let mux_argv = host.mux.attach_plan(&sel.session, sel.window);
                    let (cmd, args) = host.transport.exec_argv(true, &mux_argv);
                    let mut argv = vec![cmd];
                    argv.extend(args);
                    if host.transport.is_remote() {
                        // Marker is a remote-shell mechanism: prefix the last element so the
                        // display-tty capture fires before exec'ing the attach command.
                        if let Some(last) = argv.last_mut() {
                            *last = format!(
                                "{}{}",
                                crate::model::death::display_tty_marker_prefix(),
                                last
                            );
                        }
                    }
                    request_attach(
                        registry,
                        worker,
                        &mut host.display,
                        attach_seq,
                        &key,
                        argv,
                        cols,
                        rows,
                    );
                    host.display.set_shows(&key, &sel.session);
                }
            } else if host.display.shows(&key) != Some(sel.session.as_str()) {
                // The host's PTY is on a different session — lower a SwitchPlan to move it.
                // Wipe the grid first so the previous session's cells do not linger as
                // residue: switch-client triggers a FULL client redraw, which refills the
                // cleared grid with the new session's content (a brief blank, not stale
                // colours/glyphs). The per-host PTY reuses ONE grid across sessions, so
                // without this the old session's uncovered cells stay on screen.
                registry.clear_grid(&key);
                let tty = host.display_tty.0.clone().unwrap_or_default();
                if let Some(client) = control {
                    // Over the open -CC connection — no fresh ssh handshake.
                    client.switch_client_on(&tty, &sel.session);
                } else {
                    let plan = host.mux.switch_plan(&sel.session);
                    let lowered = {
                        let builder = |session: &str| host.mux.switch_client_argv(&tty, session);
                        host.transport.lower_switch(&plan, &builder)
                    };
                    if let Some(lowered) = lowered {
                        run_lowered(lowered);
                    }
                }
                host.display.set_shows(&key, &sel.session);
            }
        }
        SelectOutcome::PerSessionReattach => {
            registry.remove(&key);
            host.display.clear(&key);
            let mux_argv = host.mux.attach_plan(&sel.session, None);
            let (cmd, args) = host.transport.exec_argv(true, &mux_argv);
            let mut argv = vec![cmd];
            argv.extend(args);
            request_attach(
                registry,
                worker,
                &mut host.display,
                attach_seq,
                &key,
                argv,
                cols,
                rows,
            );
            host.display.set_shows(&key, &sel.session);
        }
    }

    // Window-row selection → move the session's active window. The fresh shared
    // attach above already folded the window in; otherwise lower a select-window plan.
    if let Some(win) = sel.window {
        let folded_into_attach =
            !already && matches!(host.mux.select(), SelectOutcome::SharedSwitch);
        if !folded_into_attach {
            let target = crate::mux::window_target(&sel.session, win);
            if let Some(client) = control {
                // Over the open -CC connection — no fresh ssh handshake.
                client.select_window_on(&target);
            } else {
                let mux_argv = host.mux.select_window_plan(&target);
                let (cmd, args) = host.transport.exec_argv(false, &mux_argv);
                let mut argv = vec![cmd];
                argv.extend(args);
                run_lowered(crate::model::LoweredSwitch::Local(argv));
            }
        }
    }
    true
}

/// Spawns the lowered switch command off the event loop. Local variants run as a
/// plain subprocess; RawSsh variants run the full ssh argv non-interactively.
fn run_lowered(lowered: crate::model::LoweredSwitch) {
    use crate::model::LoweredSwitch;
    use crate::source::Runner;
    let argv = match lowered {
        LoweredSwitch::Local(v) | LoweredSwitch::RawSsh(v) => v,
    };
    if argv.is_empty() {
        return;
    }
    let (name, args) = (argv[0].clone(), argv[1..].to_vec());
    tokio::spawn(async move {
        let _ = crate::source::ExecRunner.run(&name, &args).await;
    });
}

/// Keeps a source's display terminal in sync with its sessions. Shared muxes keep
/// ONE PTY per host: warm it on the first session (later selections switch it), and
/// reap it when the host has no sessions left. Reattach muxes are selected on demand.
/// Called whenever a source's inventory updates (a remote `%`-event refresh or a
/// local poll), so a new session is reachable and a killed one is torn down (#5).
#[allow(clippy::too_many_arguments)]
fn sync_source_terminals(
    registry: &mut AttachRegistry,
    env: &Env,
    hosts: &mut crate::model::Hosts,
    source: &str,
    sessions: &[crate::session::Session],
    worker: &DisplayWorker,
    attach_seq: &mut u64,
    cols: u16,
    rows: u16,
    tree_width: u16,
) {
    let Some(src) = env.by_alias.get(source) else {
        return;
    };
    let (cols, rows) = terminal_view_size(cols, rows, tree_width);
    let Some(host) = hosts.get_mut(source) else {
        return;
    };
    let remote = host.transport.is_remote();
    match host.mux.select() {
        SelectOutcome::SharedSwitch => {
            // One PTY per host. Warm it on the first session if not yet attached; reap it
            // (and forget its session) when the host has no sessions.
            match sessions.first() {
                Some(first)
                    if !registry.contains(source)
                        && !host.display.in_flight.contains_key(source) =>
                {
                    request_attach(
                        registry,
                        worker,
                        &mut host.display,
                        attach_seq,
                        source,
                        shared_display_attach_argv(remote, src, &first.name, None),
                        cols,
                        rows,
                    );
                    host.display.set_shows(source, &first.name);
                }
                None => {
                    registry.remove(source);
                    host.display.clear(source);
                }
                _ => {}
            }
        }
        SelectOutcome::PerSessionReattach => {
            if sessions.is_empty() {
                registry.remove(source);
                host.display.clear(source);
            }
        }
    }
}

/// True when a worker `Ready`/`Failed` reply is still the latest in-flight request for its
/// key. A stale reply (the key was re-requested after a reap, so a newer seq is in flight, or
/// the key is no longer in flight at all) must not register or clear state.
fn attach_reply_is_current(
    in_flight: &std::collections::HashMap<String, u64>,
    key: &str,
    seq: u64,
) -> bool {
    in_flight.get(key) == Some(&seq)
}

/// Connects the host the cursor is on (if not already + detected), so its metadata
/// channel streams that host's tree in. The manager picks the channel (control client
/// vs poll task) from the host's `event_source`; an undetected host is skipped until a
/// detection probe resolves its backend.
fn ensure_current_host(
    mgr: &mut HostManager,
    env: &Env,
    hosts: &crate::model::Hosts,
    switcher: &crate::ui::switcher::Switcher,
    cols: u16,
    rows: u16,
    tree_width: u16,
) {
    let (cols, rows) = terminal_view_size(cols, rows, tree_width);
    if let Some(id) = switcher.current_host() {
        if let (Some(host), Some(src)) = (hosts.get(&id), env.by_alias.get(&id)) {
            if host.detected {
                let _ = mgr.ensure(&id, host, src, cols, rows);
            }
        }
    }
}

fn transport_for_source(src: &crate::source::Source) -> crate::model::Transport {
    if src.remote {
        crate::model::Transport::Ssh {
            alias: src.alias.clone(),
            control_path: src.control_path.clone(),
            os: src.os.clone(),
        }
    } else {
        crate::model::Transport::Local {
            socket: src.socket.clone(),
        }
    }
}

fn spawn_host_detection(
    src: crate::source::Source,
    tx: tokio::sync::mpsc::UnboundedSender<HostEvent>,
) {
    let source = src.alias.clone();
    let transport = transport_for_source(&src);
    let bin = src.binary.clone();
    tokio::spawn(async move {
        let mut host = crate::model::Host::new(transport, crate::backend::for_binary(&bin));
        host.detect_and_correct(&crate::source::ExecRunner).await;
        let detected = host.detected.then_some(host.mux);
        let _ = tx.send(HostEvent::Scanned { source, detected });
    });
}

/// Dispatches a DETECTED host onto its metadata channel via the manager, which picks
/// the channel (control client vs poll task) from the host's `event_source`. Idempotent
/// — a no-op when the channel is already live.
fn dispatch_detected_host(
    mgr: &mut HostManager,
    env: &Env,
    hosts: &crate::model::Hosts,
    source: &str,
    cols: u16,
    rows: u16,
) {
    let Some(host) = hosts.get(source) else {
        return;
    };
    if let Some(src) = env.by_alias.get(source) {
        let _ = mgr.ensure(source, host, src, cols, rows);
    }
}

fn scan_or_dispatch_host(
    mgr: &mut HostManager,
    env: &Env,
    hosts: &crate::model::Hosts,
    detecting: &mut HashSet<String>,
    source: &str,
    cols: u16,
    rows: u16,
) {
    let Some(host) = hosts.get(source) else {
        return;
    };
    if !host.detected {
        if detecting.insert(source.to_string()) {
            if let Some(src) = env.by_alias.get(source) {
                spawn_host_detection(src.clone(), mgr.events());
            }
        }
        return;
    }
    dispatch_detected_host(mgr, env, hosts, source, cols, rows);
}

fn apply_scan_result(
    hosts: &mut crate::model::Hosts,
    source: &str,
    detected: Option<Box<dyn crate::backend::Backend>>,
) {
    let Some(host) = hosts.get_mut(source) else {
        return;
    };
    if let Some(backend) = detected {
        if backend.kind() != host.mux.kind() {
            host.mux = backend;
        }
        host.detected = true;
    }
}

/// Consumes a pending re-scan kick (set by `r` or a menu "reconnect"): re-enumerates
/// every detected source via the manager — a control host re-lists sessions, a poll
/// host respawns its task for an immediate re-enumeration — and (re)detects an
/// undetected one. A no-op when no kick is pending. Shared by the key and menu paths.
fn kick_rescan(
    switcher: &mut crate::ui::switcher::Switcher,
    env: &Env,
    hosts: &crate::model::Hosts,
    detecting: &mut HashSet<String>,
    mgr: &mut HostManager,
    cols: u16,
    rows: u16,
) {
    if !switcher.take_rescan_kick() {
        return;
    }
    for src in &env.srcs {
        if let Some(host) = hosts.get(&src.alias) {
            if host.detected {
                mgr.rescan(&src.alias, host, src, cols, rows);
                continue;
            }
        }
        scan_or_dispatch_host(mgr, env, hosts, detecting, &src.alias, cols, rows);
    }
}

/// Starts each host's first scan at startup, so each host's tree streams in without
/// waiting for a cursor move. Control hosts connect a `-CC` client; poll hosts start
/// their self-looping enumeration task — both owned by the manager. PTYs are attached
/// as each source's sessions arrive (see [`sync_source_terminals`]).
fn connect_all_sources(
    mgr: &mut HostManager,
    env: &Env,
    hosts: &crate::model::Hosts,
    detecting: &mut HashSet<String>,
    cols: u16,
    rows: u16,
    tree_width: u16,
) {
    let (cols, rows) = terminal_view_size(cols, rows, tree_width);
    for src in &env.srcs {
        scan_or_dispatch_host(mgr, env, hosts, detecting, &src.alias, cols, rows);
    }
}

/// The single key that moves focus from the tree into the terminal pane.
/// (Arrows navigate the tree; the prefix-Esc path returns focus — see TermInput.)
fn is_focus_in(code: KeyCode) -> bool {
    matches!(code, KeyCode::Enter)
}

/// Whether a wheel event should drive the TREE (scroll, or Ctrl-wheel level change).
/// Only when the tree is focused AND the pointer is over the tree: mouse input acts on
/// the pane under the cursor, and only when that pane is focused — the same rule clicks
/// and motion already follow. A wheel over the mux pane while the tree is focused is not
/// a tree scroll.
fn wheel_targets_tree(tree_focused: bool, over_mux: bool) -> bool {
    tree_focused && !over_mux
}

/// Whether a right-button press may open the tree context menu. Tree-focus only: the
/// menu operates on a tree row, so it is a tree-pane action, not a pane-independent
/// global — it never opens (nor steals focus) while the mux pane is focused. Position-
/// gated to the tree column; a right-click over the mux pane forwards to the child.
fn tree_menu_may_open(is_right_press: bool, tree_focused: bool, over_mux: bool) -> bool {
    is_right_press && tree_focused && !over_mux
}

/// What a mouse event resolves to once the modal/gesture gates (menu, divider drag,
/// idle-divider-hover, menu-open) have declined it — the focus×position routing core.
#[derive(Debug, PartialEq, Eq)]
enum ChainAction {
    /// Scroll the tree by one row (wheel, tree focus, over tree). `down` = scroll down.
    ScrollTree(bool),
    /// Change the tree level (Ctrl+wheel, tree focus, over tree). `down` = descend.
    LevelChange(bool),
    /// Toggle focus to the mux pane (left-click the mux while the tree is focused).
    FocusMux,
    /// Select the clicked tree row (left-click a tree row while the tree is focused).
    SelectRow,
    /// Toggle focus to the tree (left-click the tree while the mux is focused).
    FocusTree,
    /// Forward the event to the focused mux child (mux focus, over the mux pane).
    ForwardToMux,
    /// Nothing — the event is dropped.
    Nothing,
}

/// Pure focus×position routing for a mouse event that fell through every gate. The one
/// rule: input acts on the pane under the cursor, and only when that pane is focused.
/// A wheel over the mux while the tree is focused, or over the tree while the mux is
/// focused, resolves to Nothing — it never crosses to the unfocused pane.
fn resolve_mouse_chain(
    is_wheel: bool,
    ctrl: bool,
    down: bool,
    is_left_press: bool,
    tree_focused: bool,
    over_mux: bool,
) -> ChainAction {
    if is_wheel && wheel_targets_tree(tree_focused, over_mux) {
        return if ctrl {
            ChainAction::LevelChange(down)
        } else {
            ChainAction::ScrollTree(down)
        };
    }
    if is_left_press && tree_focused && over_mux {
        return ChainAction::FocusMux;
    }
    if is_left_press && tree_focused && !over_mux {
        return ChainAction::SelectRow;
    }
    if is_left_press && !tree_focused && !over_mux {
        return ChainAction::FocusTree;
    }
    if !tree_focused && over_mux {
        return ChainAction::ForwardToMux;
    }
    ChainAction::Nothing
}

/// Pure resolution of ONE TREE-focus key into an [`Action`] (or none, when the key
/// only arms the prefix or is an unrecognized armed command). Touches no cockpit or
/// switcher state, so it is unit-testable in isolation (mirrors how `TermInput::feed`
/// resolves the mux-focus path). `is_inputting` suppresses prefix arming and the Enter
/// focus-switch so the input row receives those keys verbatim. Resolved per key — not
/// per read — because `is_inputting` can flip mid-read (a key that opens the input row
/// changes how the next key in the same read is treated), so the caller re-queries it
/// and applies each action before resolving the next key.
fn resolve_tree_key(
    key: ratatui::crossterm::event::KeyEvent,
    armed: &mut bool,
    prefix: u8,
    is_inputting: bool,
) -> Option<Action> {
    if *armed {
        *armed = false;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        return match key.code {
            KeyCode::Char('q') => Some(Action::Quit),
            KeyCode::Left if ctrl => Some(Action::Width(-1)),
            KeyCode::Right if ctrl => Some(Action::Width(1)),
            KeyCode::Char('h') => Some(Action::Width(-1)),
            KeyCode::Char('l') => Some(Action::Width(1)),
            KeyCode::Char('t') => Some(Action::ToggleAutoHide),
            KeyCode::Char('?') => Some(Action::ShowHelp),
            // prefix Tab cycles focus to the mux (toggle, mirroring the mux side's
            // prefix Tab → tree); prefix → also focuses the mux. The byte decoder yields
            // Char('\t') for Tab, never KeyCode::Tab, so match both. (prefix ←/Esc focus
            // the tree, where we already are — a no-op that resolves to nothing.)
            KeyCode::Right | KeyCode::Tab | KeyCode::Char('\t') => Some(Action::FocusMux),
            _ => None,
        };
    }
    if !is_inputting && key.code == KeyCode::Char(prefix as char) {
        *armed = true;
        return None;
    }
    // Enter focuses the terminal pane. ←/→ navigate the tree inside `handle_key`.
    if !is_inputting && is_focus_in(key.code) {
        return Some(Action::FocusMux);
    }
    Some(Action::TreeKey(key))
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
    state: &mut crate::state::State,
    mgr: &mut HostManager,
    env: &Env,
    hosts: &crate::model::Hosts,
    detecting: &mut HashSet<String>,
    ops: &Arc<dyn crate::ui::switcher::Ops>,
    op_tx: &tokio::sync::mpsc::UnboundedSender<crate::ui::switcher::OpResult>,
    tree_width_natural: &mut u16,
    auto_hide_tree: &mut bool,
    width_changed: &mut bool,
    cols: u16,
    rows: u16,
    tree_width: u16,
) -> (bool, bool, i32, bool) {
    let mut focus_terminal = false;
    let mut quit = false;
    let mut width_delta = 0i32;
    let mut toggle_auto_hide = false;
    let mut key_cmds: Vec<crate::model::Command> = Vec::new();
    for key in tree_decoder.feed(bytes) {
        // Re-query per key: opening a modal popup (via a TreeKey applied below) flips
        // this, which changes how the next key in this same read resolves. Gating on
        // ANY modal popup (not just the inline input) makes a modal OWN its keys — a
        // kill-confirm swallows prefix/Enter, so `prefix q` can't quit and Enter can't
        // focus the mux while a confirm is on screen; only y/n/Esc act on it.
        let is_inputting = state.is_modal_popup_open();
        match resolve_tree_key(key, tree_armed, prefix, is_inputting) {
            // A committed input/kill confirm folds through State::apply, which returns
            // its Commands; collect them and dispatch the whole batch below.
            Some(Action::TreeKey(k)) => key_cmds.extend(switcher.handle_key(k, state)),
            Some(Action::FocusMux) => focus_terminal = true,
            Some(Action::Quit) => quit = true,
            Some(Action::Width(d)) => width_delta = d,
            Some(Action::ToggleAutoHide) => toggle_auto_hide = true,
            Some(Action::ShowHelp) => switcher.toggle_help(state),
            // resolve_tree_key never emits the mux-only variants; None = armed/consumed.
            Some(Action::Forward(_)) | Some(Action::FocusTree(_)) | None => {}
        }
    }
    // Route the FULL command batch through the single dispatcher (not just RunOp): a
    // switcher key emits only RunOp today, but dispatch_commands handles every variant
    // so a future non-RunOp command is acted on, never silently dropped. quit/
    // width-change it reports merge into this function's outputs.
    let (cmd_quit, cmd_width_changed) = dispatch_commands(
        key_cmds,
        switcher,
        state,
        tree_width_natural,
        auto_hide_tree,
        &env.xmux_dir,
        ops,
        op_tx,
    );
    quit |= cmd_quit;
    if cmd_width_changed {
        *width_changed = true;
    }
    ensure_current_host(mgr, env, hosts, switcher, cols, rows, tree_width);
    kick_rescan(switcher, env, hosts, detecting, mgr, cols, rows);
    (focus_terminal, quit, width_delta, toggle_auto_hide)
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
/// Returns `true` when the caller should rearm `attach_deadline` + mark `dirty`
/// (the `ClientDetached` reap path).
#[allow(clippy::too_many_arguments)]
fn handle_host_event(
    ev: HostEvent,
    mgr: &mut HostManager,
    hosts: &mut crate::model::Hosts,
    registry: &mut AttachRegistry,
    switcher: &mut crate::ui::switcher::Switcher,
    state: &mut crate::state::State,
    env: &Env,
    connected: &mut HashSet<String>,
    panes_requested: &mut HashSet<String>,
    detecting: &mut HashSet<String>,
    worker: &DisplayWorker,
    attach_seq: &mut u64,
    cols: u16,
    rows: u16,
    tree_width: u16,
) -> bool {
    // State owns the event-driven mutations: apply_event folds the self-contained
    // arms (Focus marker, Panes subtree, Sessions enumeration, Exited unreachable
    // mark) into State and returns the backend follow-ups it cannot perform itself.
    // This loop is the sole executor of those effects — it holds the host clients,
    // the registry, and the display worker the state layer must not reach.
    let mut rearm = false;
    for effect in state.apply_event(ev, switcher, connected) {
        if run_event_effect(
            effect,
            mgr,
            hosts,
            registry,
            switcher,
            state,
            env,
            panes_requested,
            detecting,
            worker,
            attach_seq,
            cols,
            rows,
            tree_width,
        ) {
            rearm = true;
        }
    }
    rearm
}

/// Carries out one [`EventEffect`](crate::model::EventEffect) `State::apply_event`
/// returned — the backend I/O the state layer cannot perform (a host client's
/// inventory lock, a control-mode probe, the attach registry, the detection
/// dispatch). Returns `true` only for the matched-client display-attach reap, which
/// asks the caller to rearm `attach_deadline` + mark `dirty` (the recover-from-detach
/// path).
#[allow(clippy::too_many_arguments)]
fn run_event_effect(
    effect: crate::model::EventEffect,
    mgr: &mut HostManager,
    hosts: &mut crate::model::Hosts,
    registry: &mut AttachRegistry,
    switcher: &mut crate::ui::switcher::Switcher,
    state: &mut crate::state::State,
    env: &Env,
    panes_requested: &mut HashSet<String>,
    detecting: &mut HashSet<String>,
    worker: &DisplayWorker,
    attach_seq: &mut u64,
    cols: u16,
    rows: u16,
    tree_width: u16,
) -> bool {
    use crate::model::EventEffect;
    match effect {
        EventEffect::ApplyInventory { host } => {
            // The metadata reply's inventory lives behind the host client's lock; apply
            // it to the tree, request each session's panes, and sync the display PTY(s).
            if let Some(client) = mgr.get(&host) {
                let sessions = {
                    let inv = client.inventory.lock().unwrap();
                    switcher.apply_source_result(host.clone(), inv.sessions.clone(), None, state);
                    for (addr, windows) in inv.panes.iter() {
                        switcher.apply_panes(addr.clone(), windows.clone(), state);
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
                sync_source_terminals(
                    registry, env, hosts, &host, &sessions, worker, attach_seq, cols, rows,
                    tree_width,
                );
            }
        }
        EventEffect::Refetch { host } => {
            // The server's session/window structure changed (a `%`-notification).
            // Refetch so the tree, panes, and PTY set resync (#5 sidebar sync).
            refetch_host(mgr, panes_requested, &host);
        }
        EventEffect::ProbeActiveWindow { host } => {
            // A session's ACTIVE WINDOW switched — the structure did NOT change, so do
            // NOT refetch the whole inventory: a full list-sessions + per-session
            // list-panes per change storms the single-threaded loop and freezes the UI
            // during rapid window navigation (each tree step issues select-window,
            // which echoes back as this notification). Instead probe ONLY the displayed
            // session's new active window; the reply (Focus) updates the marker and
            // follows the cursor without any refetch.
            let displayed = hosts
                .get(&host)
                .and_then(|h| h.display.shows(&host).map(str::to_string));
            if let (Some(client), Some(displayed)) = (mgr.get(&host), displayed) {
                client.probe_active_window(&displayed);
            }
        }
        EventEffect::ReapHost { host } => {
            mgr.reap(&host);
        }
        EventEffect::ReapDisplayAttach { host, client } => {
            // Reap our display attach ONLY when the detaching client is OUR display client
            // (matched against the in-memory Host.display_tty). An unrelated client's detach
            // can never match, so it is structurally inert — no blanket reap.
            let Some(h) = hosts.get(&host) else {
                return false;
            };
            if !h.matches_display_tty(&client) {
                return false;
            }
            let key = h.display_key(&host); // Shared ⇒ key == host id
            registry.remove(&key);
            if let Some(h) = hosts.get_mut(&host) {
                h.display.clear(&key); // forget the shown session + any in-flight spawn
                h.display_tty = crate::model::DisplayTty(None); // the dead client's tty is gone
            }
            return true; // rearm recovery
        }
        EventEffect::DispatchScanned { source, detected } => {
            // A detection probe resolved: (re)identify the backend, then dispatch the
            // now-detected host onto its metadata channel (control client or poll task).
            detecting.remove(&source);
            apply_scan_result(hosts, &source, detected);
            let (vc, vr) = terminal_view_size(cols, rows, tree_width);
            dispatch_detected_host(mgr, env, hosts, &source, vc, vr);
        }
        EventEffect::SyncPollSessions { source, sessions } => {
            // A poll host's SUCCESSFUL enumeration (the tree group is already applied).
            // The `poll enum` debug line is logged UNCONDITIONALLY at the producer
            // (`run_poll`), where `err` is in hand — `apply_event` drops the error path
            // before reaching here, so logging here would only ever see successes.
            // PerSession psmux: a session whose registry .port disappeared is dead even
            // if its PTY has not EOF'd. Drop the stale attach so it cannot show a dead grid.
            if let Some(h) = hosts.get(&source) {
                for s in &sessions {
                    if !h.psmux_session_live(&s.name) {
                        let key = match h.mux.select() {
                            SelectOutcome::SharedSwitch | SelectOutcome::PerSessionReattach => {
                                source.clone()
                            }
                        };
                        registry.remove(&key);
                    }
                }
            }
            sync_source_terminals(
                registry, env, hosts, &source, &sessions, worker, attach_seq, cols, rows,
                tree_width,
            );
        }
    }
    false
}

/// Records a pump-self-reported display tty on the host that owns the attach id.
/// The attach key is `display_key`; for a Shared host that IS the host id. Provably
/// xmux's own client (the marker is emitted only by our attach shell).
fn record_display_tty(
    hosts: &mut crate::model::Hosts,
    registry: &AttachRegistry,
    id: u64,
    tty: String,
) {
    if let Some(addr) = registry.address_of_id(id) {
        let host_id = addr.split('/').next().unwrap_or(&addr).to_string();
        if let Some(h) = hosts.get_mut(&host_id) {
            h.display_tty = crate::model::DisplayTty(Some(tty));
        }
    }
}

/// The remote attach argv with the display-tty marker prepended to its remote
/// command, so the attach shell self-reports its own tty (xmux's display client)
/// over the pump before exec. Every remote display-attach spawn routes through
/// this; a spawn site that built the argv directly would leave the host's
/// display_tty empty and the %client-detached reap unable to match.
fn marked_remote_attach_argv(
    src: &crate::source::Source,
    session: &str,
    window: Option<i64>,
) -> Vec<String> {
    let mut argv = src.attach_command(session, window);
    if let Some(last) = argv.last_mut() {
        *last = format!(
            "{}{}",
            crate::model::death::display_tty_marker_prefix(),
            last
        );
    }
    argv
}

/// The display-attach argv for a SHARED host's one kept PTY. A REMOTE attach carries
/// the marker so the attach shell self-reports its tty (which a later `switch-client
/// -c <tty>` and the `%client-detached` reap both need); a LOCAL attach has no shell
/// to run the marker snippet, so it stays bare — prepending the snippet would corrupt
/// the local argv's session-name argument. Every shared warm routes through this so
/// the marker decision matches `select_attach` (marker iff `transport.is_remote()`)
/// and cannot drift between spawn sites.
fn shared_display_attach_argv(
    remote: bool,
    src: &crate::source::Source,
    session: &str,
    window: Option<i64>,
) -> Vec<String> {
    if remote {
        marked_remote_attach_argv(src, session, window)
    } else {
        src.attach_command(session, window)
    }
}

/// Clears the display tty of the host owning the EOF'd attach `id`, so a dropped
/// display client cannot leave a stale tty that a later %client-detached matches.
/// Must run BEFORE the reap removes the registry entry (address_of_id needs it).
fn clear_display_tty_for_attach(
    hosts: &mut crate::model::Hosts,
    registry: &AttachRegistry,
    id: u64,
) {
    if let Some(addr) = registry.address_of_id(id) {
        let host_id = addr.split('/').next().unwrap_or(&addr).to_string();
        if let Some(h) = hosts.get_mut(&host_id) {
            h.display_tty = crate::model::DisplayTty(None);
        }
    }
}

/// Handles a remote host's control client dying. A host that had connected keeps its
/// last-known tree. A never-connected host that died with "no sessions" / "no server
/// running" is REACHABLE but has no mux server — it renders "(empty)" (and a session
/// can be created there), NOT "⚠". Any other never-connected death is a real
/// transport failure and renders "⚠". Returns `true` only when it marked the host
/// unreachable.
pub(crate) fn note_host_exited(
    switcher: &mut crate::ui::switcher::Switcher,
    state: &mut crate::state::State,
    connected: &mut HashSet<String>,
    host: &str,
    reason: Option<String>,
) -> bool {
    // Clear the connected mark so this host is no longer pinned to "keep last-known
    // tree". A transient drop of a once-connected host keeps its tree (no unreachable
    // flash) on THIS exit; but a later reconnect that fails (no sessions / unreachable)
    // must then resolve its real state — otherwise a refresh that set it scanning would
    // spin on "loading…" forever, since a sticky `connected` made every exit a no-op.
    if connected.remove(host) {
        return false;
    }
    if reason
        .as_deref()
        .is_some_and(crate::source::reason_is_no_sessions)
    {
        switcher.apply_source_result(host.to_string(), Vec::new(), None, state);
        return false;
    }
    let msg = reason.unwrap_or_else(|| "connection closed".into());
    switcher.apply_source_result(host.to_string(), Vec::new(), Some(msg), state);
    true
}

/// The per-event mouse-gesture/input state the `stdin_rx` arm carries across reads,
/// bundled so the extracted handlers stay behavior-preserving (the gesture latches
/// must persist across reads). Field-for-field the loop locals `run_cockpit` held.
#[derive(Default)]
struct MouseState {
    /// True while the left button is dragging the tree/mux divider rule to resize.
    dragging_divider: bool,
    /// True while the mouse hovers the divider rule (no button) — the drag-resize cue.
    hovered_divider: bool,
    /// The resize-repeat window: a bare Ctrl+←/→ keeps resizing until it lapses.
    repeat_until: Option<std::time::Instant>,
    /// True while a prefix has been pressed in tree focus, awaiting the command key.
    tree_armed: bool,
}

/// The outcome of one stdin read: what the loop must act on after the handler runs.
/// Replaces the inline arm's direct mutation of `dirty`/`quit`, so the handler is a
/// function of (bytes, state) → outcome, unit-testable without the loop. `focus_*` and
/// `tree_replay` carry the resolved focus path (applied inside the handler) for the
/// per-handler round-trip test + observability.
#[derive(Default)]
struct StdinOutcome {
    quit: bool,
    focus_terminal: bool,
    focus_tree: bool,
    dirty: bool,
    tree_replay: Vec<u8>,
    /// True if any `apply_width_delta` call changed the natural tree width; the loop
    /// uses this to schedule the debounced persist (instead of writing per tick).
    width_changed: bool,
}

/// Applies ONE parsed SGR mouse event to the gesture state + tree/registry — the body
/// of the inline `while i < bytes.len()` mouse branch, lifted verbatim. Runs the modal/
/// gesture gates (menu, divider drag, popup drag, modal swallow, divider grab, idle
/// hover, menu open) in the SAME order, then the focus×position routing. Mutates `st`
/// (the gesture latches), `state.focus` (mid-loop focus toggles — routing re-reads focus
/// per event, so deferring would change behavior), and the byte-loop accumulators
/// (`non_mouse`, `mouse_focus_toggle`, `wheel_scrolled`). Returns whether a redraw is
/// needed for this event.
#[allow(clippy::too_many_arguments)]
fn handle_mouse_event(
    ev: &crate::proxy::mouse::MouseEvent,
    st: &mut MouseState,
    switcher: &mut crate::ui::switcher::Switcher,
    state: &mut crate::state::State,
    registry: &mut AttachRegistry,
    mgr: &mut HostManager,
    env: &Env,
    hosts: &crate::model::Hosts,
    detecting: &mut HashSet<String>,
    selection: &Selection,
    non_mouse: &mut Vec<u8>,
    mouse_focus_toggle: &mut bool,
    wheel_scrolled: &mut bool,
    term_area: ratatui::layout::Rect,
    tree_width_natural: &mut u16,
    cols: u16,
    body_rows: u16,
    tree_width: u16,
) -> bool {
    let mut dirty = false;
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
    if state.menu_active() {
        if !ev.pressed {
            match switcher.menu_release(state) {
                crate::ui::switcher::MenuOutcome::FocusTerminal => {
                    // Connect the target's host (mirrors the left-click
                    // select path) so its control client streams, then
                    // focus the mux on the now-selected session.
                    ensure_current_host(mgr, env, hosts, switcher, cols, body_rows, tree_width);
                    // Focus state is `Menu{prior}` here; set the restore pane to the mux
                    // so closing the menu (next loop-top sync_modal(None)) lands on it.
                    state
                        .focus
                        .set_pane_focus(crate::proxy::app::PaneFocus::Terminal);
                }
                crate::ui::switcher::MenuOutcome::Handled => {
                    // A menu item only OPENS the next modal (input / kill confirm) — the
                    // actual mux op is committed later from that modal (Enter / y), which
                    // returns its RunOp through handle_key. Here just consume any re-scan
                    // (reconnect) kick and ensure the target's host is connected.
                    kick_rescan(switcher, env, hosts, detecting, mgr, cols, body_rows);
                    ensure_current_host(mgr, env, hosts, switcher, cols, body_rows, tree_width);
                }
                crate::ui::switcher::MenuOutcome::None => {}
            }
            dirty = true;
        } else if !is_wheel {
            switcher.menu_hover(col0, ev.row.saturating_sub(1), state);
            dirty = true;
        }
        return dirty;
    }
    if st.dragging_divider {
        if !ev.pressed {
            // Button up ends the drag; persist the final width once
            // (motion resizes live but does not write per cell).
            st.dragging_divider = false;
            crate::prefs::save_tree_width(&env.xmux_dir, *tree_width_natural);
        } else if !is_wheel {
            let target = divider_drag_width(ev.col);
            if target != *tree_width_natural {
                *tree_width_natural = target;
                dirty = true;
            }
        }
        return dirty;
    }
    let is_left_press = is_press && (ev.cb & 0x03) == 0;
    // A modal popup (help/input/confirm) moves when its border is
    // dragged. Once grabbed it owns every mouse event until release,
    // like the divider drag / menu hold above.
    if switcher.popup_drag_active() {
        if !ev.pressed {
            switcher.end_popup_drag();
        } else if !is_wheel {
            switcher.drag_popup(col0, ev.row.saturating_sub(1));
        }
        dirty = true;
        return dirty;
    }
    if is_left_press && switcher.begin_popup_drag(col0, ev.row.saturating_sub(1), state) {
        dirty = true;
        return dirty;
    }
    // A modal popup is mouse-modal: while one is open, every mouse
    // event that is not its border-drag (handled above) is swallowed,
    // so clicks, wheels, divider grabs, and hovers never reach the
    // tree/mux/divider behind it.
    if state.is_modal_popup_open() {
        return dirty;
    }
    if is_left_press && tree_width > 0 && col0 == tree_width {
        st.dragging_divider = true; // grabbed the divider
        return dirty;
    }
    // Idle motion (motion bit set, no button held) — reported only
    // because any-motion tracking (1003h) is on. Over the divider it
    // lights the hover cue and is consumed (nothing under it to forward).
    // Elsewhere it falls through to the routing below, so a hover over the
    // mux pane IS forwarded to the child (the inner app gets hover); over
    // the tree it is harmlessly dropped.
    if ev.pressed && (ev.cb & 0x23) == 0x23 {
        let over_divider = tree_width > 0 && col0 == tree_width;
        if over_divider != st.hovered_divider {
            st.hovered_divider = over_divider;
            dirty = true;
        }
        if over_divider {
            return dirty;
        }
    }
    // Right-button press over a selectable tree row opens its context
    // menu (press-hold-release). Tree-focus only: the menu acts on a
    // tree row, so it is tree-pane input, not a global — a right-click
    // while the mux is focused (or over the mux pane) does not open it
    // and does not move focus. The menu's keyboard actions (rename input,
    // kill confirm) thus always run in tree focus, so a confirmed kill
    // can't quit the cockpit out from under the mux.
    let is_right_press = is_press && (ev.cb & 0x03) == 2;
    if tree_menu_may_open(
        is_right_press,
        state.focus.is_tree_focused(),
        in_mux.is_some(),
    ) && switcher.menu_open(col0, ev.row.saturating_sub(1), state)
    {
        dirty = true;
        return dirty;
    }
    let down = (ev.cb & 0x01) != 0;
    let ctrl = (ev.cb & 0x10) != 0;
    match resolve_mouse_chain(
        is_wheel,
        ctrl,
        down,
        is_left_press,
        state.focus.is_tree_focused(),
        in_mux.is_some(),
    ) {
        ChainAction::ScrollTree(down) => {
            // Plain wheel → scroll the cursor LINEARLY through every row
            // (move_selection), like any list. NOT sibling-cycle: arrows do
            // that (move_sibling), but it wraps within a level, so a 2-sibling
            // level just bounces — the "two notches per move" report.
            switcher.mouse_scroll(down, state);
            *wheel_scrolled = true;
            dirty = true;
        }
        ChainAction::LevelChange(down) => {
            // Ctrl+wheel → change level (↑ ascend / ↓ descend); inject the
            // arrow so the tree path (decode → handle_key → ensure) drives it.
            non_mouse.extend_from_slice(if down { b"\x1b[C" } else { b"\x1b[D" });
        }
        // The unfocused pane was clicked → switch focus to it (no content
        // delivered); toggle flips Focus::Tree⇄Focus::Terminal either direction.
        ChainAction::FocusMux | ChainAction::FocusTree => {
            state.focus.toggle();
            *mouse_focus_toggle = true;
        }
        ChainAction::SelectRow => {
            // Left-click a tree row → move the cursor to it (select). The
            // loop top commits the new selection (attach); ensure the
            // clicked row's host connects so its subtree streams in.
            switcher.mouse_select(col0, ev.row.saturating_sub(1), state);
            ensure_current_host(mgr, env, hosts, switcher, cols, body_rows, tree_width);
            dirty = true;
        }
        ChainAction::ForwardToMux => {
            if let Some((gc, gr)) = in_mux {
                registry.input(
                    &display_key(hosts, selection),
                    crate::proxy::mouse::encode_sgr_mouse(ev, gc, gr),
                );
            }
        }
        ChainAction::Nothing => {}
    }
    dirty
}

/// The whole `stdin_rx` arm body, lifted. Scans the read for SGR mouse sequences
/// (routed via [`handle_mouse_event`]) vs a non-mouse byte stream, runs the lost-release
/// watchdogs, the resize-repeat window, and the help-modal / tree-focus / mux-focus
/// routing — in the SAME order as the inline arm. The final focus toggles (+ replay)
/// run inside on `state.focus`, so the loop only acts on `dirty`/`quit`. No behavior change.
#[allow(clippy::too_many_arguments)]
fn handle_stdin_bytes(
    bytes: &[u8],
    mouse: &mut MouseState,
    switcher: &mut crate::ui::switcher::Switcher,
    state: &mut crate::state::State,
    registry: &mut AttachRegistry,
    mgr: &mut HostManager,
    env: &Env,
    hosts: &mut crate::model::Hosts,
    detecting: &mut HashSet<String>,
    selection: &Selection,
    term_input: &mut crate::proxy::input::TermInput,
    tree_decoder: &mut crate::proxy::decode::KeyDecoder,
    ops: &Arc<dyn crate::ui::switcher::Ops>,
    op_tx: &tokio::sync::mpsc::UnboundedSender<crate::ui::switcher::OpResult>,
    tree_width_natural: &mut u16,
    auto_hide_tree: &mut bool,
    prefix: u8,
    cols: u16,
    body_rows: u16,
    tree_width: u16,
) -> StdinOutcome {
    use std::time::Duration;
    let mut outcome = StdinOutcome::default();
    let StdinOutcome {
        quit,
        focus_terminal,
        focus_tree,
        dirty,
        tree_replay,
        width_changed,
    } = &mut outcome;
    // Scan for SGR mouse sequences BEFORE routing to Focus::Tree/Focus::Terminal branches.
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
                if handle_mouse_event(
                    &ev,
                    mouse,
                    switcher,
                    state,
                    registry,
                    mgr,
                    env,
                    hosts,
                    detecting,
                    selection,
                    &mut non_mouse,
                    &mut mouse_focus_toggle,
                    &mut wheel_scrolled,
                    term_area,
                    tree_width_natural,
                    cols,
                    body_rows,
                    tree_width,
                ) {
                    *dirty = true;
                }
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
    if mouse.dragging_divider && !non_mouse.is_empty() {
        mouse.dragging_divider = false;
        crate::prefs::save_tree_width(&env.xmux_dir, *tree_width_natural);
    }
    // Watchdog: a keystroke (or any non-mouse byte) during a held menu ends
    // the gesture without acting — mirrors the divider-drag watchdog, so a
    // missed button-up can't strand the menu and eat later input.
    if state.menu_active() && !non_mouse.is_empty() {
        switcher.menu_cancel(state);
        non_mouse.clear();
        *dirty = true;
    }
    // Watchdog: same recovery for a popup border-drag — a lost button-up
    // must not strand `popup_drag` and eat all later mouse input.
    if switcher.popup_drag_active() && !non_mouse.is_empty() {
        switcher.end_popup_drag();
        *dirty = true;
    }
    if mouse_focus_toggle {
        *dirty = true;
    }
    if wheel_scrolled {
        // The plain-wheel scroll moved the cursor; connect the host it landed on
        // so its subtree streams in (mirrors handle_tree_bytes's ensure step).
        ensure_current_host(mgr, env, hosts, switcher, cols, body_rows, tree_width);
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
    if mouse
        .repeat_until
        .is_some_and(|d| std::time::Instant::now() < d)
        && !mouse.tree_armed
        && !term_input.is_armed()
        && !non_mouse.is_empty()
    {
        let mut n = 0;
        while let Some((d, len)) = leading_ctrl_arrow(&non_mouse[n..]) {
            if apply_width_delta(d, tree_width_natural) {
                *width_changed = true;
            }
            n += len;
        }
        if n > 0 {
            non_mouse.drain(0..n);
            *dirty = true;
            if non_mouse.is_empty() {
                mouse.repeat_until =
                    Some(std::time::Instant::now() + Duration::from_millis(RESIZE_REPEAT_MS));
                consumed_by_repeat = true;
            } else {
                mouse.repeat_until = None; // trailing non-arrow bytes end + route below
            }
        } else {
            mouse.repeat_until = None; // first key isn't a Ctrl-arrow → end the window
        }
    }
    if !consumed_by_repeat && !non_mouse.is_empty() && switcher.feed_help_key(&non_mouse, state) {
        // The help overlay is modal (tmux view-mode style): while open it
        // captures every key in EITHER focus — q/Esc closes it, the rest are
        // swallowed — so nothing leaks to the tree or the mux pane. Above the
        // tree/mux split so the behavior is identical regardless of focus.
        *dirty = true;
    } else if !consumed_by_repeat && (state.focus.is_tree_focused() || state.focus.is_modal()) {
        // Tree pane OR any modal: route to the switcher path. A modal popup (input /
        // kill-confirm) opened from EITHER pane owns its keys here; the resolver gating
        // in handle_tree_bytes swallows everything but the modal's own keys, so a modal
        // never emits FocusMux/quit and the focus toggles below never fire mid-modal.
        let (ft, q, wd, th) = handle_tree_bytes(
            &non_mouse,
            tree_decoder,
            &mut mouse.tree_armed,
            prefix,
            switcher,
            state,
            mgr,
            env,
            hosts,
            detecting,
            ops,
            op_tx,
            tree_width_natural,
            auto_hide_tree,
            width_changed,
            cols,
            body_rows,
            tree_width,
        );
        *focus_terminal = ft;
        *quit = q;
        if wd != 0 {
            // A prefix-driven resize: apply it and open the repeat window so the
            // next bare Ctrl+←/→ keeps resizing without re-pressing the prefix.
            if apply_width_delta(wd, tree_width_natural) {
                *width_changed = true;
            }
            mouse.repeat_until =
                Some(std::time::Instant::now() + Duration::from_millis(RESIZE_REPEAT_MS));
        }
        if th {
            toggle_auto_hide(auto_hide_tree, &env.xmux_dir);
            *dirty = true;
        }
    } else if !consumed_by_repeat {
        // TERMINAL focus: forward raw bytes to the selected session's PTY;
        // TermInput intercepts the prefix (→ tree / quit / help / resize / literal).
        for action in term_input.feed(&non_mouse) {
            match action {
                Action::Forward(f) => registry.input(&display_key(hosts, selection), f),
                Action::FocusTree(rest) => {
                    *focus_tree = true;
                    *tree_replay = rest;
                }
                Action::Quit => *quit = true,
                Action::ShowHelp => {
                    switcher.toggle_help(state);
                    *dirty = true;
                }
                Action::Width(d) => {
                    // Same resize + repeat-window as the tree path, so a resize
                    // started from the mux pane chains with bare Ctrl+←/→ too.
                    if apply_width_delta(d, tree_width_natural) {
                        *width_changed = true;
                    }
                    mouse.repeat_until =
                        Some(std::time::Instant::now() + Duration::from_millis(RESIZE_REPEAT_MS));
                }
                Action::ToggleAutoHide => {
                    toggle_auto_hide(auto_hide_tree, &env.xmux_dir);
                    *dirty = true;
                }
                // The mux-focus resolver (TermInput) never emits these — they
                // belong to the tree-focus path (resolve_tree).
                Action::FocusMux | Action::TreeKey(_) => {}
            }
        }
    }
    if *focus_terminal {
        state
            .focus
            .set_pane_focus(crate::proxy::app::PaneFocus::Terminal);
        // No term.clear(): both states draw the SAME split layout (only the
        // divider colour changes), so clearing would blank the screen and
        // force a full repaint for nothing.
    }
    if *focus_tree {
        state
            .focus
            .set_pane_focus(crate::proxy::app::PaneFocus::Tree);
        if !tree_replay.is_empty() {
            let (ft, q, wd, th) = handle_tree_bytes(
                tree_replay,
                tree_decoder,
                &mut mouse.tree_armed,
                prefix,
                switcher,
                state,
                mgr,
                env,
                hosts,
                detecting,
                ops,
                op_tx,
                tree_width_natural,
                auto_hide_tree,
                width_changed,
                cols,
                body_rows,
                tree_width,
            );
            if ft {
                state
                    .focus
                    .set_pane_focus(crate::proxy::app::PaneFocus::Terminal);
            }
            *quit = *quit || q;
            if wd != 0 {
                // A prefix-driven resize on the replayed bytes: apply + open the
                // repeat window, same as the direct tree-focus path above.
                if apply_width_delta(wd, tree_width_natural) {
                    *width_changed = true;
                }
                mouse.repeat_until =
                    Some(std::time::Instant::now() + Duration::from_millis(RESIZE_REPEAT_MS));
            }
            if th {
                toggle_auto_hide(auto_hide_tree, &env.xmux_dir);
                *dirty = true;
            }
        }
    }
    outcome
}

/// The `xmux` (no subcommand) entry: the persistent cockpit. Keeps one real attached
/// mux client per session alive and renders the selected one, with a control-mode
/// client per remote host for inventory/events/window-switch. It serves a picker
/// control socket so a headless driver can inject keys/text and dump the screen.
pub async fn run_cockpit(env: Arc<Env>) -> i32 {
    use crate::proxy::decode::KeyDecoder;
    use crate::proxy::input::TermInput;
    use crate::proxy::term::{parse_prefix, TermGuard};
    use crate::ui::run::{dump_overlay, serve_control, Cmd};
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

    // On a panic, log the full detail to a file (from any thread — partial writes
    // never corrupt the alternate screen), and on the MAIN thread (which owns the
    // terminal) ALSO restore the screen and print a one-line pointer to stderr, so the
    // user sees that xmux crashed and where the detail is rather than a bare exit code.
    // The restore is main-thread-only: worker threads (PTY pumps) catch+recover their
    // own panics (see Grid::feed); a stray worker panic must not tear the screen down
    // under a still-running cockpit. TermGuard's Drop also restores on the main-thread
    // unwind — idempotent with this.
    {
        let log = env.xmux_dir.join("panic.log");
        std::panic::set_hook(Box::new(move |info| {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log)
            {
                let _ = writeln!(f, "{info}");
            }
            if std::thread::current().name() == Some("main") {
                use ratatui::crossterm::{
                    event::DisableMouseCapture, execute, terminal::disable_raw_mode,
                    terminal::LeaveAlternateScreen,
                };
                let _ = disable_raw_mode();
                let _ = execute!(std::io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
                eprintln!("xmux: internal error — {info}");
                eprintln!("xmux: full detail logged to {}", log.display());
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
        crate::prefs::load_tree_width(&env.xmux_dir).unwrap_or(crate::ui::switcher::TREE_WIDTH),
        0,
    );
    let mut tree_width = tree_width_natural;
    // Auto-hide-tree mode: the live `prefix t` toggle, restored from its persisted
    // state if set, else the `auto-hide-tree` config default. The loop-top reconcile
    // reads it to size the tree (0 = hidden, mux full width) on focus changes.
    let mut auto_hide_tree = crate::prefs::load_auto_hide_tree(&env.xmux_dir)
        .unwrap_or_else(|| env.cfg.ui_auto_hide_tree());
    // The per-event mouse-gesture/input state the stdin arm carries across reads:
    //  - repeat_until: the resize-repeat window — set when a prefix-driven resize fires,
    //    it lets a bare Ctrl+←/→ keep resizing (no re-prefix) until it lapses (RESIZE_REPEAT_MS).
    //  - dragging_divider: true while the left button is dragging the tree/mux divider rule.
    //  - hovered_divider: true while the mouse hovers the divider rule (no button) — lights it
    //    up as a drag-resize grab cue. Fed to the switcher each draw via set_divider_hovered.
    //  - tree_armed: true while a prefix has been pressed in tree focus, awaiting the command key.
    let mut mouse_state = MouseState::default();

    // The control-mode metadata clients: one per remote host.
    let (host_tx, mut host_rx) = tokio::sync::mpsc::unbounded_channel::<HostEvent>();
    let mut mgr = HostManager::new(host_tx);
    mgr.set_xmux_dir(env.xmux_dir.clone());

    // The live PTY attachments: one real attached mux client per session.
    let (pty_tx, mut pty_rx) = tokio::sync::mpsc::unbounded_channel::<PtyEvent>();
    let mut worker = DisplayWorker::new(pty_tx);
    let mut registry = AttachRegistry::new();

    // Host model: keyed by id (same as Source::alias). Derives its args from the same
    // source data that source::build uses: the local source's socket from env.srcs, the
    // ssh aliases (every non-local source), and the OS from the first src's .os field
    // (all srcs share the same host OS). Ids are "local" + each alias — the same strings
    // that HostEvents carry as `host` and that the registry uses as the display key prefix.
    let ssh_aliases: Vec<String> = env
        .srcs
        .iter()
        .filter(|s| s.alias != crate::session::LOCAL_SOURCE)
        .map(|s| s.alias.clone())
        .collect();
    let host_os = env
        .srcs
        .first()
        .map(|s| s.os.as_str())
        .unwrap_or(std::env::consts::OS);
    let local_socket_opt = env
        .srcs
        .iter()
        .find(|s| s.alias == crate::session::LOCAL_SOURCE)
        .and_then(|s| s.socket.clone());
    let mut hosts = crate::model::Hosts::build(
        &env.cfg,
        &ssh_aliases,
        host_os,
        &env.xmux_dir,
        local_socket_opt,
    );

    // The cockpit's runtime state (single source of truth): the inventory the
    // components read, the canonical selection committed from the switcher's cursor at
    // the loop top, the attach debounce latch (last selection actually attached/switched
    // to) + its deadline, and the last session address persisted as the user's
    // last-selected (#1). Seeded from the source skeletons; events stream the tree in.
    let mut state =
        crate::state::State::from_sources(env.srcs.iter().map(|s| s.alias.clone()).collect());
    // The switcher, built over that seeded inventory; events stream the tree in.
    let mut switcher = Switcher::from_sources(&mut state);
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
    // The help overlay must show the prefix the user configured, not a literal.
    switcher.set_ui_prefix(env.ui_prefix.clone());
    // Restore the session the user last had selected (persisted across runs), so the
    // preselect lands there once its host streams in instead of guessing from the
    // unreliable cross-host `session_last_attached` (#1).
    switcher.set_preferred(crate::prefs::load_last_session(&env.xmux_dir));

    // Off-loop attach sequence. The in-flight set + reaped-ids + which session each display
    // shows now live on each `host.display` (HostDisplay), so the cockpit holds no free
    // host_session/in_flight/reaped_ids side-maps.
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

    let mut connected: HashSet<String> = HashSet::new();
    let mut panes_requested: HashSet<String> = HashSet::new();
    let mut detecting: HashSet<String> = HashSet::new();
    connect_all_sources(
        &mut mgr,
        &env,
        &hosts,
        &mut detecting,
        cols,
        body_rows,
        tree_width,
    );

    let spinner_start = std::time::Instant::now();
    let mut tick = tokio::time::interval(Duration::from_millis(SPINNER_FRAME_MS));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Periodic reconnect sweep: re-ensure any died remote control client (so #5
    // metadata sync self-heals) and re-attach the selected session's PTY if it
    // dropped (so a transient remote disconnect does not leave the right pane stuck
    // on "(attaching…)"). The sweep interval doubles as the retry backoff.
    let reconnect_start = tokio::time::Instant::now() + Duration::from_millis(RECONNECT_MS);
    let mut reconnect =
        tokio::time::interval_at(reconnect_start, Duration::from_millis(RECONNECT_MS));
    reconnect.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Frame timer: wakes the loop at the redraw cadence so a pending `dirty` draw is
    // flushed promptly even when no other event arrives. Redraws are gated on `dirty`
    // + elapsed ≥ FRAME_MS, so rapid input coalesces into ≤30fps instead of one
    // full-screen repaint per keystroke (the navigation freeze).
    let mut frame = tokio::time::interval(Duration::from_millis(FRAME_MS));
    frame.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut dirty = true;
    let mut last_draw = std::time::Instant::now() - Duration::from_millis(FRAME_MS);
    // Debounced tree-width persist: set on each resize tick; flushed once ~400ms after
    // the last tick (after a burst of Ctrl-arrow autorepeats settles). Avoids fsyncing
    // the state dir per keystroke during a held autorepeat.
    let mut width_dirty = false;
    let mut width_flush_at: Option<std::time::Instant> = None;

    loop {
        // Advance the spinner from wall-clock so it animates regardless of which arm
        // fired, then commit the cursor's target into the canonical selection. A
        // changed selection ensures its PTY + (for a window row) switches the window.
        switcher.set_spinner_frame(spinner_frame_at(spinner_start.elapsed()));
        switcher.set_divider_hovered(mouse_state.hovered_divider);
        // Derive the modal dimension of focus from the open-modal kind: open a modal →
        // Focus becomes Popup/Menu carrying the current pane; close it → restore that
        // pane. The single owner of the modal/pane reconciliation.
        let modal_kind = state.modal_kind();
        state.focus.sync_modal(modal_kind);
        // The single owner of the effective tree width: reconcile it to the current
        // focus + the hide setting, and to any natural-width change from prefix h/l.
        // On a change (focus toggled, hide flips the width, or h/l resized the tree),
        // resize the PTYs to the new mux view size so the mux reflows, and mark dirty.
        // ponytail: resize_all touches every live attachment on each change. It is
        // gated to fire once per actual change (not per loop), matching the existing
        // h/l and console-resize paths; debounce only if toggle-spam proves costly.
        let want_tree_width = reconciled_tree_width(
            state.focus.is_terminal_focused(),
            auto_hide_tree,
            tree_width_natural,
        );
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
        // A portable-pty child spawn clears ENABLE_MOUSE_INPUT on the parent CONIN,
        // killing mouse capture; re-assert it whenever it drifts off.
        crate::proxy::term::ensure_mouse_capture();
        // An `r` re-scan also re-attaches the CURRENT display: tear the (possibly
        // detached / dead) attachment down and clear its switch latch so the attach
        // below re-creates a fresh client for the viewed session. Keeps the per-host
        // model — recovery from a lost display client is explicit, on demand.
        if switcher.take_reattach_kick() && !state.selection.is_empty() {
            let key = display_key(&hosts, &state.selection);
            registry.remove(&key);
            if let Some(h) = hosts.get_mut(&state.selection.source) {
                h.display.clear(&key); // drop the prior latch so the re-attach is fresh
            }
            state.displayed = Selection::default(); // nothing confirmed on screen → "(attaching…)"
            state.attach_deadline = Some(std::time::Instant::now());
        }
        // In passthrough the user no longer drives the tree cursor (stdin goes to the
        // PTY), so the tree selection tracks the displayed session's active window — always.
        // select_active_window is idempotent (no move when already on the active window or
        // when the session's panes are unknown), so calling it each iteration is cheap.
        if state.focus.is_terminal_focused() {
            switcher.select_active_window(&mut state);
        }
        if sync_selection_from_switcher(&mut state, &switcher) {
            // The cursor moved → the tree needs a redraw. The attach is NOT issued
            // here: the Select above only marks it pending; the Tick below arms the
            // debounce, re-armed on every move so only the settled selection attaches.
            dirty = true;
        }
        // Drive one debounce beat. The clock and the registry/host attach facts enter
        // as DATA on the Tick; State::apply owns the arm/fire decision (one mutation
        // site). The frame timer re-enters the loop, so the settled attach fires even
        // with no further input.
        {
            let (key_live, in_flight) = selection_attach_facts(&registry, &hosts, &state.selection);
            let cmds = state.apply(crate::model::Action::Tick {
                now: std::time::Instant::now(),
                key_live,
                in_flight,
            });
            for cmd in cmds {
                match cmd {
                    crate::model::Command::PersistLastSession(addr) => {
                        crate::prefs::save_last_session(&env.xmux_dir, &addr);
                    }
                    crate::model::Command::Attach(sel) => {
                        let t = std::time::Instant::now();
                        if select_attach(
                            &mut registry,
                            &mut hosts,
                            &sel,
                            &worker,
                            &mut attach_seq,
                            cols,
                            body_rows,
                            tree_width,
                            &mgr,
                        ) {
                            // A synchronous path (switch-client / select-window / already
                            // attached) leaves a live grid for the key right now → the
                            // selection is on screen. An async path (first-attach /
                            // per-session reattach) removed or never had the key → the grid
                            // is "(attaching…)" until DisplayReady confirms it (which sets
                            // state.displayed then). Probing the registry tells them apart.
                            if registry.contains(&display_key(&hosts, &sel)) {
                                state.displayed = sel.clone();
                            }
                        }
                        dbg_ms(&env.xmux_dir, "select_attach", t);
                        dirty = true;
                        dbg_log(
                            &env.xmux_dir,
                            &format!(
                                "state.selection -> key={} sess={}",
                                display_key(&hosts, &sel),
                                sel.session
                            ),
                        );
                    }
                    // The settled-selection Tick never returns the synchronous
                    // key/ctl-only commands or a session-lifecycle RunOp.
                    crate::model::Command::SelectAddress(_)
                    | crate::model::Command::Rescan
                    | crate::model::Command::AdjustTreeWidth(_)
                    | crate::model::Command::ToggleAutoHide
                    | crate::model::Command::RunOp(_)
                    | crate::model::Command::Quit => {}
                }
            }
        }

        // Flush the debounced tree-width persist once the resize burst settles.
        // Armed by each apply_width_delta call (via StdinOutcome::width_changed or
        // the ctl path); fires once ~WIDTH_FLUSH_MS after the last resize tick. The
        // frame timer re-enters the loop so this fires even with no further input.
        if width_dirty && width_flush_at.is_some_and(|d| std::time::Instant::now() >= d) {
            crate::prefs::save_tree_width(&env.xmux_dir, tree_width_natural);
            width_dirty = false;
            width_flush_at = None;
        }

        // Draw the split: the tree (left) and the selected session's live PTY grid
        // (right). GATED — redraw only when something changed (`dirty`) AND at most
        // once per frame, so rapid navigation / a busy PTY cannot flood the terminal
        // with full-screen repaints and stall the loop. The display key is the HOST
        // for remote tmux (one PTY per host) or `source/session` for local psmux.
        if dirty && last_draw.elapsed() >= Duration::from_millis(FRAME_MS) {
            // Show the grid only when the confirmed display truth matches the selection
            // (defect A): a stale attachment mid-reattach renders "(attaching…)", never
            // the previous session.
            let grid_arc = state
                .display_matches_selection()
                .then(|| registry.grid(&display_key(&hosts, &state.selection)))
                .flatten();
            let terminal_focused = state.focus.is_terminal_focused();
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
                        switcher.render(f, guard.as_deref(), terminal_focused, tree_width, &state);
                        dbg_ms(&xmux_dir, "render", t_render);
                    })
                }
                None => term.draw(|f| {
                    let t_render = std::time::Instant::now();
                    switcher.render(f, None, terminal_focused, tree_width, &state);
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
                if handle_host_event(ev, &mut mgr, &mut hosts, &mut registry, &mut switcher, &mut state, &env, &mut connected, &mut panes_requested, &mut detecting, &worker, &mut attach_seq, cols, body_rows, tree_width) {
                    state.attach_deadline = Some(std::time::Instant::now() + Duration::from_millis(ATTACH_DEBOUNCE_MS));
                    dirty = true;
                }
                let mut budget = EVENT_DRAIN_BUDGET;
                while budget > 0 {
                    match host_rx.try_recv() {
                        Ok(ev) => {
                            if handle_host_event(ev, &mut mgr, &mut hosts, &mut registry, &mut switcher, &mut state, &env, &mut connected, &mut panes_requested, &mut detecting, &worker, &mut attach_seq, cols, body_rows, tree_width) {
                                state.attach_deadline = Some(std::time::Instant::now() + Duration::from_millis(ATTACH_DEBOUNCE_MS));
                                dirty = true;
                            }
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
                // Detach-to-recover: if the session the user is VIEWING (mux focused) exits —
                // a `detach`, the user's own client detaching, or a transient drop — re-attach
                // it instead of quitting (quit is `prefix q` only). Capture its id BEFORE any
                // reap removes it; a background session dropping (tree focus, or a non-displayed
                // attach) is just reaped.
                let displayed_attach_id = (state.focus.is_terminal_focused() && !state.selection.is_empty())
                    .then(|| registry.get(&display_key(&hosts, &state.selection)).map(|a| a.id()))
                    .flatten();
                let mut detached = false;
                match ev {
                    PtyEvent::Exited { id } => {
                        if Some(id) == displayed_attach_id {
                            detached = true;
                        }
                        clear_display_tty_for_attach(&mut hosts, &registry, id);
                        if !registry.reap(id) {
                            // pre-Ready Exited: registry has no id yet. Attribute to the owning
                            // host via pending so its Ready tears down instead of inserting a
                            // dead pane.
                            if let Some(h) = hosts.iter_mut().find(|h| h.display.pending.contains_key(&id)) {
                                h.display.reaped_ids.insert(id);
                            }
                        }
                    }
                    PtyEvent::DisplayTty { id, tty } => record_display_tty(&mut hosts, &registry, id, tty),
                    PtyEvent::Output { .. } => {}
                }
                let mut budget = EVENT_DRAIN_BUDGET;
                while budget > 0 {
                    match pty_rx.try_recv() {
                        Ok(PtyEvent::Exited { id }) => {
                            if Some(id) == displayed_attach_id {
                                detached = true;
                            }
                            clear_display_tty_for_attach(&mut hosts, &registry, id);
                            if !registry.reap(id) {
                                // pre-Ready Exited: attribute to the owning host's reaped_ids.
                                if let Some(h) = hosts.iter_mut().find(|h| h.display.pending.contains_key(&id)) {
                                    h.display.reaped_ids.insert(id);
                                }
                            }
                            budget -= 1;
                        }
                        Ok(PtyEvent::Output { .. }) => { budget -= 1; }
                        Ok(PtyEvent::DisplayTty { id, tty }) => {
                            record_display_tty(&mut hosts, &registry, id, tty);
                            budget -= 1;
                        }
                        Err(_) => break,
                    }
                }
                if detached {
                    // The viewed session's client detached/exited — recover by re-attaching
                    // it (reaped above, so the loop-top attach re-fires once its PTY is gone).
                    // Debounced so a session that is genuinely gone makes only a few attempts
                    // before the inventory refetch drops its row and the cursor moves on.
                    state.attach_deadline =
                        Some(std::time::Instant::now() + Duration::from_millis(ATTACH_DEBOUNCE_MS));
                    dirty = true;
                }
            }
            Some(ev) = worker.recv() => {
                match ev {
                    DisplayEvent::Ready { seq, key, attachment } => {
                        let hid = host_of_key(&key).to_string();
                        if let Some(h) = hosts.get_mut(&hid) {
                            dbg_log(
                                &env.xmux_dir,
                                &format!(
                                    "attach ready key={key} seq={seq} current={:?} id={}",
                                    h.display.in_flight.get(&key),
                                    attachment.id()
                                ),
                            );
                            if h.display.reaped_ids.remove(&attachment.id()) {
                                // Exited raced ahead of this Ready: the child already died and its
                                // pump (one Exited at EOF) has ended. Inserting now would leave a
                                // dead, never-reaped pane that contains() refuses to re-attach.
                                // Tear it down and clear in-flight so the next settle re-requests.
                                h.display.in_flight.remove(&key);
                                h.display.pending.remove(&attachment.id());
                                attachment.teardown();
                            } else if attach_reply_is_current(&h.display.in_flight, &key, seq) {
                                h.display.in_flight.remove(&key);
                                h.display.pending.remove(&attachment.id());
                                // This attach is now the live grid for the key. Record the
                                // session it shows as the confirmed display truth, so the
                                // terminal view switches from "(attaching…)" to its content.
                                // If the selection moved to another session of the same host
                                // while this attach was in flight, displayed now differs from
                                // selection, so the next pass re-runs select_attach to switch
                                // (Shared) / reattach (PerSession) to where the cursor is.
                                let shown = h.display.shows(&key).unwrap_or_default().to_string();
                                registry.insert(&key, attachment);
                                state.displayed = Selection {
                                    source: hid.clone(),
                                    session: shown,
                                    window: None,
                                };
                            } else {
                                // Stale Ready (a newer attach superseded this seq): forget its
                                // pending id before teardown so the id->key map cannot grow.
                                h.display.pending.remove(&attachment.id());
                                attachment.teardown();
                            }
                        } else {
                            attachment.teardown();
                        }
                    }
                    DisplayEvent::Failed { seq, key, message } => {
                        let hid = host_of_key(&key).to_string();
                        if let Some(h) = hosts.get_mut(&hid) {
                            if attach_reply_is_current(&h.display.in_flight, &key, seq) {
                                h.display.in_flight.remove(&key);
                                h.display.pending.retain(|_, k| k != &key);
                            }
                        }
                        dbg_log(&env.xmux_dir, &format!("attach failed key={key}: {message}"));
                    }
                }
            }
            Some(bytes) = stdin_rx.recv() => {
                // Clone the selection so &mut state can be threaded through alongside it
                // (the ForwardToMux path reads the selection for display_key/registry input).
                let selection = state.selection.clone();
                let outcome = handle_stdin_bytes(
                    &bytes, &mut mouse_state, &mut switcher, &mut state, &mut registry, &mut mgr,
                    &env, &mut hosts, &mut detecting, &selection, &mut term_input, &mut tree_decoder, &ops, &op_tx,
                    &mut tree_width_natural, &mut auto_hide_tree, prefix, cols, body_rows,
                    tree_width,
                );
                if outcome.dirty {
                    dirty = true;
                }
                if outcome.width_changed {
                    width_dirty = true;
                    width_flush_at = Some(std::time::Instant::now() + Duration::from_millis(WIDTH_FLUSH_MS));
                }
                if outcome.quit {
                    break;
                }
            }
            Some(cmd) = cmd_rx.recv() => {
                match cmd {
                    Cmd::Op(action) => {
                        // dispatch_action spawns any RunOp (a ctl-driven lifecycle action)
                        // off-loop itself; its OpResult folds back through op_tx as usual.
                        let (quit_op, wc) = dispatch_action(action, &mut switcher, &mut state, &mut tree_width_natural, &mut auto_hide_tree, &env.xmux_dir, &ops, &op_tx);
                        if wc {
                            width_dirty = true;
                            width_flush_at = Some(std::time::Instant::now() + Duration::from_millis(WIDTH_FLUSH_MS));
                        }
                        if quit_op {
                            break;
                        }
                        // A Switch/Focus may need the cursor's host connected.
                        ensure_current_host(&mut mgr, &env, &hosts, &switcher, cols, body_rows, tree_width);
                        if sync_selection_from_switcher(&mut state, &switcher) {
                            dirty = true;
                        }
                    }
                    Cmd::Status(reply) => { let _ = reply.send(status_line(&switcher, state.focus.pane_is_tree())); }
                    Cmd::Dump(reply) => {
                        let sz = term
                            .size()
                            .unwrap_or(ratatui::layout::Size { width: 80, height: 24 });
                        let grid_arc = state
                            .display_matches_selection()
                            .then(|| registry.grid(&display_key(&hosts, &state.selection)))
                            .flatten();
                        let dump = match &grid_arc {
                            Some(g) => {
                                let guard = g.lock().ok();
                                dump_overlay(&mut switcher, guard.as_deref(), sz.width, sz.height, &state)
                            }
                            None => dump_overlay(&mut switcher, None, sz.width, sz.height, &state),
                        };
                        let _ = reply.send(dump);
                    }
                    Cmd::RawKey(k) => {
                        // Route the FULL command batch through the single dispatcher (not
                        // just RunOp): a switcher key emits only RunOp today, but
                        // dispatch_commands handles every variant so a future non-RunOp
                        // command is acted on, never silently dropped (RunOp still spawns
                        // off-loop, its OpResult folding back through op_tx).
                        let cmds = switcher.handle_key(k, &mut state);
                        let (quit_key, wc) = dispatch_commands(cmds, &mut switcher, &mut state, &mut tree_width_natural, &mut auto_hide_tree, &env.xmux_dir, &ops, &op_tx);
                        if wc {
                            width_dirty = true;
                            width_flush_at = Some(std::time::Instant::now() + Duration::from_millis(WIDTH_FLUSH_MS));
                        }
                        if quit_key {
                            break;
                        }
                        ensure_current_host(&mut mgr, &env, &hosts, &switcher, cols, body_rows, tree_width);
                        if sync_selection_from_switcher(&mut state, &switcher) {
                            dirty = true;
                        }
                    }
                    Cmd::RawBytes(bytes) => {
                        if !bytes.is_empty() {
                            registry.input(&display_key(&hosts, &state.selection), bytes);
                        }
                    }
                }
            }
            Some(result) = op_rx.recv() => {
                switcher.apply_op_result(result, &mut state);
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
                if !state.selection.is_empty() {
                    let key = display_key(&hosts, &state.selection);
                    let in_flight_for_key = hosts
                        .get(&state.selection.source)
                        .map(|h| h.display.in_flight.contains_key(&key))
                        .unwrap_or(false);
                    if in_flight_for_key || registry.connecting(&key) {
                        sp.insert(state.selection.address());
                    }
                }
                switcher.set_spinner(sp);
            }
            _ = reconnect.tick() => {
                let (vc, vr) = terminal_view_size(cols, body_rows, tree_width);
                // Self-heal sweep over every source: a DETECTED host re-ensures its
                // metadata channel (a no-op when alive; respawns a died control client or
                // a finished poll task — the manager picks the channel from event_source),
                // an UNDETECTED host retries detection (a host down at launch populates its
                // tree once it returns). This sweep is the sole automatic retry path now
                // that the per-poll-tick re-enumeration lives inside the poll task itself.
                for src in &env.srcs {
                    let detected = hosts.get(&src.alias).map(|h| h.detected).unwrap_or(false);
                    if detected {
                        if let Some(host) = hosts.get(&src.alias) {
                            let _ = mgr.ensure(&src.alias, host, src, vc, vr);
                        }
                    } else {
                        scan_or_dispatch_host(&mut mgr, &env, &hosts, &mut detecting, &src.alias, vc, vr);
                    }
                }
                // Re-warm each shared host's per-host PTY if it dropped (ENSURE-ONLY,
                // never reap: a just-respawned control client has an empty inventory
                // until its list-sessions resolves; reaping on that would tear down a
                // live PTY. Closed-host reaping is owned by the Inventory/Changed path).
                for src in &env.srcs {
                    let shared_switch = hosts
                        .get(&src.alias)
                        .map(|h| matches!(h.mux.select(), SelectOutcome::SharedSwitch))
                        .unwrap_or(false);
                    if !shared_switch || registry.contains(&src.alias) {
                        continue;
                    }
                    let first = match mgr.get(&src.alias) {
                        Some(client) => client.inventory.lock().unwrap().sessions.first().cloned(),
                        None => continue,
                    };
                    if let Some(s) = first {
                        if let Some(host) = hosts.get_mut(&src.alias) {
                            if !host.display.in_flight.contains_key(&src.alias) {
                                let remote = host.transport.is_remote();
                                request_attach(
                                    &mut registry, &worker, &mut host.display, &mut attach_seq,
                                    &src.alias, shared_display_attach_argv(remote, src, &s.name, None), vc, vr,
                                );
                                host.display.set_shows(&src.alias, &s.name);
                            }
                        }
                    }
                }
                // Re-attach the selected session's display terminal if it dropped.
                if !state.selection.is_empty() {
                    let key = display_key(&hosts, &state.selection);
                    let in_flight_for_key = hosts
                        .get(&state.selection.source)
                        .map(|h| h.display.in_flight.contains_key(&key))
                        .unwrap_or(false);
                    if !registry.contains(&key) && !in_flight_for_key {
                        select_attach(
                            &mut registry, &mut hosts, &state.selection, &worker,
                            &mut attach_seq, cols, body_rows, tree_width, &mgr,
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

    // A resize within the last WIDTH_FLUSH_MS before quit leaves the debounce deadline
    // unreached, so the final width is still pending — persist it on the way out so the
    // tree width the user left with survives the next launch.
    if width_dirty {
        crate::prefs::save_tree_width(&env.xmux_dir, tree_width_natural);
    }
    registry.teardown_all();
    mgr.teardown_all();
    0
}

/// Runs a [`MuxOp`](crate::model::MuxOp) (the create/rename/kill/... a key resolved
/// to, via `State::apply` → [`Command::RunOp`](crate::model::Command)) OFF the loop in
/// a detached task, folding its result back through `op_tx`, so a slow ssh round-trip
/// never freezes rendering, host streaming, or the control socket.
fn spawn_op(
    op: crate::model::MuxOp,
    ops: &Arc<dyn crate::ui::switcher::Ops>,
    op_tx: &tokio::sync::mpsc::UnboundedSender<crate::ui::switcher::OpResult>,
) {
    let ops = ops.clone();
    let tx = op_tx.clone();
    tokio::spawn(async move {
        let result = crate::ui::switcher::run_op(&op, ops.as_ref()).await;
        let _ = tx.send(result);
    });
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
    Some(crate::control::socket_path(
        &env.xmux_dir,
        std::process::id(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::source::Source;
    use std::collections::HashMap;

    // --- resolve_tree_key: pure TREE-focus key resolution -------------------
    /// Resolve one read at the default prefix (C-g = 0x07), fresh decoder/armed,
    /// folding the per-key resolver over the decoded keys.
    fn rt(bytes: &[u8], is_inputting: bool) -> Vec<Action> {
        let mut dec = crate::proxy::decode::KeyDecoder::new();
        let mut armed = false;
        dec.feed(bytes)
            .into_iter()
            .filter_map(|k| resolve_tree_key(k, &mut armed, 0x07, is_inputting))
            .collect()
    }

    #[test]
    fn resolve_tree_prefix_commands() {
        assert_eq!(rt(b"\x07q", false), vec![Action::Quit], "prefix q quits");
        assert_eq!(
            rt(b"\x07l", false),
            vec![Action::Width(1)],
            "prefix l widens"
        );
        assert_eq!(
            rt(b"\x07h", false),
            vec![Action::Width(-1)],
            "prefix h narrows"
        );
        assert_eq!(
            rt(b"\x07t", false),
            vec![Action::ToggleAutoHide],
            "prefix t toggles hide"
        );
        assert_eq!(
            rt(b"\x07?", false),
            vec![Action::ShowHelp],
            "prefix ? toggles help"
        );
        // prefix Tab cycles focus to the mux pane, and prefix Right does too. (Tab
        // arrives as Char('\t') from the byte decoder, not KeyCode::Tab — both map to
        // FocusMux so prefix Tab toggles tree⇄mux like it does from the mux side.)
        assert_eq!(
            rt(b"\x07\t", false),
            vec![Action::FocusMux],
            "prefix Tab cycles focus to mux"
        );
        assert_eq!(
            rt(b"\x07\x1b[C", false),
            vec![Action::FocusMux],
            "prefix Right focuses mux"
        );
        assert_eq!(
            rt(b"\x07\x1b[1;5C", false),
            vec![Action::Width(1)],
            "prefix Ctrl-Right widens"
        );
        assert_eq!(
            rt(b"\x07\x1b[1;5D", false),
            vec![Action::Width(-1)],
            "prefix Ctrl-Left narrows"
        );
    }

    #[test]
    fn resolve_tree_enter_focuses_mux_and_nav_is_a_tree_key() {
        assert_eq!(
            rt(b"\r", false),
            vec![Action::FocusMux],
            "Enter focuses the mux pane"
        );
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        assert_eq!(
            rt(b"j", false),
            vec![Action::TreeKey(KeyEvent::new(
                KeyCode::Char('j'),
                KeyModifiers::NONE
            ))],
            "a nav key is delegated to the tree verbatim"
        );
    }

    #[test]
    fn resolve_tree_while_inputting_passes_prefix_and_enter_to_the_tree() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        // While the input row is open, the prefix is NOT special (typed into the buffer)
        // and Enter does NOT focus the mux (it submits the input) — both go to the tree.
        assert_eq!(
            rt(b"\x07", true),
            vec![Action::TreeKey(KeyEvent::new(
                KeyCode::Char('\u{7}'),
                KeyModifiers::NONE
            ))],
            "prefix while inputting is a literal tree key, not an arm"
        );
        assert_eq!(
            rt(b"\r", true),
            vec![Action::TreeKey(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE
            ))],
            "Enter while inputting goes to the tree, not focus-switch"
        );
    }

    // --- mouse focus/position rules ----------------------------------------
    #[test]
    fn wheel_targets_tree_only_when_tree_focused_and_over_tree() {
        assert!(
            wheel_targets_tree(true, false),
            "tree focus + over tree → drive the tree"
        );
        assert!(
            !wheel_targets_tree(true, true),
            "tree focus + over the MUX pane → NOT the tree"
        );
        assert!(
            !wheel_targets_tree(false, false),
            "mux focus + over tree → not the tree"
        );
        assert!(
            !wheel_targets_tree(false, true),
            "mux focus + over mux → the mux child, not the tree"
        );
    }

    #[test]
    fn resolve_mouse_chain_routes_by_focus_and_position() {
        use ChainAction::*;
        // wheel: only drives the tree when tree-focused AND over the tree.
        assert_eq!(
            resolve_mouse_chain(true, false, true, false, true, false),
            ScrollTree(true),
            "wheel, tree focus, over tree → scroll"
        );
        assert_eq!(
            resolve_mouse_chain(true, true, false, false, true, false),
            LevelChange(false),
            "Ctrl+wheel, tree focus, over tree → level"
        );
        assert_eq!(
            resolve_mouse_chain(true, false, true, false, true, true),
            Nothing,
            "wheel, tree focus, over MUX → nothing (never crosses panes)"
        );
        assert_eq!(
            resolve_mouse_chain(true, false, true, false, false, true),
            ForwardToMux,
            "wheel, mux focus, over mux → forward to child"
        );
        assert_eq!(
            resolve_mouse_chain(true, false, true, false, false, false),
            Nothing,
            "wheel, mux focus, over tree → nothing"
        );
        // left press: focus-switch on the unfocused pane, act on the focused one.
        assert_eq!(
            resolve_mouse_chain(false, false, false, true, true, true),
            FocusMux,
            "left, tree focus, over mux → focus mux"
        );
        assert_eq!(
            resolve_mouse_chain(false, false, false, true, true, false),
            SelectRow,
            "left, tree focus, over tree → select row"
        );
        assert_eq!(
            resolve_mouse_chain(false, false, false, true, false, false),
            FocusTree,
            "left, mux focus, over tree → focus tree"
        );
        assert_eq!(
            resolve_mouse_chain(false, false, false, true, false, true),
            ForwardToMux,
            "left, mux focus, over mux → forward to child"
        );
        // a non-left, non-wheel press (e.g. right-press that the menu gate declined):
        // forwards to the child only when the mux is focused and the pointer is over it.
        assert_eq!(
            resolve_mouse_chain(false, false, false, false, false, true),
            ForwardToMux,
            "right-press, mux focus, over mux → forward"
        );
        assert_eq!(
            resolve_mouse_chain(false, false, false, false, true, false),
            Nothing,
            "right-press, tree focus, over tree → nothing"
        );
    }

    #[test]
    fn tree_menu_opens_only_in_tree_focus_over_the_tree() {
        assert!(
            tree_menu_may_open(true, true, false),
            "right-press, tree focus, over tree → may open"
        );
        assert!(
            !tree_menu_may_open(true, false, false),
            "right-press while the MUX is focused → never"
        );
        assert!(
            !tree_menu_may_open(true, true, true),
            "right-press over the mux pane → forwards, no tree menu"
        );
        assert!(
            !tree_menu_may_open(false, true, false),
            "a non-right press never opens the menu"
        );
    }

    #[test]
    fn resolve_tree_arming_persists_across_reads() {
        let mut dec = crate::proxy::decode::KeyDecoder::new();
        let mut armed = false;
        let r1: Vec<Action> = dec
            .feed(b"\x07")
            .into_iter()
            .filter_map(|k| resolve_tree_key(k, &mut armed, 0x07, false))
            .collect();
        assert_eq!(r1, Vec::<Action>::new());
        assert!(
            armed,
            "the prefix arms even when its command arrives in the next read"
        );
        let r2: Vec<Action> = dec
            .feed(b"q")
            .into_iter()
            .filter_map(|k| resolve_tree_key(k, &mut armed, 0x07, false))
            .collect();
        assert_eq!(r2, vec![Action::Quit]);
        assert!(!armed, "the command consumes the armed state");
    }

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
        let by_alias: HashMap<String, Source> =
            srcs.iter().map(|s| (s.alias.clone(), s.clone())).collect();
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
        let t = TerminalViewTarget {
            source: "jupiter06".into(),
            target: "api".into(),
        };
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
        let t = TerminalViewTarget {
            source: "jupiter06".into(),
            target: "api:2".into(),
        };
        let sel = Selection::from_target(&t);
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
    fn display_key_is_per_host_for_shared_and_reattach_psmux() {
        // Shared tmux and reattach psmux both use one PTY per HOST. The key is shaped
        // by mux behavior, read off the Host — never the transport's remote flag.
        let mut hosts = crate::model::Hosts::default();
        hosts.insert(crate::model::Host::new(
            crate::model::Transport::Ssh {
                alias: "jup".into(),
                control_path: String::new(),
                os: "linux".into(),
            },
            crate::backend::for_binary("tmux"), // Shared
        ));
        hosts.insert(crate::model::Host::new(
            crate::model::Transport::Local { socket: None }, // host id == "local"
            crate::backend::for_binary("psmux"),             // PerSession
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
            crate::model::Transport::Local { socket: None },
            crate::backend::for_binary("tmux"),
        ));

        apply_scan_result(
            &mut hosts,
            "local",
            Some(crate::backend::for_kind("psmux", "tmux")),
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
            crate::model::Transport::Local { socket: None },
            crate::backend::for_binary("psmux"),
        ));

        apply_scan_result(
            &mut hosts,
            "local",
            Some(crate::backend::for_kind("tmux", "psmux")),
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
        let mut src = fake_source("jupiter06");
        src.binary = "cmd.exe".into();
        let by_alias: HashMap<String, Source> = [("jupiter06".to_string(), src.clone())]
            .into_iter()
            .collect();
        let env = Env {
            cfg: Config::default(),
            cfg_warnings: Vec::new(),
            srcs: vec![src],
            by_alias,
            local_bin: "cmd.exe".into(),
            ui_prefix: "C-g".into(),
            xmux_dir: std::path::PathBuf::from("."),
        };
        let mut hosts = crate::model::Hosts::default();
        let mut host = crate::model::Host::new(
            crate::model::Transport::Ssh {
                alias: "jupiter06".into(),
                control_path: String::new(),
                os: "linux".into(),
            },
            crate::backend::for_binary("tmux"), // Control event source
        );
        host.detected = true;
        hosts.insert(host);
        let mut detecting = HashSet::new();
        connect_all_sources(
            &mut mgr,
            &env,
            &hosts,
            &mut detecting,
            80,
            24,
            crate::ui::switcher::TREE_WIDTH,
        );
        assert!(
            mgr.get("jupiter06").is_some(),
            "control host got a control client"
        );
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
        // Backend focused + setting on: hidden (0).
        assert_eq!(reconciled_tree_width(true, true, 48), 0);
        // Backend focused + setting off: stays shown at natural width.
        assert_eq!(reconciled_tree_width(true, false, 48), 48);
    }

    #[test]
    fn leading_ctrl_arrow_peels_one_and_ignores_others() {
        assert_eq!(
            leading_ctrl_arrow(b"\x1b[1;5C"),
            Some((1, 6)),
            "Ctrl-Right widens"
        );
        assert_eq!(
            leading_ctrl_arrow(b"\x1b[1;5D"),
            Some((-1, 6)),
            "Ctrl-Left narrows"
        );
        // A LEADING Ctrl-arrow is peeled even with trailing bytes (the caller loops /
        // routes the remainder) — this is what makes a coalesced autorepeat keep going.
        assert_eq!(
            leading_ctrl_arrow(b"\x1b[1;5C\x1b[1;5C"),
            Some((1, 6)),
            "peels the first of a burst"
        );
        assert_eq!(
            leading_ctrl_arrow(b"\x1b[1;5Cx"),
            Some((1, 6)),
            "peels past trailing input"
        );
        // Bare arrows and h/l are not repeat keys.
        assert_eq!(
            leading_ctrl_arrow(b"\x1b[C"),
            None,
            "bare arrow is not a repeat key"
        );
        assert_eq!(leading_ctrl_arrow(b"l"), None, "h/l are not repeat keys");
        assert_eq!(leading_ctrl_arrow(b""), None, "empty is not a repeat key");
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
    fn divider_drag_width_clamps_to_range() {
        // The dragged 1-based column becomes the 0-based tree width, clamped to range.
        assert_eq!(divider_drag_width(51), 50);
        assert_eq!(
            divider_drag_width(5),
            TREE_WIDTH_MIN,
            "too far left clamps to min"
        );
        assert_eq!(
            divider_drag_width(500),
            TREE_WIDTH_MAX,
            "too far right clamps to max"
        );
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
        assert_eq!(
            to_grid_local(area, 0, 5),
            None,
            "col=0 triggers checked_sub None"
        );
        assert_eq!(
            to_grid_local(area, 5, 0),
            None,
            "row=0 triggers checked_sub None"
        );
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
        let out = dump_overlay(&mut switcher, None, 80, 24, &state);
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
        use crate::ui::run::dump_overlay;
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
        let out = dump_overlay(&mut switcher, None, 80, 24, &state);
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
        use crate::ui::run::dump_overlay;
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
            dump_overlay(&mut switcher, None, 80, 24, &state).contains("scanning"),
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
        let out = dump_overlay(&mut switcher, None, 80, 24, &state);
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
        // session row -> (→ descend) window 0 -> (↓ sibling) window 1.
        switcher.handle_key(
            KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
            &mut state,
        );
        switcher.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &mut state);
        assert_eq!(
            switcher.terminal_view_target().target,
            "api:1",
            "cursor on window 1"
        );

        let (htx, _hrx) = tokio::sync::mpsc::unbounded_channel::<HostEvent>();
        let mut mgr = HostManager::new(htx);
        let (ptx, _prx) = tokio::sync::mpsc::unbounded_channel::<PtyEvent>();
        let mut registry = AttachRegistry::new();
        let worker = DisplayWorker::new(ptx);
        let mut attach_seq = 0u64;
        let env = fake_env_with_sources(&["jup"]);
        let mut connected = HashSet::new();
        let mut panes_requested = HashSet::new();
        let mut hosts = crate::model::Hosts::default();
        // Focus sets the cached active-window marker (window 0).
        let _ = handle_host_event(
            HostEvent::Focus {
                host: "jup".into(),
                session: "api".into(),
                window: 0,
            },
            &mut mgr,
            &mut hosts,
            &mut registry,
            &mut switcher,
            &mut state,
            &env,
            &mut connected,
            &mut panes_requested,
            &mut HashSet::new(),
            &worker,
            &mut attach_seq,
            80,
            24,
            crate::ui::switcher::TREE_WIDTH,
        );
        // The loop-top follow (simulated here) consumes the marker and moves the cursor.
        switcher.select_active_window(&mut state);
        assert_eq!(
            switcher.terminal_view_target().target,
            "api:0",
            "loop-top follow moved cursor to active window 0"
        );
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
        ); // → window 0
        switcher.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &mut state); // ↓ → window 1
        assert_eq!(switcher.terminal_view_target().target, "api:1");

        let (htx, _hrx) = tokio::sync::mpsc::unbounded_channel::<HostEvent>();
        let mut mgr = HostManager::new(htx);
        let (ptx, _prx) = tokio::sync::mpsc::unbounded_channel::<PtyEvent>();
        let mut registry = AttachRegistry::new();
        let worker = DisplayWorker::new(ptx);
        let mut attach_seq = 0u64;
        let env = fake_env_with_sources(&["jup"]);
        let mut connected = HashSet::new();
        let mut panes_requested = HashSet::new();
        let mut hosts = crate::model::Hosts::default();
        let _ = handle_host_event(
            HostEvent::Focus {
                host: "jup".into(),
                session: "api".into(),
                window: 0,
            },
            &mut mgr,
            &mut hosts,
            &mut registry,
            &mut switcher,
            &mut state,
            &env,
            &mut connected,
            &mut panes_requested,
            &mut HashSet::new(),
            &worker,
            &mut attach_seq,
            80,
            24,
            crate::ui::switcher::TREE_WIDTH,
        );
        assert_eq!(
            switcher.terminal_view_target().target,
            "api:1",
            "handler alone must not move the cursor"
        );
    }

    #[test]
    fn prefix_s_toggles_state() {
        use crate::proxy::app::Focus;
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
    fn attach_reply_is_current_only_for_latest_seq() {
        let mut f = std::collections::HashMap::new();
        f.insert("k".to_string(), 5u64);
        assert!(attach_reply_is_current(&f, "k", 5));
        assert!(!attach_reply_is_current(&f, "k", 4), "older seq is stale");
        assert!(
            !attach_reply_is_current(&f, "absent", 5),
            "no in-flight request → stale"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shared_host_reuses_one_attachment_and_in_flight_guards_current() {
        let mut hosts = crate::model::Hosts::default();
        hosts.insert(crate::model::Host::new(
            crate::model::Transport::Ssh {
                alias: "jup".into(),
                control_path: String::new(),
                os: "linux".into(),
            },
            crate::backend::for_binary("tmux"),
        ));
        let (ptx, _prx) = tokio::sync::mpsc::unbounded_channel();
        let worker = crate::display::DisplayWorker::new(ptx);
        let mut registry = AttachRegistry::new();
        let mut attach_seq = 0u64;
        // No control client registered ⇒ select_attach falls back to the lowered-switch
        // path (this test exercises attach/in-flight latching, not the switch transport).
        let (etx, _erx) = tokio::sync::mpsc::unbounded_channel::<crate::host::HostEvent>();
        let mgr = HostManager::new(etx);

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
            &mut registry,
            &mut hosts,
            &sel_a,
            &worker,
            &mut attach_seq,
            80,
            24,
            crate::ui::switcher::TREE_WIDTH,
            &mgr
        ));
        assert_eq!(hosts.get("jup").unwrap().display.shows("jup"), Some("a"));
        assert!(
            hosts
                .get("jup")
                .unwrap()
                .display
                .in_flight
                .contains_key("jup"),
            "first attach is in flight"
        );

        // Select session b of the SAME host before a's Ready arrives: must NOT overwrite the
        // shown session (else the switch-client to b after a lands would never fire).
        assert!(select_attach(
            &mut registry,
            &mut hosts,
            &sel_b,
            &worker,
            &mut attach_seq,
            80,
            24,
            crate::ui::switcher::TREE_WIDTH,
            &mgr
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
            crate::model::Transport::Local { socket: None },
            crate::backend::for_binary("psmux"),
        ));
        let (ptx, _prx) = tokio::sync::mpsc::unbounded_channel();
        let mut worker = crate::display::DisplayWorker::with_spawner(
            ptx,
            Box::new(|_argv, _cols, _rows, id, _events| Ok(crate::proxy::run::fake_attachment(id))),
        );
        let mut registry = AttachRegistry::new();
        let mut attach_seq = 0u64;
        let mgr = empty_manager();

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
            &mut registry,
            &mut hosts,
            &sel_test2,
            &worker,
            &mut attach_seq,
            80,
            24,
            crate::ui::switcher::TREE_WIDTH,
            &mgr
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
            assert!(attach_reply_is_current(&h.display.in_flight, &key, seq));
            h.display.in_flight.remove(&key);
            h.display.pending.remove(&attachment.id());
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
            &mut registry,
            &mut hosts,
            &sel_test,
            &worker,
            &mut attach_seq,
            80,
            24,
            crate::ui::switcher::TREE_WIDTH,
            &mgr
        ));

        let h = hosts.get("local").unwrap();
        assert_eq!(h.display.shows("local"), Some("test"));
        assert!(h.display.in_flight.contains_key("local"));
        assert!(
            !registry.contains("local"),
            "old psmux display attach is removed before reattach"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn psmux_select_attach_does_not_trust_stale_display_bookkeeping() {
        let mut hosts = crate::model::Hosts::default();
        hosts.insert(crate::model::Host::new(
            crate::model::Transport::Local { socket: None },
            crate::backend::for_binary("psmux"),
        ));
        hosts
            .get_mut("local")
            .unwrap()
            .display
            .set_shows("local", "target");

        let (ptx, _prx) = tokio::sync::mpsc::unbounded_channel();
        let worker = crate::display::DisplayWorker::with_spawner(
            ptx,
            Box::new(|_argv, _cols, _rows, id, _events| Ok(crate::proxy::run::fake_attachment(id))),
        );
        let mut registry = AttachRegistry::new();
        registry.insert("local", crate::proxy::run::fake_attachment(99));
        let mut attach_seq = 0u64;
        let mgr = empty_manager();

        let sel = Selection {
            source: "local".into(),
            session: "target".into(),
            window: None,
        };

        assert!(select_attach(
            &mut registry,
            &mut hosts,
            &sel,
            &worker,
            &mut attach_seq,
            80,
            24,
            crate::ui::switcher::TREE_WIDTH,
            &mgr
        ));

        let h = hosts.get("local").unwrap();
        assert!(h.display.in_flight.contains_key("local"));
        assert!(
            !registry.contains("local"),
            "psmux select_attach must replace the host PTY even when bookkeeping is stale"
        );
    }

    #[test]
    fn display_matches_selection_gates_grid_on_confirmed_session() {
        let sel = Selection {
            source: "h".into(),
            session: "api".into(),
            window: None,
        };
        let with = |displayed: Selection, selection: Selection| {
            let s = crate::state::State {
                displayed,
                selection,
                ..crate::state::State::default()
            };
            s.display_matches_selection()
        };
        // Nothing selected → never a grid.
        assert!(!with(Selection::default(), Selection::default()));
        // Confirmed truth names the same host+session → show the grid.
        assert!(with(sel.clone(), sel.clone()));
        // A different session (mid-reattach) → "(attaching…)", not the old session.
        let other_session = Selection {
            session: "db".into(),
            ..sel.clone()
        };
        assert!(!with(other_session, sel.clone()));
        // A different host → "(attaching…)".
        let other_host = Selection {
            source: "h2".into(),
            ..sel.clone()
        };
        assert!(!with(other_host, sel.clone()));
        // Same session, different window → still shown: one PTY renders the active window.
        let displayed_w = Selection {
            window: Some(1),
            ..sel.clone()
        };
        let selection_w = Selection {
            window: Some(3),
            ..sel.clone()
        };
        assert!(with(displayed_w, selection_w));
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
            crate::model::Transport::Local { socket: None },
            crate::backend::for_binary("psmux"),
        ));
        hosts
            .get_mut("local")
            .unwrap()
            .display
            .in_flight
            .insert("local".into(), 7);

        let (ptx, _prx) = tokio::sync::mpsc::unbounded_channel();
        let worker = crate::display::DisplayWorker::with_spawner(
            ptx,
            Box::new(|_argv, _cols, _rows, id, _events| Ok(crate::proxy::run::fake_attachment(id))),
        );
        let mut registry = AttachRegistry::new();
        let mut attach_seq = 7u64;
        let mgr = empty_manager();

        let sel = Selection {
            source: "local".into(),
            session: "target".into(),
            window: None,
        };

        assert!(select_attach(
            &mut registry,
            &mut hosts,
            &sel,
            &worker,
            &mut attach_seq,
            80,
            24,
            crate::ui::switcher::TREE_WIDTH,
            &mgr
        ));

        let h = hosts.get("local").unwrap();
        assert_eq!(h.display.in_flight.get("local"), Some(&8));
    }

    #[test]
    fn local_tmux_shared_second_session_lowers_to_a_local_switch_client_argv() {
        use crate::model::{LoweredSwitch, Transport};
        let host = crate::model::Host::new(
            Transport::Local { socket: None },
            crate::backend::for_binary("tmux"),
        );
        let plan = host.mux.switch_plan("b");
        let tty = "/dev/pts/7";
        let builder = |session: &str| host.mux.switch_client_argv(tty, session);
        let got = host.transport.lower_switch(&plan, &builder);
        let LoweredSwitch::Local(argv) = got.expect("local tmux must lower to Local") else {
            panic!("expected LoweredSwitch::Local");
        };
        assert!(
            argv.iter().any(|a| a == "tmux"),
            "argv contains tmux binary"
        );
        assert!(
            argv.iter().any(|a| a == "switch-client"),
            "argv contains switch-client"
        );
        assert!(
            argv.iter().any(|a| a == tty),
            "argv contains the display tty"
        );
        assert!(
            argv.iter().any(|a| a == "b"),
            "argv contains the session name"
        );
    }

    fn empty_manager() -> HostManager {
        HostManager::new(tokio::sync::mpsc::unbounded_channel().0)
    }

    fn detach_test_hosts(alias: &str) -> crate::model::Hosts {
        let mut hosts = crate::model::Hosts::default();
        hosts.insert(crate::model::Host::new(
            crate::model::Transport::Ssh {
                alias: alias.to_string(),
                control_path: String::new(),
                os: "linux".into(),
            },
            crate::backend::for_binary("tmux"),
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
        let mut hosts = detach_test_hosts("jup");
        let mut registry = AttachRegistry::new();
        let mut state = crate::state::State::from_sources(vec!["jup".into()]);
        let mut switcher = crate::ui::switcher::Switcher::from_sources(&mut state);
        let env = fake_env_with_sources(&["jup"]);
        let mut connected = HashSet::new();
        let mut panes: HashSet<String> = HashSet::new();
        let (ptx, _prx) = tokio::sync::mpsc::unbounded_channel::<PtyEvent>();
        let worker = DisplayWorker::new(ptx);
        let mut seq = 0u64;
        let mut mgr = empty_manager();

        hosts.get_mut("jup").unwrap().display_tty =
            crate::model::DisplayTty(Some("/dev/pts/3".into()));
        registry.insert_fake("jup", 7); // live attach under key = host id (Shared)
        assert!(registry.contains("jup"));

        // An UNRELATED client detaches → inert.
        let rearm = handle_host_event(
            HostEvent::ClientDetached {
                host: "jup".into(),
                client: "/dev/pts/9".into(),
            },
            &mut mgr,
            &mut hosts,
            &mut registry,
            &mut switcher,
            &mut state,
            &env,
            &mut connected,
            &mut panes,
            &mut HashSet::new(),
            &worker,
            &mut seq,
            80,
            24,
            30,
        );
        assert!(!rearm, "an unrelated client's detach must not rearm");
        assert!(
            registry.contains("jup"),
            "an unrelated client's detach must not reap our attach"
        );
        assert_eq!(
            hosts.get("jup").unwrap().display_tty.0.as_deref(),
            Some("/dev/pts/3"),
            "an unrelated detach must not clear our captured tty"
        );

        // OUR display client (the captured tty) detaches → reap + rearm.
        let rearm = handle_host_event(
            HostEvent::ClientDetached {
                host: "jup".into(),
                client: "/dev/pts/3".into(),
            },
            &mut mgr,
            &mut hosts,
            &mut registry,
            &mut switcher,
            &mut state,
            &env,
            &mut connected,
            &mut panes,
            &mut HashSet::new(),
            &worker,
            &mut seq,
            80,
            24,
            30,
        );
        assert!(rearm, "our own client's detach must rearm recovery");
        assert!(
            !registry.contains("jup"),
            "our display attach is reaped so it cannot persist dead"
        );
        assert!(
            hosts.get("jup").unwrap().display_tty.0.is_none(),
            "the dead client's tty is forgotten so no later switch-client targets it"
        );
    }

    #[test]
    fn marked_remote_attach_argv_prepends_marker_to_last_element() {
        let src = Source {
            remote: true,
            ..fake_source("jup")
        };
        let bare = src.attach_command("mysession", None);
        let bare_last = bare.last().cloned().unwrap_or_default();

        let marked = marked_remote_attach_argv(&src, "mysession", None);
        let marked_last = marked.last().cloned().unwrap_or_default();

        let prefix = crate::model::death::display_tty_marker_prefix();
        assert!(
            marked_last.starts_with(prefix),
            "last argv element must start with the display-tty marker prefix"
        );
        assert!(
            marked_last.ends_with(&bare_last),
            "the rest of the last element after the prefix is the original attach command"
        );
        assert_eq!(
            marked.len(),
            bare.len(),
            "marked_remote_attach_argv must not change argv length"
        );
    }

    #[test]
    fn shared_display_attach_argv_marks_remote_and_leaves_local_bare() {
        let prefix = crate::model::death::display_tty_marker_prefix();
        // Remote shared warm: carries the marker so the attach shell self-reports its
        // tty — without it the host's display_tty stays empty and switch-client fails.
        let remote_src = Source {
            remote: true,
            ..fake_source("jup")
        };
        let marked = shared_display_attach_argv(true, &remote_src, "sess", None);
        assert!(
            marked.last().unwrap().starts_with(prefix),
            "a remote shared attach must carry the display-tty marker: {marked:?}"
        );
        // Local shared warm: bare attach_command. Prepending the shell snippet would
        // corrupt the session-name argument (a local argv has no shell to run it).
        let local_src = Source {
            remote: false,
            ..fake_source("local")
        };
        let bare = shared_display_attach_argv(false, &local_src, "sess", None);
        assert_eq!(
            bare,
            local_src.attach_command("sess", None),
            "a local shared attach must be the bare, uncorrupted attach command"
        );
        assert!(
            !bare.last().unwrap().starts_with(prefix),
            "a local shared attach must NOT carry the marker: {bare:?}"
        );
    }

    // =========================================================================
    // HUMAN VISUAL-GATE CHECKLIST (run in a REAL terminal — never headless):
    // 1. Launch `xmux`. Confirm it enters the alternate screen cleanly and starts in
    //    Focus::Tree: the Host·Session·Window·Pane tree on the left, the live REAL
    //    terminal of the cursor's session on the right (a true attached mux client).
    // 2. Move the cursor between sessions. Confirm the right pane shows each session's
    //    real attached terminal instantly (it is pre-attached + kept alive), with a
    //    spinner while a session's attach is still establishing.
    // 3. Select a WINDOW row — confirm the attached client switches to that window.
    // 4. Press Enter (or C-g → / C-g Tab) — focus the terminal (Focus::Terminal); the split
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
    //    Backend mouse forwarding requires the mux to have `mouse on` (`set -g mouse on`);
    //    xmux only forwards. (Windows: capture needs ENABLE_VIRTUAL_TERMINAL_INPUT +
    //    the SGR DECSET that crossterm's WinAPI path omits — see proxy::term.)
    // =========================================================================

    #[test]
    fn dispatch_action_switch_moves_cursor_focus_toggles_width_and_quit() {
        use crate::model::{Action, FocusTarget};
        use crate::proxy::app::Focus;
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

        // Switch addr → cursor lands on db; returns (quit=false, width_changed=false).
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
                &ops,
                &op_tx,
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
            &ops,
            &op_tx,
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
            &ops,
            &op_tx,
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
                &ops,
                &op_tx,
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
                &ops,
                &op_tx,
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
        assert_eq!(status_line(&sw, true), "focus=tree target=api");
        assert_eq!(status_line(&sw, false), "focus=terminal target=api");
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
            &ops,
            &op_tx,
        );

        // The switch moved the cursor to db; the loop-top derive routes it through
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
        let mut switcher = Switcher::new(&mut state);
        let mut registry = AttachRegistry::new();
        let mut mgr = HostManager::new(tokio::sync::mpsc::unbounded_channel().0);
        let mut hosts = crate::model::Hosts::default();
        let mut mouse = MouseState::default();
        let mut term_input = crate::proxy::input::TermInput::new(0x07);
        let mut tree_decoder = crate::proxy::decode::KeyDecoder::new();
        let ops = crate::ui::switcher::tests_support::noop_ops();
        let (op_tx, _r) = tokio::sync::mpsc::unbounded_channel();
        let sel = Selection::default();
        let env = fake_env_with_sources(&["local"]);
        let mut detecting = HashSet::new();
        let mut natural = 48u16;
        let mut hide = false;
        let out = handle_stdin_bytes(
            b"\x07q",
            &mut mouse,
            &mut switcher,
            &mut state,
            &mut registry,
            &mut mgr,
            &env,
            &mut hosts,
            &mut detecting,
            &sel,
            &mut term_input,
            &mut tree_decoder,
            &ops,
            &op_tx,
            &mut natural,
            &mut hide,
            0x07,
            80,
            24,
            crate::ui::switcher::TREE_WIDTH,
        );
        assert!(out.quit, "prefix+q in tree focus quits");
    }

    #[test]
    fn kill_confirm_owns_keys_so_prefix_q_and_enter_do_not_quit_or_focus_mux() {
        // A kill-confirm is a modal popup, so it OWNS every key. With the resolver gated
        // on is_modal_popup_open (true for a confirm, where is_inputting is false),
        // `prefix q` reaches the switcher instead of arming the prefix, and Enter reaches
        // it instead of resolving to FocusMux — so a confirm can neither quit the cockpit
        // nor focus the mux out from under itself. (The first swallowed key cancels the
        // confirm, tmux confirm-before style; the point is the key does not quit/focus.)
        use crate::proxy::app::{Focus, PaneFocus};
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
        let mut switcher = Switcher::new(&mut state);
        let mut registry = AttachRegistry::new();
        let mut mgr = HostManager::new(tokio::sync::mpsc::unbounded_channel().0);
        let mut hosts = crate::model::Hosts::default();
        let mut mouse = MouseState::default();
        let mut term_input = crate::proxy::input::TermInput::new(0x07);
        let mut tree_decoder = crate::proxy::decode::KeyDecoder::new();
        let ops = crate::ui::switcher::tests_support::noop_ops();
        let (op_tx, _r) = tokio::sync::mpsc::unbounded_channel();
        let env = fake_env_with_sources(&["jup"]);
        let mut detecting = HashSet::new();
        let mut natural = 48u16;
        let mut hide = false;
        macro_rules! feed {
            ($bytes:expr) => {
                handle_stdin_bytes(
                    $bytes,
                    &mut mouse,
                    &mut switcher,
                    &mut state,
                    &mut registry,
                    &mut mgr,
                    &env,
                    &mut hosts,
                    &mut detecting,
                    &Selection::default(),
                    &mut term_input,
                    &mut tree_decoder,
                    &ops,
                    &op_tx,
                    &mut natural,
                    &mut hide,
                    0x07,
                    80,
                    24,
                    crate::ui::switcher::TREE_WIDTH,
                )
            };
        }
        // `x` on the session row arms the y/n confirm (a modal popup, not an inline input).
        feed!(b"x");
        assert!(
            state.is_modal_popup_open(),
            "x armed the kill-confirm popup"
        );
        assert!(
            !state.is_inputting(),
            "a kill-confirm is NOT an inline input"
        );
        // The loop-top reconciler makes Focus a modal carrying the prior pane.
        {
            let mk = state.modal_kind();
            state.focus.sync_modal(mk);
        }
        assert_eq!(
            state.focus,
            Focus::Popup {
                prior: PaneFocus::Tree
            }
        );
        // prefix q with the confirm armed: routed to the switcher, NOT a quit.
        let out = feed!(b"\x07q");
        assert!(
            !out.quit,
            "prefix q is owned by the kill-confirm, does not quit"
        );
        assert_eq!(
            state.focus,
            Focus::Popup {
                prior: PaneFocus::Tree
            },
            "pane focus unchanged"
        );
        // Re-arm and feed Enter: routed to the switcher, NOT a mux-focus.
        feed!(b"x");
        {
            let mk = state.modal_kind();
            state.focus.sync_modal(mk);
        }
        assert_eq!(
            state.focus,
            Focus::Popup {
                prior: PaneFocus::Tree
            },
            "confirm re-armed"
        );
        let out = feed!(b"\r");
        assert!(!out.quit);
        assert_eq!(
            state.focus,
            Focus::Popup {
                prior: PaneFocus::Tree
            },
            "Enter did not focus the mux"
        );
    }

    #[test]
    fn menu_keyboard_input_is_consumed_without_changing_restore_pane_or_writing_pty() {
        use crate::proxy::app::{Focus, PaneFocus};
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
            term.draw(|f| switcher.render(f, None, false, crate::ui::switcher::TREE_WIDTH, &state))
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
                    prior: PaneFocus::Tree
                }
            );

            let mut registry = AttachRegistry::new();
            let (att, input_log) = crate::proxy::run::fake_attachment_with_input_log(1);
            registry.insert("local/api", att);
            let mut mgr = HostManager::new(tokio::sync::mpsc::unbounded_channel().0);
            let mut hosts = crate::model::Hosts::default();
            hosts.insert(crate::model::Host::new(
                crate::model::Transport::Local { socket: None },
                crate::backend::for_binary("psmux"),
            ));
            let selection = Selection {
                source: "local".into(),
                session: "api".into(),
                window: None,
            };
            let mut mouse = MouseState::default();
            let mut term_input = crate::proxy::input::TermInput::new(0x07);
            let mut tree_decoder = crate::proxy::decode::KeyDecoder::new();
            let ops = crate::ui::switcher::tests_support::noop_ops();
            let (op_tx, _r) = tokio::sync::mpsc::unbounded_channel();
            let env = fake_env_with_sources(&["local"]);
            let mut detecting = HashSet::new();
            let mut natural = 48u16;
            let mut hide = false;

            let out = handle_stdin_bytes(
                bytes,
                &mut mouse,
                &mut switcher,
                &mut state,
                &mut registry,
                &mut mgr,
                &env,
                &mut hosts,
                &mut detecting,
                &selection,
                &mut term_input,
                &mut tree_decoder,
                &ops,
                &op_tx,
                &mut natural,
                &mut hide,
                0x07,
                80,
                24,
                crate::ui::switcher::TREE_WIDTH,
            );
            let during = state.focus;
            {
                let mk = state.modal_kind();
                state.focus.sync_modal(mk);
            }
            let restored = state.focus;
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
                "{label} over a menu does not request mux focus"
            );
            assert_eq!(
                during,
                Focus::Menu {
                    prior: PaneFocus::Tree
                },
                "{label} preserves the menu restore pane"
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
    fn handle_mouse_event_divider_grab_sets_dragging() {
        use crate::ui::switcher::{Scan, Switcher};
        // A left-press exactly on the divider column sets dragging_divider, as the
        // inline gate did (is_left_press && tree_width > 0 && col0 == tree_width).
        let scan = Scan {
            groups: vec![],
            panes: Default::default(),
        };
        let mut state = crate::state::State::from_scan(scan);
        let mut switcher = Switcher::new(&mut state);
        let mut registry = AttachRegistry::new();
        let mut mgr = HostManager::new(tokio::sync::mpsc::unbounded_channel().0);
        let hosts = crate::model::Hosts::default();
        let sel = Selection::default();
        let mut st = MouseState::default();
        let tree_width = crate::ui::switcher::TREE_WIDTH;
        let mut natural = tree_width;
        // 0-based col0 = ev.col - 1 must equal tree_width to grab the divider rule.
        let divider_col = tree_width + 1; // 1-based SGR column of the divider
                                          // cb=0 → left button, press, no wheel/motion → is_left_press is true.
        let ev = crate::proxy::mouse::MouseEvent {
            cb: 0,
            col: divider_col,
            row: 3,
            pressed: true,
        };
        let (vw, vh) = terminal_view_size(80, 24, tree_width);
        let term_area = ratatui::layout::Rect::new(tree_width + 1, 0, vw, vh);
        let mut non_mouse: Vec<u8> = Vec::new();
        let mut focus_toggle = false;
        let mut wheel = false;
        let mut detecting = HashSet::new();
        handle_mouse_event(
            &ev,
            &mut st,
            &mut switcher,
            &mut state,
            &mut registry,
            &mut mgr,
            &env_for_mouse_test(),
            &hosts,
            &mut detecting,
            &sel,
            &mut non_mouse,
            &mut focus_toggle,
            &mut wheel,
            term_area,
            &mut natural,
            80,
            24,
            tree_width,
        );
        assert!(
            st.dragging_divider,
            "left-press on the divider column grabs it"
        );
    }

    // A throwaway Env for the mouse-event test (its handlers never touch the env on
    // the divider-grab path, but the signature requires one).
    fn env_for_mouse_test() -> Env {
        fake_env_with_sources(&["local"])
    }
}
