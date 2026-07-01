//! The mux DRIVER boundary: the supervisor passes INTENT (display this
//! session+window) and reads back a grid; HOW (attach / switch-client / reattach
//! / select-window) lives behind `MuxDriver`. `DriverCtx` injects the
//! supervisor-owned spawn capability + registry so the driver owns the DECISION
//! and per-host display STATE while the PTY infrastructure stays in the loop.
//!
//! The per-mux drivers (`TmuxDriver`, `PsmuxDriver`) OWN the display decision: the
//! supervisor picks one off the host's model ([`driver_for`]) and never branches on the
//! mux kind itself. Each driver is zero-sized — the per-host display STATE stays in
//! `host.display`/`AttachRegistry`, borrowed through `DriverCtx`, so the boundary moved
//! the decision without relocating the state (a later step inverts that ownership).

use std::sync::{Arc, Mutex};

use crate::cockpit::{
    host_selection_key, request_attach, run_lowered, terminal_view_size, Selection,
};
use crate::display::DisplayWorker;
use crate::host::HostManager;
use crate::model::{Host, Hosts};
use crate::proxy::registry::AttachRegistry;
use crate::proxy::screen::Grid;

/// A supervisor INTENT: show this session (and optionally land on a window). The
/// generic shape the supervisor knows; the driver maps it onto mux mechanics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub session: String,
    pub window: Option<i64>,
}

impl Target {
    pub fn from_selection(sel: &Selection) -> Self {
        Target {
            session: sel.session.clone(),
            window: sel.window,
        }
    }
    pub fn into_selection(&self, source: &str) -> Selection {
        Selection {
            source: source.to_string(),
            session: self.session.clone(),
            window: self.window,
        }
    }
}

/// The generic capabilities the supervisor injects into a driver call: the off-loop
/// spawner, the attachment registry it fills, the transport-aware hosts, the source
/// config, the open control channel (via `mgr`), the view size, and the attach seq.
/// Attach argv is composed from each host's own `mux`/`transport` (the two axes), so a
/// driver reads `hosts`, not `env`, to build one. The driver owns the DECISION +
/// per-host display state; these stay supervisor-owned.
pub struct DriverCtx<'a> {
    pub registry: &'a mut AttachRegistry,
    pub hosts: &'a mut Hosts,
    pub worker: &'a DisplayWorker,
    pub mgr: &'a HostManager,
    pub env: &'a crate::env::Env,
    /// The off-loop event sink (a clone of the loop's `PtyEvent` channel). A driver may
    /// spawn a read-only probe that feeds a `PtyEvent` back to the loop — e.g. the psmux
    /// driver captures its display client's tty with an off-loop `list-clients` probe.
    pub pty_tx: &'a tokio::sync::mpsc::UnboundedSender<crate::proxy::run::PtyEvent>,
    pub attach_seq: &'a mut u64,
    pub cols: u16,
    pub body_rows: u16,
    pub tree_width: u16,
}

/// One mux driver per host: intent in, screen out.
pub trait MuxDriver {
    /// The mux identity this driver speaks for, for diagnostics + driver selection tests.
    fn kind(&self) -> &str;
    /// Make the selected session live and landed on its window. Returns true when the
    /// selection has a session to show (so the caller can confirm the display truth).
    fn show(&mut self, sel: &Selection, ctx: &mut DriverCtx) -> bool;
    /// The grid the supervisor renders for the selection, if a live attach exists.
    fn grid(&self, sel: &Selection, ctx: &DriverCtx) -> Option<Arc<Mutex<Grid>>>;
    /// Forward input bytes to the selected session's attachment.
    fn input(&mut self, sel: &Selection, bytes: Vec<u8>, ctx: &DriverCtx);
    /// Reconcile the host's display terminal with its current `sessions` (an inventory
    /// update — a remote `%`-event refresh or a local poll). Shared keeps ONE PTY per
    /// host: warm it on the first session, reap it when the host has no sessions.
    /// PerSession is selected on demand: only reap the host PTY when no sessions remain.
    fn sync(&mut self, source: &str, sessions: &[crate::session::Session], ctx: &mut DriverCtx);
}

/// Picks the host's mux driver off its server model — the DECISION is this dispatch,
/// not a `match` at the call site. Drivers are zero-sized, so a fresh value per call is
/// free; the per-host state lives in `host.display`/`AttachRegistry` (via `DriverCtx`).
pub fn driver_for(host: &Host) -> Box<dyn MuxDriver> {
    match host.mux.server_model() {
        crate::model::ServerModel::Shared => Box::new(TmuxDriver),
        crate::model::ServerModel::PerSession => Box::new(PsmuxDriver),
    }
}

/// Shared-server mux (tmux): ONE PTY per host, warmed on the first session and moved to
/// another session with `switch-client`. Owns the shared-switch arm of the old
/// `SelectOutcome` match.
pub struct TmuxDriver;

impl MuxDriver for TmuxDriver {
    fn kind(&self) -> &str {
        "tmux"
    }

