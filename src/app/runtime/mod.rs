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
//! draws the SAME split (tree + selected PTY grid) in both focus states — Focus::Nav
//! (tree focused) and Focus::Terminal (terminal focused) differ only in the view border
//! colour and where keys go, so toggling focus needs no screen clear.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use crate::app::input::{
    leading_ctrl_arrow, nav_menu_may_open, resolve_mouse_chain, resolve_nav_key, to_grid_local,
    view_border_drag_height, view_border_drag_width, ChainAction, MouseState, StdinOutcome,
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

pub(crate) const NAV_WIDTH_MIN: u16 = 20;
pub(crate) const NAV_WIDTH_MAX: u16 = 100;

/// The Top-layout tree height drag range. The min keeps a few tree rows; compute_regions
/// clamps the max down to the body so the terminal always keeps room.
pub(crate) const NAV_HEIGHT_MIN: u16 = 3;
pub(crate) const NAV_HEIGHT_MAX: u16 = 100;

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

fn adjust_nav_width(w: u16, delta: i32) -> u16 {
    (w as i32 + delta).clamp(NAV_WIDTH_MIN as i32, NAV_WIDTH_MAX as i32) as u16
}

/// Adjusts the natural tree width by `wd`, clamped to the allowed range. Returns
/// true if the width actually changed (so the loop can schedule a debounced
/// persist). A zero delta or a clamp-noop returns false. Write-free: the loop
/// owns the single persist.
fn apply_width_delta(wd: i32, natural: &mut u16) -> bool {
    if wd == 0 {
        return false;
    }
    let next = adjust_nav_width(*natural, wd);
    if next == *natural {
        return false;
    }
    *natural = next;
    true
}

