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
//! colour and where keys go, so toggling focus needs no screen clear. The app launches
//! straight into this split; there is no separate picker mode.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use crate::app::input::{
    leading_ctrl_arrow, resolve_mouse_chain, resolve_nav_key, to_grid_local,
    view_border_drag_height, view_border_drag_width, ChainAction, MouseState, StdinOutcome,
};
use crate::app::model::{adjust_nav_width, update, AppModel, Effect, Msg};
#[cfg(test)]
use crate::app::model::{nav_width_min, note_host_exited, NAV_WIDTH_MAX};
use crate::display::attachment::PtyEvent;
use crate::display::dispatch::Action;
use crate::display::registry::AttachRegistry;
use crate::display::{DisplayEvent, DisplayWorker};
use crate::driver::{display_key, host_selection_key, DriverCtx};
use crate::link::{HostEvent, HostManager};
use crate::model::Selection;
use crate::provision::env::Env;
#[cfg(test)]
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

/// The ceiling keeps timer rounding from exceeding the configured draw rate.
fn frame_interval(fps: u16) -> std::time::Duration {
    std::time::Duration::from_nanos(1_000_000_000_u64.div_ceil(u64::from(fps)))
}

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

/// Adjusts the natural nav width by `wd`, clamped to the allowed range. Returns
/// true if the width actually changed (so the loop can schedule a debounced
/// persist). A zero delta or a clamp-noop returns false. Write-free: the loop
/// owns the single persist.
#[cfg(test)]
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

/// The mutate-op sink handed to [`start_login`]: the `Ops` interface plus the channel
/// used by the off-loop work. Bundled as one argument to stay under the argument-count
/// lint.
type OpSink<'a> = (
    &'a Arc<dyn crate::ui::switcher::Ops>,
    &'a tokio::sync::mpsc::UnboundedSender<crate::ui::switcher::OpResult>,
);

impl Runtime {
    /// Folds one domain [`Action`](crate::model::Action) through
    /// [`State::apply`](crate::state::State::apply) and executes every command it
    /// returns.
    fn dispatch_action(&mut self, action: crate::model::Action) -> (bool, bool) {
        let effects = update(&mut self.model, Msg::Action(action));
        let (quit, width_changed, _) = self.execute_effects(effects);
        (quit, width_changed)
    }

    /// Executes every [`Command`](crate::model::Command) produced by the runtime state.
    /// Returns `(quit, width_changed)` for the loop bookkeeping owned by the caller.
    #[cfg(test)]
    fn execute_commands(&mut self, commands: Vec<crate::model::Command>) -> (bool, bool) {
        let effects = update(&mut self.model, Msg::Commands(commands));
        let (quit, width_changed, _) = self.execute_effects(effects);
        (quit, width_changed)
    }

    #[cfg(test)]
    fn execute_source_effect_for_test(&mut self, effect: crate::model::EventEffect) -> bool {
        self.execute_effects(vec![Effect::Event(effect)]).2
    }

