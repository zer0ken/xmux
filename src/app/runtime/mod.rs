//! The app: a persistent supervisor that owns the terminal for the whole
//! session. It keeps ONE real attached mux client per session - a `tmux attach` /
//! `psmux attach` running inside a `portable-pty` PTY ([`AttachRegistry`]) - alive
//! across selections, and renders the SELECTED session's live `Grid` on the right.
//! A separate control-mode client per remote host ([`HostManager`]) supplies the
//! nav view inventory and mux-side change events; local psmux is enumerated/polled
//! with plain commands (it is one-server-per-session, so a host-level control
//! client cannot see across its sessions).
//!
//! State is explicit: [`Selection`] (the canonical `source`/`session`) is
//! the single source of truth the display reads - the `Switcher` owns only the nav
//! and selection. One `select!` loop interleaves stdin, host events, PTY events, the
//! control socket, terminal resize, and an animation tick. ratatui owns stdout and
//! draws the SAME split (nav + selected PTY grid) in both focus states - Focus::Nav
//! (nav focused) and Focus::Terminal (terminal focused) differ only in the view border
//! colour and where keys go, so toggling focus needs no screen clear.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use crate::app::input::{
    leading_ctrl_arrow, resolve_mouse_chain, resolve_nav_key, to_grid_local,
    view_border_drag_height, view_border_drag_width, ChainAction, MouseState, StdinOutcome,
};
use crate::display::attachment::PtyEvent;
use crate::display::dispatch::Action;
use crate::display::registry::AttachRegistry;
use crate::display::{DisplayEnsure, DisplayEvent, DisplayWorker};
use crate::link::{HostEvent, HostManager};
use crate::model::Selection;
use crate::provision::env::Env;
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
/// loop redraws at most once per frame - so no navigation pattern can flood the
/// terminal with full-screen repaints and stall the single-threaded loop. A frame
/// timer at this cadence flushes a pending dirty draw promptly even with no input.
const FRAME_MS: u64 = 33;

/// How often the reconnect sweep runs: re-ensures a died remote control client and
/// re-attaches the selected session's PTY if it dropped. Doubles as the retry
/// backoff so a genuinely-down host is retried at this cadence, never hot-looped.
const RECONNECT_MS: u64 = 2000;

pub(crate) const NAV_WIDTH_MAX: u16 = 100;

/// The nav's floor width: the resting prefix label plus a one-cell breathing gap on
/// each side, so the view border can sit right after the `[C-g]` status text and a
/// wider configured prefix still fits.
pub(crate) fn nav_width_min(ui_prefix: &str) -> u16 {
    ui_prefix.chars().count() as u16 + 2
}

/// The band-layout nav height drag range. The min keeps a few nav rows; compute_regions
/// clamps the max down to the body so the terminal always keeps room.
pub(crate) const NAV_HEIGHT_MIN: u16 = 3;
pub(crate) const NAV_HEIGHT_MAX: u16 = 100;

/// The ratatui terminal the app draws into. Loop-local in [`run_app`] (owns stdout);
/// passed to the `Runtime` methods that draw / resize / dump.
type Term = ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>;

/// Clears the physical screen and forces the next draw to repaint every cell.
///
/// Not `Terminal::clear`: that preserves the cursor by querying the terminal for
/// its position first, and the reply to that query arrives on fd 0, where the
/// app's own stdin reader thread outraces crossterm's event source - on Unix the
/// query then times out (2 s) and the clear fails. Clearing through the backend
/// sends no query, and resetting both buffers makes the next diff treat every
/// cell as changed, so a cell the render leaves untouched comes up blank rather
/// than resurrecting pre-clear content.
fn clear_screen<B>(term: &mut ratatui::Terminal<B>) -> Result<(), B::Error>
where
    B: ratatui::backend::Backend,
{
    use ratatui::backend::ClearType;
    term.backend_mut().clear_region(ClearType::All)?;
    term.swap_buffers();
    term.swap_buffers();
    Ok(())
}

/// How long the resize-repeat window stays open after a prefix-driven nav resize:
/// during it a bare Ctrl+←/→ (no prefix) keeps resizing and refreshes the window -
/// tmux's `bind -r` repeat applied to the nav width. Each repeat resets the window.
const RESIZE_REPEAT_MS: u64 = 400;

/// How long after the last resize tick before the debounced nav-width persist fires.
/// Longer than `RESIZE_REPEAT_MS` so a held Ctrl-arrow autorepeat burst persists once
/// at the end, not per tick.
const WIDTH_FLUSH_MS: u64 = 400;

fn adjust_nav_width(w: u16, delta: i32, ui_prefix: &str) -> u16 {
    (w as i32 + delta).clamp(nav_width_min(ui_prefix) as i32, NAV_WIDTH_MAX as i32) as u16
}

