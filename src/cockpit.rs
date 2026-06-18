//! The cockpit: a persistent supervisor that owns the terminal, runs one
//! mux-client child at a time, and serves a control socket so the in-mux popup
//! can ask it to re-attach a session on another server — the cross-host switch.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use interprocess::local_socket::tokio::{Listener, Stream};
use interprocess::local_socket::traits::tokio::Listener as _;
use interprocess::local_socket::ListenerOptions;
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::attach;
use crate::control;
use crate::env::Env;
use crate::manage;
use crate::proxy;
use crate::session::{self, Session};
use crate::ui::run::run_switcher;

/// A target the cockpit attaches: a session and an optional window to land on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub session: Session,
    pub window: Option<i64>,
}

/// The single well-known pointer file naming the live cockpit's control socket.
pub fn cockpit_pointer_path(xmux_dir: &Path) -> PathBuf {
    xmux_dir.join("cockpit")
}

/// Records the cockpit socket path so the popup can find a live cockpit.
pub fn write_cockpit_pointer(xmux_dir: &Path, socket_path: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(xmux_dir)?;
    std::fs::write(
        cockpit_pointer_path(xmux_dir),
        socket_path.to_string_lossy().as_bytes(),
    )
}

/// Reads the recorded cockpit socket path, if a pointer exists and is non-empty.
/// Liveness is proven by dialing the socket, not by this read.
pub fn read_cockpit_pointer(xmux_dir: &Path) -> Option<PathBuf> {
    let s = std::fs::read_to_string(cockpit_pointer_path(xmux_dir)).ok()?;
    let s = s.trim();
    (!s.is_empty()).then(|| PathBuf::from(s))
}

/// Removes the pointer file (on cockpit exit).
pub fn remove_cockpit_pointer(xmux_dir: &Path) {
    let _ = std::fs::remove_file(cockpit_pointer_path(xmux_dir));
}

/// Removes the pointer only if it still names `sock`, so a sibling cockpit's
/// pointer is not orphaned when this one exits.
pub fn remove_cockpit_pointer_if_ours(xmux_dir: &Path, sock: &Path) {
    if read_cockpit_pointer(xmux_dir).as_deref() == Some(sock) {
        remove_cockpit_pointer(xmux_dir);
    }
}

/// Appends a line to `~/.xmux/cockpit.log`. The cockpit cannot render UI between
/// attaches (the picker owns the screen next), so attach/bind failures are
/// recorded here to survive the picker's screen clears.
fn log_cockpit(xmux_dir: &Path, msg: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(xmux_dir.join("cockpit.log"))
    {
        let _ = writeln!(f, "{msg}");
    }
}

/// Handles one cockpit control request line. Returns the reply payload and, for a
/// valid `switch <source>/<name>` naming a known source, the target the loop
/// should attach next.
pub fn dispatch_cockpit(
    line: &str,
    known_source: &dyn Fn(&str) -> bool,
) -> (String, Option<Target>) {
    let req = control::parse_request(line);
    match req.verb.as_str() {
        "ping" => ("pong".to_string(), None),
        "switch" => {
            let (window, addr) = parse_switch_arg(&req.arg);
            match session::parse_target(addr.trim()) {
                Ok(s) if known_source(&s.source) => {
                    ("ok".to_string(), Some(Target { session: s, window }))
                }
                Ok(s) => (format!("err: unknown source {:?}", s.source), None),
                Err(e) => (format!("err: {e}"), None),
            }
        }
        _ => ("err: unknown command".to_string(), None),
    }
}

/// Parses a `switch` argument. The popup sends `<window> <addr>`, where `<window>`
/// is a window index or `-` for none; a bare `<addr>` (no leading window token) is
/// also accepted so the address — which may contain spaces — is taken verbatim
/// whenever the first token is not a window spec.
fn parse_switch_arg(arg: &str) -> (Option<i64>, &str) {
    match arg.split_once(' ') {
        Some(("-", rest)) => (None, rest),
        Some((head, rest)) => match head.parse::<i64>() {
            Ok(n) => (Some(n), rest),
            Err(_) => (None, arg),
        },
        None => (None, arg),
    }
}

/// What the popup does with a pick.
#[derive(Debug, PartialEq, Eq)]
pub enum PopupAction {
    /// Same-server pick: `switch-client` in place (instant).
    SwitchClient,
    /// Cross-server pick with a cockpit pointer present: signal it to re-attach.
    SignalCockpit,
    /// Cross-server pick with no cockpit pointer: cannot cross hosts from here.
    NoCockpit,
}