    fn execute_effects(&mut self, effects: Vec<Effect>) -> (bool, bool, bool) {
        use crate::model::Command;

        let mut quit = false;
        let mut width_changed = false;
        let mut rearm = false;
        let mut pending: std::collections::VecDeque<_> = effects.into();
        while let Some(effect) = pending.pop_front() {
            match effect {
                Effect::Event(effect) => {
                    let (event_rearm, followups) = self.perform_source_effect(effect);
                    rearm |= event_rearm;
                    for followup in followups.into_iter().rev() {
                        pending.push_front(followup);
                    }
                }
                Effect::EventBatch(effects) => {
                    for effect in effects.into_iter().rev() {
                        pending.push_front(Effect::Event(effect));
                    }
                }
                Effect::LoginApplied { source, login } => {
                    let machine = crate::session::machine_of(&source).to_owned();
                    self.hosts.for_each_transport_of(&machine, |transport| {
                        transport.set_login(login.clone())
                    });
                    self.login_probes += 1;
                    let probe = self.login_probes;
                    let effects = update(
                        &mut self.model,
                        Msg::LoginSettled {
                            source,
                            credential_held: self.env.credentials().contains(&machine),
                            machine_has_sources: self.hosts.serves_any(&machine),
                            probe,
                        },
                    );
                    debug_assert!(effects.is_empty());
                    probe_machine(
                        &machine,
                        &self.hosts,
                        self.mgr.events(),
                        &self.scan_pool,
                        false,
                        probe,
                    );
                    self.dirty = true;
                }
                Effect::StartLogin {
                    source,
                    login,
                    password,
                    after_login,
                    attempt,
                    cancel,
                } => {
                    let key_gate = self.key_gates.of(crate::session::machine_of(&source));
                    start_login(
                        LoginRun {
                            source,
                            login,
                            attempt,
                            write_config: after_login == crate::model::AfterLogin::SshConfig,
                            register_key: after_login == crate::model::AfterLogin::RegisterKey,
                        },
                        password,
                        cancel,
                        key_gate,
                        (&self.ops, &self.op_tx),
                    )
                }
                Effect::PersistNavWidth(width) => {
                    crate::app::prefs::save_nav_width(&self.env.xmux_dir, width);
                }
                Effect::PersistNavHeight(height) => {
                    crate::app::prefs::save_nav_height(&self.env.xmux_dir, height);
                }
                Effect::PersistNavCollapsed(collapsed) => {
                    crate::app::prefs::save_nav_collapsed(&self.env.xmux_dir, collapsed);
                }
                Effect::PersistNavPosition(position) => {
                    crate::app::prefs::save_nav_position(&self.env.xmux_dir, position);
                }
                Effect::PersistFirstKeyHelpSeen => {
                    crate::app::prefs::mark_first_key_help_seen(&self.env.xmux_dir);
                }
                Effect::ReattachDisplay(selection) => {
                    let key = display_key(&self.hosts, &selection);
                    self.registry.remove(&key);
                    if let Some(host) = self.hosts.get_mut(&selection.source) {
                        host.display.clear(&key);
                    }
                }
                // Both key steps run over the machine's transport, before the logout
                // clears it, so they reach the host the way every command has. The
                // search waits at the machine's key gate, so a registration already
                // under way lands first and its line is found, and the logout keeps the
                // gate until it clears the machine.
                Effect::FindHostKeys {
                    machine,
                    cancel_login,
                } => {
                    for login in cancel_login {
                        login.cancel();
                    }
                    let transport = self.hosts.host_transport(&machine).map(|t| t.clone_box());
                    let gates = self.key_gates.clone();
                    let tx = self.op_tx.clone();
                    tokio::spawn(async move {
                        gates.hold(&machine).await;
                        let result = match transport {
                            Some(transport) => {
                                crate::provision::env::find_host_keys(
                                    &crate::model::source::ExecRunner,
                                    &transport,
                                )
                                .await
                            }
                            None => Err("xmux has no way to reach this host".into()),
                        };
                        let _ = tx
                            .send(crate::ui::switcher::OpResult::HostKeysFound { machine, result });
                    });
                }
                Effect::RemoveHostKeys { machine, lines } => {
                    let transport = self.hosts.host_transport(&machine).map(|t| t.clone_box());
                    let tx = self.op_tx.clone();
                    tokio::spawn(async move {
                        let result = match transport {
                            Some(transport) => {
                                crate::provision::env::remove_host_keys(
                                    &crate::model::source::ExecRunner,
                                    &transport,
                                    &lines,
                                )
                                .await
                            }
                            None => Err("xmux has no way to reach this host".into()),
                        };
                        let _ = tx.send(crate::ui::switcher::OpResult::HostKeysRemoved {
                            machine,
                            result,
                        });
                    });
                }
                Effect::LogoutMachine {
                    machine,
                    cancel_login,
                } => {
                    for login in cancel_login {
                        login.cancel();
                    }
                    self.key_gates.release(&machine);
                    let close_master = self
                        .hosts
                        .host_transport(&machine)
                        .and_then(|transport| transport.close_shared_connection_argv());
                    self.env.credentials().remove(&machine);
                    let (event_rearm, followups) =
                        self.perform_source_effect(crate::model::EventEffect::DisconnectMachine {
                            machine: machine.clone(),
                        });
                    rearm |= event_rearm;
                    for followup in followups.into_iter().rev() {
                        pending.push_front(followup);
                    }
                    if let Some(command) = close_master {
                        tokio::spawn(async move {
                            if let Err(error) = crate::model::source::ExecRunner
                                .run_spec_output(&command)
                                .await
                            {
                                tracing::debug!(machine, error = %error, "ssh_master_logout");
                            }
                        });
                    }
                    self.dirty = true;
                }
                Effect::CancelLogin(login) => login.cancel(),
                Effect::Command(command) => match command {
                    Command::SelectAddress(address) => {
                        unreachable!("selection commands are applied inside update: {address:?}")
                    }
                    Command::Rescan => {
                        self.rescan_pending = true;
                    }
                    // The machine's reachability probe, marked as a re-scan so a machine
                    // that answers re-enumerates every source it serves; nothing else is
                    // asked, and the roster is not re-resolved.
                    Command::RescanHost(machine) => {
                        #[cfg(test)]
                        self.host_rescans.push(machine.clone());
                        probe_machine(
                            &machine,
                            &self.hosts,
                            self.mgr.events(),
                            &self.scan_pool,
                            true,
                            0,
                        );
                    }
                    Command::Logout(_) | Command::RemoveUnmarkedKeys(_) => {
                        unreachable!("logout commands become key and logout effects in update")
                    }
                    Command::AdjustNavWidth(_) => {
                        width_changed = true;
                    }
                    Command::ToggleAutoHide => {
                        crate::app::prefs::save_auto_hide_nav(
                            &self.env.xmux_dir,
                            self.model.auto_hide_nav,
                        );
                    }
                    Command::PersistLastSession(address) => {
                        crate::app::prefs::save_last_session(&self.env.xmux_dir, &address);
                    }
                    Command::Attach(selection) => {
                        let started = std::time::Instant::now();
                        let nav = self.nav_size();
                        // select_attach picks the host's driver and hands it the intent.
                        let shown = select_attach(
                            &selection,
                            &mut crate::driver::DriverCtx {
                                registry: &mut self.registry,
                                hosts: &mut self.hosts,
                                instance_name: &self.instance_name,
                                mgr: &self.mgr,
                                worker: &self.worker,
                                pty_tx: &self.driver_pty_tx,
                                attach_seq: &mut self.attach_seq,
                                viewport: terminal_view_size(self.cols, self.body_rows, nav),
                            },
                        );
                        let key = display_key(&self.hosts, &selection);
                        if shown {
                            // Advance the display truth synchronously ONLY for a confirmed
                            // in-place path: a live grid for the key exists AND no reattach
                            // is in flight. A pending reattach KEEPS the prior session's grid
                            // (stale-while-revalidate) until the paint gate swaps it in.
                            let reattach_pending =
                                self.hosts.get(&selection.source).is_some_and(|h| {
                                    h.display.in_flight_contains(&key)
                                        || h.display.pending_paint_contains(&key)
                                });
                            if self.registry.contains(&key) && !reattach_pending {
                                let effects = update(
                                    &mut self.model,
                                    Msg::Action(crate::model::Action::ConfirmDisplay(
                                        selection.clone(),
                                    )),
                                );
                                debug_assert!(effects.is_empty());
                            }
                        }
                        DrawObserver::slow_step("select_attach", started);
                        self.dirty = true;
                        let session = &selection.session;
                        tracing::debug!(key, session, "selection");
                    }
                    Command::Quit => quit = true,
                    Command::RunOp(op) => spawn_op(op, &self.ops, &self.op_tx),
                    Command::RunLogin { .. } => {
                        unreachable!("login commands become StartLogin effects in update")
                    }
                },
            }
        }
        (quit, width_changed, rearm)
    }