    fn show(&mut self, sel: &Selection, ctx: &mut DriverCtx) -> bool {
        if sel.is_empty() {
            return false;
        }
        let (cols, rows) = terminal_view_size(ctx.cols, ctx.body_rows, ctx.tree_width);
        // The host's open `-CC` control connection, if any. switch-client/select-window
        // ride it instead of a fresh `ssh` per switch (the slow path on Windows, which
        // has no ssh ControlMaster — each exec re-handshakes, ~0.5s; see #2).
        let control = ctx.mgr.get(&sel.source);
        let Some(host) = ctx.hosts.get_mut(&sel.source) else {
            return false;
        };
        let key = host_selection_key(host);
        let pre_mismatch = host.display.shows(&key) != Some(sel.session.as_str());
        let already = ctx.registry.contains(&key);
        let first_attach = !already && !host.display.in_flight.contains_key(&key);

        if !already {
            // Off-loop first-attach: request the spawn ONLY if one is not already in flight.
            // Do NOT overwrite display.current while an attach is in flight — the in-flight
            // attach lands on its ORIGINAL target session, and the post-Ready re-evaluation
            // (see the Ready arm) issues a switch-client to the current selection. Overwriting
            // it here would make the switch-client guard think the PTY is already on the new
            // session.
            if !host.display.in_flight.contains_key(&key) {
                tracing::info!(
                    host = %sel.source,
                    model = "shared",
                    decision = "reattach",
                    reason = "no-live-client",
                    session = %sel.session,
                    "display_show"
                );
                // Build the argv (immutable mux/transport reads) BEFORE taking &mut display.
                let mux_argv = host.mux.attach_plan(&sel.session, sel.window);
                let (cmd, args) = host.transport.exec_argv(true, &mux_argv);
                let mut argv = vec![cmd];
                argv.extend(args);
                // A remote shared attach records its own tty before exec (for a later
                // in-place switch); the record snippet is a remote-shell mechanism, so a
                // local attach stays bare.
                argv = with_display_tty_record(argv, host, &key);
                let id = request_attach(
                    ctx.registry,
                    ctx.worker,
                    &mut host.display,
                    ctx.attach_seq,
                    &key,
                    argv,
                    cols,
                    rows,
                );
                tracing::info!(addr = %key, id, count = ctx.registry.len(), "attach_created");
                host.display.set_shows(&key, &sel.session);
            }
        } else if host.display.shows(&key) != Some(sel.session.as_str()) {
            // IN-PLACE SWITCH: move the live display client to the selected session by
            // reading, in-shell on the host, the tty the attach recorded to its per-host
            // file — so `switch-client -c <that tty>` moves xmux's OWN client and never
            // the user's own attached client (which `list-clients` cannot tell apart, the
            // class of bug a "first non-control client" capture caused). The grid is NOT
            // pre-cleared: the switch's repaint replaces it, so the prior session stays on
            // screen until the new content lands (stale-while-revalidate) — no blank frame.
            let switched = host
                .mux
                .switch_via_recorded_tty_cmd(&key, &sel.session)
                .and_then(|cmd| host.transport.raw_ssh_argv(&cmd))
                .map(|argv| {
                    tracing::info!(
                        host = %sel.source,
                        model = "shared",
                        decision = "switch",
                        reason = "recorded-tty",
                        session = %sel.session,
                        "display_show"
                    );
                    run_lowered(crate::model::LoweredSwitch::RawSsh(argv));
                })
                .is_some();
            if switched {
                host.display.set_shows(&key, &sel.session);
            } else if !host.display.in_flight.contains_key(&key) {
                // No in-place switch (a LOCAL shared host has no remote shell to record /
                // read the tty, or the mux uses no recorded-tty strategy): reattach the
                // host PTY to the new session. Reattach needs no tty and repaints fully;
                // the held grid stays on screen until DisplayReady swaps it.
                tracing::info!(
                    host = %sel.source,
                    model = "shared",
                    decision = "reattach",
                    reason = "no-switch",
                    session = %sel.session,
                    "display_show"
                );
                let mux_argv = host.mux.attach_plan(&sel.session, sel.window);
                let (cmd, args) = host.transport.exec_argv(true, &mux_argv);
                let mut argv = vec![cmd];
                argv.extend(args);
                argv = with_display_tty_record(argv, host, &key);
                let id = request_attach(
                    ctx.registry,
                    ctx.worker,
                    &mut host.display,
                    ctx.attach_seq,
                    &key,
                    argv,
                    cols,
                    rows,
                );
                tracing::info!(addr = %key, id, count = ctx.registry.len(), "attach_created");
                host.display.set_shows(&key, &sel.session);
            }
        } else {
            tracing::info!(
                host = %sel.source,
                model = "shared",
                decision = "warm",
                reason = "already-on",
                session = %sel.session,
                "display_show"
            );
        }

        // Window-row selection → move the session's active window. A fresh first attach
        // already folded the window into the attach argv; otherwise lower a select-window.
        if let Some(win) = sel.window {
            if !first_attach {
                lower_select_window(host, control, &sel.session, win);
            }
        }
        {
            let attached: Vec<String> = ctx
                .registry
                .addresses()
                .into_iter()
                .map(|addr| {
                    let host_id = addr.split_once('/').map_or(addr.as_str(), |(h, _)| h);
                    let shown = ctx
                        .hosts
                        .get(host_id)
                        .and_then(|h| h.display.shows(&addr))
                        .unwrap_or("?");
                    format!("{}={}", addr, shown)
                })
                .collect();
            tracing::debug!(
                count = ctx.registry.len(),
                attached = %attached.join(","),
                displayed = %sel.session,
                mismatch = pre_mismatch,
                "display_inventory"
            );
        }
        true
    }

    fn grid(&self, sel: &Selection, ctx: &DriverCtx) -> Option<Arc<Mutex<Grid>>> {
        ctx.registry
            .grid(&crate::cockpit::display_key(ctx.hosts, sel))
    }

    fn input(&mut self, sel: &Selection, bytes: Vec<u8>, ctx: &DriverCtx) {
        ctx.registry
            .input(&crate::cockpit::display_key(ctx.hosts, sel), bytes);
    }

    fn sync(&mut self, source: &str, sessions: &[crate::session::Session], ctx: &mut DriverCtx) {
        // One PTY per host. Warm it on the first session if not yet attached; reap it
        // (and forget its session) when the host has no sessions.
        let (cols, rows) = terminal_view_size(ctx.cols, ctx.body_rows, ctx.tree_width);
        let Some(host) = ctx.hosts.get_mut(source) else {
            return;
        };
        match sessions.first() {
            Some(first)
                if !ctx.registry.contains(source)
                    && !host.display.in_flight.contains_key(source) =>
            {
                // Compose the two axes: the MUX supplies the attach argv (attach_plan),
                // the MACHINE lowers it (ssh -t + exec / local -S) — the same composition
                // `show()` uses. A remote shared attach records its own tty before exec
                // (for a later in-place switch); local attaches and non-recording muxes
                // stay bare. (Immutable host reads before the &mut host.display below.)
                let mux_argv = host.mux.attach_plan(&first.name, None);
                let (cmd, args) = host.transport.interactive_attach_argv(&mux_argv, None);
                let mut argv = vec![cmd];
                argv.extend(args);
                let argv = with_display_tty_record(argv, host, source);
                request_attach(
                    ctx.registry,
                    ctx.worker,
                    &mut host.display,
                    ctx.attach_seq,
                    source,
                    argv,
                    cols,
                    rows,
                );
                host.display.set_shows(source, &first.name);
            }
            None => {
                ctx.registry.remove(source);
                host.display.clear(source);
            }
            _ => {}
        }
    }
}

