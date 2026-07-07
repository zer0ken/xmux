//! The app: a persistent supervisor that owns the terminal for the whole
//! session. It keeps ONE real attached mux client per session — a `tmux attach` /
//! `psmux attach` running inside a `portable-pty` PTY ([`AttachRegistry`]) — alive
//! across selections, and renders the SELECTED session's live `Grid` on the right.
//! A separate control-mode client per remote host ([`HostManager`]) supplies the
//! tree view inventory, mux-side change events, and programmatic `select-window`;
//! local psmux is enumerated/polled with plain commands (it is one-server-per-
//! session, so a host-level control client cannot see across its sessions).
//!
//! State is explicit: [`Selection`] (the canonical `source`/`session`/`window`) is
//! the single source of truth the display reads — the `Switcher` owns only the tree
//! and selection. One `select!` loop interleaves stdin, host events, PTY events, the
//! control socket, terminal resize, and an animation tick. ratatui owns stdout and
//! draws the SAME split (tree + selected PTY grid) in both focus states — Focus::Tree
//! (tree focused) and Focus::Terminal (terminal focused) differ only in the view border
//! colour and where keys go, so toggling focus needs no screen clear.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use crate::app::input::{
    leading_ctrl_arrow, resolve_mouse_chain, resolve_tree_key, to_grid_local, tree_menu_may_open,
    view_border_drag_width, ChainAction, MouseState, StdinOutcome,
};
use crate::attach;
use crate::display::attachment::PtyEvent;
use crate::display::dispatch::Action;
use crate::display::registry::AttachRegistry;
use crate::display::{DisplayEnsure, DisplayEvent, DisplayWorker};
use crate::env::Env;
use crate::host::{HostEvent, HostManager};
use crate::model::Selection;
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

pub(crate) const TREE_WIDTH_MIN: u16 = 20;
pub(crate) const TREE_WIDTH_MAX: u16 = 100;

/// The ratatui terminal the app draws into. Loop-local in [`run_app`] (owns stdout);
/// passed to the `Runtime` methods that draw / resize / dump.
type Term = ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>;

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
/// Shared by the tree- and terminal-view focus `prefix t` paths. The effective tree width is
/// reconciled at the next loop top (`reconciled_tree_width`); the caller marks dirty.
fn toggle_auto_hide(mode: &mut bool, xmux_dir: &std::path::Path) {
    *mode = !*mode;
    crate::prefs::save_auto_hide_tree(xmux_dir, *mode);
}

/// Folds ONE domain [`Action`] in at the single mutation site ([`State::apply`]) and
/// runs the [`Command`]s it returns — the site both a keypress (via
/// `display::dispatch::Action::as_action`) and a ctl command resolve through, so the two
/// surfaces can never take divergent effect. Returns `(quit, width_changed)`: `quit`
/// signals the loop to exit; `width_changed` signals the loop to schedule the debounced
/// tree-width persist. `Switch` only moves the selection (a `SelectAddress` command); the
/// loop-top `Tick`/`select_attach` commits the attach on a later pass.
///
/// Only the synchronous, registry-free commands arise here — `Attach`/
/// `PersistLastSession` come exclusively from `Action::Tick`, which the run loop drives
/// with full registry access. `Action::Quit` is the only quit path through this dispatcher.
///
/// [`Action`]: crate::model::Action
/// [`Command`]: crate::model::Command
/// [`State::apply`]: crate::state::State::apply
/// The mutate-op sink the dispatchers hand to [`spawn_op`]: the `Ops` interface plus
/// the channel its off-loop `OpResult` folds back through. Bundled as one argument so
/// the two dispatchers stay under the argument-count lint.
type OpSink<'a> = (
    &'a Arc<dyn crate::ui::switcher::Ops>,
    &'a tokio::sync::mpsc::UnboundedSender<crate::ui::switcher::OpResult>,
);

fn dispatch_action(
    action: crate::model::Action,
    switcher: &mut crate::ui::switcher::Switcher,
    state: &mut crate::state::State,
    tree_width_natural: &mut u16,
    auto_hide_tree: &mut bool,
    xmux_dir: &std::path::Path,
    op_sink: OpSink<'_>,
) -> (bool, bool) {
    dispatch_commands(
        state.apply(action),
        switcher,
        state,
        tree_width_natural,
        auto_hide_tree,
        xmux_dir,
        op_sink,
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
fn dispatch_commands(
    cmds: Vec<crate::model::Command>,
    switcher: &mut crate::ui::switcher::Switcher,
    state: &mut crate::state::State,
    tree_width_natural: &mut u16,
    auto_hide_tree: &mut bool,
    xmux_dir: &std::path::Path,
    op_sink: OpSink<'_>,
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
            Command::RunOp(op) => spawn_op(op, op_sink.0, op_sink.1),
            // Settled-selection effects come only from Action::Tick, dispatched by the
            // run loop with registry/host access — never from a key/ctl action here.
            Command::PersistLastSession(_) | Command::Attach(_) => {}
        }
    }
    (quit, width_changed)
}

/// The `status` verb reply: focus side, displayed session, and this instance's
/// working directory + controlling tty. A flat, TAB-separated `key=value` line an
/// agent reads to confirm a `switch`/`focus` landed and that `xmux ctl list` parses to
/// tell instances apart. The wire format lives in `control` so producer and parser
/// cannot drift.
fn status_line(
    switcher: &crate::ui::switcher::Switcher,
    tree_focused: bool,
    cwd: &str,
    tty: &str,
) -> String {
    crate::control::format_status(&crate::control::StatusFields {
        focus: if tree_focused { "tree" } else { "terminal" }.to_string(),
        target: switcher.terminal_view_target().target.to_string(),
        cwd: cwd.to_string(),
        tty: tty.to_string(),
    })
}

/// This process's working directory for the `status` reply, or `-` if unreadable.
fn self_cwd() -> String {
    std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "-".to_string())
}

/// This process's controlling terminal, for `xmux ctl list`: `/dev/pts/N` on Linux
/// when stdin is a tty, `-` where there is none (a redirect) or on Windows (a console
/// has no pts). Best-effort and dependency-free — a `-` never breaks the listing, it
/// just leaves that column blank while pid + cwd + displayed session still identify
/// the instance.
fn self_tty() -> String {
    #[cfg(unix)]
    {
        use std::io::IsTerminal;
        if std::io::stdin().is_terminal() {
            if let Ok(link) = std::fs::read_link("/proc/self/fd/0") {
                let s = link.display().to_string();
                if s.starts_with("/dev/") {
                    return s;
                }
            }
        }
        "-".to_string()
    }
    #[cfg(not(unix))]
    {
        "-".to_string()
    }
}

/// The EFFECTIVE tree width to render and size the terminal view against. Hidden (0, terminal view
/// full width) only while the terminal view is focused AND auto-hide-tree mode is on;
/// otherwise the tree's natural width. Pure so the focus/mode interaction is
/// unit-testable; the loop owns the natural width and the PTY resize on change.
fn reconciled_tree_width(terminal_focused: bool, auto_hide_tree: bool, natural: u16) -> u16 {
    if terminal_focused && auto_hide_tree {
        0
    } else {
        natural
    }
}

/// The draw hot path's observability, kept OUT of the draw block so that block does
/// nothing but lock → render. Owns the per-key grid fingerprints (the
/// `display_grid_changed` dedup) and the `slow_step` probe that locates what stalls the
/// single-threaded loop during rapid navigation.
#[derive(Default)]
struct DrawObserver {
    /// Last (fingerprint, session) rendered per display key, so a `display_grid_changed`
    /// event fires at most once per real content change, never per frame.
    fingerprints: HashMap<String, (u64, String)>,
}

/// How a freshly-computed grid fingerprint relates to the last one rendered for its key —
/// the pure classification the draw block turns into a `display_grid_changed` log grade.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FpOutcome {
    /// Fingerprint unchanged — screen content did not change (no event).
    Unchanged,
    /// Fingerprint changed, same session — a steady-state repaint (TRACE grade).
    Steady,
    /// Fingerprint changed and the session differs, or first paint for this key — the
    /// transition's first frame landed (INFO grade).
    Switched,
}

impl DrawObserver {
    /// Classify a freshly-computed fingerprint for `addr`/`session` against the last one
    /// rendered, updating the record on any change. Returns the grade the caller emits.
    fn observe(&mut self, addr: &str, session: &str, fp: u64) -> FpOutcome {
        match self.fingerprints.get(addr) {
            Some((last_fp, _)) if *last_fp == fp => FpOutcome::Unchanged,
            Some((_, last_session)) if last_session == session => {
                self.fingerprints
                    .insert(addr.to_string(), (fp, session.to_string()));
                FpOutcome::Steady
            }
            _ => {
                self.fingerprints
                    .insert(addr.to_string(), (fp, session.to_string()));
                FpOutcome::Switched
            }
        }
    }

    /// Emits a `slow_step` DEBUG event when a synchronous step took at least 10ms — used
    /// to locate what stalls the single-threaded event loop during rapid navigation.
    fn slow_step(label: &str, start: std::time::Instant) {
        let ms = start.elapsed().as_millis();
        if ms >= 10 {
            tracing::debug!(label, ms, "slow_step");
        }
    }
}

