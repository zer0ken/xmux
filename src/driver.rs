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
/// config (for a shared warm's source-built attach argv), the open control channel
/// (via `mgr`), the view size, and the attach seq. The driver owns the DECISION +
/// per-host display state; these stay supervisor-owned.
pub struct DriverCtx<'a> {
    pub registry: &'a mut AttachRegistry,
    pub hosts: &'a mut Hosts,
    pub worker: &'a DisplayWorker,
    pub mgr: &'a HostManager,
    pub env: &'a crate::env::Env,
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
                    ctx.registry,
                    ctx.worker,
                    &mut host.display,
                    ctx.attach_seq,
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
            ctx.registry.clear_grid(&key);
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

        // Window-row selection → move the session's active window. A fresh first attach
        // already folded the window into the attach argv; otherwise lower a select-window.
        if let Some(win) = sel.window {
            if !first_attach {
                lower_select_window(host, control, &sel.session, win);
            }
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
        let Some(src) = ctx.env.by_alias.get(source) else {
            return;
        };
        let (cols, rows) = terminal_view_size(ctx.cols, ctx.body_rows, ctx.tree_width);
        let Some(host) = ctx.hosts.get_mut(source) else {
            return;
        };
        let remote = host.transport.is_remote();
        match sessions.first() {
            Some(first)
                if !ctx.registry.contains(source)
                    && !host.display.in_flight.contains_key(source) =>
            {
                request_attach(
                    ctx.registry,
                    ctx.worker,
                    &mut host.display,
                    ctx.attach_seq,
                    source,
                    crate::cockpit::shared_display_attach_argv(remote, src, &first.name, None),
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

        ctx.registry.remove(&key);
        host.display.clear(&key);
        let mux_argv = host.mux.attach_plan(&sel.session, None);
        let (cmd, args) = host.transport.exec_argv(true, &mux_argv);
        let mut argv = vec![cmd];
        argv.extend(args);
        request_attach(
            ctx.registry,
            ctx.worker,
            &mut host.display,
            ctx.attach_seq,
            &key,
            argv,
            cols,
            rows,
        );
        host.display.set_shows(&key, &sel.session);

        // Window-row selection → move the session's active window. A psmux reattach never
        // folds the window into the attach argv, so always lower a select-window.
        if let Some(win) = sel.window {
            lower_select_window(host, control, &sel.session, win);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cockpit::Selection;

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
            !registry.contains("local"),
            "the stale display attachment is removed before reattach"
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
            !registry.contains("local"),
            "the stale display attachment is removed before reattach"
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
        let sessions = vec![sess("local", "api"), sess("local", "build")];

        let mut driver = TmuxDriver;
        {
            let mut ctx = DriverCtx {
                registry: &mut registry,
                hosts: &mut hosts,
                worker: &worker,
                mgr: &mgr,
                env: &env,
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

        let mut driver = TmuxDriver;
        {
            let mut ctx = DriverCtx {
                registry: &mut registry,
                hosts: &mut hosts,
                worker: &worker,
                mgr: &mgr,
                env: &env,
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

        let mut driver = PsmuxDriver;
        // A non-empty inventory: no warm, the on-demand attach stays.
        {
            let mut ctx = DriverCtx {
                registry: &mut registry,
                hosts: &mut hosts,
                worker: &worker,
                mgr: &mgr,
                env: &env,
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
}