/// Adjusts the natural nav width by `wd`, clamped to the allowed range. Returns
/// true if the width actually changed (so the loop can schedule a debounced
/// persist). A zero delta or a clamp-noop returns false. Write-free: the loop
/// owns the single persist.
fn apply_width_delta(wd: i32, natural: &mut u16, ui_prefix: &str) -> bool {
    if wd == 0 {
        return false;
    }
    let next = adjust_nav_width(*natural, wd, ui_prefix);
    if next == *natural {
        return false;
    }
    *natural = next;
    true
}

/// Flips the auto-hide-nav mode and persists it, so the next launch restores it.
/// Shared by the nav- and terminal-view focus `prefix t` paths. The effective nav width is
/// reconciled at the next loop top (`reconciled_nav_width`); the caller marks dirty.
fn toggle_auto_hide(mode: &mut bool, xmux_dir: &std::path::Path) {
    *mode = !*mode;
    crate::ui::prefs::save_auto_hide_nav(xmux_dir, *mode);
}

/// Applies one step of the `prefix p` cycle to the pin and saves it at once (the same
/// moment `toggle_auto_hide` saves its toggle). `None` (the fifth step) stores "auto",
/// which returns the nav to following the [ui] settings.
fn cycle_nav_position(
    pinned: &mut Option<crate::ui::switcher::NavPosition>,
    effective: crate::ui::switcher::NavPosition,
    xmux_dir: &std::path::Path,
) {
    let next = crate::ui::switcher::step_nav_position(*pinned, effective);
    *pinned = next;
    crate::ui::prefs::save_nav_position(xmux_dir, next);
}

/// Folds ONE domain [`Action`] in at the single mutation site ([`State::apply`]) and
/// runs the [`Command`]s it returns - the site both a keypress (via
/// `display::dispatch::Action::as_action`) and a ctl command resolve through, so the two
/// surfaces can never take divergent effect. Returns `(quit, width_changed)`: `quit`
/// signals the loop to exit; `width_changed` signals the loop to schedule the debounced
/// nav-width persist. `Switch` only moves the selection (a `SelectAddress` command); the
/// loop-top `Tick`/`select_attach` commits the attach on a later pass.
///
/// Only the synchronous, registry-free commands arise here - `Attach`/
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

/// Runs the [`Command`]s an [`Action`] produced - the sole dispatcher of the
/// synchronous, registry-free effects. `SelectAddress`/`Rescan`/`AdjustNavWidth`/
/// `ToggleAutoHide`/`Quit` act on the switcher/width/loop here; `RunOp` is spawned
/// off-loop against the live mux (its `OpResult` folds back through `op_tx`, the
/// existing channel). `Attach`/`PersistLastSession` arise only from `Action::Tick`,
/// dispatched by the run loop with full registry access - never here.
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
            Command::AdjustNavWidth(d) => {
                if apply_width_delta(d, nav_width_natural, &state.chrome.ui_prefix) {
                    width_changed = true;
                }
            }
            Command::ToggleAutoHide => toggle_auto_hide(auto_hide_nav, xmux_dir),
            Command::Quit => quit = true,
            Command::RunOp(op) => spawn_op(op, op_sink.0, op_sink.1),
            Command::RunUnlock {
                source,
                user,
                password,
            } => spawn_unlock(source, user, password, op_sink.0, op_sink.1),
            // Settled-selection effects come only from Action::Tick, dispatched by the
            // run loop with registry/host access - never from a key/ctl action here.
            Command::PersistLastSession(_) | Command::Attach(_) => {}
        }
    }
    (quit, width_changed)
}