/// Decides the popup action from the pick and whether a cockpit pointer exists.
pub fn decide_popup_action(
    chosen: &Session,
    local_source: &str,
    cockpit_available: bool,
) -> PopupAction {
    if chosen.source == local_source {
        PopupAction::SwitchClient
    } else if cockpit_available {
        PopupAction::SignalCockpit
    } else {
        PopupAction::NoCockpit
    }
}

/// Hands the terminal to the attach for a target and returns when the child exits.
#[async_trait]
pub trait Attacher: Send + Sync {
    async fn attach(&self, target: &Target);
}

/// Runs the full-screen picker; returns the chosen target, or `None` to quit.
#[async_trait]
pub trait Picker: Send + Sync {
    async fn pick(&self) -> Option<Target>;
}

/// Maps a picker/overlay `SwitchResult` to a `Target`, or `None` when nothing was
/// chosen. The window is carried only when the result names a real one (`>= 0`).
fn switch_result_to_target(result: crate::ui::switcher::SwitchResult) -> Option<Target> {
    let chosen = result.chosen?;
    Some(Target {
        session: chosen,
        window: (result.window >= 0).then_some(result.window),
    })
}

/// A switch is honored only if the loop reaches it within this window of being
/// queued. Bounds a leak: if a popup queues a switch but its detach never lands,
/// a much-later unrelated child exit must not trigger a stale teleport. The
/// normal popup→ok→detach→child-exit path completes in well under a second.
const SWITCH_FRESH_WINDOW: Duration = Duration::from_secs(15);

/// A recorded cross-server switch and the instant it was queued.
#[derive(Debug, Clone)]
pub struct PendingSwitch {
    pub target: Target,
    pub at: Instant,
}

/// A recorded cross-server switch, shared between the accept task and the loop.
pub type Pending = Arc<Mutex<Option<PendingSwitch>>>;

/// Whether a switch queued at `at` is still fresh as of `now`.
fn switch_is_fresh(at: Instant, now: Instant, window: Duration) -> bool {
    now.saturating_duration_since(at) < window
}

/// Drains the pending cell, returning the target only if the queued switch is
/// still fresh; a stale switch is consumed (cleared) but not acted on.
fn take_fresh_switch(pending: &Pending) -> Option<Target> {
    let p = pending.lock().unwrap().take()?;
    switch_is_fresh(p.at, Instant::now(), SWITCH_FRESH_WINDOW).then_some(p.target)
}

/// The cockpit supervisor loop. Attaches the current target; when the child
/// exits, a fresh recorded switch re-attaches with no picker (the seamless
/// cross-host switch), otherwise the picker chooses the next target (or quits).
pub async fn cockpit_loop(
    attacher: &dyn Attacher,
    picker: &dyn Picker,
    pending: Pending,
    initial: Option<Target>,
) -> i32 {
    let mut target = match initial {
        Some(t) => t,
        None => match picker.pick().await {
            Some(t) => t,
            None => return 0,
        },
    };
    loop {
        attacher.attach(&target).await;
        target = match take_fresh_switch(&pending) {
            Some(t) => t,
            None => match picker.pick().await {
                Some(t) => t,
                None => return 0,
            },
        };
    }
}

/// Tells the cockpit at `sock` to switch to `addr`, landing on `window` when set.
/// Returns `Ok(true)` when the cockpit acks `ok`, `Ok(false)` on any other reply,
/// `Err` if no cockpit answers. The wire form is `switch <window|-> <addr>`.
pub async fn signal_cockpit_switch(
    sock: &Path,
    addr: &str,
    window: Option<i64>,
) -> anyhow::Result<bool> {
    let w = window.map_or_else(|| "-".to_string(), |n| n.to_string());
    let mut client = control::Client::dial(sock).await?;
    let reply = client.do_cmd(&format!("switch {w} {addr}")).await?;
    Ok(reply.trim() == "ok")
}

/// Tests whether a source alias is known to this cockpit (so `switch` to an
/// unknown source is rejected).
type KnownSource = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// Serves the cockpit control socket: each `switch` stores the target into
/// `pending` for the loop to consume after the current child exits.
async fn cockpit_accept(listener: Listener, pending: Pending, known: KnownSource) {
    while let Ok(conn) = listener.accept().await {
        let pending = pending.clone();
        let known = known.clone();
        tokio::spawn(handle_cockpit_conn(conn, pending, known));
    }
}

