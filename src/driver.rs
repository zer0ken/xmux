//! The mux DRIVER boundary: the supervisor passes INTENT (display this session)
//! and reads back a grid; HOW (attach / switch-client / reattach) lives behind
//! `MuxDriver`. `DriverCtx` injects the
//! supervisor-owned spawn capability + registry so the driver owns the DECISION
//! and per-host display STATE while the PTY infrastructure stays in the loop.
//!
//! The per-mux drivers (`TmuxDriver`, `PsmuxDriver`) live in their mux implementation
//! (`crate::mux::{tmux, psmux}`) and OWN the display decision. Each mux
//! constructs its own driver via [`Mux::driver`](crate::mux::Mux::driver),
//! so [`driver_for`] is a thin mux-agnostic wrapper (`host.mux.driver()`) that names no
//! concrete mux type. Each driver is zero-sized — the per-host display STATE lives in
//! `host.display`/`AttachRegistry`, borrowed through `DriverCtx`, so the driver owns the
//! DECISION while that state stays supervisor-owned.

use std::sync::{Arc, Mutex};

use crate::display::grid::Grid;
use crate::display::registry::AttachRegistry;
use crate::display::DisplayWorker;
use crate::model::Selection;
use crate::model::{Host, Hosts};

/// A supervisor INTENT: show this session. The generic shape the supervisor knows;
/// the driver maps it onto mux mechanics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub session: String,
}

impl Target {
    pub fn from_selection(sel: &Selection) -> Self {
        Target {
            session: sel.session.clone(),
        }
    }
    pub fn into_selection(&self, source: &str) -> Selection {
        Selection {
            source: source.to_string(),
            session: self.session.clone(),
        }
    }
}

/// The generic capabilities the supervisor injects into a driver call: the off-loop
/// spawner, the attachment registry it fills, the transport-aware hosts, the open
/// control channel (via `mgr`), the view size, and the attach seq. Attach argv is
/// composed from each host's own `mux`/`transport` (the two axes), so a driver reads
/// `hosts` to build one. The driver owns the DECISION + per-host display state; these
/// stay supervisor-owned.
pub struct DriverCtx<'a> {
    pub registry: &'a mut AttachRegistry,
    pub hosts: &'a mut Hosts,
    pub worker: &'a DisplayWorker,
    /// The off-loop event sink (a clone of the loop's `PtyEvent` channel). A driver may
    /// spawn a read-only probe that feeds a `PtyEvent` back to the loop — e.g. the psmux
    /// driver captures its display client's tty with an off-loop `list-clients` probe.
    pub pty_tx: &'a tokio::sync::mpsc::UnboundedSender<crate::display::attachment::PtyEvent>,
    pub attach_seq: &'a mut u64,
    pub cols: u16,
    pub body_rows: u16,
    /// The nav's live size (the width the user set, the width on screen, the `Top` band
    /// height), so the driver sizes the PTY to the same terminal region the renderer
    /// draws, in either layout.
    pub nav: crate::ui::switcher::NavSize,
}

/// One mux driver per host: intent in, screen out.
pub trait MuxDriver {
    /// The mux identity this driver speaks for, for diagnostics + driver selection tests.
    fn kind(&self) -> &str;
    /// Make the selected session live. Returns true when the selection has a session
    /// to show (so the caller can confirm the display truth).
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

/// The host's mux driver — the DECISION is a Mux method (`host.mux.driver()`), not a
/// `match` at the call site. Each mux constructs its OWN driver, so mux selection
/// lives in the mux implementation (`crate::mux::{tmux, psmux}`), never a central match here.
/// Drivers are zero-sized, so a fresh value per call is free; the per-host state lives in
/// `host.display`/`AttachRegistry` (via `DriverCtx`).
pub fn driver_for(host: &Host) -> Box<dyn MuxDriver> {
    host.mux.driver()
}

/// Emit the `display_inventory` debug event: for every attachment in the registry,
/// `addr=<the session it shows>`, tagged with the currently-`displayed` session and the
/// pre-show `mismatch` flag. Shared by both drivers at each display-decision branch.
///
/// A `macro_rules!` rather than a `fn` so the `tracing` event's target stays the per-mux
/// driver module that invokes it (`module_path!()` resolves at the expansion site). The
/// mux Working Notes document `XMUX_LOG=xmux::mux::tmux=debug` / `xmux::mux::psmux=debug`
/// as the way to filter these; a `fn` would retarget every event to `xmux::driver` and
/// break that filter.
macro_rules! log_display_inventory {
    ($ctx:expr, $displayed:expr, $mismatch:expr $(,)?) => {{
        let ctx = &*$ctx;
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
            displayed = %$displayed,
            mismatch = $mismatch,
            "display_inventory"
        );
    }};
}
pub(crate) use log_display_inventory;