/// The `status` verb reply: this instance's name and pid, the focus side, the
/// displayed session, and its working directory + controlling tty. A flat,
/// TAB-separated `key=value` line an agent reads to confirm a `switch`/`focus` landed
/// and that `xmux instances` parses to tell instances apart. The wire format lives in
/// `control` so producer and parser cannot drift.
fn status_line(
    switcher: &crate::ui::switcher::Switcher,
    name: &str,
    nav_focused: bool,
    cwd: &str,
    tty: &str,
) -> String {
    crate::link::control::format_status(&crate::link::control::StatusFields {
        name: name.to_string(),
        pid: std::process::id().to_string(),
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
/// has no pts). Best-effort and dependency-free - a `-` never breaks the listing, it
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

/// The EFFECTIVE nav width to render and size the terminal view against. Hidden (0,
/// terminal view full width) only while the terminal view is focused, auto-hide-nav
/// mode is on, and no prefix interaction is active; otherwise the nav's natural width.
/// A prefix press is an interaction with xmux, so the nav comes back for it even under
/// auto-hide (the user needs the card numbers to jump, resize, or act on a card).
/// Pure so the focus/mode interaction is unit-testable; the loop owns the natural
/// width and the PTY resize on change.
fn reconciled_nav_width(
    terminal_focused: bool,
    auto_hide_nav: bool,
    prefix_active: bool,
    natural: u16,
) -> u16 {
    if terminal_focused && auto_hide_nav && !prefix_active {
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

/// How a freshly-computed grid fingerprint relates to the last one rendered for its key -
/// the pure classification the draw block turns into a `display_grid_changed` log grade.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FpOutcome {
    /// Fingerprint unchanged - screen content did not change (no event).
    Unchanged,
    /// Fingerprint changed, same session - a steady-state repaint (TRACE grade).
    Steady,
    /// Fingerprint changed and the session differs, or first paint for this key - the
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

    /// Emits a `slow_step` DEBUG event when a synchronous step took at least 10ms - used
    /// to locate what stalls the single-threaded event loop during rapid navigation.
    fn slow_step(label: &str, start: std::time::Instant) {
        let ms = start.elapsed().as_millis();
        if ms >= 10 {
            tracing::debug!(label, ms, "slow_step");
        }
    }
}

/// Derives a [`Selection`] from the switcher's current terminal-view target. The
/// whole target is the session name the card carries, which keys the PTY attachment.
/// Stays in `app` because it depends on the ui [`TerminalViewTarget`] - the
/// [`Selection`] value itself is a pure `model` type.
fn selection_from_target(t: &TerminalViewTarget) -> Selection {
    // The target is the session name as the card carries it, whole - no window suffix
    // to part off, so a session name holding a colon survives as it is.
    Selection {
        source: t.source.clone(),
        session: t.target.clone(),
    }
}

/// Derives the selection from the switcher selection and, if it moved, routes it through
/// the single mutation site as [`Action::Select`] - which records the new selection
/// and marks the attach pending. It arms NO deadline; the trailing [`Action::Tick`]
/// arms the debounce (re-armed on every move, so rapid navigation coalesces into one
/// trailing attach). Returns true when the selection changed (the nav needs a redraw).
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

/// The session a source's display client is ON: the one fact the nav selection is held
/// against. It is the host's display record, which each mux keeps true its own way - the
/// control notice a mux pushes when it moves a client, and, for a mux that moves its
/// client inside the client process and pushes nothing, the live client read on the
/// animation beat. `None` while no display has been established for the source, which is
/// the first attach's own case and not a disagreement.
///
/// It is not what xmux last decided to show: that is `state.displayed`, and a session
/// change the mux made moves the client without touching it, which is precisely how the
/// nav and the terminal view came to name different sessions.
fn display_session<'a>(hosts: &'a crate::model::Hosts, source: &str) -> Option<&'a str> {
    let host = hosts.get(source)?;
    host.display.shows(&host_selection_key(host))
}

/// Whether the display client sits on a session the selection does not name AND the
/// selection is the one to keep, so the attach beat must carry the client back to it.
///
/// A CONDITION, evaluated here every beat and stored nowhere. Nothing records that a
/// switch happened, when it happened, or that a move is owed for it, so there is no
/// policy for when to pay such a record and none for when to cancel it: while the client
/// is away from the selection the answer is yes, and the moment either side moves to the
/// other it is no.
///
/// FOCUS decides which of the two moves, and it is the only thing that does. In nav focus
/// the selection is the user's own, so the client comes back to it - this. In terminal
/// focus the user is driving the mux, so the SELECTION goes to the client instead
/// ([`Runtime::follow_selection_to_display`]) and this stays false, since carrying the
/// client back would undo the switch the user just made with the mux's own keys.
///
/// WHY IT SETTLES, being a condition rather than an event. Each answer makes the two
/// names EQUAL and nothing here makes them differ: this attaches the client to the
/// session the selection already names, the follow moves the selection to the session the
/// client is already on, and neither one moves the side it is comparing against. So the
/// condition is false as soon as one of them has acted, and stays false until something
/// outside - the user, or the mux - moves one of the two again, which is the difference
/// that ought to be answered. The two can never take turns undoing each other either,
/// because the focus admits exactly one of them at a time. The debounce and the in-flight
/// gate bound the rate rather than the outcome: while an attach is under way this is not
/// re-armed, so the client is carried once and not once per beat until it arrives.
fn display_astray(state: &crate::state::State, hosts: &crate::model::Hosts) -> bool {
    if state.selection.is_empty() || state.focus.is_terminal_focused() {
        return false;
    }
    display_session(hosts, &state.selection.source)
        .is_some_and(|shown| shown != state.selection.session)
}

/// The size to give a PTY attachment: the terminal view (right of the nav +
/// view border), NOT the whole terminal. Sizing a session to the full terminal makes
/// the remote wrap at a width wider than the visible view, so a line overflows the
/// right edge (and a double-width char straddles the clip boundary). The view width
/// is `cols - nav_width - 1` (nav + the single view border rule), except `nav_width == 0`
/// (the nav-hidden sentinel) gives the full `cols` with no view border. The hint bar
/// lives INSIDE the nav region, so it costs the terminal view no height in a column: the
/// view gets the full `body_rows + 1`. In a band layout the terminal view is what is
/// left below the nav band. Both clamp to at least 1.
pub(crate) fn terminal_view_size(
    cols: u16,
    body_rows: u16,
    nav: crate::ui::switcher::NavSize,
) -> (u16, u16) {
    // Derive from the one shared geometry (`compute_regions`) so the PTY size always
    // matches what the renderer draws, in either layout. `body_rows` is full_height - 1,
    // so the full area is `body_rows + 1` tall; sizing assumes a one-row hint bar inside
    // the nav. A portrait area stacks the nav on top and shrinks the terminal view
    // height accordingly; a hidden nav gives the full area.
    let area = ratatui::layout::Rect::new(0, 0, cols, body_rows.saturating_add(1));
    let t = crate::ui::switcher::compute_regions(area, nav, 1).terminal;
    (t.width.max(1), t.height.max(1))
}

/// The `AttachRegistry` key for a selection.
pub(crate) fn display_key(hosts: &crate::model::Hosts, sel: &Selection) -> String {
    hosts
        .get(&sel.source)
        .map(host_selection_key)
        .unwrap_or_else(|| sel.source.clone())
}

/// The display key for a host's selection. Both server models key the live display by
/// HOST id: tmux keeps one PTY per host (shared, moved by switch-client), and psmux -
/// though one-server-per-session - is displayed through ONE per-host PTY that is
/// reattached on every session change. This is the supervisor/driver authority for the
/// live attach path - the sole keying authority for both models.
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
/// `(false, false)` - the gate short-circuits on emptiness anyway.
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
/// grid - the read counterpart to [`select_attach`]'s show. Shared by the draw hot path
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

/// Spawns the dispatched switch command off the event loop. Local variants run as a
/// plain subprocess; RawSsh variants run the full ssh argv non-interactively.
pub(crate) fn run_lowered(lowered: crate::transport::LoweredSwitch) {
    use crate::model::source::Runner;
    use crate::transport::LoweredSwitch;
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
        match crate::model::source::ExecRunner.run(&name, &args).await {
            Ok(out) => tracing::debug!(cmd = %name, out_bytes = out.len(), "lowered_ok"),
            Err(e) => tracing::debug!(cmd = %name, error = %e, "lowered_err"),
        }
    });
}