/// Per-session mux (psmux): one server per session, displayed through ONE per-host PTY
/// that is REATTACHED whenever the selected session changes (`new-session -A -s <name>`
/// routes to that session's own server — the 4a5f053 correctness fix). Owns the
/// per-session-reattach arm of the old `SelectOutcome` match.
pub struct PsmuxDriver;

impl MuxDriver for PsmuxDriver {
    fn kind(&self) -> &str {
        "psmux"
    }

    fn show(&mut self, sel: &Selection, ctx: &mut DriverCtx) -> bool {
        if sel.is_empty() {
            return false;
        }
        let (cols, rows) = terminal_view_size(ctx.cols, ctx.body_rows, ctx.tree_width);
        let control = ctx.mgr.get(&sel.source);
        let Some(host) = ctx.hosts.get_mut(&sel.source) else {
            return false;
        };
        let key = host_selection_key(host);
        let live = ctx.registry.contains(&key);
        let already_on = host.display.shows(&key) == Some(sel.session.as_str());
        let pre_mismatch = !already_on;
        // The captured tty of xmux's OWN display client (the linchpin for an in-place
        // switch). Empty/absent ⇒ fall back to reattach so 4a5f053 never regresses.
        let tty = host.display_tty.0.clone().filter(|t| !t.is_empty());

        // The in-place world is entered ONLY with a live client AND its captured tty.
        // Without the tty we cannot target switch-client, so we stay on the proven
        // reattach path — which never trusts stale bookkeeping (it always reattaches).
        if let (true, Some(tty)) = (live, tty) {
            if already_on {
                // The live client already shows this session — only a window row needs
                // moving (no teardown, no switch).
                tracing::info!(
                    host = %sel.source,
                    model = "per-session",
                    decision = "warm",
                    reason = "already-on",
                    session = %sel.session,
                    "display_show"
                );
                if let Some(win) = sel.window {
                    lower_select_window(host, control, &sel.session, win);
                }
                {
                    let attached: Vec<String> = ctx
                        .registry
                        .addresses()
                        .into_iter()
                        .map(|addr| {
                            let host_id = addr.split_once('/').map_or(addr.as_str(), |(h, _)| h);
                            let shown = ctx
                                .hosts
                                .get(host_id)
                                .and_then(|h| h.display.shows(&addr))
                                .unwrap_or("?");
                            format!("{}={}", addr, shown)
                        })
                        .collect();
                    tracing::debug!(
                        count = ctx.registry.len(),
                        attached = %attached.join(","),
                        displayed = %sel.session,
                        mismatch = pre_mismatch,
                        "display_inventory"
                    );
                }
                return true;
            }
            // IN-PLACE SWITCH (the user's core want): switch the live client to a
            // DIFFERENT session. `switch-client -c <tty> -t <session>` crosses psmux's
            // per-session servers on the default socket (verified), with NO teardown. The
            // grid is NOT wiped: the previous session's content stays on screen until the
            // forced full repaint below fills it with the new session — no blank frame
            // between the two (stale-while-revalidate).
            tracing::info!(
                host = %sel.source,
                model = "per-session",
                decision = "switch",
                reason = "live+tty",
                session = %sel.session,
                "display_show"
            );
            let argv = host.mux.switch_client_argv(&tty, &sel.session);
            let (cmd, args) = host.transport.exec_argv(false, &argv);
            let mut v = vec![cmd];
            v.extend(args);
            run_lowered(crate::model::LoweredSwitch::Local(v));
            // Force a full repaint after the switch so the new session's content fills the
            // cleared grid (a switch-client alone may leave it blank; a reattach repaints
            // fully, and this gives the in-place switch the same).
            run_lowered(refresh_client_lowered(host, &tty));
            host.display.set_shows(&key, &sel.session);
            if let Some(win) = sel.window {
                lower_select_window(host, control, &sel.session, win);
            }
            {
                let attached: Vec<String> = ctx
                    .registry
                    .addresses()
                    .into_iter()
                    .map(|addr| {
                        let host_id = addr.split_once('/').map_or(addr.as_str(), |(h, _)| h);
                        let shown = ctx
                            .hosts
                            .get(host_id)
                            .and_then(|h| h.display.shows(&addr))
                            .unwrap_or("?");
                        format!("{}={}", addr, shown)
                    })
                    .collect();
                tracing::debug!(
                    count = ctx.registry.len(),
                    attached = %attached.join(","),
                    displayed = %sel.session,
                    mismatch = pre_mismatch,
                    "display_inventory"
                );
            }
            return true;
        }

        // REATTACH (first display / no captured tty / fallback): request a fresh attach
        // for the selected session on its own per-session server. The stale attachment is
        // KEPT in the registry (not removed) so its grid stays on screen until the new
        // attach is confirmed — DisplayReady swaps it in and tears the stale one down
        // (stale-while-revalidate). At first display there is nothing to keep, so the view
        // is blank until Ready.
        let reason = if !live { "no-live-client" } else { "no-tty" };
        tracing::info!(
            host = %sel.source,
            model = "per-session",
            decision = "reattach",
            reason,
            session = %sel.session,
            "display_show"
        );
        host.display.clear(&key);
        let mux_argv = host.mux.attach_plan(&sel.session, None);
        let remote = host.transport.is_remote();
        let (cmd, args) = host.transport.exec_argv(true, &mux_argv);
        let mut argv = vec![cmd];
        argv.extend(args);
        let id = request_attach(
            ctx.registry,
            ctx.worker,
            &mut host.display,
            ctx.attach_seq,
            &key,
            argv,
            cols,
            rows,
        );
        tracing::info!(addr = %key, id, count = ctx.registry.len(), "attach_created");
        host.display.set_shows(&key, &sel.session);

        // Capture xmux's display-client tty off-loop so the NEXT switch is in-place. A
        // LOCAL psmux attach runs the binary directly (no shell), so the remote shell
        // marker never fires; instead probe `list-clients` (read-only) and correlate the
        // client by the session it shows. If the probe finds nothing the tty stays unset
        // and the next switch simply reattaches again — no regression. A REMOTE psmux
        // host has its registry on the far side and is enumerated/displayed the generic
        // way; skip the local probe there.
        if !remote {
            spawn_local_psmux_tty_capture(
                host.mux.bin().to_string(),
                sel.session.clone(),
                id,
                ctx.pty_tx.clone(),
            );
        }

        if let Some(win) = sel.window {
            lower_select_window(host, control, &sel.session, win);
        }
        {
            let attached: Vec<String> = ctx
                .registry
                .addresses()
                .into_iter()
                .map(|addr| {
                    let host_id = addr.split_once('/').map_or(addr.as_str(), |(h, _)| h);
                    let shown = ctx
                        .hosts
                        .get(host_id)
                        .and_then(|h| h.display.shows(&addr))
                        .unwrap_or("?");
                    format!("{}={}", addr, shown)
                })
                .collect();
            tracing::debug!(
                count = ctx.registry.len(),
                attached = %attached.join(","),
                displayed = %sel.session,
                mismatch = pre_mismatch,
                "display_inventory"
            );
        }
        true
    }