/// Flips the auto-hide-nav mode and persists it, so the next launch restores it.
/// Shared by the tree- and terminal-view focus `prefix t` paths. The effective tree width is
/// reconciled at the next loop top (`reconciled_nav_width`); the caller marks dirty.
fn toggle_auto_hide(mode: &mut bool, xmux_dir: &std::path::Path) {
    *mode = !*mode;
    crate::prefs::save_auto_hide_nav(xmux_dir, *mode);
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
    nav_width_natural: &mut u16,
    auto_hide_nav: &mut bool,
    xmux_dir: &std::path::Path,
    op_sink: OpSink<'_>,
) -> (bool, bool) {
    dispatch_commands(
        state.apply(action),
        switcher,
        state,
        nav_width_natural,
        auto_hide_nav,
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
    nav_width_natural: &mut u16,
    auto_hide_nav: &mut bool,
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
                if apply_width_delta(d, nav_width_natural) {
                    width_changed = true;
                }
            }
            Command::ToggleAutoHide => toggle_auto_hide(auto_hide_nav, xmux_dir),
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
    nav_focused: bool,
    cwd: &str,
    tty: &str,
) -> String {
    crate::control::format_status(&crate::control::StatusFields {
        focus: if nav_focused { "nav" } else { "terminal" }.to_string(),
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
/// full width) only while the terminal view is focused AND auto-hide-nav mode is on;
/// otherwise the tree's natural width. Pure so the focus/mode interaction is
/// unit-testable; the loop owns the natural width and the PTY resize on change.
fn reconciled_nav_width(terminal_focused: bool, auto_hide_nav: bool, natural: u16) -> u16 {
    if terminal_focused && auto_hide_nav {
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
/// is `cols - nav_width - 1` (tree + the single view border rule), except `nav_width == 0`
/// (the tree-hidden sentinel) gives the full `cols` with no view border. The hint_bar now
/// spans the full width along the bottom, so a shown tree gives the terminal view height
/// `body_rows` (full height minus the one hint_bar row); the tree-hidden sentinel has no
/// hint_bar and keeps the full height `body_rows + 1`. Both clamp to at least 1.
pub(crate) fn terminal_view_size(
    cols: u16,
    body_rows: u16,
    nav_width: u16,
    nav_height: u16,
) -> (u16, u16) {
    // Derive from the one shared geometry (`compute_regions`) so the PTY size always
    // matches what the renderer draws, in either layout. `body_rows` is full_height - 1
    // (the hint bar row), so the full area is `body_rows + 1` tall; sizing assumes a
    // one-row hint bar. A portrait area stacks the tree on top and shrinks the terminal
    // view height accordingly; `nav_width == 0` gives the full area (tree hidden).
    let area = ratatui::layout::Rect::new(0, 0, cols, body_rows.saturating_add(1));
    let t = crate::ui::switcher::compute_regions(area, nav_width, nav_height, 1).terminal;
    (t.width.max(1), t.height.max(1))
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
    nav_width: u16,
) {
    // Auto height (0) is fine here: this sizes the host's METADATA control client, not the
    // displayed grid (that goes through the DriverCtx, which carries the real nav_height),
    // and on_tick's resize_all reconciles it to the exact height. Avoids threading nav_height
    // through every ensure_current_host caller for a size the user never sees.
    let (cols, rows) = terminal_view_size(cols, rows, nav_width, 0);
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
    nav_width: u16,
) {
    // Auto height (0): the initial metadata-client size only; the display PTY and on_tick
    // resize carry the real nav_height. (See ensure_current_host.)
    let (cols, rows) = terminal_view_size(cols, rows, nav_width, 0);
    for id in hosts.ids() {
        scan_or_dispatch_host(mgr, hosts, detecting, id, cols, rows);
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
        rt.nav_width,
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
            Some((src, ra, ri)) = io.border_rx.recv() => rt.on_border_styles(src, ra, ri),
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
        crate::prefs::save_nav_width(&rt.env.xmux_dir, rt.nav_width_natural);
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
    nav_width: u16,
    /// The tree's natural width (what prefix h/l adjusts; restored when shown again).
    nav_width_natural: u16,
    /// The Top-layout tree height, set by dragging the horizontal view border or the resize
    /// keys. 0 = auto (~40% of the body). Only used in the portrait Top layout; ignored in Side.
    nav_height: u16,
    /// The `nav_height` last applied to the PTY sizes, so the loop-top reconcile resizes the
    /// mux terminals when the Top height changes (not only on a width change). `u16::MAX`
    /// forces the first reconcile to size them.
    applied_nav_height: u16,
    auto_hide_nav: bool,
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
    /// The sink a detached border-style query posts its `(source, active_raw, inactive_raw)`
    /// result to; the loop's `border_rx` arm folds it via `on_border_styles`.
    border_tx: tokio::sync::mpsc::UnboundedSender<(String, String, String)>,
    /// Resolved view border colours per displayed source (border-style is server-global
    /// and rarely changes, so one query per host is cached here).
    border_cache: HashMap<String, crate::ui::switcher::ViewBorderColors>,
    /// Sources with a border-style query in flight — prevents duplicate spawns.
    border_inflight: HashSet<String>,
    /// The source whose cached colours are currently applied to the chrome, so the
    /// per-frame trigger re-applies only on a displayed-source change.
    border_applied: Option<String>,
}

/// The loop's receiver halves, whose send halves `Runtime::new` wired into the world
/// state (mgr's host events, the worker's PTY events, the op-result channel). Held
/// loop-local in [`run_app`] so an arm can `select!` on one while its body borrows
/// `&mut Runtime`.
struct LoopIo {
    host_rx: tokio::sync::mpsc::UnboundedReceiver<HostEvent>,
    pty_rx: tokio::sync::mpsc::UnboundedReceiver<PtyEvent>,
    op_rx: tokio::sync::mpsc::UnboundedReceiver<crate::ui::switcher::OpResult>,
    border_rx: tokio::sync::mpsc::UnboundedReceiver<(String, String, String)>,
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

mod handlers;
mod input;

#[cfg(test)]
mod tests;