async fn handle_cockpit_conn(conn: Stream, pending: Pending, known: KnownSource) {
    let mut buf = BufReader::new(conn);
    loop {
        let mut line = String::new();
        match buf.read_line(&mut line).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let known_fn = |s: &str| (known)(s);
        let (reply, target) = dispatch_cockpit(&line, &known_fn);
        if let Some(t) = target {
            *pending.lock().unwrap() = Some(PendingSwitch {
                target: t,
                at: Instant::now(),
            });
        }
        if control::write_frame(&mut buf, &reply).await.is_err() {
            return;
        }
    }
}

/// A running cockpit socket: the accept task plus paths to clean up on drop.
struct CockpitHandle {
    task: tokio::task::JoinHandle<()>,
    sock: PathBuf,
    xmux_dir: PathBuf,
}
impl Drop for CockpitHandle {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_file(&self.sock);
        remove_cockpit_pointer_if_ours(&self.xmux_dir, &self.sock);
    }
}

/// Binds the cockpit socket, writes the pointer, and serves it. A bind failure
/// returns `None` (the cockpit still runs; cross-host-from-popup is unavailable).
fn serve_cockpit(env: &Arc<Env>, sock: PathBuf, pending: Pending) -> Option<CockpitHandle> {
    let _ = std::fs::remove_file(&sock);
    let name = match control::endpoint_name(&sock) {
        Ok(n) => n,
        Err(e) => {
            log_cockpit(&env.xmux_dir, &format!("cockpit socket name error: {e}"));
            return None;
        }
    };
    let listener = match ListenerOptions::new().name(name).create_tokio() {
        Ok(l) => l,
        Err(e) => {
            log_cockpit(&env.xmux_dir, &format!("cockpit socket bind failed: {e}"));
            return None;
        }
    };
    #[cfg(windows)]
    let _ = std::fs::write(&sock, b"");
    let _ = write_cockpit_pointer(&env.xmux_dir, &sock);
    let known_keys: std::collections::HashSet<String> = env.by_alias.keys().cloned().collect();
    let known: KnownSource = Arc::new(move |s: &str| known_keys.contains(s));
    let task = tokio::spawn(cockpit_accept(listener, pending, known));
    Some(CockpitHandle {
        task,
        sock,
        xmux_dir: env.xmux_dir.clone(),
    })
}

/// The real terminal-handover attach. It runs the attach child under the PTY
/// proxy so the in-pane prefix hotkey can open the picker overlay without a
/// detach. A pick (same- or cross-server) is recorded into `pending`; the
/// cockpit loop drains it and re-attaches, so both kinds of pick flow through
/// the one re-attach path. The proxy is async, so the cockpit serves its socket
/// concurrently with the child's run.
struct RealAttacher {
    env: Arc<Env>,
    ops: Arc<dyn crate::ui::switcher::Ops>,
    pending: Pending,
}
#[async_trait]
impl Attacher for RealAttacher {
    async fn attach(&self, target: &Target) {
        let dir = &self.env.xmux_dir;
        let addr = target.session.address();
        let Some(src) = self.env.by_alias.get(&target.session.source).cloned() else {
            let m = format!("attach {addr} failed: unknown source");
            eprintln!("xmux: {m}");
            log_cockpit(dir, &m);
            return;
        };
        if let Err(e) = attach::nest_guard(attach::in_mux()) {
            eprintln!("xmux: {e}");
            log_cockpit(dir, &format!("attach {addr} refused: {e}"));
            return;
        }
        // Local pre-selects the window with an instant local command; a remote
        // folds the selection into the single ssh attach connection.
        if !src.remote {
            if let Some(w) = target.window {
                let _ = manage::select_window(&src, &target.session.name, w).await;
            }
        }
        let argv = src.attach_command(&target.session.name, target.window);
        let cfg = proxy::run::ProxyConfig {
            prefix: proxy::run::prefix_from_env(),
            action_key: b's',
        };
        match proxy::run::proxy_attach(&argv, self.ops.clone(), cfg).await {
            // The overlay yielded a pick: record it (fresh) for the loop to
            // re-attach, mirroring the socket popup path. Same- and cross-server
            // picks both flow through this one re-attach path.
            Ok(Some(result)) => {
                if let Some(t) = switch_result_to_target(result) {
                    *self.pending.lock().unwrap() = Some(PendingSwitch {
                        target: t,
                        at: Instant::now(),
                    });
                }
            }
            // The child exited on its own: behave as the old bare-exit path —
            // return and let the loop re-pick. (The proxy does not surface the
            // child's exit status, so a non-zero remote exit, e.g. ssh 255, is no
            // longer logged here; see the Task 7 report.)
            Ok(None) => {}
            Err(e) => {
                let m = format!("attach {addr} failed: {e}");
                eprintln!("xmux: {m}");
                log_cockpit(dir, &m);
            }
        }
    }
}