    fn grid(&self, sel: &Selection, ctx: &DriverCtx) -> Option<Arc<Mutex<Grid>>> {
        ctx.registry
            .grid(&crate::cockpit::display_key(ctx.hosts, sel))
    }

    fn input(&mut self, sel: &Selection, bytes: Vec<u8>, ctx: &DriverCtx) {
        ctx.registry
            .input(&crate::cockpit::display_key(ctx.hosts, sel), bytes);
    }

    fn sync(&mut self, source: &str, sessions: &[crate::session::Session], ctx: &mut DriverCtx) {
        // Per-session attaches are selected on demand by `show`, not pre-warmed: sync
        // only tears down the host PTY when the host has no sessions left.
        if sessions.is_empty() {
            ctx.registry.remove(source);
            if let Some(host) = ctx.hosts.get_mut(source) {
                host.display.clear(source);
            }
        }
    }
}

/// Moves the session's active window server-side (the real attached client follows).
/// Over the host's open `-CC` connection if any (no fresh ssh handshake), else a lowered
/// select-window subprocess. Shared by both drivers' window-row handling.
fn lower_select_window(
    host: &Host,
    control: Option<&crate::host::HostClient>,
    session: &str,
    win: i64,
) {
    let target = crate::mux::window_target(session, win);
    if let Some(client) = control {
        client.select_window_on(&target);
    } else {
        let mux_argv = host.mux.select_window_plan(&target);
        let (cmd, args) = host.transport.exec_argv(false, &mux_argv);
        let mut argv = vec![cmd];
        argv.extend(args);
        run_lowered(crate::model::LoweredSwitch::Local(argv));
    }
}

/// A lowered `refresh-client -t <tty>` for the per-host display client, run right after a
/// lowered `switch-client` so the new session repaints the whole screen. A switch moves
/// the client but need not refill the cleared grid; a fresh attach repaints fully, and
/// this gives the in-place switch the same full repaint. The transport prepends ssh for a
/// remote host; psmux is tmux-compatible so the `refresh-client` verb is the same.
fn refresh_client_lowered(host: &Host, tty: &str) -> crate::model::LoweredSwitch {
    let argv = vec![
        host.mux.bin().to_string(),
        "refresh-client".to_string(),
        "-t".to_string(),
        tty.to_string(),
    ];
    let (cmd, args) = host.transport.exec_argv(false, &argv);
    let mut v = vec![cmd];
    v.extend(args);
    crate::model::LoweredSwitch::Local(v)
}

/// Folds the mux's display-tty record prefix into a REMOTE shared attach's command
/// (the last argv element), so the attach shell records its OWN tty before exec'ing
/// the attach — the value a later in-place `switch-client -c <tty>` targets (xmux's
/// own display client, never the user's own attached client). A LOCAL attach (no shell
/// to run the snippet) or a mux with no record strategy is returned unchanged.
fn with_display_tty_record(mut argv: Vec<String>, host: &Host, host_key: &str) -> Vec<String> {
    if host.transport.is_remote() {
        if let Some(prefix) = host.mux.display_tty_record_prefix(host_key) {
            if let Some(last) = argv.last_mut() {
                *last = format!("{prefix}{last}");
            }
        }
    }
    argv
}

/// The tty of the psmux client currently showing `session`, parsed from `list-clients`
/// output. psmux prints one client per line as `<tty>: <session>: <cmd> [<size>] …`
/// (its `-F` is unreliable, so the default format is parsed). The FIRST line whose
/// second `:`-field equals `session` is xmux's display client for that session — psmux
/// is one-server-per-session, so the client showing session S is on S's own server.
/// `None` when no line matches (the capture stays unset → the next switch reattaches).
pub(crate) fn parse_psmux_client_tty(out: &str, session: &str) -> Option<String> {
    out.lines().find_map(|line| {
        let mut parts = line.splitn(3, ':');
        let tty = parts.next()?.trim();
        let sess = parts.next()?.trim();
        (sess == session && !tty.is_empty()).then(|| tty.to_string())
    })
}