    /// Runs a requested discovery once after the current input or control batch has
    /// finished its other effects and ensured the selected host.
    fn flush_rescan(&mut self) {
        if !std::mem::take(&mut self.rescan_pending) {
            return;
        }
        #[cfg(test)]
        {
            self.discovery_runs += 1;
        }
        let skip_machine = self.model.take_rescan_skip_machine();
        run_discovery(
            &self.env,
            &self.hosts,
            &self.mgr,
            &self.scan_pool,
            true,
            skip_machine.as_deref(),
        );
    }
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
/// mode is on, and no prefix interaction is active. Otherwise it uses the compact width
/// while collapsed and the user's natural width while expanded.
/// A prefix press is an interaction with xmux, so the nav comes back for it even under
/// auto-hide (the user needs the card numbers to jump, resize, or act on a card).
/// Pure so the focus/mode interaction is unit-testable; the loop owns the natural
/// width and the PTY resize on change.
fn reconciled_nav_width(
    terminal_focused: bool,
    auto_hide_nav: bool,
    prefix_active: bool,
    natural: u16,
    collapsed: bool,
    ui_prefix: &str,
) -> u16 {
    if terminal_focused && auto_hide_nav && !prefix_active {
        0
    } else if collapsed {
        crate::ui::switcher::collapsed_nav_width(ui_prefix)
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
#[cfg(test)]
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
fn sync_selection_from_switcher(model: &mut AppModel) -> bool {
    let previous = model.state.selection.clone();
    let effects = update(model, Msg::SyncSelection);
    debug_assert!(effects.is_empty());
    model.state.selection != previous
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
fn selection_attach_in_flight(hosts: &crate::model::Hosts, selection: &Selection) -> bool {
    if selection.is_empty() {
        return false;
    }
    let key = display_key(hosts, selection);
    hosts
        .get(&selection.source)
        .map(|h| h.display.in_flight_contains(&key) || h.display.pending_paint_contains(&key))
        .unwrap_or(false)
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
pub(crate) fn select_attach(sel: &Selection, ctx: &mut DriverCtx) -> bool {
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

/// (Re)opens the CONTROL metadata channel of the host the selection is on, so its push
/// stream streams that host's rows in. A CONTROL host's dropped client is reconnected
/// here; a POLL host is deliberately NOT ensured from the selection - its task is spawned
/// at launch and re-arms only on an explicit re-scan, so selecting its card never
/// re-enumerates it. An undetected host is skipped until a detection probe resolves its
/// mux.
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
    if switcher.current_host_blocked() {
        return;
    }
    if let Some(id) = switcher.current_host() {
        if let Some(host) = hosts.get(&id) {
            // Only a CONTROL host needs the selection to (re)open its metadata channel.
            // A POLL host's task is spawned at launch and re-arms only on an explicit
            // re-scan; selecting its card must not re-enumerate it.
            if host.detected
                && matches!(host.mux.event_source(), crate::model::EventSource::Control)
            {
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
    gate: std::sync::Arc<tokio::sync::Semaphore>,
) {
    tokio::spawn(async move {
        let Ok(_permit) = gate.acquire().await else {
            return;
        };
        let mut host = crate::model::Host::new(transport, mux);
        let err = host
            .detect_and_correct(&crate::model::source::ExecRunner)
            .await;
        let detected = host.detected.then_some(host.mux);
        let _ = tx.send(HostEvent::Scanned {
            source,
            detected,
            err,
        });
    });
}

/// Runs one MACHINE's mux discovery off the loop, over a clone of the transport that
/// reaches the machine, so the probes travel the same axes as everything else and carry
/// what its reachability probe and login established. The answer is emitted as
/// `HostEvent::MuxesFound`.
///
/// Fire and forget, and deliberately AFTER a machine connects: a remote probe is an ssh
/// round trip per mux, and only a reachable machine is worth asking. Nothing waits for
/// it, so a machine that never answers costs a task and no more. A permit is held on
/// `gate` (the shared scan pool) for the whole probe, so at most
/// [`crate::provision::config::SCAN_CONCURRENCY_MAX`]
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
        let muxes = crate::mux::host_muxes(&*transport, &crate::model::source::ExecRunner).await;
        // Every answer is sent, an empty one and a failed one too: a host that serves no
        // source yet is waiting on it, and either is what settles its card.
        let _ = tx.send(HostEvent::MuxesFound { machine, muxes });
    });
}

/// Re-resolves the ROSTER off the loop and hands the answer back as
/// `HostEvent::RosterResolved`.
///
/// Off the loop for the same reason mux discovery is: resolving reads the config and asks
/// each roster provider, and a provider is a subprocess (`tailscale status`, `wsl.exe -l`).
/// Running that on the loop would freeze rendering and input for its whole duration.
/// Bounded by the shared scan pool, like every other piece of discovery work.
///
/// A config that stopped PARSING resolves to defaults, which would silently narrow the
/// roster to this machine. That answer is dropped rather than applied: a typo must cost the
/// user a warning, never every remote card on screen.
fn spawn_roster_resolve(
    xmux_dir: std::path::PathBuf,
    local_socket: Option<String>,
    tx: tokio::sync::mpsc::UnboundedSender<HostEvent>,
    gate: std::sync::Arc<tokio::sync::Semaphore>,
) {
    tokio::spawn(async move {
        let Ok(_permit) = gate.acquire().await else {
            return;
        };
        let (roster, err) = crate::provision::env::resolve_roster(&xmux_dir, local_socket).await;
        if let Some(e) = err {
            tracing::warn!(error = %e, "config did not parse; keeping the roster as it stands");
            let _ = tx.send(HostEvent::RosterKept);
            return;
        }
        let _ = tx.send(HostEvent::RosterResolved {
            roster: Box::new(roster),
            rescan: true,
        });
    });
}

struct StartupResolution {
    roster: crate::provision::env::Roster,
    own_session: Option<crate::session::Address>,
    force_askpass: bool,
}

/// Runs the two launch roster answers on ONE task, so the full roster can never land
/// before the quick one and be reconciled away by it. `quick` carries the startup-only
/// facts; `full` is applied like a re-scan's roster.
fn spawn_startup_resolution_with<Q, R>(
    tx: tokio::sync::mpsc::UnboundedSender<HostEvent>,
    quick: Q,
    full: R,
) where
    Q: std::future::Future<Output = Option<StartupResolution>> + Send + 'static,
    R: std::future::Future<Output = Option<crate::provision::env::Roster>> + Send + 'static,
{
    tokio::spawn(async move {
        let Some(resolved) = quick.await else {
            return;
        };
        let _ = tx.send(HostEvent::StartupResolved {
            roster: Box::new(resolved.roster),
            own_session: resolved.own_session,
            force_askpass: resolved.force_askpass,
        });
        if let Some(roster) = full.await {
            let _ = tx.send(HostEvent::RosterResolved {
                roster: Box::new(roster),
                rescan: false,
            });
        }
    });
}

/// Resolves every launch fact that can wait on another process after the first frame.
///
/// The roster arrives in two answers. The first leaves out the neighbor scan, which
/// waits out every silent address on the network, so this machine's cards and the
/// configured hosts do not wait for it. The second is the full roster: the neighbors it
/// adds are probed as they land and every machine already on screen keeps its cards.
fn spawn_startup_resolution(
    xmux_dir: std::path::PathBuf,
    local_socket: Option<String>,
    tx: tokio::sync::mpsc::UnboundedSender<HostEvent>,
) {
    let quick_dir = xmux_dir.clone();
    let quick_socket = local_socket.clone();
    let quick = async move {
        let ((roster, err), force_askpass) = tokio::join!(
            crate::provision::env::resolve_roster_with(&quick_dir, quick_socket, false),
            crate::transport::auth::detect_force_askpass(),
        );
        if let Some(e) = err {
            tracing::warn!(error = %e, "config did not parse; keeping the startup roster");
            return None;
        }
        let own_session = crate::provision::env::own_session_address(&roster.sources);
        Some(StartupResolution {
            roster,
            own_session,
            force_askpass,
        })
    };
    let full = async move {
        let (roster, err) = crate::provision::env::resolve_roster(&xmux_dir, local_socket).await;
        (err.is_none() && roster.cfg.discovery.neighbors).then_some(roster)
    };
    spawn_startup_resolution_with(tx, quick, full);
}

/// Runs one machine's REACHABILITY probe off the loop - the shell probe over the
/// machine's raw shell, bounded by the shared `gate` - and carries the outcome back as
/// [`HostEvent::MachineProbed`]. A zero exit is connected (`err` `None`); ssh's own
/// failure line is the reason otherwise, classified locked (its auth-failure signature)
/// or unreachable at the card. It also warms the shared ControlMaster socket the
/// connected machine's later channels reuse without re-authenticating.
///
/// The probe asks WHICH SHELL answers rather than only whether one does
/// ([`crate::transport::vocab::SHELL_PROBE`]), because every later command is composed
/// for a shell family and the family costs no round trip of its own to learn. It goes
/// over the raw shell shape, not the mux-argv one: the probe's own `$0` has to reach
/// the remote unquoted, and per-arg quoting exists to stop exactly that.
fn spawn_machine_probe(
    machine: String,
    transport: Box<dyn crate::transport::Transport>,
    tx: tokio::sync::mpsc::UnboundedSender<HostEvent>,
    gate: std::sync::Arc<tokio::sync::Semaphore>,
    rescan: bool,
    probe: u64,
) {
    tokio::spawn(async move {
        let Ok(_permit) = gate.acquire().await else {
            return;
        };
        // A machine with no raw shell answers no probe of this shape; it is also never
        // reached here, since only a remote machine is probed at all.
        let Some(argv) = transport.raw_shell_argv(crate::transport::vocab::SHELL_PROBE) else {
            return;
        };
        let credential_generation = argv.credential_generation();
        let (err, shell) = match crate::model::source::ExecRunner
            .run_spec_output(&argv)
            .await
        {
            Ok((out, stderr)) => {
                if let Some(method) = argv
                    .auth_trace_allowed()
                    .then(|| crate::model::AuthMethod::from_ssh_stderr(&stderr))
                    .flatten()
                {
                    let _ = tx.send(HostEvent::AuthObserved {
                        machine: machine.clone(),
                        method,
                        credential_generation,
                    });
                }
                (
                    None,
                    Some(crate::transport::vocab::RemoteShell::from_probe(&out)),
                )
            }
            Err(e) => (Some(transport.probe_diagnostic(e.to_string())), None),
        };
        // This verdict decides whether the machine has cards at all: a failure makes every
        // source it serves unreachable, and hiding then takes them off the list. So it is
        // said out loud. A probe that answered is the routine case and says only what it
        // read; a probe that failed carries the reason, which is otherwise recoverable
        // only from the host's own unreachable screen.
        match &err {
            Some(reason) => {
                tracing::warn!(machine = %machine, error = %reason, "machine_probe_failed")
            }
            None => tracing::info!(machine = %machine, shell = ?shell, "machine_probed"),
        }
        let _ = tx.send(HostEvent::MachineProbed {
            machine,
            err,
            shell,
            password_supplied: argv.password_was_supplied(),
            credential_rejection_generation: argv.credential_rejection_generation(),
            credential_held: transport.has_credential(),
            credential_generation,
            current_credential_generation: transport.credential_generation(),
            rescan,
            probe,
        });
    });
}

/// Probes ONE machine's reachability. A local or WSL machine is on this box, so it is
/// reachable without an ssh round trip and connects inline; a remote machine is probed
/// off the loop under `gate`. `probe` is the number a login gave this probe, or zero.
fn probe_machine(
    machine: &str,
    hosts: &crate::model::Hosts,
    tx: tokio::sync::mpsc::UnboundedSender<HostEvent>,
    gate: &std::sync::Arc<tokio::sync::Semaphore>,
    rescan: bool,
    probe: u64,
) {
    let Some(transport) = hosts.host_transport(machine) else {
        return;
    };
    let machine = machine.to_string();
    if !transport.is_remote() {
        let _ = tx.send(HostEvent::MachineProbed {
            machine,
            err: None,
            // This box and a WSL distribution are POSIX by construction, so there is
            // nothing to read back: `None` leaves the transport's default standing.
            shell: None,
            password_supplied: false,
            credential_rejection_generation: None,
            credential_held: false,
            credential_generation: 0,
            current_credential_generation: 0,
            rescan,
            probe,
        });
        return;
    }
    spawn_machine_probe(
        machine,
        transport.clone_box(),
        tx,
        gate.clone(),
        rescan,
        probe,
    );
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
    skip_machine: Option<&str>,
) {
    for machine in hosts.machines() {
        if skip_machine == Some(machine.as_str()) {
            continue;
        }
        probe_machine(&machine, hosts, tx.clone(), gate, rescan, 0);
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
    model: &mut AppModel,
    source: &str,
    cols: u16,
    rows: u16,
    gate: &std::sync::Arc<tokio::sync::Semaphore>,
) {
    let Some(host) = hosts.get(source) else {
        return;
    };
    if !host.detected {
        if !model.detecting.contains(source) {
            let effects = update(model, Msg::DetectionStarted(source.to_string()));
            debug_assert!(effects.is_empty());
            spawn_host_detection(
                source.to_string(),
                host.transport.clone(),
                host.mux.clone_box(),
                mgr.events(),
                gate.clone(),
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

/// The re-scan discovery pass: probe every machine's reachability while re-resolving
/// the roster. A machine's
/// answer (`HostEvent::MachineProbed`) drives the rest - a connected machine detects and
/// dispatches its sources and, if auto, discovers its muxes; a locked or unreachable one
/// classifies its cards - so this pass opens no channel itself.
///
/// The roster is re-resolved concurrently; when it lands (`RosterResolved`),
/// the freshly ADDED machines are probed, so a machine that just came online turns into a
/// card without a restart. The machines standing right now are probed here regardless, so
/// a slow provider delays no card already on screen.
/// When a one-machine re-scan is already asking a machine, its probe supplies that
/// machine's answer to the full re-scan; discovery does not ask it again.
fn run_discovery(
    env: &Env,
    hosts: &crate::model::Hosts,
    mgr: &HostManager,
    gate: &std::sync::Arc<tokio::sync::Semaphore>,
    rescan: bool,
    skip_machine: Option<&str>,
) {
    if rescan {
        spawn_roster_resolve(
            env.xmux_dir.clone(),
            env.local_socket.clone(),
            mgr.events(),
            gate.clone(),
        );
    }
    probe_machines(hosts, mgr.events(), gate, rescan, skip_machine);
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
            tracing::info!(id, addr, tty, "tty_recorded");
            h.display_tty = crate::model::DisplayTty(Some(tty));
        }
    } else {
        // The marker fired but no registry entry has this id yet - diagnostic for a
        // capture that arrives before the attach is recorded (would silently drop).
        tracing::info!(id, tty, "tty_record_missed_no_addr");
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

/// The `xmux` (no subcommand) entry: the persistent app. Keeps one real attached
/// mux client per session alive and renders the selected one, with a control-mode
/// client per remote host for inventory/events/window-switch. It serves a picker
/// control socket so a headless driver can inject keys/text and dump the screen.
pub async fn run_app(env: Arc<Env>, requested_name: Option<String>) -> i32 {
    use crate::app::control::{serve_control, Cmd};
    use crate::display::term::TermGuard;
    use std::io::Read;
    use std::time::Duration;

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
    rt.prepare_and_draw(&mut term);
    spawn_startup_resolution(
        rt.env.xmux_dir.clone(),
        rt.env.local_socket.clone(),
        rt.mgr.events(),
    );

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

    // What the newest release is, from the answer recorded on disk. The flash carries
    // it, so the user reads it where every other transient notice appears rather than
    // in a banner of its own.
    //
    // Nothing here waits on the network: the line comes from the recorded answer, and
    // the refresh below runs on its own thread and only writes the file. So a launch
    // with no network paints exactly as fast as one with it, and the release that
    // arrived today is announced on tomorrow's launch.
    {
        let check_enabled = rt.env.with_roster(|r| r.cfg.update.check);
        let current = env!("CARGO_PKG_VERSION");
        if let Some(line) = crate::cli::update::notify::notice(
            crate::cli::update::notify::read(&rt.env.xmux_dir).as_ref(),
            current,
        ) {
            let effects = update(&mut rt.model, Msg::Notice(line));
            debug_assert!(effects.is_empty());
        }
        crate::cli::update::notify::refresh_in_background(&rt.env.xmux_dir, check_enabled);
    }

    let mut tick = tokio::time::interval(Duration::from_millis(SPINNER_FRAME_MS));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Frame timer: wakes the loop at the redraw cadence so a pending `dirty` draw is
    // flushed promptly even when no other event arrives.
    let mut frame_period = frame_interval(rt.model.max_fps);
    let mut frame =
        tokio::time::interval_at(tokio::time::Instant::now() + frame_period, frame_period);
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
            _ = frame.tick() => {
                from_frame = true;
                // Cheap live config reload: on the redraw cadence, stat the config
                // file and re-apply the `[ui]` presentation settings when it changed.
                // Marked dirty so the re-applied styles actually repaint this frame.
                //
                // Active view screens and spinners advance on the frame cadence even
                // when no new event arrives.
                if rt.on_config_check()
                    || rt.model.render_plan.view_screen.is_some()
                    || !rt.model.state.scanning.is_empty()
                    || !rt.model.state.chrome.spinner.is_empty()
                    || rt
                        .model
                        .state
                        .login_progress
                        .values()
                        .any(crate::model::LoginProgress::running)
                {
                    rt.dirty = true;
                }
            }
        }
        // Any real event (not the bare frame wake) means the UI may have changed.
        if !from_frame {
            rt.dirty = true;
        }
        let next_period = frame_interval(rt.model.max_fps);
        if next_period != frame_period {
            frame_period = next_period;
            frame =
                tokio::time::interval_at(tokio::time::Instant::now() + frame_period, frame_period);
            frame.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        }
    }

    // A resize within the last WIDTH_FLUSH_MS before quit leaves the debounce deadline
    // unreached, so the final width is still pending - persist it on the way out so the
    // nav width the user left with survives the next launch.
    // A login still on screen at quit is a child nobody will watch again: end it here so
    // the ssh it started goes with the app rather than outliving it. A pending nav width
    // is persisted through the same effect stream before the runtime tears I/O down.
    let effects = update(&mut rt.model, Msg::Shutdown);
    let _ = rt.execute_effects(effects);
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
    /// Bounds the discovery fan-out - the roster resolve, each machine's reachability
    /// probe, the mux discovery a connected machine runs, and each source's mux
    /// detection - at the configured `[discovery] scan-concurrency` (clamped to
    /// [`crate::provision::config::SCAN_CONCURRENCY_MAX`]), shared across the
    /// launch pass, every re-scan, and the roster-add path so they never flood together.
    scan_pool: Arc<tokio::sync::Semaphore>,
    registry: AttachRegistry,
    /// The off-loop attach worker. Its reply receiver is taken out in `run_app`
    /// ([`DisplayWorker::take_events`]); this keeps only the send half (`ensure`).
    worker: DisplayWorker,
    model: AppModel,
    attach_seq: u64,
    /// A clone of the loop's `PtyEvent` sender handed to drivers for off-loop probes.
    driver_pty_tx: tokio::sync::mpsc::UnboundedSender<PtyEvent>,
    op_tx: tokio::sync::mpsc::UnboundedSender<crate::ui::switcher::OpResult>,
    /// One gate per machine that a login's follow-ups and a logout's key search pass in
    /// turn. See [`KeyGates`].
    key_gates: KeyGates,
    cols: u16,
    body_rows: u16,
    term_input: crate::display::input::TermInput,
    nav_decoder: crate::display::decode::KeyDecoder,
    prefix: u8,
    draw_observer: DrawObserver,
    spinner_start: std::time::Instant,
    /// The last number given to a machine probe a login started.
    login_probes: u64,
    dirty: bool,
    last_draw: std::time::Instant,
    rescan_pending: bool,
    #[cfg(test)]
    discovery_runs: usize,
    /// The machines a one-machine re-scan probed, in order, for tests.
    #[cfg(test)]
    host_rescans: Vec<String>,
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

/// One async gate per machine, taken by a login's follow-ups and by a logout. A
/// registration appends this machine's key well after the login's verdict, so without the
/// gate a logout that started meanwhile could search the host, find nothing, and finish,
/// and the line would land afterwards and log the machine back in by key. A logout takes
/// the gate before its key search and keeps it until it clears the machine, so no
/// follow-up on that machine runs between the search and the removal either.
#[derive(Clone, Default)]
struct KeyGates {
    gates: Arc<std::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
    /// The gates a running logout holds, by machine.
    held: Arc<std::sync::Mutex<HashMap<String, tokio::sync::OwnedMutexGuard<()>>>>,
}

impl KeyGates {
    fn of(&self, machine: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.gates
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(machine.to_owned())
            .or_default()
            .clone()
    }

    /// Waits for `machine`'s gate and keeps it for the logout until [`Self::release`].
    async fn hold(&self, machine: &str) {
        let guard = self.of(machine).lock_owned().await;
        self.held
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(machine.to_owned(), guard);
    }

    /// Lets go of the gate a logout of `machine` held.
    fn release(&self, machine: &str) {
        self.held
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(machine);
    }
}

/// Runs a login's follow-ups at the machine's key gate. `follow_ups` is told whether to
/// do anything: a logout cancels the login before its key search takes the gate, so a
/// login that finds itself cancelled once it holds the gate does nothing, and one that
/// held the gate first finishes before the search begins.
async fn follow_ups_at_key_gate<T, F: std::future::Future<Output = T>>(
    gate: &tokio::sync::Mutex<()>,
    cancel: &std::sync::atomic::AtomicBool,
    follow_ups: impl FnOnce(bool) -> F,
) -> T {
    let _held = gate.lock().await;
    follow_ups(!cancel.load(std::sync::atomic::Ordering::Acquire)).await
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

/// Starts the login validation the pane submitted and hands the app its verdict.
///
/// The connection is not an op: it waits on a child, on a network, and on a server's
/// pace, so it runs on its own thread and only the handle that says it is running is
/// parked on [`State::login_run`]. Only what comes AFTER the verdict is an op - the
/// pane's after-login choice over what a working login left behind - and that folds back
/// through the same channel as any other, so the switcher reacts to one login result
/// however the login was had.
///
/// A machine with no login to run (it is local, or it is a WSL distribution) never opens
/// a PTY: its verdict is posted directly.
///
/// [`State::login_run`]: crate::state::State::login_run
/// One submitted login: what it reaches, which submission it is, and what follows a
/// connection that worked.
struct LoginRun {
    source: String,
    login: crate::transport::Login,
    attempt: u64,
    write_config: bool,
    register_key: bool,
}

fn start_login(
    run: LoginRun,
    mut password: crate::state::SecretInput,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    key_gate: Arc<tokio::sync::Mutex<()>>,
    op_sink: OpSink<'_>,
) {
    let LoginRun {
        source,
        login,
        attempt,
        write_config,
        register_key,
    } = run;
    let ops = op_sink.0.clone();
    let tx = op_sink.1.clone();
    let password = password.take_plain();
    tokio::spawn(async move {
        let unavailable = |connect| crate::ui::switcher::OpResult::Login {
            source: source.clone(),
            login: login.clone(),
            attempt,
            outcome: crate::ui::ops::LoginOutcome {
                connect,
                auth_method: None,
                output: String::new(),
                saved: None,
                registration: crate::ui::ops::RegistrationOutcome::NotRequested,
            },
        };
        let command = match ops.login_command(&source, &login, password).await {
            Ok(Some(command)) => command,
            Ok(None) => {
                let _ = tx.send(unavailable(crate::link::unlock::UnlockOutcome::Unavailable));
                return;
            }
            Err(error) => {
                let _ = tx.send(unavailable(crate::link::unlock::UnlockOutcome::Failed {
                    kind: crate::link::unlock::FailureKind::Other,
                    reason: error.to_string(),
                }));
                return;
            }
        };
        // Every step boundary rides the same channel as the verdict, so the app sees them
        // in the order they happened and before the result that ends the login.
        let progress = {
            let tx = tx.clone();
            let source = source.clone();
            std::sync::Arc::new(move |event| {
                let _ = tx.send(crate::ui::switcher::OpResult::LoginProgress {
                    source: source.clone(),
                    attempt,
                    event,
                });
            })
        };
        let asked = progress.clone();
        tracing::info!(source = %source, "login started");
        let done = crate::link::unlock::start_login_with_cancel(
            source.clone(),
            command,
            crate::link::unlock::LOGIN_IDLE,
            cancel.clone(),
            Box::new(move || asked(crate::model::LoginEvent::PasswordAsked)),
        );
        let conversation = done
            .await
            .unwrap_or_else(|_| crate::link::unlock::Conversation {
                outcome: crate::link::unlock::UnlockOutcome::Failed {
                    kind: crate::link::unlock::FailureKind::Other,
                    reason: "the login ended without a verdict".into(),
                },
                output: String::new(),
                shell: None,
                password_supplied: false,
                auth_method: None,
            });
        tracing::info!(source = %source, outcome = ?conversation.outcome, "login finished");
        progress(crate::model::LoginEvent::Verdict(
            conversation.outcome.clone(),
        ));
        let outcome = follow_ups_at_key_gate(&key_gate, &cancel, |go| {
            crate::ui::switcher::run_login_follow_ups(
                &source,
                &login,
                conversation,
                write_config && go,
                register_key && go,
                ops.as_ref(),
                progress.as_ref(),
            )
        })
        .await;
        let _ = tx.send(crate::ui::switcher::OpResult::Login {
            source,
            login,
            attempt,
            outcome,
        });
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