/// Runs a mux's opaque [`crate::mux::SwitchPlan`] BLIND: the driver hands the whole plan
/// here and this dispatches each variant through the host's transport, never naming the mux
/// type. `Exec` argv(s) run non-interactively in order; a `Shell` command runs over the
/// host's raw shell (`raw_shell_argv`). Returns whether the switch was issued - `false` when
/// a `Shell` plan has no host shell (a local machine), so the caller falls back to a
/// reattach. The variant→lowering mapping is 1:1 with [`crate::transport::LoweredSwitch`].
pub(crate) fn run_switch_plan(host: &crate::model::Host, plan: crate::mux::SwitchPlan) -> bool {
    use crate::mux::SwitchPlan;
    use crate::transport::LoweredSwitch;
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
        SwitchPlan::Shell(cmd) => match host.transport.raw_shell_argv(&cmd) {
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
/// channel streams that host's rows in. The manager picks the channel (control client
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
    let (cols, rows) =
        terminal_view_size(cols, rows, crate::ui::switcher::NavSize::visible(nav_width));
    // A locked selected host gets no control channel from here: opening a `-CC` that
    // dies on auth would overwrite its locked reason with "connection closed". The
    // reconnect sweep re-probes its reachability instead.
    if switcher.current_host_locked() {
        return;
    }
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
    transport: Box<dyn crate::transport::Transport>,
    mux: Box<dyn crate::mux::Mux>,
    tx: tokio::sync::mpsc::UnboundedSender<HostEvent>,
) {
    tokio::spawn(async move {
        let mut host = crate::model::Host::new(transport, mux);
        host.detect_and_correct(&crate::model::source::ExecRunner)
            .await;
        let detected = host.detected.then_some(host.mux);
        let _ = tx.send(HostEvent::Scanned { source, detected });
    });
}

/// Runs one MACHINE's mux discovery off the loop, cloning the transport of a source that
/// already reaches it so the probes travel the same axes as everything else. The answer
/// is emitted as `HostEvent::MuxesFound`.
///
/// Fire and forget, and deliberately AFTER a machine connects: a remote probe is an ssh
/// round trip per mux, and only a reachable machine is worth asking. Nothing waits for
/// it, so a machine that never answers costs a task and no more. A permit is held on
/// `gate` (the shared probe gate) for the whole probe, so at most [`PROBE_CONCURRENCY`]
/// probe tasks run at once.
fn spawn_mux_discovery(
    machine: String,
    transport: Box<dyn crate::transport::Transport>,
    tx: tokio::sync::mpsc::UnboundedSender<HostEvent>,
    gate: std::sync::Arc<tokio::sync::Semaphore>,
) {
    tokio::spawn(async move {
        let Ok(_permit) = gate.acquire().await else {
            return;
        };
        let muxes =
            crate::mux::installed_muxes(&*transport, &crate::model::source::ExecRunner).await;
        if !muxes.is_empty() {
            let _ = tx.send(HostEvent::MuxesFound { machine, muxes });
        }
    });
}

/// Re-resolves the ROSTER off the loop and hands the answer back as
/// `HostEvent::RosterResolved`.
///
/// Off the loop for the same reason mux discovery is: resolving reads the config and asks
/// each roster provider, and a provider is a subprocess (`tailscale status`, `wsl.exe -l`).
/// Running that on the loop would freeze rendering and input for its whole duration.
///
/// A config that stopped PARSING resolves to defaults, which would silently narrow the
/// roster to this machine. That answer is dropped rather than applied: a typo must cost the
/// user a warning, never every remote card on screen.
fn spawn_roster_resolve(
    xmux_dir: std::path::PathBuf,
    local_socket: Option<String>,
    tx: tokio::sync::mpsc::UnboundedSender<HostEvent>,
) {
    tokio::spawn(async move {
        let (roster, err) = crate::provision::env::resolve_roster(&xmux_dir, local_socket).await;
        if let Some(e) = err {
            tracing::warn!(error = %e, "config did not parse; keeping the roster as it stands");
            return;
        }
        let _ = tx.send(HostEvent::RosterResolved {
            roster: Box::new(roster),
        });
    });
}

/// How many probe tasks may run at once, from a machine's reachability probe onward
/// (that probe, and the mux discovery a connected machine goes on to). The shared gate
/// bounds them together so a launch or re-scan over a large roster never floods the
/// network with a subprocess per machine all at the same instant.
const PROBE_CONCURRENCY: usize = 12;

/// Runs one machine's REACHABILITY probe off the loop - `ssh <opts> <machine> true`,
/// bounded by the shared `gate` - and carries the outcome back as
/// [`HostEvent::MachineProbed`]. A zero exit is connected (`err` `None`); ssh's own
/// failure line is the reason otherwise, classified locked (its auth-failure signature)
/// or unreachable at the card. The `true` also warms the shared ControlMaster socket the
/// connected machine's later channels reuse without re-authenticating.
fn spawn_machine_probe(
    machine: String,
    transport: Box<dyn crate::transport::Transport>,
    tx: tokio::sync::mpsc::UnboundedSender<HostEvent>,
    gate: std::sync::Arc<tokio::sync::Semaphore>,
    rescan: bool,
) {
    use crate::model::source::Runner;
    tokio::spawn(async move {
        let Ok(_permit) = gate.acquire().await else {
            return;
        };
        let (name, args) = transport.exec_argv(false, &["true".to_string()]);
        let err = match crate::model::source::ExecRunner.run(&name, &args).await {
            Ok(_) => None,
            Err(e) => Some(e.to_string()),
        };
        let _ = tx.send(HostEvent::MachineProbed {
            machine,
            err,
            rescan,
        });
    });
}

/// Probes ONE machine's reachability, named by any source `id` it serves. A local or
/// WSL machine is on this box, so it is reachable without an ssh round trip and connects
/// inline; a remote machine is probed off the loop under `gate`.
fn probe_machine(
    id: &str,
    hosts: &crate::model::Hosts,
    tx: tokio::sync::mpsc::UnboundedSender<HostEvent>,
    gate: &std::sync::Arc<tokio::sync::Semaphore>,
    rescan: bool,
) {
    let Some(host) = hosts.get(id) else {
        return;
    };
    let machine = crate::session::machine_of(id).to_string();
    if !host.transport.is_remote() {
        let _ = tx.send(HostEvent::MachineProbed {
            machine,
            err: None,
            rescan,
        });
        return;
    }
    spawn_machine_probe(machine, host.transport.clone(), tx, gate.clone(), rescan);
}

/// Probes the reachability of every MACHINE the roster serves, once each (deduped by
/// machine), bounded by the shared `gate`. This is discovery's front: a connected
/// machine goes on to mux discovery and its channels, a locked or unreachable one
/// classifies its cards without opening any.
fn probe_machines(
    hosts: &crate::model::Hosts,
    tx: tokio::sync::mpsc::UnboundedSender<HostEvent>,
    gate: &std::sync::Arc<tokio::sync::Semaphore>,
    rescan: bool,
) {
    let mut seen: HashSet<&str> = HashSet::new();
    for id in hosts.ids() {
        if seen.insert(crate::session::machine_of(id)) {
            probe_machine(id, hosts, tx.clone(), gate, rescan);
        }
    }
}

/// Dispatches a DETECTED host onto its metadata channel via the manager, which picks
/// the channel (control client vs poll task) from the host's `event_source`. Idempotent
/// - a no-op when the channel is already live.
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

/// Consumes a pending re-scan kick (set by `r` or a menu "reconnect"): runs the shared
/// discovery pass with the rescan flag, so a re-scan refreshes WHICH MACHINES exist
/// (re-resolves the roster) and re-probes every machine's reachability, exactly the work
/// a fresh launch runs. A no-op when no kick is pending. Shared by the key and menu paths.
fn kick_rescan(
    switcher: &mut crate::ui::switcher::Switcher,
    env: &Env,
    hosts: &crate::model::Hosts,
    mgr: &HostManager,
    gate: &std::sync::Arc<tokio::sync::Semaphore>,
) {
    if !switcher.take_rescan_kick() {
        return;
    }
    run_discovery(env, hosts, mgr, gate, true);
}

/// The shared discovery pass a fresh launch and a re-scan both run: probe every
/// machine's reachability, and on a re-scan re-resolve the roster too. A machine's
/// answer (`HostEvent::MachineProbed`) drives the rest - a connected machine detects and
/// dispatches its sources and, if auto, discovers its muxes; a locked or unreachable one
/// classifies its cards - so this pass opens no channel itself.
///
/// On a re-scan the roster is re-resolved concurrently; when it lands (`RosterResolved`),
/// the freshly ADDED machines are probed, so a machine that just came online turns into a
/// card without a restart. The machines standing right now are probed here regardless, so
/// a slow provider delays no card already on screen.
fn run_discovery(
    env: &Env,
    hosts: &crate::model::Hosts,
    mgr: &HostManager,
    gate: &std::sync::Arc<tokio::sync::Semaphore>,
    rescan: bool,
) {
    if rescan {
        spawn_roster_resolve(env.xmux_dir.clone(), env.local_socket.clone(), mgr.events());
    }
    probe_machines(hosts, mgr.events(), gate, rescan);
}

/// Refetches a host's inventory after a `%`-change notification: re-runs
/// list-sessions - its reply (Connected/Inventory) re-applies the nav and re-syncs
/// the PTY set (a new session attaches, a closed one is reaped). #5 nav view sync.
fn refetch_host(mgr: &HostManager, host: &str) {
    if let Some(client) = mgr.get(host) {
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
        // The marker fired but no registry entry has this id yet - diagnostic for a
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
/// last-known rows. A never-connected host that died with "no sessions" / "no server
/// running" is REACHABLE but has no mux server - it renders "(empty)" (and a session
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
    // rows". A transient drop of a once-connected host keeps its rows (no unreachable
    // flash) on THIS exit; but a later reconnect that fails (no sessions / unreachable)
    // must then resolve its real state - otherwise a refresh that set it scanning would
    // spin on "loading…" forever, since a sticky `connected` made every exit a no-op.
    if connected.remove(host) {
        return false;
    }
    if reason
        .as_deref()
        .is_some_and(crate::model::source::reason_is_no_sessions)
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
pub async fn run_app(env: Arc<Env>, requested_name: Option<String>) -> i32 {
    use crate::display::term::TermGuard;
    use crate::ui::run::{serve_control, Cmd};
    use std::io::Read;
    use std::time::Duration;

    let _ = std::fs::create_dir_all(&env.xmux_dir);

    // The palette is ANSI-16 slots and attributes throughout, so it needs nothing from
    // the terminal but the theme the terminal already has: no colour query, no probing,
    // no fallback to guess at. The colours from outside those slots are the ones the
    // user names in `[ui]` role keys and `[ui] selection-style`.
    {
        let ui = &env.roster().cfg.ui;
        crate::ui::palette::apply(&ui.theme, crate::ui::chrome::palette_overrides(ui));
    }

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
    // the main-thread unwind - idempotent with this.
    {
        let log = env.xmux_dir.join("panic.log");
        let prev_hook = std::panic::take_hook();
        // How many times each panic SITE has fired. A recovered worker panic (a PTY pump's
        // vt100 edge case) fires again on the next frame that hits it, so writing every one
        // fills the file with one line repeated: the count is written at each power of two
        // instead, which keeps the first, keeps the scale, and turns thousands of lines into
        // a dozen. Keyed by the site, not the message, because a message carries the
        // indexes that varied and would defeat the count.
        let seen: std::sync::Mutex<std::collections::HashMap<String, u64>> =
            std::sync::Mutex::new(std::collections::HashMap::new());
        std::panic::set_hook(Box::new(move |info| {
            let site = match info.location() {
                Some(l) => format!("{}:{}:{}", l.file(), l.line(), l.column()),
                None => "<unknown>".to_string(),
            };
            // A poisoned lock is a panic inside this hook: report the site rather than
            // counting it, so a hook that broke once still logs.
            let count = match seen.lock() {
                Ok(mut seen) => {
                    let c = seen.entry(site).or_insert(0);
                    *c += 1;
                    *c
                }
                Err(_) => 1,
            };
            // A main-thread panic is the app dying, once: it is always written, whatever
            // a worker has already counted at the same site, because the message on the
            // way out names the file it says the detail is in.
            let fatal = std::thread::current().name() == Some("main");
            if fatal || count.is_power_of_two() {
                // Emit to the structured log first: the non-blocking writer flushes on
                // WorkerGuard drop, which happens after main unwinds, so this record is
                // not lost even though the subscriber may not have flushed yet.
                tracing::error!(count, "panic: {info}");
                // Append to the raw file as a fallback readable without a log viewer.
                use std::io::Write;
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&log)
                {
                    let _ = writeln!(f, "[x{count}] {info}");
                }
            }
            if fatal {
                use ratatui::crossterm::{
                    event::DisableMouseCapture, execute, terminal::disable_raw_mode,
                    terminal::LeaveAlternateScreen,
                };
                let _ = disable_raw_mode();
                let _ = execute!(std::io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
                eprintln!("xmux: internal error - {info}");
                eprintln!("xmux: full detail logged to {}", log.display());
                // Only the main-thread crash reaches the default hook (stderr/backtrace)
                // - the terminal is restored above, so the print is safe and useful.
                prev_hook(info);
            }
            // A worker-thread panic (a PTY pump's vt100 edge case) is caught and
            // recovered by Grid::feed; the log and panic.log above carry it, counted. Do
            // NOT forward it to the default hook - its stderr print lands on the live
            // TUI's terminal and corrupts the screen (the panic-spam bug).
        }));
    }

    // Build the world state (Runtime) + the loop's I/O (the receivers `select!` polls).
    let (mut rt, mut io) = Runtime::new(env);
    // Kick the shared discovery pass at launch - the same one a re-scan runs, so a fresh
    // launch and a re-scan are functionally identical. Each machine's reachability is
    // probed first (bounded), and a connected one streams its rows in without waiting for
    // a selection move: control hosts connect a `-CC` client, poll hosts start their
    // self-looping enumeration task, both owned by the manager, and each auto machine is
    // asked which muxes it has (a mux nobody wrote down appears as its machine answers).
    // Deliberately off `Runtime::new` so a headless unit test can build a `Runtime`
    // without launching real probes / control clients. PTYs are attached as each source's
    // sessions arrive (see [`sync_source_terminals`]).
    run_discovery(&rt.env, &rt.hosts, &rt.mgr, &rt.probe_gate, false);
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
    if let Err(e) = clear_screen(&mut term) {
        tracing::warn!(error = %e, "term_clear_failed");
    }

    // The picker control socket: serves headless key/text/dump, and IS this instance's
    // identity - `xmux send <name>` dials exactly this path. An explicit `--name` is
    // taken as given (the user will type it again); otherwise walk the generated names
    // until one no live instance holds, seeded by the pid so two simultaneous starts
    // rarely probe the same name first.
    let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::channel::<Cmd>(256);
    let instance_name = match requested_name {
        Some(n) => n,
        None => {
            let _ = std::fs::create_dir_all(&rt.env.xmux_dir);
            crate::link::control::pick_free_name(&rt.env.xmux_dir, std::process::id() as u64).await
        }
    };
    rt.instance_name = instance_name.clone();
    let control = pick_control_path(&rt.env, &instance_name);
    let _control_handle = control.and_then(|p| serve_control(p, cmd_tx));
    // Off the startup path, sweep `ctl-*.sock` markers left by crashed instances (a
    // clean exit removes its own on drop; a hard-kill does not) so discovery does not
    // over-count dead instances.
    {
        let dir = rt.env.xmux_dir.clone();
        let keep = instance_name.clone();
        tokio::spawn(async move { crate::link::control::prune_stale(&dir, &keep).await });
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
                // Cheap live config reload: on the redraw cadence, stat the config
                // file and re-apply the `[ui]` presentation settings when it changed.
                // Marked dirty so the re-applied styles actually repaint this frame.
                if rt.on_config_check() {
                    rt.dirty = true;
                }
            }
        }
        // Any real event (not the bare frame wake) means the UI may have changed.
        if !from_frame {
            rt.dirty = true;
        }
    }

    // A resize within the last WIDTH_FLUSH_MS before quit leaves the debounce deadline
    // unreached, so the final width is still pending - persist it on the way out so the
    // nav width the user left with survives the next launch.
    if rt.width_dirty {
        crate::ui::prefs::save_nav_width(&rt.env.xmux_dir, rt.nav_width_natural);
    }
    rt.registry.teardown_all();
    rt.mgr.teardown_all();
    0
}

/// The persistent app's WORLD STATE: everything the `select!` loop mutates across
/// iterations. The `select!` receivers/timers and the ratatui `Terminal` stay
/// loop-local in [`run_app`] - a receiver cannot be polled from `self.<rx>.recv()`
/// while an arm body borrows `&mut self` - so `Runtime` owns the long-lived state and
/// each `select!` arm is one `&mut self` method.
struct Runtime {
    env: Arc<Env>,
    /// This instance's name: its identity on the control socket (`ctl-<name>.sock`) and
    /// the address `xmux send` uses. Resolved once in `run_app`, then only read.
    instance_name: String,
    ops: Arc<dyn crate::ui::switcher::Ops>,
    hosts: crate::model::Hosts,
    mgr: HostManager,
    /// Bounds the discovery probe fan-out (each machine's reachability probe and the mux
    /// discovery a connected machine runs) at [`PROBE_CONCURRENCY`], shared across the
    /// launch pass, every re-scan, and the roster-add path so they never flood together.
    probe_gate: Arc<tokio::sync::Semaphore>,
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
    /// The EFFECTIVE nav width (0 = nav hidden, terminal full width).
    nav_width: u16,
    /// The nav's natural width (what prefix h/l adjusts; restored when shown again).
    nav_width_natural: u16,
    /// The band-layout nav height, set by dragging the horizontal view border or the resize
    /// keys. 0 = auto (~40% of the body). Only used in a band layout; ignored in a column.
    nav_height: u16,
    /// The side the nav is attached to this frame; the layout and every region cut
    /// follows it.
    nav_position: crate::ui::switcher::NavPosition,
    /// The keyboard/config pin on the nav position (`prefix p`): `Some(side)` overrides
    /// the [ui] resolution outright, `None` follows it. Persisted on every change.
    nav_position_pinned: Option<crate::ui::switcher::NavPosition>,
    /// The [ui] nav-position settings the loop-top resolution reads. Refreshed when a
    /// live config apply lands; the reconcile at the next loop top applies the new value.
    nav_pos_setting: crate::ui::switcher::NavPositionSetting,
    /// The `nav_height` last applied to the PTY sizes, so the loop-top reconcile resizes the
    /// mux terminals when the band height changes (not only on a width change). `u16::MAX`
    /// forces the first reconcile to size them.
    applied_nav_height: u16,
    auto_hide_nav: bool,
    mouse_state: MouseState,
    term_input: crate::display::input::TermInput,
    nav_decoder: crate::display::decode::KeyDecoder,
    prefix: u8,
    connected: HashSet<String>,
    detecting: HashSet<String>,
    draw_observer: DrawObserver,
    spinner_start: std::time::Instant,
    dirty: bool,
    last_draw: std::time::Instant,
    /// The last modified time of `~/.config/xmux/config.toml`, for the live config
    /// watch. `None` until the first frame tick records a baseline, so the startup
    /// apply (which already ran) is not duplicated.
    config_last_mtime: Option<std::time::SystemTime>,
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

/// Runs the unlock off the loop the way [`spawn_op`] runs a mux op: the PTY
/// prompt-answer is blocking I/O with its own timeout, so it must never freeze the
/// loop; its [`OpResult::Unlock`] folds back through the same op channel.
fn spawn_unlock(
    source: String,
    user: String,
    password: String,
    ops: &Arc<dyn crate::ui::switcher::Ops>,
    op_tx: &tokio::sync::mpsc::UnboundedSender<crate::ui::switcher::OpResult>,
) {
    let ops = ops.clone();
    let tx = op_tx.clone();
    tokio::spawn(async move {
        let result = crate::ui::switcher::run_unlock(&source, &user, &password, ops.as_ref()).await;
        let _ = tx.send(result);
    });
}

/// The braille-spinner frame index for `elapsed` since the app started.
fn spinner_frame_at(elapsed: std::time::Duration) -> usize {
    (elapsed.as_millis() / SPINNER_FRAME_MS as u128) as usize
}

/// The picker's control socket path (`ctl-<name>.sock`), unless `XMUX_CONTROL=0`.
fn pick_control_path(env: &Env, name: &str) -> Option<PathBuf> {
    if std::env::var("XMUX_CONTROL").as_deref() == Ok("0") {
        return None;
    }
    let _ = std::fs::create_dir_all(&env.xmux_dir);
    Some(crate::link::control::socket_path(&env.xmux_dir, name))
}

mod handlers;
mod input;

#[cfg(test)]
mod tests;