/// The real picker: runs the full-screen switcher and maps its result to a target.
struct RealPicker {
    ops: Arc<dyn crate::ui::switcher::Ops>,
    control: Option<PathBuf>,
}
#[async_trait]
impl Picker for RealPicker {
    async fn pick(&self) -> Option<Target> {
        let result = match run_switcher(self.ops.clone(), self.control.clone()).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("xmux: {e}");
                return None;
            }
        };
        switch_result_to_target(result)
    }
}

/// The `xmux` (no subcommand) entry: the persistent cockpit. Owns the terminal,
/// runs one mux-client child at a time, and serves a control socket so the in-mux
/// popup can signal a cross-host switch that re-attaches with no picker between.
pub async fn run_cockpit(env: Arc<Env>) -> i32 {
    // The cockpit owns the terminal and attaches mux clients as children; nested
    // inside a mux every attach is refused, leaving only a doomed picker loop. So
    // running it inside a mux is refused outright, not warned.
    if let Err(e) = attach::nest_guard(attach::in_mux()) {
        eprintln!("xmux: {e}");
        eprintln!("xmux: the cockpit must be your terminal entry, not run inside a mux.");
        return 2;
    }
    let _ = std::fs::create_dir_all(&env.xmux_dir);
    let pending: Pending = Arc::new(Mutex::new(None));
    let sock = control::cockpit_socket_path(&env.xmux_dir, std::process::id());
    let handle = serve_cockpit(&env, sock, pending.clone());
    if handle.is_none() {
        log_cockpit(
            &env.xmux_dir,
            "cockpit control socket unavailable; cross-host popup switching disabled",
        );
    }
    let _handle = handle;

    let attacher = RealAttacher {
        env: env.clone(),
        ops: env.ops(),
        pending: pending.clone(),
    };
    let picker = RealPicker {
        ops: env.ops(),
        control: pick_control_path(&env),
    };
    cockpit_loop(&attacher, &picker, pending, None).await
}

