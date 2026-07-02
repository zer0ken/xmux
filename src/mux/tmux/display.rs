//! The tmux display driver: a shared-server mux keeps ONE PTY per host, warmed on the
//! first session and moved to another session with `switch-client`. `Tmux::driver`
//! constructs it, so mux selection lives in the tmux family, not a central match.

use std::sync::{Arc, Mutex};

use crate::app::runtime::{host_selection_key, request_attach, terminal_view_size};
use crate::display::grid::Grid;
use crate::driver::{lower_select_window, DriverCtx, MuxDriver};
use crate::model::Host;
use crate::model::Selection;

/// Shared-server mux (tmux): ONE PTY per host, warmed on the first session and moved to
/// another session with `switch-client`. `Tmux::driver` constructs it for a `Shared` host.
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
                let mux_argv = host.mux.attach_plan(&sel.session);
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
                .switch_in_place(&key, &sel.session, None)
                .map(|plan| crate::app::runtime::run_switch_plan(host, plan))
                .unwrap_or(false);
            if switched {
                tracing::info!(
                    host = %sel.source,
                    model = "shared",
                    decision = "switch",
                    reason = "recorded-tty",
                    session = %sel.session,
                    "display_show"
                );
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
                let mux_argv = host.mux.attach_plan(&sel.session);
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
            .grid(&crate::app::runtime::display_key(ctx.hosts, sel))
    }

    fn input(&mut self, sel: &Selection, bytes: Vec<u8>, ctx: &DriverCtx) {
        ctx.registry
            .input(&crate::app::runtime::display_key(ctx.hosts, sel), bytes);
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
                let mux_argv = host.mux.attach_plan(&first.name);
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

/// Folds the tmux record prefix into a shell-based shared attach's command (the last argv
/// element), so the attach shell records its OWN tty before exec'ing the attach — the
/// value a later `switch_in_place` reads back to target xmux's own display client, never
/// the user's own attached client. An attach that does not run through a host shell has
/// nowhere to run the snippet, so it is returned unchanged.
fn with_display_tty_record(mut argv: Vec<String>, host: &Host, host_key: &str) -> Vec<String> {
    if host.transport.runs_through_shell() {
        let prefix = super::record_prefix(host_key);
        if let Some(last) = argv.last_mut() {
            *last = format!("{prefix}{last}");
        }
    }
    argv
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::registry::AttachRegistry;
    use crate::host::HostManager;
    use crate::model::Selection;

    /// A REMOTE shared attach gets the mux's record prefix folded into its remote
    /// command (the last argv element), so the attach shell records its OWN tty for a
    /// later in-place switch — identifying xmux's display client and not the user's.
    #[test]
    fn remote_shared_attach_records_its_display_tty() {
        let host = crate::model::Host::new(
            crate::machine::ssh("jup".into(), String::new(), "linux".into()),
            crate::mux::for_binary("tmux"),
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
        let host =
            crate::model::Host::new(crate::machine::local(None), crate::mux::for_binary("tmux"));
        let argv = vec![
            "tmux".to_string(),
            "attach".to_string(),
            "-t".to_string(),
            "api".to_string(),
        ];
        let out = with_display_tty_record(argv.clone(), &host, "local");
        assert_eq!(out, argv, "local attach is untouched");
    }

    /// The tmux driver owns the shared-switch decision: the first `show()` for a host
    /// with no live attachment WARMS the one host-keyed PTY (records the shown session +
    /// an in-flight spawn), the shared behavior, now owned by the driver type.
    #[tokio::test(flavor = "current_thread")]
    async fn tmux_driver_show_warms_the_shared_host_pty_on_first_attach() {
        let mut hosts = crate::model::Hosts::default();
        hosts.insert(crate::model::Host::new(
            crate::machine::ssh("jup".into(), String::new(), "linux".into()),
            crate::mux::for_binary("tmux"),
        ));

        let (ptx, _prx) = tokio::sync::mpsc::unbounded_channel();
        let worker = crate::display::DisplayWorker::with_spawner(
            ptx,
            Box::new(|_argv, _cols, _rows, id, _events, _env_clear| {
                Ok(crate::display::attachment::fake_attachment(id))
            }),
        );
        let mut registry = AttachRegistry::new();
        let mut attach_seq = 0u64;
        let mgr = HostManager::new(tokio::sync::mpsc::unbounded_channel().0);
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

    /// The tmux driver's `sync` owns the shared warm/reap decision: an inventory with a
    /// first session WARMS the one host-keyed PTY when nothing is attached yet.
    #[tokio::test(flavor = "current_thread")]
    async fn tmux_driver_sync_warms_the_host_pty_on_the_first_session() {
        let mut hosts = crate::model::Hosts::default();
        hosts.insert(crate::model::Host::new(
            crate::machine::local(None),
            crate::mux::for_binary("tmux"),
        ));
        let (ptx, _prx) = tokio::sync::mpsc::unbounded_channel();
        let worker = crate::display::DisplayWorker::with_spawner(
            ptx,
            Box::new(|_argv, _cols, _rows, id, _events, _env_clear| {
                Ok(crate::display::attachment::fake_attachment(id))
            }),
        );
        let mut registry = AttachRegistry::new();
        let mut attach_seq = 0u64;
        let mgr = HostManager::new(tokio::sync::mpsc::unbounded_channel().0);
        let (cap_tx, _cap_rx) = tokio::sync::mpsc::unbounded_channel();
        let sessions = vec![
            crate::driver::tests::sess("local", "api"),
            crate::driver::tests::sess("local", "build"),
        ];

        let mut driver = TmuxDriver;
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
            crate::machine::local(None),
            crate::mux::for_binary("tmux"),
        ));
        hosts
            .get_mut("local")
            .unwrap()
            .display
            .set_shows("local", "api");
        let (ptx, _prx) = tokio::sync::mpsc::unbounded_channel();
        let worker = crate::display::DisplayWorker::with_spawner(
            ptx,
            Box::new(|_argv, _cols, _rows, id, _events, _env_clear| {
                Ok(crate::display::attachment::fake_attachment(id))
            }),
        );
        let mut registry = AttachRegistry::new();
        registry.insert("local", crate::display::attachment::fake_attachment(5));
        let mut attach_seq = 0u64;
        let mgr = HostManager::new(tokio::sync::mpsc::unbounded_channel().0);
        let (cap_tx, _cap_rx) = tokio::sync::mpsc::unbounded_channel();

        let mut driver = TmuxDriver;
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
            "no sessions ⇒ the host PTY is reaped"
        );
        assert_eq!(
            hosts.get("local").unwrap().display.shows("local"),
            None,
            "the reaped PTY's bookkeeping is forgotten"
        );
    }
}