/// Captures xmux's local psmux display-client tty off the event loop, so the next
/// session switch can be IN PLACE (`switch-client -c <tty>`) instead of a reattach.
/// Runs a read-only `list-clients` a few times (the just-spawned attach needs a moment
/// to register a client), correlates the client by the session it shows, and feeds the
/// tty back as a `PtyEvent::DisplayTty { id, … }` so the existing capture pipeline
/// records it on the owning host. Read-only and identity-correct for psmux's
/// one-server-per-session model; never runs `switch-client -c ""` or moves a client.
fn spawn_local_psmux_tty_capture(
    bin: String,
    session: String,
    id: u64,
    pty_tx: tokio::sync::mpsc::UnboundedSender<crate::proxy::run::PtyEvent>,
) {
    use crate::source::Runner;
    // The addr string used for tty_probe events is the list-clients command target.
    let addr = format!("local/{}", session);
    tokio::spawn(async move {
        // The list-clients argv against the default socket; the client showing `session`
        // is on that session's own server, which the default socket coordinates.
        let argv = [bin, "list-clients".to_string()];
        for attempt in 0..5u8 {
            // Let the attach register a client before the first probe, then back off.
            tokio::time::sleep(std::time::Duration::from_millis(120 * (attempt as u64 + 1))).await;
            let Ok(out) = crate::source::ExecRunner.run(&argv[0], &argv[1..]).await else {
                tracing::debug!(addr = %addr, attempt, result = "none", "tty_probe");
                continue;
            };
            let text = String::from_utf8_lossy(&out);
            let result = parse_psmux_client_tty(&text, &session);
            tracing::debug!(
                addr = %addr,
                attempt,
                result = result.as_deref().unwrap_or("none"),
                "tty_probe"
            );
            if let Some(tty) = result {
                let _ = pty_tx.send(crate::proxy::run::PtyEvent::DisplayTty { id, tty });
                return;
            }
        }
        // No client matched in the window — leave the tty unset; the next switch
        // reattaches (no regression) and re-arms this capture.
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cockpit::Selection;

    /// A REMOTE shared attach gets the mux's record prefix folded into its remote
    /// command (the last argv element), so the attach shell records its OWN tty for a
    /// later in-place switch — identifying xmux's display client and not the user's.
    #[test]
    fn remote_shared_attach_records_its_display_tty() {
        let host = crate::model::Host::new(
            crate::model::Transport::Ssh {
                alias: "jup".into(),
                control_path: String::new(),
                os: "linux".into(),
            },
            crate::backend::for_binary("tmux"),
        );
        let argv = vec![
            "ssh".to_string(),
            "jup".to_string(),
            "tmux attach -t api".to_string(),
        ];
        let out = with_display_tty_record(argv, &host, "jup");
        let last = out.last().unwrap();
        assert!(last.starts_with("tty >"), "records its tty first: {out:?}");
        assert!(
            last.contains("tmux attach -t api"),
            "then runs the attach: {out:?}"
        );
    }

    /// A LOCAL attach has no shell to run the record snippet, so it is left bare —
    /// prepending the snippet would corrupt the local argv's session-name argument.
    #[test]
    fn local_shared_attach_is_not_prefixed() {
        let host = crate::model::Host::new(
            crate::model::Transport::Local { socket: None },
            crate::backend::for_binary("tmux"),
        );
        let argv = vec![
            "tmux".to_string(),
            "attach".to_string(),
            "-t".to_string(),
            "api".to_string(),
        ];
        let out = with_display_tty_record(argv.clone(), &host, "local");
        assert_eq!(out, argv, "local attach is untouched");
    }

    /// A minimal `Env` whose `by_alias` carries one source per alias, so a driver that
    /// builds a shared warm argv (`shared_display_attach_argv` via `ctx.env`) finds the
    /// source. Sources are local `cmd.exe` (the warm argv is then the bare attach).
    fn fake_env(aliases: &[&str]) -> crate::env::Env {
        let srcs: Vec<crate::source::Source> = aliases
            .iter()
            .map(|a| crate::source::Source {
                alias: (*a).into(),
                binary: "cmd.exe".into(),
                remote: false,
                control_path: String::new(),
                os: "windows".into(),
                socket: None,
                runner: None,
            })
            .collect();
        let by_alias = srcs.iter().map(|s| (s.alias.clone(), s.clone())).collect();
        crate::env::Env {
            cfg: crate::config::Config::default(),
            cfg_warnings: Vec::new(),
            srcs,
            by_alias,
            local_bin: "cmd.exe".into(),
            ui_prefix: "C-g".into(),
            xmux_dir: std::path::PathBuf::from("."),
        }
    }

    fn sess(source: &str, name: &str) -> crate::session::Session {
        crate::session::Session {
            source: source.into(),
            name: name.into(),
            windows: 1,
            attached: false,
            last_attached: 0,
        }
    }

    #[test]
    fn target_round_trips_through_selection() {
        let sel = Selection {
            source: "jup".into(),
            session: "api".into(),
            window: Some(2),
        };
        let t = Target::from_selection(&sel);
        assert_eq!(t.session, "api");
        assert_eq!(t.window, Some(2));
        assert_eq!(t.into_selection("jup"), sel);
    }

    #[test]
    fn drivers_are_object_safe() {
        // The whole point: a Box<dyn MuxDriver> must compile. If the trait gains a
        // non-dispatchable method this stops compiling.
        let _t: Box<dyn MuxDriver> = Box::new(TmuxDriver);
        let _p: Box<dyn MuxDriver> = Box::new(PsmuxDriver);
    }

    /// The decision is the driver's TYPE, not a `match` in the cockpit: a Shared host
    /// is driven by the tmux driver, a PerSession host by the psmux driver. This is the
    /// factory that replaces `match host.mux.select()` at the call sites.
    #[test]
    fn driver_for_picks_the_mux_specific_driver_by_server_model() {
        let tmux_host = crate::model::Host::new(
            crate::model::Transport::Ssh {
                alias: "jup".into(),
                control_path: String::new(),
                os: "linux".into(),
            },
            crate::backend::for_binary("tmux"),
        );
        let psmux_host = crate::model::Host::new(
            crate::model::Transport::Local { socket: None },
            crate::backend::for_binary("psmux"),
        );
        assert_eq!(driver_for(&tmux_host).kind(), "tmux");
        assert_eq!(driver_for(&psmux_host).kind(), "psmux");
    }

    /// The psmux driver owns the per-session reattach decision: `show()` REPLACES the
    /// single host-keyed display attachment (drop the stale one, request a fresh attach
    /// for the selected session). This is the 4a5f053 behavior, now owned by the driver
    /// type rather than a `SelectOutcome` match. Headless: a fake spawner, no live psmux.
    #[tokio::test(flavor = "current_thread")]
    async fn psmux_driver_show_replaces_the_display_attachment() {
        let mut hosts = crate::model::Hosts::default();
        hosts.insert(crate::model::Host::new(
            crate::model::Transport::Local { socket: None },
            crate::backend::for_binary("psmux"),
        ));
        hosts
            .get_mut("local")
            .unwrap()
            .display
            .set_shows("local", "old");

        let (ptx, _prx) = tokio::sync::mpsc::unbounded_channel();
        let worker = crate::display::DisplayWorker::with_spawner(
            ptx,
            Box::new(|_argv, _cols, _rows, id, _events| Ok(crate::proxy::run::fake_attachment(id))),
        );
        let mut registry = AttachRegistry::new();
        registry.insert("local", crate::proxy::run::fake_attachment(99));
        let mut attach_seq = 0u64;
        let mgr = HostManager::new(tokio::sync::mpsc::unbounded_channel().0);
        let env = fake_env(&["local"]);
        let (cap_tx, _cap_rx) = tokio::sync::mpsc::unbounded_channel();

        let sel = Selection {
            source: "local".into(),
            session: "target".into(),
            window: None,
        };

        let mut driver = PsmuxDriver;
        let shown = {
            let mut ctx = DriverCtx {
                registry: &mut registry,
                hosts: &mut hosts,
                worker: &worker,
                mgr: &mgr,
                env: &env,
                pty_tx: &cap_tx,
                attach_seq: &mut attach_seq,
                cols: 80,
                body_rows: 24,
                tree_width: crate::ui::switcher::TREE_WIDTH,
            };
            driver.show(&sel, &mut ctx)
        };

        assert!(shown, "a selection with a session has something to show");
        let h = hosts.get("local").unwrap();
        assert_eq!(
            h.display.shows("local"),
            Some("target"),
            "show records the newly-selected session on the host key"
        );
        assert!(
            h.display.in_flight.contains_key("local"),
            "show requests a fresh per-session reattach"
        );
        assert!(
            registry.contains("local"),
            "the stale attachment is HELD (kept on screen) while the fresh reattach is \
             requested; the swap + teardown happens at DisplayReady (stale-while-revalidate)"
        );
    }

    /// The tmux driver owns the shared-switch decision: the first `show()` for a host
    /// with no live attachment WARMS the one host-keyed PTY (records the shown session +
    /// an in-flight spawn), the shared behavior, now owned by the driver type.
    #[tokio::test(flavor = "current_thread")]
    async fn tmux_driver_show_warms_the_shared_host_pty_on_first_attach() {
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
        let worker = crate::display::DisplayWorker::with_spawner(
            ptx,
            Box::new(|_argv, _cols, _rows, id, _events| Ok(crate::proxy::run::fake_attachment(id))),
        );
        let mut registry = AttachRegistry::new();
        let mut attach_seq = 0u64;
        let mgr = HostManager::new(tokio::sync::mpsc::unbounded_channel().0);
        let env = fake_env(&["jup"]);
        let (cap_tx, _cap_rx) = tokio::sync::mpsc::unbounded_channel();

        let sel = Selection {
            source: "jup".into(),
            session: "api".into(),
            window: None,
        };

        let mut driver = TmuxDriver;
        let shown = {
            let mut ctx = DriverCtx {
                registry: &mut registry,
                hosts: &mut hosts,
                worker: &worker,
                mgr: &mgr,
                env: &env,
                pty_tx: &cap_tx,
                attach_seq: &mut attach_seq,
                cols: 80,
                body_rows: 24,
                tree_width: crate::ui::switcher::TREE_WIDTH,
            };
            driver.show(&sel, &mut ctx)
        };

        assert!(shown);
        let h = hosts.get("jup").unwrap();
        assert_eq!(
            h.display.shows("jup"),
            Some("api"),
            "the shared host PTY is keyed by host id and shows the first session"
        );
        assert!(
            h.display.in_flight.contains_key("jup"),
            "the first shared attach is requested off-loop"
        );
    }

    /// Through the driver boundary, a psmux selection still REPLACES the single
    /// host-keyed display attachment (the per-session reattach). This pins the seam's
    /// faithfulness independently of `select_attach` keeping its current name/shape, so
    /// a future driver that owns the decision must preserve the same observable effect
    /// (the 4a5f053 per-session attach behavior). Headless: a fake spawner, no live psmux.
    #[tokio::test(flavor = "current_thread")]
    async fn seam_show_replaces_the_psmux_display_attachment() {
        let mut hosts = crate::model::Hosts::default();
        hosts.insert(crate::model::Host::new(
            crate::model::Transport::Local { socket: None },
            crate::backend::for_binary("psmux"),
        ));
        // A stale attachment + bookkeeping for a different session: show() must drop it
        // and reattach for the selected session (psmux is one PTY per host, reattached).
        hosts
            .get_mut("local")
            .unwrap()
            .display
            .set_shows("local", "old");

        let (ptx, _prx) = tokio::sync::mpsc::unbounded_channel();
        let worker = crate::display::DisplayWorker::with_spawner(
            ptx,
            Box::new(|_argv, _cols, _rows, id, _events| Ok(crate::proxy::run::fake_attachment(id))),
        );
        let mut registry = AttachRegistry::new();
        registry.insert("local", crate::proxy::run::fake_attachment(99));
        let mut attach_seq = 0u64;
        let mgr = HostManager::new(tokio::sync::mpsc::unbounded_channel().0);
        let env = fake_env(&["local"]);
        let (cap_tx, _cap_rx) = tokio::sync::mpsc::unbounded_channel();

        let sel = Selection {
            source: "local".into(),
            session: "target".into(),
            window: None,
        };

        // Through the FACTORY dispatch (driver_for) + the concrete driver — the same
        // path the cockpit takes — so this pins the whole boundary, not one impl.
        let mut driver = driver_for(hosts.get("local").unwrap());
        let shown = {
            let mut ctx = DriverCtx {
                registry: &mut registry,
                hosts: &mut hosts,
                worker: &worker,
                mgr: &mgr,
                env: &env,
                pty_tx: &cap_tx,
                attach_seq: &mut attach_seq,
                cols: 80,
                body_rows: 24,
                tree_width: crate::ui::switcher::TREE_WIDTH,
            };
            driver.show(&sel, &mut ctx)
        };

        assert!(shown, "a selection with a session has something to show");
        let h = hosts.get("local").unwrap();
        assert_eq!(
            h.display.shows("local"),
            Some("target"),
            "show records the newly-selected session on the host key"
        );
        assert!(
            h.display.in_flight.contains_key("local"),
            "show requests a fresh per-session reattach"
        );
        assert!(
            registry.contains("local"),
            "the stale attachment is HELD (kept on screen) while the fresh reattach is \
             requested; the swap + teardown happens at DisplayReady (stale-while-revalidate)"
        );
    }

    /// The tmux driver's `sync` owns the shared warm/reap decision: an inventory with a
    /// first session WARMS the one host-keyed PTY when nothing is attached yet.
    #[tokio::test(flavor = "current_thread")]
    async fn tmux_driver_sync_warms_the_host_pty_on_the_first_session() {
        let mut hosts = crate::model::Hosts::default();
        hosts.insert(crate::model::Host::new(
            crate::model::Transport::Local { socket: None },
            crate::backend::for_binary("tmux"),
        ));
        let (ptx, _prx) = tokio::sync::mpsc::unbounded_channel();
        let worker = crate::display::DisplayWorker::with_spawner(
            ptx,
            Box::new(|_argv, _cols, _rows, id, _events| Ok(crate::proxy::run::fake_attachment(id))),
        );
        let mut registry = AttachRegistry::new();
        let mut attach_seq = 0u64;
        let mgr = HostManager::new(tokio::sync::mpsc::unbounded_channel().0);
        let env = fake_env(&["local"]);
        let (cap_tx, _cap_rx) = tokio::sync::mpsc::unbounded_channel();
        let sessions = vec![sess("local", "api"), sess("local", "build")];

        let mut driver = TmuxDriver;
        {
            let mut ctx = DriverCtx {
                registry: &mut registry,
                hosts: &mut hosts,
                worker: &worker,
                mgr: &mgr,
                env: &env,
                pty_tx: &cap_tx,
                attach_seq: &mut attach_seq,
                cols: 80,
                body_rows: 24,
                tree_width: crate::ui::switcher::TREE_WIDTH,
            };
            driver.sync("local", &sessions, &mut ctx);
        }
        let h = hosts.get("local").unwrap();
        assert_eq!(
            h.display.shows("local"),
            Some("api"),
            "shared sync warms the host PTY on the first session"
        );
        assert!(
            h.display.in_flight.contains_key("local"),
            "the warm is requested off-loop"
        );
    }

    /// The tmux driver's `sync` reaps the host PTY when the host has NO sessions left.
    #[tokio::test(flavor = "current_thread")]
    async fn tmux_driver_sync_reaps_the_host_pty_when_empty() {
        let mut hosts = crate::model::Hosts::default();
        hosts.insert(crate::model::Host::new(
            crate::model::Transport::Local { socket: None },
            crate::backend::for_binary("tmux"),
        ));
        hosts
            .get_mut("local")
            .unwrap()
            .display
            .set_shows("local", "api");
        let (ptx, _prx) = tokio::sync::mpsc::unbounded_channel();
        let worker = crate::display::DisplayWorker::with_spawner(
            ptx,
            Box::new(|_argv, _cols, _rows, id, _events| Ok(crate::proxy::run::fake_attachment(id))),
        );
        let mut registry = AttachRegistry::new();
        registry.insert("local", crate::proxy::run::fake_attachment(5));
        let mut attach_seq = 0u64;
        let mgr = HostManager::new(tokio::sync::mpsc::unbounded_channel().0);
        let env = fake_env(&["local"]);
        let (cap_tx, _cap_rx) = tokio::sync::mpsc::unbounded_channel();

        let mut driver = TmuxDriver;
        {
            let mut ctx = DriverCtx {
                registry: &mut registry,
                hosts: &mut hosts,
                worker: &worker,
                mgr: &mgr,
                env: &env,
                pty_tx: &cap_tx,
                attach_seq: &mut attach_seq,
                cols: 80,
                body_rows: 24,
                tree_width: crate::ui::switcher::TREE_WIDTH,
            };
            driver.sync("local", &[], &mut ctx);
        }
        assert!(
            !registry.contains("local"),
            "no sessions ⇒ the host PTY is reaped"
        );
        assert_eq!(
            hosts.get("local").unwrap().display.shows("local"),
            None,
            "the reaped PTY's bookkeeping is forgotten"
        );
    }

    /// The psmux driver's `sync` only reaps when empty — it never WARMS (per-session
    /// attaches are selected on demand by `show`, not pre-warmed). A non-empty inventory
    /// leaves the on-demand display attachment untouched.
    #[tokio::test(flavor = "current_thread")]
    async fn psmux_driver_sync_does_not_warm_and_reaps_only_when_empty() {
        let mut hosts = crate::model::Hosts::default();
        hosts.insert(crate::model::Host::new(
            crate::model::Transport::Local { socket: None },
            crate::backend::for_binary("psmux"),
        ));
        hosts
            .get_mut("local")
            .unwrap()
            .display
            .set_shows("local", "work");
        let (ptx, _prx) = tokio::sync::mpsc::unbounded_channel();
        let worker = crate::display::DisplayWorker::with_spawner(
            ptx,
            Box::new(|_argv, _cols, _rows, id, _events| Ok(crate::proxy::run::fake_attachment(id))),
        );
        let mut registry = AttachRegistry::new();
        registry.insert("local", crate::proxy::run::fake_attachment(7));
        let mut attach_seq = 0u64;
        let mgr = HostManager::new(tokio::sync::mpsc::unbounded_channel().0);
        let env = fake_env(&["local"]);
        let (cap_tx, _cap_rx) = tokio::sync::mpsc::unbounded_channel();

        let mut driver = PsmuxDriver;
        // A non-empty inventory: no warm, the on-demand attach stays.
        {
            let mut ctx = DriverCtx {
                registry: &mut registry,
                hosts: &mut hosts,
                worker: &worker,
                mgr: &mgr,
                env: &env,
                pty_tx: &cap_tx,
                attach_seq: &mut attach_seq,
                cols: 80,
                body_rows: 24,
                tree_width: crate::ui::switcher::TREE_WIDTH,
            };
            driver.sync("local", &[sess("local", "work")], &mut ctx);
        }
        assert!(
            registry.contains("local"),
            "a non-empty psmux inventory does not reap or re-warm the on-demand attach"
        );
        assert!(
            hosts.get("local").unwrap().display.in_flight.is_empty(),
            "psmux sync never requests a warm spawn"
        );
        // Now empty: the host PTY is reaped.
        {
            let mut ctx = DriverCtx {
                registry: &mut registry,
                hosts: &mut hosts,
                worker: &worker,
                mgr: &mgr,
                env: &env,
                pty_tx: &cap_tx,
                attach_seq: &mut attach_seq,
                cols: 80,
                body_rows: 24,
                tree_width: crate::ui::switcher::TREE_WIDTH,
            };
            driver.sync("local", &[], &mut ctx);
        }
        assert!(
            !registry.contains("local"),
            "an empty psmux inventory reaps the host PTY"
        );
    }

    /// THE USER'S CORE WANT: when a live psmux client + its captured tty are known,
    /// switching to a DIFFERENT session switches the client IN PLACE — no teardown, so
    /// no "(attaching…)". Observable headless: the live attachment is NOT removed and NO
    /// new reattach is requested (in_flight stays empty); the shown session updates.
    #[tokio::test(flavor = "current_thread")]
    async fn psmux_driver_show_switches_in_place_when_tty_known() {
        let mut hosts = crate::model::Hosts::default();
        hosts.insert(crate::model::Host::new(
            crate::model::Transport::Local { socket: None },
            crate::backend::for_binary("psmux"),
        ));
        {
            let h = hosts.get_mut("local").unwrap();
            h.display.set_shows("local", "old"); // a session is already displayed
            h.record_display_tty(Some("/dev/pts/3".into())); // and its client tty is known
        }
        let (ptx, _prx) = tokio::sync::mpsc::unbounded_channel();
        let worker = crate::display::DisplayWorker::with_spawner(
            ptx,
            Box::new(|_argv, _cols, _rows, id, _events| Ok(crate::proxy::run::fake_attachment(id))),
        );
        let mut registry = AttachRegistry::new();
        registry.insert("local", crate::proxy::run::fake_attachment(42)); // the live client
        let mut attach_seq = 0u64;
        let mgr = HostManager::new(tokio::sync::mpsc::unbounded_channel().0);
        let env = fake_env(&["local"]);
        let (cap_tx, _cap_rx) = tokio::sync::mpsc::unbounded_channel();

        let sel = Selection {
            source: "local".into(),
            session: "target".into(),
            window: None,
        };
        let mut driver = PsmuxDriver;
        {
            let mut ctx = DriverCtx {
                registry: &mut registry,
                hosts: &mut hosts,
                worker: &worker,
                mgr: &mgr,
                env: &env,
                pty_tx: &cap_tx,
                attach_seq: &mut attach_seq,
                cols: 80,
                body_rows: 24,
                tree_width: crate::ui::switcher::TREE_WIDTH,
            };
            assert!(driver.show(&sel, &mut ctx));
        }
        assert!(
            registry.contains("local"),
            "in-place switch keeps the live client (no teardown ⇒ no \"(attaching…)\")"
        );
        assert!(
            hosts.get("local").unwrap().display.in_flight.is_empty(),
            "in-place switch requests NO reattach"
        );
        assert_eq!(
            hosts.get("local").unwrap().display.shows("local"),
            Some("target"),
            "the shown session updates to the switched-to session"
        );
    }

    /// FALLBACK (the 4a5f053 guard): with NO captured tty, even a live attachment
    /// REATTACHES (drop + new-session -A -s) rather than switching — so a box where the
    /// tty is never captured behaves exactly like today (no regression).
    #[tokio::test(flavor = "current_thread")]
    async fn psmux_driver_show_reattaches_when_tty_unknown() {
        let mut hosts = crate::model::Hosts::default();
        hosts.insert(crate::model::Host::new(
            crate::model::Transport::Local { socket: None },
            crate::backend::for_binary("psmux"),
        ));
        hosts
            .get_mut("local")
            .unwrap()
            .display
            .set_shows("local", "old");
        // No display_tty captured — the linchpin is missing.
        let (ptx, _prx) = tokio::sync::mpsc::unbounded_channel();
        let worker = crate::display::DisplayWorker::with_spawner(
            ptx,
            Box::new(|_argv, _cols, _rows, id, _events| Ok(crate::proxy::run::fake_attachment(id))),
        );
        let mut registry = AttachRegistry::new();
        registry.insert("local", crate::proxy::run::fake_attachment(42));
        let mut attach_seq = 0u64;
        let mgr = HostManager::new(tokio::sync::mpsc::unbounded_channel().0);
        let env = fake_env(&["local"]);
        let (cap_tx, _cap_rx) = tokio::sync::mpsc::unbounded_channel();

        let sel = Selection {
            source: "local".into(),
            session: "target".into(),
            window: None,
        };
        let mut driver = PsmuxDriver;
        {
            let mut ctx = DriverCtx {
                registry: &mut registry,
                hosts: &mut hosts,
                worker: &worker,
                mgr: &mgr,
                env: &env,
                pty_tx: &cap_tx,
                attach_seq: &mut attach_seq,
                cols: 80,
                body_rows: 24,
                tree_width: crate::ui::switcher::TREE_WIDTH,
            };
            assert!(driver.show(&sel, &mut ctx));
        }
        assert!(
            registry.contains("local"),
            "no tty ⇒ the stale attachment is HELD on screen while a fresh reattach is \
             requested (stale-while-revalidate); the swap happens at DisplayReady"
        );
        assert!(
            hosts
                .get("local")
                .unwrap()
                .display
                .in_flight
                .contains_key("local"),
            "no tty ⇒ a fresh reattach is requested"
        );
        assert_eq!(
            hosts.get("local").unwrap().display.shows("local"),
            Some("target")
        );
    }

    /// The local tty capture correlates the `list-clients` line by SESSION: the client
    /// showing session S is xmux's display client (psmux is one-server-per-session). It
    /// picks that line's tty, ignores other clients, and yields None when no line shows
    /// the session (→ the tty stays unset and the next switch reattaches).
    #[test]
    fn parse_psmux_client_tty_correlates_the_client_by_session() {
        let out = "/dev/pts/0: other: pwsh [80x24] (utf8)\n\
                   /dev/pts/3: target: pwsh [80x24] (utf8)\n";
        assert_eq!(
            parse_psmux_client_tty(out, "target").as_deref(),
            Some("/dev/pts/3"),
            "the client showing the target session is xmux's display client"
        );
        assert_eq!(
            parse_psmux_client_tty(out, "other").as_deref(),
            Some("/dev/pts/0")
        );
        assert_eq!(
            parse_psmux_client_tty(out, "absent"),
            None,
            "no client shows that session ⇒ no tty (the switch reattaches instead)"
        );
        assert_eq!(parse_psmux_client_tty("", "target"), None);
    }
}
