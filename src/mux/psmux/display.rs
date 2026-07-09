//! The psmux display driver: a per-session mux (one server per session) displayed
//! through ONE per-host PTY that is REATTACHED whenever the selected session changes,
//! or switched in place when a live client with a captured tty is known. `Psmux::driver`
//! constructs it, so mux selection lives in the psmux family, not a central match.

use std::sync::{Arc, Mutex};

use crate::app::runtime::{host_selection_key, request_attach, terminal_view_size};
use crate::display::grid::Grid;
use crate::driver::{lower_select_window, DriverCtx, MuxDriver};
use crate::model::Selection;

/// Per-session mux (psmux): one server per session, displayed through ONE per-host PTY
/// that is REATTACHED whenever the selected session changes (`new-session -A -s <name>`
/// routes to that session's own server — the 4a5f053 correctness fix). `Psmux::driver`
/// constructs it for a `PerSession` host.
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
                crate::driver::log_display_inventory!(ctx, sel.session, pre_mismatch);
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
            // The mux authors the opaque switch plan (switch-client + a repaint-forcing
            // refresh-client); the driver runs it blind through the transport. The guard
            // above already proved a non-empty tty, so this is always `Some`.
            if let Some(plan) = host
                .mux
                .switch_in_place(&key, &sel.session, Some(tty.as_str()))
            {
                crate::app::runtime::run_switch_plan(host, plan);
            }
            host.display.set_shows(&key, &sel.session);
            if let Some(win) = sel.window {
                lower_select_window(host, control, &sel.session, win);
            }
            crate::driver::log_display_inventory!(ctx, sel.session, pre_mismatch);
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
        let mux_argv = host.mux.attach_plan(&sel.session);
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
            (cols, rows),
        );
        tracing::info!(addr = %key, id, count = ctx.registry.len(), "attach_created");
        host.display.set_shows(&key, &sel.session);

        // Capture xmux's display-client tty off-loop so the NEXT switch is in-place. A
        // LOCAL psmux attach runs the binary directly (no shell), so the remote shell
        // marker never fires; instead probe `list-clients` (read-only) and correlate the
        // client by the session it shows. If the probe finds nothing the tty stays unset
        // and the next switch simply reattaches again — no regression. The probe reads
        // THIS box's default socket, so it runs only where the local registry is
        // authoritative; a host whose registry is on the far side skips it.
        if host.transport.local_registry_scope() {
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
        crate::driver::log_display_inventory!(ctx, sel.session, pre_mismatch);
        true
    }

    fn grid(&self, sel: &Selection, ctx: &DriverCtx) -> Option<Arc<Mutex<Grid>>> {
        ctx.registry
            .grid(&crate::app::runtime::display_key(ctx.hosts, sel))
    }

    fn input(&mut self, sel: &Selection, bytes: Vec<u8>, ctx: &DriverCtx) {
        ctx.registry
            .input(&crate::app::runtime::display_key(ctx.hosts, sel), bytes);
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

/// The tty of xmux's OWN psmux display client for `session`, parsed from `list-clients`
/// (default format `<tty>: <session>: <cmd> [<size>] …`, since `-F` is unreliable).
/// Returns the tty ONLY when EXACTLY ONE client shows `session` — xmux is then that sole
/// client, so the tty is unambiguously ours. When ZERO match, OR MULTIPLE do (an external
/// psmux client is also attached to that session), `list-clients` cannot tell xmux's client
/// apart, so a `switch-client -c <tty>` could move the WRONG (external) client — return
/// `None`, leaving the tty unset so the next switch REATTACHES (safe) rather than guessing.
pub(crate) fn parse_psmux_client_tty(out: &str, session: &str) -> Option<String> {
    let mut ttys = out.lines().filter_map(|line| {
        let mut parts = line.splitn(3, ':');
        let tty = parts.next()?.trim();
        let sess = parts.next()?.trim();
        (sess == session && !tty.is_empty()).then(|| tty.to_string())
    });
    let first = ttys.next()?;
    // Unambiguous only: a second client on the same session means we can't identify ours.
    ttys.next().is_none().then_some(first)
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
    pty_tx: tokio::sync::mpsc::UnboundedSender<crate::display::attachment::PtyEvent>,
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
                let _ = pty_tx.send(crate::display::attachment::PtyEvent::DisplayTty { id, tty });
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
    use crate::display::registry::AttachRegistry;
    use crate::host::HostManager;
    use crate::model::Selection;

    /// The psmux driver owns the per-session reattach decision: `show()` REPLACES the
    /// single host-keyed display attachment (drop the stale one, request a fresh attach
    /// for the selected session). This is the 4a5f053 behavior, owned by the driver
    /// type. Headless: a fake spawner, no live psmux.
    #[tokio::test(flavor = "current_thread")]
    async fn psmux_driver_show_replaces_the_display_attachment() {
        let mut hosts = crate::model::Hosts::default();
        hosts.insert(crate::model::Host::new(
            crate::machine::local(None),
            crate::mux::for_binary("psmux"),
        ));
        hosts
            .get_mut("local")
            .unwrap()
            .display
            .set_shows("local", "old");

        let (ptx, _prx) = tokio::sync::mpsc::unbounded_channel();
        let worker = crate::display::DisplayWorker::with_spawner(
            ptx,
            Box::new(|_argv, _cols, _rows, id, _events, _env_clear| {
                Ok(crate::display::attachment::fake_attachment(id))
            }),
        );
        let mut registry = AttachRegistry::new();
        registry.insert("local", crate::display::attachment::fake_attachment(99));
        let mut attach_seq = 0u64;
        let mgr = HostManager::new(tokio::sync::mpsc::unbounded_channel().0);
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
            h.display.in_flight_contains("local"),
            "show requests a fresh per-session reattach"
        );
        assert!(
            registry.contains("local"),
            "the stale attachment is HELD (kept on screen) while the fresh reattach is \
             requested; the swap + teardown happens at DisplayReady (stale-while-revalidate)"
        );
    }

    /// The psmux driver's `sync` only reaps when empty — it never WARMS (per-session
    /// attaches are selected on demand by `show`, not pre-warmed). A non-empty inventory
    /// leaves the on-demand display attachment untouched.
    #[tokio::test(flavor = "current_thread")]
    async fn psmux_driver_sync_does_not_warm_and_reaps_only_when_empty() {
        let mut hosts = crate::model::Hosts::default();
        hosts.insert(crate::model::Host::new(
            crate::machine::local(None),
            crate::mux::for_binary("psmux"),
        ));
        hosts
            .get_mut("local")
            .unwrap()
            .display
            .set_shows("local", "work");
        let (ptx, _prx) = tokio::sync::mpsc::unbounded_channel();
        let worker = crate::display::DisplayWorker::with_spawner(
            ptx,
            Box::new(|_argv, _cols, _rows, id, _events, _env_clear| {
                Ok(crate::display::attachment::fake_attachment(id))
            }),
        );
        let mut registry = AttachRegistry::new();
        registry.insert("local", crate::display::attachment::fake_attachment(7));
        let mut attach_seq = 0u64;
        let mgr = HostManager::new(tokio::sync::mpsc::unbounded_channel().0);
        let (cap_tx, _cap_rx) = tokio::sync::mpsc::unbounded_channel();

        let mut driver = PsmuxDriver;
        // A non-empty inventory: no warm, the on-demand attach stays.
        {
            let mut ctx = DriverCtx {
                registry: &mut registry,
                hosts: &mut hosts,
                worker: &worker,
                mgr: &mgr,
                pty_tx: &cap_tx,
                attach_seq: &mut attach_seq,
                cols: 80,
                body_rows: 24,
                tree_width: crate::ui::switcher::TREE_WIDTH,
            };
            driver.sync(
                "local",
                &[crate::driver::tests::sess("local", "work")],
                &mut ctx,
            );
        }
        assert!(
            registry.contains("local"),
            "a non-empty psmux inventory does not reap or re-warm the on-demand attach"
        );
        assert!(
            hosts.get("local").unwrap().display.in_flight_is_empty(),
            "psmux sync never requests a warm spawn"
        );
        // Now empty: the host PTY is reaped.
        {
            let mut ctx = DriverCtx {
                registry: &mut registry,
                hosts: &mut hosts,
                worker: &worker,
                mgr: &mgr,
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
    /// the terminal view never goes blank. Observable headless: the live attachment is NOT removed and NO
    /// new reattach is requested (in_flight stays empty); the shown session updates.
    #[tokio::test(flavor = "current_thread")]
    async fn psmux_driver_show_switches_in_place_when_tty_known() {
        let mut hosts = crate::model::Hosts::default();
        hosts.insert(crate::model::Host::new(
            crate::machine::local(None),
            crate::mux::for_binary("psmux"),
        ));
        {
            let h = hosts.get_mut("local").unwrap();
            h.display.set_shows("local", "old"); // a session is already displayed
            h.record_display_tty(Some("/dev/pts/3".into())); // and its client tty is known
        }
        let (ptx, _prx) = tokio::sync::mpsc::unbounded_channel();
        let worker = crate::display::DisplayWorker::with_spawner(
            ptx,
            Box::new(|_argv, _cols, _rows, id, _events, _env_clear| {
                Ok(crate::display::attachment::fake_attachment(id))
            }),
        );
        let mut registry = AttachRegistry::new();
        registry.insert("local", crate::display::attachment::fake_attachment(42)); // the live client
        let mut attach_seq = 0u64;
        let mgr = HostManager::new(tokio::sync::mpsc::unbounded_channel().0);
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
            "in-place switch keeps the live client (no teardown ⇒ terminal view never blanks)"
        );
        assert!(
            hosts.get("local").unwrap().display.in_flight_is_empty(),
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
            crate::machine::local(None),
            crate::mux::for_binary("psmux"),
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
            Box::new(|_argv, _cols, _rows, id, _events, _env_clear| {
                Ok(crate::display::attachment::fake_attachment(id))
            }),
        );
        let mut registry = AttachRegistry::new();
        registry.insert("local", crate::display::attachment::fake_attachment(42));
        let mut attach_seq = 0u64;
        let mgr = HostManager::new(tokio::sync::mpsc::unbounded_channel().0);
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
                .in_flight_contains("local"),
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

    #[test]
    fn parse_psmux_client_tty_is_none_when_ambiguous() {
        // Two clients on the SAME session (xmux's own + an external psmux client): we
        // cannot tell them apart from list-clients, so a switch-client -c <tty> could move
        // the wrong one. Return None → the switch reattaches (safe) instead of guessing.
        let dup = "/dev/pts/3: target: pwsh [80x24] (utf8)\n\
                   /dev/pts/9: target: pwsh [80x24] (utf8)\n";
        assert_eq!(
            parse_psmux_client_tty(dup, "target"),
            None,
            "an external client sharing the session makes the tty ambiguous ⇒ reattach"
        );
        // A single client for the session is still unambiguously ours.
        let one = "/dev/pts/3: target: pwsh [80x24] (utf8)\n\
                   /dev/pts/9: other: pwsh [80x24] (utf8)\n";
        assert_eq!(
            parse_psmux_client_tty(one, "target").as_deref(),
            Some("/dev/pts/3"),
            "the sole client on the session is xmux's own"
        );
    }
}