/// Derives a [`Selection`] from the switcher's current terminal-view target. The target
/// is either `session` or `session:window`; the session part keys the PTY attachment, the
/// optional window part drives `select-window`. Stays in `app` because it depends on the
/// ui [`TerminalViewTarget`] — the [`Selection`] value itself is a pure `model` type.
fn selection_from_target(t: &TerminalViewTarget) -> Selection {
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

/// Derives the selection from the switcher selection and, if it moved, routes it through
/// the single mutation site as [`Action::Select`] — which records the new selection
/// and marks the attach pending. It arms NO deadline; the trailing [`Action::Tick`]
/// arms the debounce (re-armed on every move, so rapid navigation coalesces into one
/// trailing attach). Returns true when the selection changed (the tree needs a redraw).
///
/// The switcher selection is the selection authority; this routes the derived value
/// through `apply` as an intent rather than mutating `state` directly, so a selection
/// change still funnels through the single mutation site.
///
/// [`Action::Select`]: crate::model::Action::Select
/// [`Action::Tick`]: crate::model::Action::Tick
fn sync_selection_from_switcher(
    state: &mut crate::state::State,
    switcher: &crate::ui::switcher::Switcher,
) -> bool {
    let new_sel = selection_from_target(&switcher.terminal_view_target());
    if new_sel == state.selection {
        return false;
    }
    state.apply(crate::model::Action::Select(new_sel));
    true
}

/// The size to give a PTY attachment: the terminal view (right of the tree +
/// view border), NOT the whole terminal. Sizing a session to the full terminal makes
/// the remote wrap at a width wider than the visible view, so a line overflows the
/// right edge (and a double-width char straddles the clip boundary). The view width
/// is `cols - tree_width - 1` (tree + the single view border rule), except `tree_width == 0`
/// (the tree-hidden sentinel) gives the full `cols` with no view border; the view HEIGHT is
/// the full terminal height (`body_rows + 1`) because the hint_bar and input occupy
/// the tree column, leaving the terminal column the full height. Both clamp to at least 1.
pub(crate) fn terminal_view_size(cols: u16, body_rows: u16, tree_width: u16) -> (u16, u16) {
    // tree_width == 0 is the "tree hidden" sentinel: the terminal view takes the full width
    // with no view border column. Otherwise subtract the tree column + the 1-col view border.
    let view_cols = if tree_width == 0 {
        cols.max(1)
    } else {
        cols.saturating_sub(tree_width + 1).max(1)
    };
    (view_cols, (body_rows + 1).max(1))
}

/// The `AttachRegistry` key for a selection.
pub(crate) fn display_key(hosts: &crate::model::Hosts, sel: &Selection) -> String {
    hosts
        .get(&sel.source)
        .map(host_selection_key)
        .unwrap_or_else(|| sel.address())
}

/// The display key for a host's selection. Both server models key the live display by
/// HOST id: tmux keeps one PTY per host (shared, moved by switch-client), and psmux —
/// though one-server-per-session — is displayed through ONE per-host PTY that is
/// reattached on every session change. This is the supervisor/driver authority for the
/// live attach path — the sole keying authority for both models.
pub(crate) fn host_selection_key(host: &crate::model::Host) -> String {
    host.id().to_string()
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
        .map(|h| h.display.in_flight_contains(&key))
        .unwrap_or(false);
    (key_live, in_flight)
}

/// Issues an OFF-LOOP attach for `key`: allocates the attachment id, records the request's
/// seq in the owning host's `display.in_flight` + the id→key in `display.pending`, and asks
/// the worker to spawn. The worker's `Ready` reply (handled in the app loop) inserts the
/// finished attachment into the registry. `display` MUST be the host that owns `key`. Returns
/// the allocated attachment id so a caller can correlate a follow-up probe to it.
pub(crate) fn request_attach(
    registry: &mut AttachRegistry,
    worker: &DisplayWorker,
    display: &mut crate::model::HostDisplay,
    attach_seq: &mut u64,
    key: &str,
    argv: Vec<String>,
    size: (u16, u16),
) -> u64 {
    let id = registry.alloc_id();
    *attach_seq += 1;
    display.mark_in_flight(key, *attach_seq);
    display.mark_pending(id, key);
    worker.ensure(DisplayEnsure {
        seq: *attach_seq,
        key: key.to_string(),
        argv,
        cols: size.0,
        rows: size.1,
        id,
    });
    id
}

/// Makes the SELECTED session live in its host's display terminal and lands it on
/// the selected window. Returns `true` when the selection has a session to show.
///
/// The per-mux DECISION lives in the host's [`MuxDriver`](crate::driver::MuxDriver):
/// this dispatcher picks the driver off the host's model ([`driver_for`]) and hands it
/// the supervisor capabilities via [`DriverCtx`]. Shared (tmux) keeps one PTY per host,
/// moved with `switch-client`; PerSession (psmux) reattaches a per-host PTY on each
/// session change. The bookkeeping (current session per key + in-flight spawn) lives on
/// the owning `host.display`.
///
/// [`driver_for`]: crate::driver::driver_for
/// [`DriverCtx`]: crate::driver::DriverCtx
pub(crate) fn select_attach(sel: &Selection, ctx: &mut crate::driver::DriverCtx) -> bool {
    if sel.is_empty() {
        return false;
    }
    let Some(host) = ctx.hosts.get(&sel.source) else {
        return false;
    };
    let mut driver = crate::driver::driver_for(host);
    driver.show(sel, ctx)
}

/// The grid the supervisor renders for the CONFIRMED display truth (`displayed`), or
/// `None` when nothing is confirmed (empty selection ⇒ blank terminal on first launch).
/// Picks the host's driver off its model ([`driver_for`]) and reads back its live attach
/// grid — the read counterpart to [`select_attach`]'s show. Shared by the draw hot path
/// and the ctl `dump` path so the two never drift.
///
/// [`driver_for`]: crate::driver::driver_for
pub(crate) fn current_grid(
    displayed: &Selection,
    ctx: &crate::driver::DriverCtx,
) -> Option<Arc<std::sync::Mutex<crate::display::grid::Grid>>> {
    let driver = ctx
        .hosts
        .get(&displayed.source)
        .map(crate::driver::driver_for);
    driver.and_then(|driver| driver.grid(displayed, ctx))
}

/// Spawns the lowered switch command off the event loop. Local variants run as a
/// plain subprocess; RawSsh variants run the full ssh argv non-interactively.
pub(crate) fn run_lowered(lowered: crate::machine::LoweredSwitch) {
    use crate::machine::LoweredSwitch;
    use crate::source::Runner;
    let argv = match lowered {
        LoweredSwitch::Local(v) | LoweredSwitch::RawSsh(v) => v,
    };
    if argv.is_empty() {
        return;
    }
    let (name, args) = (argv[0].clone(), argv[1..].to_vec());
    tokio::spawn(async move {
        // Log the exact spawned command + its result: a silent switch is invisible, so a
        // session-switch that does not land is diagnosed from the program's real output.
        tracing::debug!(cmd = %name, ?args, "lowered_run");
        match crate::source::ExecRunner.run(&name, &args).await {
            Ok(out) => tracing::debug!(cmd = %name, out_bytes = out.len(), "lowered_ok"),
            Err(e) => tracing::debug!(cmd = %name, error = %e, "lowered_err"),
        }
    });
}

/// Runs a mux's opaque [`crate::mux::SwitchPlan`] BLIND: the driver hands the whole plan
/// here and this lowers each variant through the host's transport, never naming the mux
/// type. `Exec` argv(s) run non-interactively in order; a `Shell` command runs over the
/// host's raw shell (`raw_ssh_argv`). Returns whether the switch was issued — `false` when
/// a `Shell` plan has no host shell (a local machine), so the caller falls back to a
/// reattach. The variant→lowering mapping is 1:1 with [`crate::machine::LoweredSwitch`].
pub(crate) fn run_switch_plan(host: &crate::model::Host, plan: crate::mux::SwitchPlan) -> bool {
    use crate::machine::LoweredSwitch;
    use crate::mux::SwitchPlan;
    match plan {
        SwitchPlan::Exec(argvs) => {
            for a in &argvs {
                let (cmd, args) = host.transport.exec_argv(false, a);
                let mut v = vec![cmd];
                v.extend(args);
                run_lowered(LoweredSwitch::Local(v));
            }
            true
        }
        SwitchPlan::Shell(cmd) => match host.transport.raw_ssh_argv(&cmd) {
            Some(argv) => {
                run_lowered(LoweredSwitch::RawSsh(argv));
                true
            }
            None => false,
        },
    }
}

/// Keeps a source's display terminal in sync with its sessions by delegating to the
/// host's driver, which owns the warm/reap decision (shared warms one host PTY on the
/// first session and reaps it when empty; per-session is selected on demand and only
/// reaps when empty). Called whenever a source's inventory updates (a remote `%`-event
/// refresh or a local poll), so a new session is reachable and a killed one is torn
/// down (#5).
fn sync_source_terminals(
    source: &str,
    sessions: &[crate::session::Session],
    ctx: &mut crate::driver::DriverCtx,
) {
    let Some(host) = ctx.hosts.get(source) else {
        return;
    };
    let mut driver = crate::driver::driver_for(host);
    driver.sync(source, sessions, ctx);
}

/// Connects the host the selection is on (if not already + detected), so its metadata
/// channel streams that host's tree in. The manager picks the channel (control client
/// vs poll task) from the host's `event_source`; an undetected host is skipped until a
/// detection probe resolves its mux.
fn ensure_current_host(
    mgr: &mut HostManager,
    hosts: &crate::model::Hosts,
    switcher: &crate::ui::switcher::Switcher,
    cols: u16,
    rows: u16,
    tree_width: u16,
) {
    let (cols, rows) = terminal_view_size(cols, rows, tree_width);
    if let Some(id) = switcher.current_host() {
        if let Some(host) = hosts.get(&id) {
            if host.detected {
                let _ = mgr.ensure(&id, host, cols, rows);
            }
        }
    }
}

/// Runs a host's mux-detection probe off the loop, cloning the host's transport + mux
/// (built by `Hosts::build`) so the probe reaches the same machine over the same axes
/// without re-deriving anything from a `Source`. The resolved mux (or `None` when the
/// probe fails) is emitted as `HostEvent::Scanned`.
fn spawn_host_detection(
    source: String,
    transport: Box<dyn crate::machine::Transport>,
    mux: Box<dyn crate::mux::Mux>,
    tx: tokio::sync::mpsc::UnboundedSender<HostEvent>,
) {
    tokio::spawn(async move {
        let mut host = crate::model::Host::new(transport, mux);
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
    hosts: &crate::model::Hosts,
    source: &str,
    cols: u16,
    rows: u16,
) {
    let Some(host) = hosts.get(source) else {
        return;
    };
    let _ = mgr.ensure(source, host, cols, rows);
}

fn scan_or_dispatch_host(
    mgr: &mut HostManager,
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
            spawn_host_detection(
                source.to_string(),
                host.transport.clone(),
                host.mux.clone_box(),
                mgr.events(),
            );
        }
        return;
    }
    dispatch_detected_host(mgr, hosts, source, cols, rows);
}

fn apply_scan_result(
    hosts: &mut crate::model::Hosts,
    source: &str,
    detected: Option<Box<dyn crate::mux::Mux>>,
) {
    let Some(host) = hosts.get_mut(source) else {
        return;
    };
    if let Some(mux) = detected {
        if mux.kind() != host.mux.kind() {
            host.mux = mux;
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
    hosts: &crate::model::Hosts,
    detecting: &mut HashSet<String>,
    mgr: &mut HostManager,
    panes_requested: &mut HashSet<String>,
    cols: u16,
    rows: u16,
) {
    if !switcher.take_rescan_kick() {
        return;
    }
    // `request_rescan` cleared every session's panes from `state.panes`; clear the
    // loop-local pane-request dedup in lockstep so each control host's re-`list_sessions`
    // reply actually re-issues `list-panes` (`request_session_panes` only asks for
    // addresses not already in this set — otherwise the subtree stays "loading…" until a
    // `%`-change or relaunch). A global clear is safe: this set gates only control hosts;
    // poll hosts re-emit their panes regardless.
    panes_requested.clear();
    for id in hosts.ids() {
        if let Some(host) = hosts.get(id) {
            if host.detected {
                mgr.rescan(id, host, cols, rows);
                continue;
            }
        }
        scan_or_dispatch_host(mgr, hosts, detecting, id, cols, rows);
    }
}

/// Starts each host's first scan at startup, so each host's tree streams in without
/// waiting for a selection move. Control hosts connect a `-CC` client; poll hosts start
/// their self-looping enumeration task — both owned by the manager. PTYs are attached
/// as each source's sessions arrive (see [`sync_source_terminals`]).
fn connect_all_sources(
    mgr: &mut HostManager,
    hosts: &crate::model::Hosts,
    detecting: &mut HashSet<String>,
    cols: u16,
    rows: u16,
    tree_width: u16,
) {
    let (cols, rows) = terminal_view_size(cols, rows, tree_width);
    for id in hosts.ids() {
        scan_or_dispatch_host(mgr, hosts, detecting, id, cols, rows);
    }
}

impl Runtime {
    /// Processes a batch of TREE-focus input bytes through ONE path — used for both real
    /// stdin and bytes replayed after a terminal→tree switch. Handles prefix arming
    /// (`C-g` then `q` → quit, `h`/`Ctrl+←` → shrink tree, `l`/`Ctrl+→` → grow tree),
    /// Enter → focus terminal (unless an inline input is open),
    /// ←/→ navigate the tree; then the off-loop op dispatch, ensure-current-host, and
    /// the `r` re-scan. Returns `(focus_terminal, quit, width_delta, toggle_auto_hide)`.
    /// The selection is committed at the loop top, so this only drives navigation +
    /// metadata, not the display. `width_changed` is the caller's out-flag.
    fn handle_tree_bytes(
        &mut self,
        bytes: &[u8],
        width_changed: &mut bool,
    ) -> (bool, bool, i32, bool) {
        // Split-borrow the world state into the loose names the body uses (a tree read
        // touches most of it: decoder, switcher/state, host orchestration, width prefs).
        let Self {
            tree_decoder,
            switcher,
            state,
            mgr,
            env,
            hosts,
            detecting,
            panes_requested,
            ops,
            op_tx,
            tree_width_natural,
            auto_hide_tree,
            cols,
            body_rows: rows,
            tree_width,
            mouse_state,
            prefix,
            ..
        } = self;
        let tree_armed = &mut mouse_state.tree_armed;
        let (prefix, cols, rows, tree_width) = (*prefix, *cols, *rows, *tree_width);
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
            // focus the terminal while a confirm is on screen; only y/n/Esc act on it.
            let is_inputting = state.is_modal_popup_open();
            match resolve_tree_key(key, tree_armed, prefix, is_inputting) {
                // A committed input/kill confirm folds through State::apply, which returns
                // its Commands; collect them and dispatch the whole batch below.
                Some(Action::TreeKey(k)) => key_cmds.extend(switcher.handle_key(k, state)),
                Some(Action::FocusTerminal) => focus_terminal = true,
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
            (&*ops, &*op_tx),
        );
        quit |= cmd_quit;
        if cmd_width_changed {
            *width_changed = true;
        }
        ensure_current_host(mgr, hosts, switcher, cols, rows, tree_width);
        kick_rescan(switcher, hosts, detecting, mgr, panes_requested, cols, rows);
        (focus_terminal, quit, width_delta, toggle_auto_hide)
    }
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
/// the PTY set (a new session attaches, a closed one is reaped). #5 tree view sync.
fn refetch_host(mgr: &HostManager, panes_requested: &mut HashSet<String>, host: &str) {
    if let Some(client) = mgr.get(host) {
        let prefix = format!("{host}/");
        panes_requested.retain(|a| !a.starts_with(&prefix));
        client.list_sessions();
    }
}

impl Runtime {
    /// Applies one [`HostEvent`]: [`State::apply_event`] folds the self-contained arms
    /// (Focus marker, Panes subtree, Sessions enumeration, Exited unreachable mark) into
    /// `State` and returns the mux follow-ups it cannot perform; this executes them (it
    /// holds the host clients, the registry, and the display worker the state layer must
    /// not reach). Drained in a burst by `on_host_event`. Returns `true` when the caller
    /// should rearm `attach_deadline` + mark dirty (the matched-client detach-reap path).
    fn handle_host_event(&mut self, ev: HostEvent) -> bool {
        let mut rearm = false;
        for effect in self
            .state
            .apply_event(ev, &mut self.switcher, &mut self.connected)
        {
            if self.run_event_effect(effect) {
                rearm = true;
            }
        }
        rearm
    }

    /// Carries out one [`EventEffect`](crate::model::EventEffect) `State::apply_event`
    /// returned — the mux I/O the state layer cannot perform (the single-owner inventory
    /// fold into `model::Host`, a control-mode probe, the attach registry, the detection
    /// dispatch). Returns `true` only for the matched-client display-attach reap, which
    /// asks the caller to rearm `attach_deadline` + mark `dirty` (the recover-from-detach
    /// path).
    fn run_event_effect(&mut self, effect: crate::model::EventEffect) -> bool {
        use crate::model::EventEffect;
        // Split-borrow the world state into the loose names the arms below use, so this
        // body stays the loop's imperative effect executor without a per-line `self.`.
        let Self {
            mgr,
            hosts,
            registry,
            switcher,
            state,
            panes_requested,
            detecting,
            worker,
            driver_pty_tx: pty_tx,
            attach_seq,
            cols,
            body_rows: rows,
            tree_width,
            ..
        } = self;
        let (cols, rows, tree_width) = (*cols, *rows, *tree_width);
        match effect {
            EventEffect::ApplyInventory { host, sessions } => {
                // The reader carried the parsed sessions on the event. Fold them into the
                // single owner (`model::Host.inventory`), apply them to the tree, request
                // each session's panes, and sync the display PTY(s). Pane subtrees arrive
                // separately as `HostEvent::Panes` (applied purely by `apply_event`).
                if let Some(h) = hosts.get_mut(&host) {
                    h.inventory.sessions = sessions.clone();
                }
                // Act on the tree/terminals ONLY while the host still has a live client.
                // Per-host FIFO delivers this inventory before the host's `Exited`/reap, so
                // `mgr.get` is normally `Some` here; the gate is the backstop that keeps a
                // broken ordering from reviving a reaped host in the tree
                // (`apply_source_result`) or resyncing its dead terminals. (`ApplyInventory`
                // is emitted only for control-mode hosts, so a poll host is never gated out.)
                if mgr.get(&host).is_some() {
                    switcher.apply_source_result(host.clone(), sessions.clone(), None, state);
                    if let Some(client) = mgr.get(&host) {
                        request_session_panes(client, &sessions, panes_requested);
                    }
                    let n = sessions.len();
                    let names: Vec<&str> = sessions.iter().map(|s| s.name.as_str()).collect();
                    tracing::info!(host, n, ?names, "sessions_applied");
                    // Sync this host's display terminal(s) (per-host for remote tmux).
                    let mut ctx = crate::driver::DriverCtx {
                        registry: &mut *registry,
                        hosts: &mut *hosts,
                        worker,
                        mgr,
                        pty_tx,
                        attach_seq: &mut *attach_seq,
                        cols,
                        body_rows: rows,
                        tree_width,
                    };
                    sync_source_terminals(&host, &sessions, &mut ctx);
                }
            }
            EventEffect::Refetch { host } => {
                // The server's session/window structure changed (a `%`-notification).
                // Refetch so the tree, panes, and PTY set resync (#5 tree view sync).
                refetch_host(mgr, panes_requested, &host);
            }
            EventEffect::ProbeActiveWindow { host, session_ref } => {
                // A session's ACTIVE WINDOW switched — the structure did NOT change, so do
                // NOT refetch the whole inventory: a full list-sessions + per-session
                // list-panes per change storms the single-threaded loop and freezes the UI
                // during rapid window navigation (each tree step issues select-window,
                // which echoes back as this notification). Probe ONLY the session the
                // notification names (its tmux id, `session_ref`) — never a guessed displayed
                // session; the reply (Focus) resolves the session name + new active window
                // and updates THAT session's marker without any refetch.
                if let Some(client) = mgr.get(&host) {
                    client.probe_active_window(&session_ref);
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
                let key = host_selection_key(h); // Shared ⇒ key == host id
                registry.remove(&key);
                if let Some(h) = hosts.get_mut(&host) {
                    h.display.clear(&key); // forget the shown session + any in-flight spawn
                    h.display_tty = crate::model::DisplayTty(None); // the dead client's tty is gone
                }
                return true; // rearm recovery
            }
            EventEffect::DispatchScanned { source, detected } => {
                // A detection probe resolved: (re)identify the mux, then dispatch the
                // now-detected host onto its metadata channel (control client or poll task).
                detecting.remove(&source);
                apply_scan_result(hosts, &source, detected);
                let (vc, vr) = terminal_view_size(cols, rows, tree_width);
                dispatch_detected_host(mgr, hosts, &source, vc, vr);
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
                        if !h.session_is_live(&s.name) {
                            // The host-keyed display attachment (one per-host PTY, reattached).
                            registry.remove(&host_selection_key(h));
                        }
                    }
                }
                let mut ctx = crate::driver::DriverCtx {
                    registry: &mut *registry,
                    hosts: &mut *hosts,
                    worker,
                    mgr,
                    pty_tx,
                    attach_seq: &mut *attach_seq,
                    cols,
                    body_rows: rows,
                    tree_width,
                };
                sync_source_terminals(&source, &sessions, &mut ctx);
            }
            EventEffect::RecordDisplayTty { host, tty } => {
                // The -CC `list-clients` probe resolved xmux's display-client tty. Record it
                // on the Host so a session switch is an in-place `switch-client -c <tty>`;
                // `None` (only the control client attached so far) clears any stale tty.
                if let Some(h) = hosts.get_mut(&host) {
                    if tty.is_some() {
                        tracing::debug!(host, ?tty, "display_tty_recorded");
                    }
                    h.record_display_tty(tty);
                }
            }
        }
        false
    }
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
            tracing::debug!(id, addr, tty, "tty_recorded");
            h.display_tty = crate::model::DisplayTty(Some(tty));
        }
    } else {
        // The marker fired but no registry entry has this id yet — diagnostic for a
        // capture that arrives before the attach is recorded (would silently drop).
        tracing::debug!(id, tty, "tty_record_missed_no_addr");
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

/// Applies ONE parsed SGR mouse event to the gesture state + tree/registry — the body
/// of the inline `while i < bytes.len()` mouse branch, lifted verbatim. Runs the modal/
/// gesture gates (menu, view border drag, popup drag, modal swallow, view border grab, idle
/// hover, menu open) in the SAME order, then the focus×position routing. Mutates `st`
/// (the gesture latches), `state.focus` (mid-loop focus toggles — routing re-reads focus
/// per event, so deferring would change behavior), and the byte-loop accumulators
/// (`non_mouse`, `mouse_focus_toggle`, `wheel_scrolled`). Returns whether a redraw is
/// needed for this event.
impl Runtime {
    fn handle_mouse_event(
        &mut self,
        ev: &crate::display::mouse::MouseEvent,
        selection: &Selection,
        non_mouse: &mut Vec<u8>,
        mouse_focus_toggle: &mut bool,
        wheel_scrolled: &mut bool,
        term_area: ratatui::layout::Rect,
    ) -> bool {
        // Split-borrow the world state into the loose names the (verbatim) gesture body uses.
        let Self {
            mouse_state: st,
            switcher,
            state,
            registry,
            mgr,
            env,
            hosts,
            detecting,
            panes_requested,
            tree_width_natural,
            cols,
            body_rows,
            tree_width,
            ..
        } = self;
        let (cols, body_rows, tree_width) = (*cols, *body_rows, *tree_width);
        let mut dirty = false;
        let in_mux = to_grid_local(term_area, ev.col, ev.row);
        // A LEFT-button press in the UNFOCUSED view switches focus to that
        // view — focus only; the click is not delivered. Right-click is
        // reserved for the tree context menu, so it never moves focus.
        // Within the focused terminal view, the click forwards.
        let is_press = ev.pressed && (ev.cb & 0x60) == 0;
        // Wheel events carry the 0x40 bit (cb 64=up, 65=down; +16=Ctrl).
        let is_wheel = ev.pressed && (ev.cb & 0x40) != 0;
        // View border drag: grab the view border rule (the column at the effective
        // tree width, only when the tree is shown) with the left button and
        // drag to resize. Once grabbed it owns every mouse event until the
        // button is released. Sets the NATURAL width; the loop-top reconcile
        // applies it and resizes the PTYs (same path as prefix h/l).
        let col0 = ev.col.saturating_sub(1); // 1-based SGR → 0-based screen col
                                             // A context menu owns every mouse event until the right
                                             // button is released (press-hold-release), exactly like the
                                             // view border drag below. Motion sets the hovered item; button-up
                                             // acts on it (or cancels if released off-menu).
        if state.menu_active() {
            if !ev.pressed {
                match switcher.menu_release(state) {
                    crate::ui::modal::MenuOutcome::FocusTerminal => {
                        // Connect the target's host (mirrors the left-click
                        // select path) so its control client streams, then
                        // focus the terminal on the now-selected session.
                        ensure_current_host(mgr, hosts, switcher, cols, body_rows, tree_width);
                        // Focus state is `Menu{prior}` here; set the restore view to the terminal
                        // so closing the menu (next loop-top sync_modal(None)) lands on it.
                        state.apply(crate::model::Action::Focus(
                            crate::model::FocusTarget::Terminal,
                        ));
                    }
                    crate::ui::modal::MenuOutcome::Handled => {
                        // A menu item only OPENS the next modal (input / kill confirm) — the
                        // actual mux op is committed later from that modal (Enter / y), which
                        // returns its RunOp through handle_key. Here just consume any re-scan
                        // (reconnect) kick and ensure the target's host is connected.
                        kick_rescan(
                            switcher,
                            hosts,
                            detecting,
                            mgr,
                            panes_requested,
                            cols,
                            body_rows,
                        );
                        ensure_current_host(mgr, hosts, switcher, cols, body_rows, tree_width);
                    }
                    crate::ui::modal::MenuOutcome::None => {}
                }
                dirty = true;
            } else if !is_wheel {
                switcher.menu_hover(col0, ev.row.saturating_sub(1), state);
                dirty = true;
            }
            return dirty;
        }
        if st.dragging_view_border {
            if !ev.pressed {
                // Button up ends the drag; persist the final width once
                // (motion resizes live but does not write per cell).
                st.dragging_view_border = false;
                crate::prefs::save_tree_width(&env.xmux_dir, *tree_width_natural);
            } else if !is_wheel {
                let target = view_border_drag_width(ev.col);
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
        // like the view border drag / menu hold above.
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
        // so clicks, wheels, view border grabs, and hovers never reach the
        // tree/terminal/view border behind it.
        if state.is_modal_popup_open() {
            return dirty;
        }
        if is_left_press && tree_width > 0 && col0 == tree_width {
            st.dragging_view_border = true; // grabbed the view border
            return dirty;
        }
        // Idle motion (motion bit set, no button held) — reported only
        // because any-motion tracking (1003h) is on. Over the view border it
        // lights the hover cue and is consumed (nothing under it to forward).
        // Elsewhere it falls through to the routing below, so a hover over the
        // terminal view IS forwarded to the child (the inner app gets hover); over
        // the tree it is harmlessly dropped.
        if ev.pressed && (ev.cb & 0x23) == 0x23 {
            let over_view_border = tree_width > 0 && col0 == tree_width;
            if over_view_border != st.hovered_view_border {
                st.hovered_view_border = over_view_border;
                dirty = true;
            }
            if over_view_border {
                return dirty;
            }
        }
        // Right-button press over a selectable tree row opens its context
        // menu (press-hold-release). Tree-focus only: the menu acts on a
        // tree row, so it is tree-view input, not a global — a right-click
        // while the terminal view is focused (or over the terminal view) does not open it
        // and does not move focus. The menu's keyboard actions (rename input,
        // kill confirm) thus always run in tree focus, so a confirmed kill
        // can't quit the app out from under the mux.
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
                // Plain wheel → scroll the selection LINEARLY through every row
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
            // The unfocused view was clicked → switch focus to it (no content
            // delivered); toggle flips Focus::Tree⇄Focus::Terminal either direction.
            ChainAction::FocusTerminal | ChainAction::FocusTree => {
                state.apply(crate::model::Action::FocusToggle);
                *mouse_focus_toggle = true;
            }
            ChainAction::SelectRow => {
                // Left-click a tree row → move the selection to it (select). The
                // loop top commits the new selection (attach); ensure the
                // clicked row's host connects so its subtree streams in.
                switcher.mouse_select(col0, ev.row.saturating_sub(1), state);
                ensure_current_host(mgr, hosts, switcher, cols, body_rows, tree_width);
                dirty = true;
            }
            ChainAction::ForwardToMux => {
                if let Some((gc, gr)) = in_mux {
                    registry.input(
                        &display_key(hosts, selection),
                        crate::display::mouse::encode_sgr_mouse(ev, gc, gr),
                    );
                }
            }
            ChainAction::Nothing => {}
        }
        dirty
    }
}

impl Runtime {
    /// The whole `stdin_rx` arm body, lifted. Scans the read for SGR mouse sequences
    /// (routed via [`Runtime::handle_mouse_event`]) vs a non-mouse byte stream, runs the
    /// lost-release watchdogs, the resize-repeat window, and the help-modal / tree-focus /
    /// terminal-view focus routing — in the SAME order as the inline arm. The final focus
    /// toggles (+ replay) run on `self.state.focus`, so the caller only acts on the returned
    /// `dirty`/`quit`. No behavior change.
    fn handle_stdin_bytes(&mut self, bytes: &[u8], selection: &Selection) -> StdinOutcome {
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
        let (vw, vh) = terminal_view_size(self.cols, self.body_rows, self.tree_width);
        let term_x = if self.tree_width == 0 {
            0
        } else {
            self.tree_width + 1
        };
        let term_area = ratatui::layout::Rect::new(term_x, 0, vw, vh);
        let mut non_mouse: Vec<u8> = Vec::with_capacity(bytes.len());
        let mut mouse_focus_toggle = false;
        let mut wheel_scrolled = false;
        {
            let mut i = 0;
            while i < bytes.len() {
                if let Some((ev, len)) = crate::display::mouse::parse_sgr_mouse(&bytes[i..]) {
                    if self.handle_mouse_event(
                        &ev,
                        selection,
                        &mut non_mouse,
                        &mut mouse_focus_toggle,
                        &mut wheel_scrolled,
                        term_area,
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
        // Watchdog: a view border drag is normally ended by the button-up event, but a
        // release can be lost (split across reads, released off-window, or a terminal
        // that omits it) — which would strand `dragging_view_border` and eat all later
        // mouse input. Any non-mouse byte (a keystroke, or the split release's own
        // leftover bytes) ends the drag and persists the final width, so the user is
        // never trapped past the next input.
        if self.mouse_state.dragging_view_border && !non_mouse.is_empty() {
            self.mouse_state.dragging_view_border = false;
            crate::prefs::save_tree_width(&self.env.xmux_dir, self.tree_width_natural);
        }
        // Watchdog: a keystroke (or any non-mouse byte) during a held menu ends
        // the gesture without acting — mirrors the view border-drag watchdog, so a
        // missed button-up can't strand the menu and eat later input.
        if self.state.menu_active() && !non_mouse.is_empty() {
            self.switcher.menu_cancel(&mut self.state);
            non_mouse.clear();
            *dirty = true;
        }
        // Watchdog: same recovery for a popup border-drag — a lost button-up
        // must not strand `popup_drag` and eat all later mouse input.
        if self.switcher.popup_drag_active() && !non_mouse.is_empty() {
            self.switcher.end_popup_drag();
            *dirty = true;
        }
        if mouse_focus_toggle {
            *dirty = true;
        }
        if wheel_scrolled {
            // The plain-wheel scroll moved the selection; connect the host it landed on
            // so its subtree streams in (mirrors handle_tree_bytes's ensure step).
            ensure_current_host(
                &mut self.mgr,
                &self.hosts,
                &self.switcher,
                self.cols,
                self.body_rows,
                self.tree_width,
            );
        }
        // Resize-repeat: while the window from a prefix-driven resize is open, a
        // bare Ctrl+←/→ (no prefix, in either focus) keeps resizing and refreshes
        // the window. Gated on NOT being mid-prefix (an armed prefix's next key is
        // a command, not a repeat — else skipping the input path would leave the
        // prefix armed and mis-read the following key). A pure-mouse read (empty
        // non_mouse) leaves the window untouched. Leading Ctrl-arrows are peeled off
        // (handles a coalesced autorepeat burst); any remaining bytes end the window
        // and fall through to the normal tree/terminal routing below.
        let mut consumed_by_repeat = false;
        if self
            .mouse_state
            .repeat_until
            .is_some_and(|d| std::time::Instant::now() < d)
            && !self.mouse_state.tree_armed
            && !self.term_input.is_armed()
            && !non_mouse.is_empty()
        {
            let mut n = 0;
            while let Some((d, len)) = leading_ctrl_arrow(&non_mouse[n..]) {
                if apply_width_delta(d, &mut self.tree_width_natural) {
                    *width_changed = true;
                }
                n += len;
            }
            if n > 0 {
                non_mouse.drain(0..n);
                *dirty = true;
                if non_mouse.is_empty() {
                    self.mouse_state.repeat_until =
                        Some(std::time::Instant::now() + Duration::from_millis(RESIZE_REPEAT_MS));
                    consumed_by_repeat = true;
                } else {
                    self.mouse_state.repeat_until = None; // trailing non-arrow bytes end + route below
                }
            } else {
                self.mouse_state.repeat_until = None; // first key isn't a Ctrl-arrow → end the window
            }
        }
        if !consumed_by_repeat
            && !non_mouse.is_empty()
            && self.switcher.feed_help_key(&non_mouse, &mut self.state)
        {
            // The help modal is modal (tmux view-mode style): while open it
            // captures every key in EITHER focus — q/Esc closes it, the rest are
            // swallowed — so nothing leaks to the tree or the terminal view. Above the
            // tree/terminal split so the behavior is identical regardless of focus.
            *dirty = true;
        } else if !consumed_by_repeat
            && (self.state.focus.is_tree_focused() || self.state.focus.is_modal())
        {
            // Tree view OR any modal: route to the switcher path. A modal popup (input /
            // kill-confirm) opened from EITHER view owns its keys here; the resolver gating
            // in handle_tree_bytes swallows everything but the modal's own keys, so a modal
            // never emits FocusTerminal/quit and the focus toggles below never fire mid-modal.
            let (ft, q, wd, th) = self.handle_tree_bytes(&non_mouse, width_changed);
            *focus_terminal = ft;
            *quit = q;
            if wd != 0 {
                // A prefix-driven resize: apply it and open the repeat window so the
                // next bare Ctrl+←/→ keeps resizing without re-pressing the prefix.
                if apply_width_delta(wd, &mut self.tree_width_natural) {
                    *width_changed = true;
                }
                self.mouse_state.repeat_until =
                    Some(std::time::Instant::now() + Duration::from_millis(RESIZE_REPEAT_MS));
            }
            if th {
                toggle_auto_hide(&mut self.auto_hide_tree, &self.env.xmux_dir);
                *dirty = true;
            }
        } else if !consumed_by_repeat {
            // TERMINAL focus: forward raw bytes to the selected session's PTY;
            // TermInput intercepts the prefix (→ tree / quit / help / resize / literal).
            for action in self.term_input.feed(&non_mouse) {
                match action {
                    // Forward keystrokes to the VISIBLE session (`displayed`), not the
                    // selection: until the new session is ready the prior one is on screen,
                    // so input must reach what the user actually sees (no blind typing).
                    Action::Forward(f) => self
                        .registry
                        .input(&display_key(&self.hosts, &self.state.displayed), f),
                    Action::FocusTree(rest) => {
                        *focus_tree = true;
                        *tree_replay = rest;
                    }
                    Action::Quit => *quit = true,
                    Action::ShowHelp => {
                        self.switcher.toggle_help(&mut self.state);
                        *dirty = true;
                    }
                    Action::Width(d) => {
                        // Same resize + repeat-window as the tree path, so a resize
                        // started from the terminal view chains with bare Ctrl+←/→ too.
                        if apply_width_delta(d, &mut self.tree_width_natural) {
                            *width_changed = true;
                        }
                        self.mouse_state.repeat_until = Some(
                            std::time::Instant::now() + Duration::from_millis(RESIZE_REPEAT_MS),
                        );
                    }
                    Action::ToggleAutoHide => {
                        toggle_auto_hide(&mut self.auto_hide_tree, &self.env.xmux_dir);
                        *dirty = true;
                    }
                    // The terminal-view focus resolver (TermInput) never emits these — they
                    // belong to the tree-focus path (resolve_tree).
                    Action::FocusTerminal | Action::TreeKey(_) => {}
                }
            }
        }
        if *focus_terminal {
            self.state.apply(crate::model::Action::Focus(
                crate::model::FocusTarget::Terminal,
            ));
            // No term.clear(): both states draw the SAME split layout (only the
            // view border colour changes), so clearing would blank the screen and
            // force a full repaint for nothing.
        }
        if *focus_tree {
            self.state
                .apply(crate::model::Action::Focus(crate::model::FocusTarget::Tree));
            if !tree_replay.is_empty() {
                let (ft, q, wd, th) = self.handle_tree_bytes(tree_replay, width_changed);
                if ft {
                    self.state.apply(crate::model::Action::Focus(
                        crate::model::FocusTarget::Terminal,
                    ));
                }
                *quit = *quit || q;
                if wd != 0 {
                    // A prefix-driven resize on the replayed bytes: apply + open the
                    // repeat window, same as the direct tree-focus path above.
                    if apply_width_delta(wd, &mut self.tree_width_natural) {
                        *width_changed = true;
                    }
                    self.mouse_state.repeat_until =
                        Some(std::time::Instant::now() + Duration::from_millis(RESIZE_REPEAT_MS));
                }
                if th {
                    toggle_auto_hide(&mut self.auto_hide_tree, &self.env.xmux_dir);
                    *dirty = true;
                }
            }
        }
        outcome
    }
}

/// The `xmux` (no subcommand) entry: the persistent app. Keeps one real attached
/// mux client per session alive and renders the selected one, with a control-mode
/// client per remote host for inventory/events/window-switch. It serves a picker
/// control socket so a headless driver can inject keys/text and dump the screen.
pub async fn run_app(env: Arc<Env>) -> i32 {
    use crate::display::term::TermGuard;
    use crate::ui::run::{serve_control, Cmd};
    use std::io::Read;
    use std::time::Duration;

    // The app owns the terminal and attaches mux clients as PTY children; nested
    // inside a mux every attach is refused. So running it inside a mux is refused.
    if let Err(e) = attach::nest_guard(attach::in_mux()) {
        eprintln!("xmux: {e}");
        eprintln!("xmux: the app must be your terminal entry, not run inside a mux.");
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

    // On a panic, restore the terminal (main thread only) and emit the detail to
    // both the structured log (tracing) and a raw append-only file (`panic.log`).
    // The restore is main-thread-only: worker threads (PTY pumps) catch+recover
    // their own panics (see Grid::feed); a stray worker panic must not tear the
    // screen down under a still-running app. TermGuard's Drop also restores on
    // the main-thread unwind — idempotent with this.
    {
        let log = env.xmux_dir.join("panic.log");
        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            // Emit to the structured log first: the non-blocking writer flushes on
            // WorkerGuard drop, which happens after main unwinds, so this record is
            // not lost even though the subscriber may not have flushed yet.
            tracing::error!("panic: {info}");
            // Append to the raw file as a fallback readable without a log viewer.
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
                // Only the main-thread crash reaches the default hook (stderr/backtrace)
                // — the terminal is restored above, so the print is safe and useful.
                prev_hook(info);
            }
            // A worker-thread panic (a PTY pump's vt100 edge case) is caught and
            // recovered by Grid::feed; it is already in the log + panic.log above. Do
            // NOT forward it to the default hook — its stderr print lands on the live
            // TUI's terminal and corrupts the screen (the panic-spam bug).
        }));
    }

    // Build the world state (Runtime) + the loop's I/O (the receivers `select!` polls).
    let (mut rt, mut io) = Runtime::new(env);
    // Kick each host's first scan here (NOT in `Runtime::new`), so a headless unit test
    // can build a `Runtime` without launching real detection probes / control clients.
    connect_all_sources(
        &mut rt.mgr,
        &rt.hosts,
        &mut rt.detecting,
        rt.cols,
        rt.body_rows,
        rt.tree_width,
    );
    // Take the worker's reply receiver out so the loop can `select!` on it while `&mut rt`
    // is borrowed for the arm body (the send half stays on `rt.worker`).
    let mut worker_events = rt.worker.take_events();

    // Single stdin reader thread: raw host bytes → channel (a loop-local receiver).
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

    // The ratatui terminal: loop-local I/O the draw/tick/dump methods borrow as a
    // param (kept off `Runtime` so a headless test never constructs one).
    let mut term =
        match ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(std::io::stdout())) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("xmux: {e}");
                return 1;
            }
        };
    if let Err(e) = term.clear() {
        tracing::warn!(error = %e, "term_clear_failed");
    }

    // The picker control socket: serves headless key/text/dump.
    let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::channel::<Cmd>(256);
    let control = pick_control_path(&rt.env);
    let _control_handle = control.and_then(|p| serve_control(p, cmd_tx));
    // Off the startup path, sweep `ctl-*.sock` markers left by crashed instances (a
    // clean exit removes its own on drop; a hard-kill does not) so discovery does not
    // over-count dead instances.
    {
        let dir = rt.env.xmux_dir.clone();
        tokio::spawn(async move { crate::control::prune_stale(&dir, std::process::id()).await });
    }

    let mut tick = tokio::time::interval(Duration::from_millis(SPINNER_FRAME_MS));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Periodic reconnect sweep: re-ensure any died remote control client (so #5
    // metadata sync self-heals) and re-attach the selected session's PTY if it
    // dropped. The sweep interval doubles as the retry backoff.
    let reconnect_start = tokio::time::Instant::now() + Duration::from_millis(RECONNECT_MS);
    let mut reconnect =
        tokio::time::interval_at(reconnect_start, Duration::from_millis(RECONNECT_MS));
    reconnect.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Frame timer: wakes the loop at the redraw cadence so a pending `dirty` draw is
    // flushed promptly even when no other event arrives.
    let mut frame = tokio::time::interval(Duration::from_millis(FRAME_MS));
    frame.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        rt.prepare_and_draw(&mut term);

        // NOT biased: a biased select polls host_rx first every iteration, so a
        // sustained output flood would starve stdin, the control socket, ops,
        // enumeration, and the tick. Unbiased select gives every branch a fair share.
        //
        // Every arm EXCEPT the bare frame timer represents a real state change, so it
        // marks the UI dirty (drawn on the next gated pass); the frame timer only wakes
        // the loop to flush an already-pending dirty draw, so it must NOT set dirty.
        let mut from_frame = false;
        tokio::select! {
            Some(ev) = io.host_rx.recv() => rt.on_host_event(ev, &mut io.host_rx),
            Some(ev) = io.pty_rx.recv() => rt.on_pty_event(ev, &mut io.pty_rx),
            Some(ev) = worker_events.recv() => rt.on_display_event(ev),
            Some(bytes) = stdin_rx.recv() => {
                if rt.on_stdin(&bytes) {
                    break;
                }
            }
            Some(cmd) = cmd_rx.recv() => {
                if rt.on_ctl_command(cmd, &mut term) {
                    break;
                }
            }
            Some(result) = io.op_rx.recv() => rt.on_op_result(result),
            _ = tick.tick() => rt.on_tick(&mut term),
            _ = reconnect.tick() => rt.on_reconnect(),
            _ = frame.tick() => {
                from_frame = true;
            }
        }
        // Any real event (not the bare frame wake) means the UI may have changed.
        if !from_frame {
            rt.dirty = true;
        }
    }

    // A resize within the last WIDTH_FLUSH_MS before quit leaves the debounce deadline
    // unreached, so the final width is still pending — persist it on the way out so the
    // tree width the user left with survives the next launch.
    if rt.width_dirty {
        crate::prefs::save_tree_width(&rt.env.xmux_dir, rt.tree_width_natural);
    }
    rt.registry.teardown_all();
    rt.mgr.teardown_all();
    0
}

/// The persistent app's WORLD STATE: everything the `select!` loop mutates across
/// iterations. The `select!` receivers/timers and the ratatui `Terminal` stay
/// loop-local in [`run_app`] — a receiver cannot be polled from `self.<rx>.recv()`
/// while an arm body borrows `&mut self` — so `Runtime` owns the long-lived state and
/// each `select!` arm is one `&mut self` method.
struct Runtime {
    env: Arc<Env>,
    ops: Arc<dyn crate::ui::switcher::Ops>,
    hosts: crate::model::Hosts,
    mgr: HostManager,
    registry: AttachRegistry,
    /// The off-loop attach worker. Its reply receiver is taken out in `run_app`
    /// ([`DisplayWorker::take_events`]); this keeps only the send half (`ensure`).
    worker: DisplayWorker,
    switcher: crate::ui::switcher::Switcher,
    state: crate::state::State,
    attach_seq: u64,
    /// A clone of the loop's `PtyEvent` sender handed to drivers for off-loop probes.
    driver_pty_tx: tokio::sync::mpsc::UnboundedSender<PtyEvent>,
    op_tx: tokio::sync::mpsc::UnboundedSender<crate::ui::switcher::OpResult>,
    cols: u16,
    body_rows: u16,
    /// The EFFECTIVE tree width (0 = tree hidden, terminal full width).
    tree_width: u16,
    /// The tree's natural width (what prefix h/l adjusts; restored when shown again).
    tree_width_natural: u16,
    auto_hide_tree: bool,
    mouse_state: MouseState,
    term_input: crate::display::input::TermInput,
    tree_decoder: crate::display::decode::KeyDecoder,
    prefix: u8,
    connected: HashSet<String>,
    panes_requested: HashSet<String>,
    detecting: HashSet<String>,
    draw_observer: DrawObserver,
    spinner_start: std::time::Instant,
    dirty: bool,
    last_draw: std::time::Instant,
    width_dirty: bool,
    width_flush_at: Option<std::time::Instant>,
}

/// The loop's receiver halves, whose send halves `Runtime::new` wired into the world
/// state (mgr's host events, the worker's PTY events, the op-result channel). Held
/// loop-local in [`run_app`] so an arm can `select!` on one while its body borrows
/// `&mut Runtime`.
struct LoopIo {
    host_rx: tokio::sync::mpsc::UnboundedReceiver<HostEvent>,
    pty_rx: tokio::sync::mpsc::UnboundedReceiver<PtyEvent>,
    op_rx: tokio::sync::mpsc::UnboundedReceiver<crate::ui::switcher::OpResult>,
}

impl Runtime {
    /// Builds the world state from `env` and returns it alongside the loop's receiver
    /// halves ([`LoopIo`]). Pure construction — it starts NO probes (the startup scan
    /// is kicked from `run_app`), so a headless unit test can build a `Runtime`.
    fn new(env: Arc<Env>) -> (Runtime, LoopIo) {
        let size = ratatui::crossterm::terminal::size().unwrap_or((80, 24));
        let (cols, body_rows) = (size.0, size.1.saturating_sub(1)); // status bar = last row
                                                                    // Restore the natural tree width the user last set; clamp a stale out-of-range
                                                                    // value, fall back to the default when none is saved.
        let tree_width_natural = adjust_tree_width(
            crate::prefs::load_tree_width(&env.xmux_dir).unwrap_or(crate::ui::switcher::TREE_WIDTH),
            0,
        );
        let tree_width = tree_width_natural;
        let auto_hide_tree = crate::prefs::load_auto_hide_tree(&env.xmux_dir)
            .unwrap_or_else(|| env.cfg.ui_auto_hide_tree());

        // The control-mode metadata clients: one per remote host.
        let (host_tx, host_rx) = tokio::sync::mpsc::unbounded_channel::<HostEvent>();
        let mgr = HostManager::new(host_tx);
        // The live PTY attachments: one real attached mux client per session.
        let (pty_tx, pty_rx) = tokio::sync::mpsc::unbounded_channel::<PtyEvent>();
        let driver_pty_tx = pty_tx.clone();
        let worker = DisplayWorker::new(pty_tx);
        let registry = AttachRegistry::new();

        // Host model: the single runtime registry, keyed by id (local first, then each
        // ssh alias in config order), built from the config-assembly products on `Env`.
        let host_os = std::env::consts::OS;
        let hosts = crate::model::Hosts::build(
            &env.cfg,
            &env.ssh_aliases,
            host_os,
            &env.xmux_dir,
            env.local_socket.clone(),
        );

        // The app's runtime state (single source of truth), seeded from the host ids;
        // events stream the tree in.
        let mut state = crate::state::State::from_sources(hosts.ids().to_vec());
        let switcher = crate::ui::switcher::Switcher::from_sources(&mut state);
        // Feed the switcher the ssh config so an unreachable host's info panel can show
        // its Host/Match stanza. Read once; a missing file just yields no stanza.
        state.chrome.set_ssh_config_text(
            std::fs::read_to_string(crate::env::ssh_config_path()).unwrap_or_default(),
        );
        // View border colours from config's tmux-style pane-border options.
        state
            .chrome
            .set_view_border_colors(crate::ui::switcher::ViewBorderColors {
                active: crate::ui::chrome::map_color(&env.cfg.ui.view_active_border_style),
                inactive: crate::ui::chrome::map_color(&env.cfg.ui.view_border_style),
                hover: crate::ui::chrome::map_color(&env.cfg.ui.view_border_hover_style),
            });
        // The help modal must show the prefix the user configured, not a literal.
        state.chrome.set_ui_prefix(env.ui_prefix.clone());

        // The live mutate ops (create/rename/kill) — NOT tree probing.
        let ops = env.ops();
        let prefix = crate::display::term::parse_prefix(Some(&env.ui_prefix));
        let term_input = crate::display::input::TermInput::new(prefix);
        let tree_decoder = crate::display::decode::KeyDecoder::new();
        let (op_tx, op_rx) = tokio::sync::mpsc::unbounded_channel();

        let rt = Runtime {
            env,
            ops,
            hosts,
            mgr,
            registry,
            worker,
            switcher,
            state,
            // Off-loop attach sequence. The in-flight set / reaped-ids / which session
            // each display shows live on each `host.display` (HostDisplay).
            attach_seq: 0,
            driver_pty_tx,
            op_tx,
            cols,
            body_rows,
            tree_width,
            tree_width_natural,
            auto_hide_tree,
            mouse_state: MouseState::default(),
            term_input,
            tree_decoder,
            prefix,
            connected: HashSet::new(),
            panes_requested: HashSet::new(),
            detecting: HashSet::new(),
            // The draw hot path's observability (per-key grid fingerprints + slow-step
            // probe), owned off the draw block so it does nothing but lock → render.
            draw_observer: DrawObserver::default(),
            spinner_start: std::time::Instant::now(),
            dirty: true,
            last_draw: std::time::Instant::now() - std::time::Duration::from_millis(FRAME_MS),
            width_dirty: false,
            width_flush_at: None,
        };
        (
            rt,
            LoopIo {
                host_rx,
                pty_rx,
                op_rx,
            },
        )
    }

    /// The loop top: advance the spinner, reconcile the modal/tree-width, run the `r`
    /// reattach-kick, follow the active window in terminal focus, sync the selection,
    /// drive one debounce beat (the settled attach), flush the debounced width persist,
    /// then draw the gated frame. `term` is the loop-local ratatui terminal.
    fn prepare_and_draw(&mut self, term: &mut Term) {
        use std::time::Duration;
        // Advance the spinner from wall-clock so it animates regardless of which arm fired.
        self.state
            .chrome
            .set_spinner_frame(spinner_frame_at(self.spinner_start.elapsed()));
        self.state
            .chrome
            .set_view_border_hovered(self.mouse_state.hovered_view_border);
        // Derive the modal dimension of focus from the open-modal kind (single owner of
        // the modal/view reconciliation).
        let modal_kind = self.state.modal_kind();
        self.state.focus.sync_modal(modal_kind);
        // The single owner of the effective tree width: reconcile it to the focus + the
        // hide setting + any natural-width change. On a change, resize the PTYs so the
        // mux reflows, and mark dirty.
        let want_tree_width = reconciled_tree_width(
            self.state.focus.is_terminal_focused(),
            self.auto_hide_tree,
            self.tree_width_natural,
        );
        if want_tree_width != self.tree_width {
            // Crossing the hidden sentinel (0) flips the column TOPOLOGY; a stale wide-char
            // cell at the new boundary can survive ratatui's diff, so force a full repaint.
            let crossed_hidden = (want_tree_width == 0) != (self.tree_width == 0);
            self.tree_width = want_tree_width;
            let (vc, vr) = terminal_view_size(self.cols, self.body_rows, self.tree_width);
            self.registry.resize_all(vc, vr);
            self.mgr.resize_all(vc, vr);
            if crossed_hidden {
                if let Err(e) = term.clear() {
                    tracing::warn!(error = %e, "term_clear_failed");
                }
            }
            self.dirty = true;
        }
        // A portable-pty child spawn clears ENABLE_MOUSE_INPUT on the parent CONIN,
        // killing mouse capture; re-assert it whenever it drifts off.
        crate::display::term::ensure_mouse_capture();
        // An `r` re-scan also re-attaches the CURRENT display: tear the (possibly dead)
        // attachment down and clear its latch so the attach below re-creates a fresh
        // client for the viewed session.
        if self.switcher.take_reattach_kick() && !self.state.selection.is_empty() {
            let key = display_key(&self.hosts, &self.state.selection);
            self.registry.remove(&key);
            if let Some(h) = self.hosts.get_mut(&self.state.selection.source) {
                h.display.clear(&key); // drop the prior latch so the re-attach is fresh
            }
            self.state.apply(crate::model::Action::ClearDisplay); // nothing confirmed → blank view
            self.state.apply(crate::model::Action::RearmAttachNow {
                now: std::time::Instant::now(),
            });
        }
        // In terminal focus the tree selection tracks the displayed session's active
        // window (idempotent, so calling it each iteration is cheap).
        if self.state.focus.is_terminal_focused() {
            self.switcher.select_active_window(&mut self.state);
        }
        if sync_selection_from_switcher(&mut self.state, &self.switcher) {
            // The selection moved → the tree needs a redraw. The attach is NOT issued
            // here; the Tick below arms the debounce, re-armed on every move.
            self.dirty = true;
        }
        // Drive one debounce beat. The clock + the registry/host attach facts enter as
        // DATA on the Tick; State::apply owns the arm/fire decision.
        {
            let (key_live, in_flight) =
                selection_attach_facts(&self.registry, &self.hosts, &self.state.selection);
            let cmds = self.state.apply(crate::model::Action::Tick {
                now: std::time::Instant::now(),
                key_live,
                in_flight,
            });
            for cmd in cmds {
                match cmd {
                    crate::model::Command::PersistLastSession(addr) => {
                        crate::prefs::save_last_session(&self.env.xmux_dir, &addr);
                    }
                    crate::model::Command::Attach(sel) => {
                        let t = std::time::Instant::now();
                        // select_attach picks the host's driver and hands it the intent.
                        let shown = select_attach(
                            &sel,
                            &mut crate::driver::DriverCtx {
                                registry: &mut self.registry,
                                hosts: &mut self.hosts,
                                worker: &self.worker,
                                mgr: &self.mgr,
                                pty_tx: &self.driver_pty_tx,
                                attach_seq: &mut self.attach_seq,
                                cols: self.cols,
                                body_rows: self.body_rows,
                                tree_width: self.tree_width,
                            },
                        );
                        if shown {
                            // Advance the display truth synchronously ONLY for a confirmed
                            // in-place path: a live grid for the key exists AND no reattach
                            // is in flight. A pending reattach KEEPS the prior session's grid
                            // (stale-while-revalidate) until DisplayReady swaps it in.
                            let k = display_key(&self.hosts, &sel);
                            let reattach_pending = self
                                .hosts
                                .get(&sel.source)
                                .is_some_and(|h| h.display.in_flight_contains(&k));
                            if self.registry.contains(&k) && !reattach_pending {
                                self.state
                                    .apply(crate::model::Action::ConfirmDisplay(sel.clone()));
                            }
                        }
                        DrawObserver::slow_step("select_attach", t);
                        self.dirty = true;
                        let key = display_key(&self.hosts, &sel);
                        let session = &sel.session;
                        tracing::debug!(key, session, "selection");
                    }
                    // The settled-selection Tick never returns the synchronous key/ctl-only
                    // commands or a session-lifecycle RunOp.
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
        if self.width_dirty
            && self
                .width_flush_at
                .is_some_and(|d| std::time::Instant::now() >= d)
        {
            crate::prefs::save_tree_width(&self.env.xmux_dir, self.tree_width_natural);
            self.width_dirty = false;
            self.width_flush_at = None;
        }

        // Draw the split (tree + selected session's live grid). GATED — redraw only when
        // something changed AND at most once per frame, so rapid navigation / a busy PTY
        // cannot flood the terminal.
        if self.dirty && self.last_draw.elapsed() >= Duration::from_millis(FRAME_MS) {
            // Render the CONFIRMED display truth (`displayed`), not the selection: the prior
            // session stays on screen until the new one is ready (stale-while-revalidate).
            let grid_arc = current_grid(
                &self.state.displayed,
                &crate::driver::DriverCtx {
                    registry: &mut self.registry,
                    hosts: &mut self.hosts,
                    worker: &self.worker,
                    mgr: &self.mgr,
                    pty_tx: &self.driver_pty_tx,
                    attach_seq: &mut self.attach_seq,
                    cols: self.cols,
                    body_rows: self.body_rows,
                    tree_width: self.tree_width,
                },
            );
            let terminal_focused = self.state.focus.is_terminal_focused();
            // The view border glyph reflects auto-hide-tree mode (║ on, │ off).
            self.state.chrome.set_auto_hide(self.auto_hide_tree);
            let t_draw = std::time::Instant::now();
            if let Err(e) = match &grid_arc {
                Some(g) => {
                    let t_lock = std::time::Instant::now();
                    let guard = g.lock().ok();
                    DrawObserver::slow_step("grid_lock", t_lock);
                    // Compute the grid fingerprint under the same lock used for rendering;
                    // the observer emits display_grid_changed only on a real content change.
                    if let Some(grid) = guard.as_deref() {
                        let addr = display_key(&self.hosts, &self.state.displayed);
                        let session = &self.state.displayed.session;
                        let fp = grid.fingerprint();
                        match self.draw_observer.observe(&addr, session, fp) {
                            FpOutcome::Unchanged => {}
                            FpOutcome::Steady => {
                                tracing::trace!(addr = %addr, session = %session, fp, "display_grid_changed");
                            }
                            FpOutcome::Switched => {
                                tracing::info!(addr = %addr, session = %session, fp, "display_grid_changed");
                            }
                        }
                    }
                    // Split-borrow so the draw closure captures only these fields, not all
                    // of `self` (the fingerprint block's borrows have ended above).
                    let switcher = &mut self.switcher;
                    let state = &self.state;
                    let tree_width = self.tree_width;
                    term.draw(|f| {
                        let t_render = std::time::Instant::now();
                        switcher.render(f, guard.as_deref(), terminal_focused, tree_width, state);
                        DrawObserver::slow_step("render", t_render);
                    })
                }
                None => {
                    let switcher = &mut self.switcher;
                    let state = &self.state;
                    let tree_width = self.tree_width;
                    term.draw(|f| {
                        let t_render = std::time::Instant::now();
                        switcher.render(f, None, terminal_focused, tree_width, state);
                        DrawObserver::slow_step("render", t_render);
                    })
                }
            } {
                tracing::warn!(error = %e, "term_draw_failed");
            }
            DrawObserver::slow_step("draw", t_draw);
            // The grids are now on screen — clear every attachment's output-coalescing flag.
            self.registry.clear_all_pending();
            self.dirty = false;
            self.last_draw = std::time::Instant::now();
        }
    }

    /// The `host_rx` arm: apply one host event, then drain a burst (bounded) so a `%`-event
    /// flood coalesces into one redraw. Re-arms the attach debounce on the detach-reap path.
    fn on_host_event(
        &mut self,
        ev: HostEvent,
        host_rx: &mut tokio::sync::mpsc::UnboundedReceiver<HostEvent>,
    ) {
        let t = std::time::Instant::now();
        if self.handle_host_event(ev) {
            self.state.apply(crate::model::Action::RearmAttach {
                now: std::time::Instant::now(),
            });
            self.dirty = true;
        }
        let mut budget = EVENT_DRAIN_BUDGET;
        while budget > 0 {
            match host_rx.try_recv() {
                Ok(ev) => {
                    if self.handle_host_event(ev) {
                        self.state.apply(crate::model::Action::RearmAttach {
                            now: std::time::Instant::now(),
                        });
                        self.dirty = true;
                    }
                    budget -= 1;
                }
                Err(_) => break,
            }
        }
        DrawObserver::slow_step("host_drain", t);
    }

    /// The `pty_rx` arm: a kept attachment fed its grid or hit EOF (reap). Detach-to-recover
    /// re-attaches the VIEWED session if its client exits; a background session is just reaped.
    fn on_pty_event(
        &mut self,
        ev: PtyEvent,
        pty_rx: &mut tokio::sync::mpsc::UnboundedReceiver<PtyEvent>,
    ) {
        // Capture the viewed attach id BEFORE any reap removes it; a background session
        // dropping (tree focus, or a non-displayed attach) is just reaped.
        let displayed_attach_id = (self.state.focus.is_terminal_focused()
            && !self.state.selection.is_empty())
        .then(|| {
            self.registry
                .get(&display_key(&self.hosts, &self.state.selection))
                .map(|a| a.id())
        })
        .flatten();
        let mut detached = false;
        match ev {
            PtyEvent::Exited { id } => {
                if Some(id) == displayed_attach_id {
                    detached = true;
                }
                clear_display_tty_for_attach(&mut self.hosts, &self.registry, id);
                if !self.registry.reap(id) {
                    // pre-Ready Exited: registry has no id yet. Attribute to the owning host
                    // via pending so its Ready tears down instead of inserting a dead pane.
                    self.hosts
                        .iter_mut()
                        .any(|h| h.display.mark_reaped_if_pending(id));
                }
            }
            PtyEvent::DisplayTty { id, tty } => {
                record_display_tty(&mut self.hosts, &self.registry, id, tty)
            }
            PtyEvent::Output { .. } => {}
        }
        let mut budget = EVENT_DRAIN_BUDGET;
        while budget > 0 {
            match pty_rx.try_recv() {
                Ok(PtyEvent::Exited { id }) => {
                    if Some(id) == displayed_attach_id {
                        detached = true;
                    }
                    clear_display_tty_for_attach(&mut self.hosts, &self.registry, id);
                    if !self.registry.reap(id) {
                        self.hosts
                            .iter_mut()
                            .any(|h| h.display.mark_reaped_if_pending(id));
                    }
                    budget -= 1;
                }
                Ok(PtyEvent::Output { .. }) => {
                    budget -= 1;
                }
                Ok(PtyEvent::DisplayTty { id, tty }) => {
                    record_display_tty(&mut self.hosts, &self.registry, id, tty);
                    budget -= 1;
                }
                Err(_) => break,
            }
        }
        if detached {
            // The viewed session's client detached/exited — recover by re-attaching it
            // (reaped above, so the loop-top attach re-fires once its PTY is gone).
            self.state.apply(crate::model::Action::RearmAttach {
                now: std::time::Instant::now(),
            });
            self.dirty = true;
        }
    }

    /// The worker `Ready`/`Failed` arm. `HostDisplay` owns the reap/install/stale DECISION;
    /// the loop performs the registry install/teardown it alone can.
    fn on_display_event(&mut self, ev: DisplayEvent) {
        match ev {
            DisplayEvent::Ready {
                seq,
                key,
                attachment,
            } => {
                let hid = host_of_key(&key).to_string();
                let id = attachment.id();
                let outcome = match self.hosts.get_mut(&hid) {
                    Some(h) => {
                        tracing::info!(key, seq, id, "attach_ready");
                        Some(h.display.resolve_ready(&key, seq, id))
                    }
                    None => None,
                };
                match outcome {
                    Some(crate::model::ReadyOutcome::Install { shown }) => {
                        // Swap: tear down the stale attachment held under this key (the prior
                        // session, kept on screen until now) and install the fresh one.
                        self.registry.remove(&key);
                        self.registry.insert(&key, attachment);
                        self.state
                            .apply(crate::model::Action::ConfirmDisplay(Selection {
                                source: hid.clone(),
                                session: shown,
                                window: None,
                            }));
                    }
                    // Reaped-race, stale seq, or unknown host: tear the fresh attachment down
                    // (resolve_ready already cleared the bookkeeping for the first two).
                    Some(_) | None => attachment.teardown(),
                }
            }
            DisplayEvent::Failed { seq, key, message } => {
                let hid = host_of_key(&key).to_string();
                if let Some(h) = self.hosts.get_mut(&hid) {
                    h.display.resolve_failed(&key, seq);
                }
                tracing::warn!(key, error = %message, "attach_failed");
            }
        }
    }

    /// The `stdin_rx` arm: route a raw read (mouse/keys) through the input core. Returns
    /// whether the app should quit.
    fn on_stdin(&mut self, bytes: &[u8]) -> bool {
        use std::time::Duration;
        // Clone the selection so &mut state can be threaded alongside it (the ForwardToMux
        // path reads the selection for display_key/registry input).
        let selection = self.state.selection.clone();
        let outcome = self.handle_stdin_bytes(bytes, &selection);
        if outcome.dirty {
            self.dirty = true;
        }
        if outcome.width_changed {
            self.width_dirty = true;
            self.width_flush_at =
                Some(std::time::Instant::now() + Duration::from_millis(WIDTH_FLUSH_MS));
        }
        outcome.quit
    }

    /// The control-socket arm: headless op/status/dump/key/bytes. Returns whether to quit.
    fn on_ctl_command(&mut self, cmd: crate::ui::run::Cmd, term: &mut Term) -> bool {
        use crate::ui::run::{dump_screen, Cmd};
        use std::time::Duration;
        match cmd {
            Cmd::Op(action) => {
                // dispatch_action spawns any RunOp off-loop itself; its OpResult folds back
                // through op_tx as usual.
                let (quit_op, wc) = dispatch_action(
                    action,
                    &mut self.switcher,
                    &mut self.state,
                    &mut self.tree_width_natural,
                    &mut self.auto_hide_tree,
                    &self.env.xmux_dir,
                    (&self.ops, &self.op_tx),
                );
                if wc {
                    self.width_dirty = true;
                    self.width_flush_at =
                        Some(std::time::Instant::now() + Duration::from_millis(WIDTH_FLUSH_MS));
                }
                if quit_op {
                    return true;
                }
                // A Switch/Focus may need the selection's host connected.
                ensure_current_host(
                    &mut self.mgr,
                    &self.hosts,
                    &self.switcher,
                    self.cols,
                    self.body_rows,
                    self.tree_width,
                );
                if sync_selection_from_switcher(&mut self.state, &self.switcher) {
                    self.dirty = true;
                }
            }
            Cmd::Status(reply) => {
                let _ = reply.send(status_line(
                    &self.switcher,
                    self.state.focus.view_is_tree(),
                    &self_cwd(),
                    &self_tty(),
                ));
            }
            Cmd::Dump(reply) => {
                let sz = term.size().unwrap_or(ratatui::layout::Size {
                    width: 80,
                    height: 24,
                });
                let grid_arc = current_grid(
                    &self.state.displayed,
                    &crate::driver::DriverCtx {
                        registry: &mut self.registry,
                        hosts: &mut self.hosts,
                        worker: &self.worker,
                        mgr: &self.mgr,
                        pty_tx: &self.driver_pty_tx,
                        attach_seq: &mut self.attach_seq,
                        cols: self.cols,
                        body_rows: self.body_rows,
                        tree_width: self.tree_width,
                    },
                );
                let dump = match &grid_arc {
                    Some(g) => {
                        let guard = g.lock().ok();
                        dump_screen(
                            &mut self.switcher,
                            guard.as_deref(),
                            sz.width,
                            sz.height,
                            &self.state,
                        )
                    }
                    None => dump_screen(&mut self.switcher, None, sz.width, sz.height, &self.state),
                };
                let _ = reply.send(dump);
            }
            Cmd::RawKey(k) => {
                // Route the FULL command batch through the single dispatcher (RunOp spawns
                // off-loop, its OpResult folding back through op_tx).
                let cmds = self.switcher.handle_key(k, &mut self.state);
                let (quit_key, wc) = dispatch_commands(
                    cmds,
                    &mut self.switcher,
                    &mut self.state,
                    &mut self.tree_width_natural,
                    &mut self.auto_hide_tree,
                    &self.env.xmux_dir,
                    (&self.ops, &self.op_tx),
                );
                if wc {
                    self.width_dirty = true;
                    self.width_flush_at =
                        Some(std::time::Instant::now() + Duration::from_millis(WIDTH_FLUSH_MS));
                }
                if quit_key {
                    return true;
                }
                ensure_current_host(
                    &mut self.mgr,
                    &self.hosts,
                    &self.switcher,
                    self.cols,
                    self.body_rows,
                    self.tree_width,
                );
                if sync_selection_from_switcher(&mut self.state, &self.switcher) {
                    self.dirty = true;
                }
            }
            Cmd::RawBytes(bytes) => {
                if !bytes.is_empty() {
                    // Inject into the VISIBLE session (`displayed`), matching the interactive
                    // keystroke path.
                    if let Some(host) = self.hosts.get(&self.state.displayed.source) {
                        let mut driver = crate::driver::driver_for(host);
                        let ctx = crate::driver::DriverCtx {
                            registry: &mut self.registry,
                            hosts: &mut self.hosts,
                            worker: &self.worker,
                            mgr: &self.mgr,
                            pty_tx: &self.driver_pty_tx,
                            attach_seq: &mut self.attach_seq,
                            cols: self.cols,
                            body_rows: self.body_rows,
                            tree_width: self.tree_width,
                        };
                        driver.input(&self.state.displayed, bytes, &ctx);
                    }
                }
            }
        }
        false
    }

    /// The op-result arm: fold a finished mutate op back into the tree/state.
    fn on_op_result(&mut self, result: crate::ui::switcher::OpResult) {
        self.switcher.apply_op_result(result, &mut self.state);
    }

    /// The animation-tick arm: detect a console resize (push the new size to PTYs +
    /// control clients, force a full repaint) and refresh the connecting-spinner set.
    fn on_tick(&mut self, term: &mut Term) {
        // Resize detection: poll the console size (an ioctl, not a stdin read).
        if let Ok((c, r)) = ratatui::crossterm::terminal::size() {
            if (c, r) != (self.cols, self.body_rows + 1) {
                let body = r.saturating_sub(1);
                self.cols = c;
                self.body_rows = body;
                let (vc, vr) = terminal_view_size(c, body, self.tree_width);
                self.registry.resize_all(vc, vr);
                self.mgr.resize_all(vc, vr);
                let _ = term.autoresize();
                // A console resize reflows the existing cells; force a full repaint.
                if let Err(e) = term.clear() {
                    tracing::warn!(error = %e, "term_clear_failed");
                }
                self.dirty = true;
            }
        }
        // Spinner set = the selected session if its PTY is still connecting.
        let mut sp = HashSet::new();
        if !self.state.selection.is_empty() {
            let key = display_key(&self.hosts, &self.state.selection);
            let in_flight_for_key = self
                .hosts
                .get(&self.state.selection.source)
                .map(|h| h.display.in_flight_contains(&key))
                .unwrap_or(false);
            if in_flight_for_key || self.registry.connecting(&key) {
                sp.insert(self.state.selection.address());
            }
        }
        self.state.chrome.set_spinner(sp);
    }

    /// The reconnect-sweep arm: re-ensure died metadata channels, re-detect undetected
    /// hosts, re-warm dropped control-host PTYs, capture display ttys, and re-attach the
    /// selected session if its display terminal dropped. The sole automatic retry path.
    fn on_reconnect(&mut self) {
        let (vc, vr) = terminal_view_size(self.cols, self.body_rows, self.tree_width);
        // Snapshot the ids so the loops can re-borrow `hosts` (incl. &mut) without holding
        // the `ids()` borrow across the body.
        let ids: Vec<String> = self.hosts.ids().to_vec();
        // Self-heal sweep: a DETECTED host re-ensures its metadata channel; an UNDETECTED
        // one retries detection.
        for id in &ids {
            let detected = self.hosts.get(id).map(|h| h.detected).unwrap_or(false);
            if detected {
                if let Some(host) = self.hosts.get(id) {
                    let _ = self.mgr.ensure(id, host, vc, vr);
                }
            } else {
                scan_or_dispatch_host(&mut self.mgr, &self.hosts, &mut self.detecting, id, vc, vr);
            }
        }
        // Re-warm each control host's dropped per-host PTY via its driver (ENSURE-ONLY;
        // a host with no known sessions yet is skipped rather than reaping a live PTY).
        for id in &ids {
            if self.mgr.get(id).is_none() {
                continue;
            }
            let inventory = match self.hosts.get(id) {
                Some(h) => h.inventory.sessions.clone(),
                None => continue,
            };
            if inventory.is_empty() {
                continue;
            }
            let mut ctx = crate::driver::DriverCtx {
                registry: &mut self.registry,
                hosts: &mut self.hosts,
                worker: &self.worker,
                mgr: &self.mgr,
                pty_tx: &self.driver_pty_tx,
                attach_seq: &mut self.attach_seq,
                cols: self.cols,
                body_rows: self.body_rows,
                tree_width: self.tree_width,
            };
            sync_source_terminals(id, &inventory, &mut ctx);
        }
        // Capture the display-client tty for any shared host whose display attach is live
        // but whose tty is not yet known (retried each sweep).
        for id in &ids {
            let Some(h) = self.hosts.get(id) else {
                continue;
            };
            if h.display_tty.0.is_some() {
                continue;
            }
            if self.registry.contains(&host_selection_key(h)) {
                if let Some(client) = self.mgr.get(id) {
                    client.capture_display_tty();
                }
            }
        }
        // Re-attach the selected session's display terminal if it dropped.
        if !self.state.selection.is_empty() {
            let key = display_key(&self.hosts, &self.state.selection);
            let in_flight_for_key = self
                .hosts
                .get(&self.state.selection.source)
                .map(|h| h.display.in_flight_contains(&key))
                .unwrap_or(false);
            if !self.registry.contains(&key) && !in_flight_for_key {
                let mut ctx = crate::driver::DriverCtx {
                    registry: &mut self.registry,
                    hosts: &mut self.hosts,
                    worker: &self.worker,
                    mgr: &self.mgr,
                    pty_tx: &self.driver_pty_tx,
                    attach_seq: &mut self.attach_seq,
                    cols: self.cols,
                    body_rows: self.body_rows,
                    tree_width: self.tree_width,
                };
                select_attach(&self.state.selection, &mut ctx);
            }
        }
    }
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

/// The braille-spinner frame index for `elapsed` since the app started.
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
mod tests;
