//! The psmux display driver: a per-session mux (one server per session) displayed
//! through ONE per-host PTY that is REATTACHED whenever the selected session changes.
//! `Psmux::driver` constructs it, so mux selection lives in the psmux family, not a
//! central match.

use std::sync::{Arc, Mutex};

use crate::app::runtime::{host_selection_key, request_attach, terminal_view_size};
use crate::display::grid::Grid;
use crate::driver::{lower_select_window, DriverCtx, MuxDriver};
use crate::model::Selection;

/// Per-session mux (psmux): one server per session, displayed through ONE per-host PTY
/// that is REATTACHED whenever the selected session changes (`new-session -A -s <name>`
/// routes to that session's own server - the 4a5f053 correctness fix). `Psmux::driver`
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
        let (cols, rows) = terminal_view_size(ctx.cols, ctx.body_rows, ctx.nav);
        let control = ctx.mgr.get(&sel.source);
        let Some(host) = ctx.hosts.get_mut(&sel.source) else {
            return false;
        };
        let key = host_selection_key(host);
        let live = ctx.registry.contains(&key);
        let pre_mismatch = host.display.shows(&key) != Some(sel.session.as_str());

        // REATTACH, always: request a fresh attach for the selected session on its own
        // per-session server. This is the ONLY way psmux reaches another session, because
        // psmux names no client from outside its own session, and a switch aimed at a tty
        // psmux cannot resolve moves whatever client the command's own route reached,
        // which is a separate psmux terminal of the user's. A reattach is addressed by
        // session NAME, so it can only ever land on xmux's own PTY, and it never trusts
        // display bookkeeping that says the client is already there. The stale attachment
        // is KEPT in the registry (not removed) so its grid stays on screen until the new
        // attach is confirmed: DisplayReady swaps it in and tears the stale one down
        // (stale-while-revalidate). At first display there is nothing to keep, so the view
        // is blank until Ready.
        let reason = if live { "reshow" } else { "no-live-client" };
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
                nav: crate::ui::switcher::NavSize::visible(crate::ui::switcher::NAV_WIDTH),
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

    /// The psmux driver's `sync` only reaps when empty - it never WARMS (per-session
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
                nav: crate::ui::switcher::NavSize::visible(crate::ui::switcher::NAV_WIDTH),
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
                nav: crate::ui::switcher::NavSize::visible(crate::ui::switcher::NAV_WIDTH),
            };
            driver.sync("local", &[], &mut ctx);
        }
        assert!(
            !registry.contains("local"),
            "an empty psmux inventory reaps the host PTY"
        );
    }

    /// A KNOWN client tty changes nothing: psmux still REATTACHES. psmux honors no
    /// client selector, so a switch could not be aimed at xmux's own client and would
    /// move whatever client the command's own route reached, which is a separate psmux
    /// terminal of the user's. Observable headless: a fresh reattach is requested even
    /// though a tty is on record, and the stale attachment is held meanwhile.
    #[tokio::test(flavor = "current_thread")]
    async fn psmux_driver_show_reattaches_even_when_a_client_tty_is_known() {
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
                nav: crate::ui::switcher::NavSize::visible(crate::ui::switcher::NAV_WIDTH),
            };
            assert!(driver.show(&sel, &mut ctx));
        }
        assert!(
            registry.contains("local"),
            "the stale attachment is HELD on screen while the fresh reattach is requested              (stale-while-revalidate)"
        );
        assert!(
            hosts
                .get("local")
                .unwrap()
                .display
                .in_flight_contains("local"),
            "a recorded tty grants no in-place switch: the session change reattaches"
        );
        assert_eq!(
            hosts.get("local").unwrap().display.shows("local"),
            Some("target"),
            "the shown session updates to the newly-attached session"
        );
    }

    /// FALLBACK (the 4a5f053 guard): with NO captured tty, even a live attachment
    /// REATTACHES (drop + new-session -A -s) rather than switching - so a box where the
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
        // No display_tty captured - the linchpin is missing.
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
                nav: crate::ui::switcher::NavSize::visible(crate::ui::switcher::NAV_WIDTH),
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
}