/// The session xmux's own display client is on, as the CLIENT ITSELF reports it: the mux
/// names the environment variable its client carries that in, and the live attach child
/// is read for it. `None` is NO SIGNAL, and every reason for it is a reason to trust
/// nothing: the mux's client does not carry its session in its environment, the host is
/// not this machine, no client is attached for this host, or the child gave no answer.
///
/// It answers the question no control channel can for a mux that switches sessions inside
/// its client process, and it answers it about xmux's OWN client and no other, because
/// the child read is the one xmux spawned. Mux-blind: the variable's NAME is the mux's
/// knowledge and the reachability of the process is the transport's, so this composes the
/// two and names neither a mux nor a variable itself.
///
/// The transport gate is the honesty rule, not an optimization. A process's environment is
/// readable only on the machine it runs on, so a host reached over ssh or through a WSL
/// distribution has no such source of truth, and the local registry scope is what says whether a
/// host's processes are this machine's processes.
pub fn live_client_session(host: &Host, registry: &AttachRegistry) -> Option<String> {
    let (key, var) = session_truth_source(host)?;
    registry.child_env(&key, var)
}

/// Where a host's display client can be READ for the session it is on: the display key
/// its attachment is registered under, and the environment variable the mux carries the
/// session in. `None` when the host has no source of truth at all, which is the honesty gate the
/// read is built on and is decided WITHOUT looking at any attachment, so it answers the
/// same before, during, and after one.
///
/// Two independent conditions, and both must hold. The MUX has to say its client carries
/// its session where it can be read; a mux whose client does not is not read for it. And
/// the host has to be THIS MACHINE, because a process's environment is readable only on
/// the machine it runs on: a psmux over ssh or inside a WSL distribution runs its client
/// on the far side, where nothing here can look, and the local registry scope is what says
/// whether a host's processes are this machine's processes. A host that fails either one has NO
/// source of truth: nothing is read for it, and where its client sits is claimed from nothing
/// else.
pub(crate) fn session_truth_source(host: &Host) -> Option<(String, &str)> {
    if !host.transport.local_registry_scope() {
        return None;
    }
    let var = host.mux.display_session_env()?;
    Some((crate::app::runtime::host_selection_key(host), var))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::model::Selection;

    pub(crate) fn sess(source: &str, name: &str) -> crate::session::Session {
        crate::session::Session {
            source: source.into(),
            name: name.into(),
            mux: String::new(),
            windows: 1,
            attached: false,
        }
    }

    #[test]
    fn target_round_trips_through_selection() {
        let sel = Selection {
            source: "jup".into(),
            session: "api".into(),
        };
        let t = Target::from_selection(&sel);
        assert_eq!(t.session, "api");
        assert_eq!(t.into_selection("jup"), sel);
    }

    /// The LOCAL-ONLY gate, for every mux that names a variable. Such a client's session
    /// is read out of the client PROCESS, and a process is readable only on the machine
    /// it runs on. An ssh host and a WSL distribution both run their client on the far
    /// side, so neither has a source of truth to read, and this must be decided from the host
    /// alone - never from a value that happens to be readable here, which would report
    /// THIS box's session for a remote one.
    #[test]
    fn only_a_host_on_this_box_has_a_client_to_read() {
        for (bin, expected) in [
            ("psmux", "PSMUX_SESSION_NAME"),
            ("zellij", "ZELLIJ_SESSION_NAME"),
        ] {
            let local = crate::model::Host::new(
                crate::transport::local(None),
                crate::mux::for_binary(bin).unwrap(),
            );
            let (key, var) = session_truth_source(&local).expect("this box's own client");
            assert_eq!(key, "local", "read under the host's display key");
            assert_eq!(var, expected);

            let remote = crate::model::Host::new(
                crate::transport::ssh("prod".into(), String::new(), "linux".into()),
                crate::mux::for_binary(bin).unwrap(),
            );
            assert!(
                session_truth_source(&remote).is_none(),
                "an ssh {bin} client runs on the far side, where no process can be read"
            );

            let wsl = crate::model::Host::new(
                crate::transport::wsl("Ubuntu-24.04".into()),
                crate::mux::for_binary(bin).unwrap(),
            );
            assert!(
                session_truth_source(&wsl).is_none(),
                "a WSL {bin} client runs inside the distribution, not on this box"
            );
        }
    }

    /// A mux whose client does not carry its session in its environment has nothing to
    /// read even on this machine: the mux half of the gate refuses it whatever the transport
    /// answers, so the two conditions are independent and both are load-bearing.
    #[test]
    fn a_mux_that_names_no_variable_has_no_client_to_read() {
        for bin in ["tmux", "abduco", "screen"] {
            let host = crate::model::Host::new(
                crate::transport::local(None),
                crate::mux::for_binary(bin).unwrap(),
            );
            assert!(
                session_truth_source(&host).is_none(),
                "{bin} names no session variable on its client"
            );
        }
    }

    /// No attachment for the host means no client, and no client means NO SIGNAL - not a
    /// session name guessed from anywhere else. This is the leg that keeps a torn-down or
    /// not-yet-spawned display from being read as an answer.
    #[test]
    fn a_host_with_no_attachment_reports_no_session() {
        let host = crate::model::Host::new(
            crate::transport::local(None),
            crate::mux::for_binary("psmux").unwrap(),
        );
        let registry = AttachRegistry::new();
        assert!(live_client_session(&host, &registry).is_none());
    }

    #[test]
    fn drivers_are_object_safe() {
        // The whole point: a Box<dyn MuxDriver> must compile. If the trait gains a
        // non-dispatchable method this stops compiling. Obtained via the production
        // path (`Mux::driver()` through `driver_for`) so this seam names no
        // concrete driver type — those live in `crate::mux::{tmux, psmux}`.
        let tmux_host = crate::model::Host::new(
            crate::transport::local(None),
            crate::mux::for_binary("tmux").unwrap(),
        );
        let psmux_host = crate::model::Host::new(
            crate::transport::local(None),
            crate::mux::for_binary("psmux").unwrap(),
        );
        let zellij_host = crate::model::Host::new(
            crate::transport::local(None),
            crate::mux::for_binary("zellij").unwrap(),
        );
        let _t: Box<dyn MuxDriver> = driver_for(&tmux_host);
        let _p: Box<dyn MuxDriver> = driver_for(&psmux_host);
        let z: Box<dyn MuxDriver> = driver_for(&zellij_host);
        assert_eq!(z.kind(), "zellij", "each mux constructs its own driver");
    }

    /// The decision is a Mux method, not a `match` in the app: a Shared host is
    /// driven by the tmux driver, a PerSession host by the psmux driver. This is
    /// `driver_for` delegating to `host.mux.driver()` — each mux builds its own.
    #[test]
    fn driver_for_picks_the_mux_specific_driver_by_backend() {
        let tmux_host = crate::model::Host::new(
            crate::transport::ssh("jup".into(), String::new(), "linux".into()),
            crate::mux::for_binary("tmux").unwrap(),
        );
        let psmux_host = crate::model::Host::new(
            crate::transport::local(None),
            crate::mux::for_binary("psmux").unwrap(),
        );
        assert_eq!(driver_for(&tmux_host).kind(), "tmux");
        assert_eq!(driver_for(&psmux_host).kind(), "psmux");
    }

    /// Through the driver boundary, a psmux selection REPLACES the single host-keyed
    /// display attachment (the per-session reattach). This pins the seam by its observable
    /// effect rather than by which helper carries it out, because a per-session mux reaches
    /// another session only by reattaching, whatever owns the decision. Headless: a fake
    /// spawner, no live psmux.
    #[tokio::test(flavor = "current_thread")]
    async fn seam_show_replaces_the_psmux_display_attachment() {
        let mut hosts = crate::model::Hosts::default();
        hosts.insert(crate::model::Host::new(
            crate::transport::local(None),
            crate::mux::for_binary("psmux").unwrap(),
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
            Box::new(|_argv, _cols, _rows, id, _events, _env_clear| {
                Ok(crate::display::attachment::fake_attachment(id))
            }),
        );
        let mut registry = AttachRegistry::new();
        registry.insert("local", crate::display::attachment::fake_attachment(99));
        let mut attach_seq = 0u64;
        let (cap_tx, _cap_rx) = tokio::sync::mpsc::unbounded_channel();

        let sel = Selection {
            source: "local".into(),
            session: "target".into(),
        };

        // Through the Mux dispatch (driver_for → host.mux.driver()) + the concrete
        // driver — the same path the app takes — so this pins the whole boundary.
        let mut driver = driver_for(hosts.get("local").unwrap());
        let shown = {
            let mut ctx = DriverCtx {
                registry: &mut registry,
                hosts: &mut hosts,
                worker: &worker,
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
}