/// The picker's control socket path (`ctl-<pid>.sock`), unless `XMUX_CONTROL=0`.
fn pick_control_path(env: &Env) -> Option<PathBuf> {
    if std::env::var("XMUX_CONTROL").as_deref() == Ok("0") {
        return None;
    }
    let _ = std::fs::create_dir_all(&env.xmux_dir);
    Some(control::socket_path(&env.xmux_dir, std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("xmux-cockpit-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn pointer_round_trip_and_absent() {
        let dir = tmp("ptr");
        assert!(
            read_cockpit_pointer(&dir).is_none(),
            "absent pointer is None"
        );
        let sock = dir.join("cockpit-123.sock");
        write_cockpit_pointer(&dir, &sock).unwrap();
        assert_eq!(read_cockpit_pointer(&dir), Some(sock));
        remove_cockpit_pointer(&dir);
        assert!(
            read_cockpit_pointer(&dir).is_none(),
            "removed pointer is None"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dispatch_switch_ping_and_errors() {
        let known = |s: &str| matches!(s, "local" | "jupiter06");
        let known: &dyn Fn(&str) -> bool = &known;

        assert_eq!(dispatch_cockpit("ping", known).0, "pong");

        let (reply, target) = dispatch_cockpit("switch jupiter06/api", known);
        assert_eq!(reply, "ok");
        let t = target.expect("a valid switch yields a target");
        assert_eq!(t.session.source, "jupiter06");
        assert_eq!(t.session.name, "api");
        assert_eq!(t.window, None);

        // Unknown source → err, no target.
        let (reply, target) = dispatch_cockpit("switch nope/x", known);
        assert!(reply.starts_with("err:"), "{reply}");
        assert!(target.is_none());

        // Malformed address → err, no target.
        let (reply, target) = dispatch_cockpit("switch noslash", known);
        assert!(reply.starts_with("err:"), "{reply}");
        assert!(target.is_none());

        // Unknown verb.
        assert_eq!(dispatch_cockpit("bogus", known).0, "err: unknown command");
    }

    #[test]
    fn dispatch_switch_carries_window() {
        let known = |s: &str| s == "jupiter06";
        let known: &dyn Fn(&str) -> bool = &known;

        // Window-first wire form: "<window> <addr>".
        let (reply, target) = dispatch_cockpit("switch 3 jupiter06/api", known);
        assert_eq!(reply, "ok");
        let t = target.unwrap();
        assert_eq!(t.session.address(), "jupiter06/api");
        assert_eq!(t.window, Some(3));

        // Explicit no-window sentinel.
        let (_, target) = dispatch_cockpit("switch - jupiter06/api", known);
        assert_eq!(target.unwrap().window, None);

        // Bare address (no leading window token) still works, window None.
        let (_, target) = dispatch_cockpit("switch jupiter06/api", known);
        assert_eq!(target.unwrap().window, None);
    }

    #[test]
    fn switch_result_maps_to_target() {
        use crate::session::Session;
        use crate::ui::switcher::SwitchResult;

        // No choice → no target (the picker/overlay was cancelled).
        assert!(switch_result_to_target(SwitchResult {
            chosen: None,
            window: 3,
        })
        .is_none());

        // A real window (>= 0) is carried.
        let t = switch_result_to_target(SwitchResult {
            chosen: Some(Session {
                source: "jupiter06".into(),
                name: "api".into(),
                ..Default::default()
            }),
            window: 2,
        })
        .expect("a chosen session yields a target");
        assert_eq!(t.session.address(), "jupiter06/api");
        assert_eq!(t.window, Some(2));

        // A sentinel window (< 0) means "no specific window".
        let t = switch_result_to_target(SwitchResult {
            chosen: Some(Session {
                source: "local".into(),
                name: "work".into(),
                ..Default::default()
            }),
            window: -1,
        })
        .expect("a chosen session yields a target");
        assert_eq!(t.window, None);
    }

    #[test]
    fn switch_freshness_window() {
        let at = Instant::now();
        let w = Duration::from_secs(15);
        assert!(switch_is_fresh(at, at + Duration::from_secs(5), w));
        assert!(!switch_is_fresh(at, at + Duration::from_secs(20), w));
        // A `now` earlier than `at` saturates to zero elapsed → still fresh.
        assert!(switch_is_fresh(at, at, w));
    }

    #[test]
    fn pointer_removed_only_if_ours() {
        let dir = tmp("ptr-ours");
        let ours = dir.join("cockpit-111.sock");
        let theirs = dir.join("cockpit-222.sock");
        write_cockpit_pointer(&dir, &ours).unwrap();
        // A different cockpit's exit must not clear our pointer.
        remove_cockpit_pointer_if_ours(&dir, &theirs);
        assert_eq!(read_cockpit_pointer(&dir), Some(ours.clone()));
        // Our own exit clears it.
        remove_cockpit_pointer_if_ours(&dir, &ours);
        assert!(read_cockpit_pointer(&dir).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn popup_decision_table() {
        use crate::session::{Session, LOCAL_SOURCE};
        let local = Session {
            source: LOCAL_SOURCE.into(),
            name: "w".into(),
            ..Default::default()
        };
        let remote = Session {
            source: "jupiter06".into(),
            name: "api".into(),
            ..Default::default()
        };

        assert_eq!(
            decide_popup_action(&local, LOCAL_SOURCE, false),
            PopupAction::SwitchClient
        );
        assert_eq!(
            decide_popup_action(&local, LOCAL_SOURCE, true),
            PopupAction::SwitchClient
        );
        assert_eq!(
            decide_popup_action(&remote, LOCAL_SOURCE, true),
            PopupAction::SignalCockpit
        );
        assert_eq!(
            decide_popup_action(&remote, LOCAL_SOURCE, false),
            PopupAction::NoCockpit
        );
    }

    fn target(source: &str, name: &str) -> Target {
        Target {
            session: Session {
                source: source.into(),
                name: name.into(),
                ..Default::default()
            },
            window: None,
        }
    }

    #[tokio::test]
    async fn loop_reattaches_on_pending_then_picks_on_bare_exit() {
        use std::sync::Mutex;

        // Attacher logs each target and injects a pending switch "during" each attach.
        struct ScriptAttacher {
            log: Mutex<Vec<String>>,
            pending: Pending,
            inject: Mutex<Vec<Option<Target>>>,
        }
        #[async_trait::async_trait]
        impl Attacher for ScriptAttacher {
            async fn attach(&self, t: &Target) {
                self.log.lock().unwrap().push(t.session.address());
                let inj = self.inject.lock().unwrap().remove(0);
                *self.pending.lock().unwrap() = inj.map(|target| PendingSwitch {
                    target,
                    at: Instant::now(),
                });
            }
        }
        struct QuitPicker;
        #[async_trait::async_trait]
        impl Picker for QuitPicker {
            async fn pick(&self) -> Option<Target> {
                None
            }
        }

        let pending: Pending = std::sync::Arc::new(Mutex::new(None));
        let attacher = ScriptAttacher {
            log: Mutex::new(Vec::new()),
            pending: pending.clone(),
            inject: Mutex::new(vec![Some(target("jupiter06", "api")), None]),
        };
        let rc = cockpit_loop(
            &attacher,
            &QuitPicker,
            pending.clone(),
            Some(target("local", "work")),
        )
        .await;
        assert_eq!(rc, 0);
        assert_eq!(
            *attacher.log.lock().unwrap(),
            vec!["local/work", "jupiter06/api"]
        );
    }

    #[tokio::test]
    async fn loop_runs_picker_first_when_no_initial_target() {
        use std::sync::Mutex;
        struct RecordAttacher {
            log: Mutex<Vec<String>>,
            pending: Pending,
        }
        #[async_trait::async_trait]
        impl Attacher for RecordAttacher {
            async fn attach(&self, t: &Target) {
                self.log.lock().unwrap().push(t.session.address());
                *self.pending.lock().unwrap() = None;
            }
        }
        struct ScriptPicker(Mutex<Vec<Option<Target>>>);
        #[async_trait::async_trait]
        impl Picker for ScriptPicker {
            async fn pick(&self) -> Option<Target> {
                self.0.lock().unwrap().remove(0)
            }
        }
        let pending: Pending = std::sync::Arc::new(Mutex::new(None));
        let attacher = RecordAttacher {
            log: Mutex::new(Vec::new()),
            pending: pending.clone(),
        };
        let picker = ScriptPicker(Mutex::new(vec![Some(target("local", "work")), None]));
        let rc = cockpit_loop(&attacher, &picker, pending.clone(), None).await;
        assert_eq!(rc, 0);
        assert_eq!(*attacher.log.lock().unwrap(), vec!["local/work"]);
    }

    #[tokio::test]
    async fn loop_switches_local_remote_both_directions() {
        use std::sync::Mutex;
        // UC-11: the cockpit re-attaches whatever the next queued switch names —
        // local OR remote — in any order, with no picker between. The picker is
        // only reached when no switch is queued (here: to quit at the end).
        struct SeqAttacher {
            log: Mutex<Vec<String>>,
            pending: Pending,
            queue: Mutex<Vec<Option<Target>>>,
        }
        #[async_trait::async_trait]
        impl Attacher for SeqAttacher {
            async fn attach(&self, t: &Target) {
                self.log.lock().unwrap().push(t.session.address());
                let next = self.queue.lock().unwrap().remove(0);
                *self.pending.lock().unwrap() = next.map(|target| PendingSwitch {
                    target,
                    at: Instant::now(),
                });
            }
        }
        struct QuitPicker;
        #[async_trait::async_trait]
        impl Picker for QuitPicker {
            async fn pick(&self) -> Option<Target> {
                None
            }
        }
        let pending: Pending = std::sync::Arc::new(Mutex::new(None));
        let attacher = SeqAttacher {
            log: Mutex::new(Vec::new()),
            pending: pending.clone(),
            queue: Mutex::new(vec![
                Some(target("jupiter06", "api")), // local  -> remote
                Some(target("local", "db")),      // remote -> local
                Some(target("jupiter00", "web")), // local  -> remote
                None,                             // no switch -> picker -> quit
            ]),
        };
        let rc = cockpit_loop(
            &attacher,
            &QuitPicker,
            pending.clone(),
            Some(target("local", "work")),
        )
        .await;
        assert_eq!(rc, 0);
        assert_eq!(
            *attacher.log.lock().unwrap(),
            vec!["local/work", "jupiter06/api", "local/db", "jupiter00/web"],
            "the cockpit must re-attach in both directions (local<->remote), no picker between"
        );
    }

    #[tokio::test]
    async fn loop_discards_stale_pending() {
        use std::sync::Mutex;
        // During the first attach a STALE switch is queued; the loop must discard
        // it (not re-attach) and fall to the picker, which quits.
        struct StaleAttacher {
            log: Mutex<Vec<String>>,
            pending: Pending,
            armed: Mutex<bool>,
        }
        #[async_trait::async_trait]
        impl Attacher for StaleAttacher {
            async fn attach(&self, t: &Target) {
                self.log.lock().unwrap().push(t.session.address());
                let mut armed = self.armed.lock().unwrap();
                if !*armed {
                    *armed = true;
                    if let Some(old) = Instant::now().checked_sub(Duration::from_secs(3600)) {
                        *self.pending.lock().unwrap() = Some(PendingSwitch {
                            target: target("jupiter06", "api"),
                            at: old,
                        });
                    }
                }
            }
        }
        struct QuitPicker;
        #[async_trait::async_trait]
        impl Picker for QuitPicker {
            async fn pick(&self) -> Option<Target> {
                None
            }
        }
        let pending: Pending = std::sync::Arc::new(Mutex::new(None));
        let attacher = StaleAttacher {
            log: Mutex::new(Vec::new()),
            pending: pending.clone(),
            armed: Mutex::new(false),
        };
        let rc = cockpit_loop(
            &attacher,
            &QuitPicker,
            pending.clone(),
            Some(target("local", "work")),
        )
        .await;
        assert_eq!(rc, 0);
        // The stale switch was discarded: only the initial attach ran.
        assert_eq!(*attacher.log.lock().unwrap(), vec!["local/work"]);
    }

    #[tokio::test]
    async fn cockpit_socket_switch_sets_pending() {
        let dir = std::env::temp_dir().join(format!("xmux-cockpit-sock-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let sock = crate::control::cockpit_socket_path(&dir, std::process::id());
        let _ = std::fs::remove_file(&sock);

        let name = crate::control::endpoint_name(&sock).unwrap();
        let listener = ListenerOptions::new().name(name).create_tokio().unwrap();
        #[cfg(windows)]
        let _ = std::fs::write(&sock, b"");

        let pending: Pending = Arc::new(Mutex::new(None));
        let known: KnownSource = Arc::new(|s: &str| s == "jupiter06");
        let task = tokio::spawn(cockpit_accept(listener, pending.clone(), known));

        let mut client = crate::control::Client::dial(&sock).await.unwrap();
        assert_eq!(client.do_cmd("ping").await.unwrap(), "pong");
        assert_eq!(client.do_cmd("switch jupiter06/api").await.unwrap(), "ok");

        let got = pending
            .lock()
            .unwrap()
            .clone()
            .expect("switch sets pending");
        assert_eq!(got.target.session.address(), "jupiter06/api");

        task.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn signal_cockpit_switch_acks_and_sets_pending() {
        let dir = std::env::temp_dir().join(format!("xmux-cockpit-signal-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let sock = crate::control::cockpit_socket_path(&dir, std::process::id().wrapping_add(11));
        let _ = std::fs::remove_file(&sock);
        let name = crate::control::endpoint_name(&sock).unwrap();
        let listener = ListenerOptions::new().name(name).create_tokio().unwrap();
        #[cfg(windows)]
        let _ = std::fs::write(&sock, b"");

        let pending: Pending = Arc::new(Mutex::new(None));
        let known: KnownSource = Arc::new(|s: &str| s == "jupiter06");
        let task = tokio::spawn(cockpit_accept(listener, pending.clone(), known));

        let ok = signal_cockpit_switch(&sock, "jupiter06/api", None)
            .await
            .unwrap();
        assert!(ok, "a known target acks ok");
        assert_eq!(
            pending
                .lock()
                .unwrap()
                .clone()
                .unwrap()
                .target
                .session
                .address(),
            "jupiter06/api"
        );

        let rejected = signal_cockpit_switch(&sock, "nope/x", None).await.unwrap();
        assert!(!rejected, "an unknown source is not ok");

        task.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
