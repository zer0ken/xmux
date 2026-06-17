# Seamless cross-host switch — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a cross-host session pick from the in-mux popup re-attach the terminal to that session in a single action, in place, with no home-tree flash — via a persistent cockpit supervisor that owns the terminal and swaps its mux-client child on a control-socket signal.

**Architecture:** `xmux` (no subcommand) becomes the cockpit: it owns the terminal, runs one mux-client child at a time, and serves a control socket. The in-mux popup picks a target; same-server → `switch-client` (native, instant); cross-server → signal the cockpit over its socket then detach, and the cockpit re-attaches the new target with no picker between. The fragile `pending-jump` file handoff is retired.

**Tech Stack:** Rust, tokio (current-thread), interprocess local sockets (named pipes on Windows), ratatui/crossterm (the picker), async-trait. Reuses the existing `control.rs` wire protocol (`parse_request`, `write_frame`, `Client`, `endpoint_name`).

## Global Constraints

- Toolchain: the rustup shim is harness-blocked. Run cargo via the real binary:
  `TC="$HOME/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin"; RUSTC=$TC/rustc.exe RUSTDOC=$TC/rustdoc.exe "$TC/cargo.exe" <cmd>`.
- `cargo test` does NOT rebuild the binary; run `cargo build` before any live binary check.
- AS-IS principle: no change-narration in code/comments/docs — state current behavior as fact.
- Local platform is Windows (psmux); remotes are POSIX tmux over ssh. Windows OpenSSH has no ControlMaster.
- The local mux `switch-client`/`detach-client` are run via `OsExecer` (inherits `$TMUX`), not the source layer, so they target the current server through the environment.

---

### Task 1: Cockpit module skeleton + pointer file

**Files:**
- Create: `src/cockpit.rs`
- Modify: `src/lib.rs` (add `pub mod cockpit;`)
- Test: in `src/cockpit.rs` `#[cfg(test)]`

**Interfaces:**
- Produces: `Target { session: Session, window: Option<i64> }`;
  `cockpit_pointer_path(&Path) -> PathBuf`; `write_cockpit_pointer(&Path, &Path) -> io::Result<()>`;
  `read_cockpit_pointer(&Path) -> Option<PathBuf>`; `remove_cockpit_pointer(&Path)`.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("xmux-cockpit-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn pointer_round_trip_and_absent() {
        let dir = tmp("ptr");
        assert!(read_cockpit_pointer(&dir).is_none(), "absent pointer is None");
        let sock = dir.join("cockpit-123.sock");
        write_cockpit_pointer(&dir, &sock).unwrap();
        assert_eq!(read_cockpit_pointer(&dir), Some(sock));
        remove_cockpit_pointer(&dir);
        assert!(read_cockpit_pointer(&dir).is_none(), "removed pointer is None");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `…/cargo.exe test --lib cockpit::tests::pointer_round_trip_and_absent`
Expected: FAIL — `cockpit` module / functions not found.

- [ ] **Step 3: Write minimal implementation** (top of `src/cockpit.rs`)

```rust
//! The cockpit: a persistent supervisor that owns the terminal, runs one
//! mux-client child at a time, and serves a control socket so the in-mux popup
//! can ask it to re-attach a session on another server — the cross-host switch.

use std::path::{Path, PathBuf};

use crate::session::Session;

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
```

Add to `src/lib.rs` next to the other `pub mod` lines: `pub mod cockpit;`

- [ ] **Step 4: Run test to verify it passes**

Run: `…/cargo.exe test --lib cockpit::tests::pointer_round_trip_and_absent`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/cockpit.rs src/lib.rs
git commit -m "feat(cockpit): pointer file naming the live cockpit control socket"
```

---

### Task 2: Cockpit control dispatch

**Files:**
- Modify: `src/cockpit.rs`
- Test: `src/cockpit.rs` `#[cfg(test)]`

**Interfaces:**
- Consumes: `crate::control::parse_request`, `crate::session::parse_target`.
- Produces: `dispatch_cockpit(line: &str, known_source: &dyn Fn(&str) -> bool) -> (String, Option<Target>)`.

- [ ] **Step 1: Write the failing test** (add to the `tests` module)

```rust
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `…/cargo.exe test --lib cockpit::tests::dispatch_switch_ping_and_errors`
Expected: FAIL — `dispatch_cockpit` not found.

- [ ] **Step 3: Write minimal implementation** (add to `src/cockpit.rs`)

```rust
use crate::control;
use crate::session;

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
        "switch" => match session::parse_target(req.arg.trim()) {
            Ok(s) if known_source(&s.source) => (
                "ok".to_string(),
                Some(Target {
                    session: s,
                    window: None,
                }),
            ),
            Ok(s) => (format!("err: unknown source {:?}", s.source), None),
            Err(e) => (format!("err: {e}"), None),
        },
        _ => ("err: unknown command".to_string(), None),
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `…/cargo.exe test --lib cockpit::tests::dispatch_switch_ping_and_errors`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/cockpit.rs
git commit -m "feat(cockpit): control dispatch for switch/ping"
```

---

### Task 3: Popup decision

**Files:**
- Modify: `src/cockpit.rs`
- Test: `src/cockpit.rs` `#[cfg(test)]`

**Interfaces:**
- Produces: `enum PopupAction { SwitchClient, SignalCockpit, NoCockpit }`;
  `decide_popup_action(chosen: &Session, local_source: &str, cockpit_available: bool) -> PopupAction`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn popup_decision_table() {
    use crate::session::{Session, LOCAL_SOURCE};
    let local = Session { source: LOCAL_SOURCE.into(), name: "w".into(), ..Default::default() };
    let remote = Session { source: "jupiter06".into(), name: "api".into(), ..Default::default() };

    assert_eq!(decide_popup_action(&local, LOCAL_SOURCE, false), PopupAction::SwitchClient);
    assert_eq!(decide_popup_action(&local, LOCAL_SOURCE, true), PopupAction::SwitchClient);
    assert_eq!(decide_popup_action(&remote, LOCAL_SOURCE, true), PopupAction::SignalCockpit);
    assert_eq!(decide_popup_action(&remote, LOCAL_SOURCE, false), PopupAction::NoCockpit);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `…/cargo.exe test --lib cockpit::tests::popup_decision_table`
Expected: FAIL — `PopupAction`/`decide_popup_action` not found.

- [ ] **Step 3: Write minimal implementation** (add to `src/cockpit.rs`)

```rust
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
```

- [ ] **Step 4: Run test to verify it passes**

Run: `…/cargo.exe test --lib cockpit::tests::popup_decision_table`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/cockpit.rs
git commit -m "feat(cockpit): popup action decision (switch-client/signal/no-cockpit)"
```

---

### Task 4: Cockpit supervisor loop (testable core)

**Files:**
- Modify: `src/cockpit.rs`, `Cargo.toml` (confirm `async-trait` is a dependency — it already is, used by `source.rs`)
- Test: `src/cockpit.rs` `#[cfg(test)]`

**Interfaces:**
- Produces: `trait Attacher { async fn attach(&self, &Target); }`; `trait Picker { async fn pick(&self) -> Option<Target>; }`;
  `type Pending = Arc<Mutex<Option<Target>>>`;
  `async fn cockpit_loop(attacher: &dyn Attacher, picker: &dyn Picker, pending: Pending, initial: Option<Target>) -> i32`.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn loop_reattaches_on_pending_then_picks_on_bare_exit() {
    use std::sync::{Arc, Mutex};
    use crate::session::Session;

    fn target(source: &str, name: &str) -> Target {
        Target { session: Session { source: source.into(), name: name.into(), ..Default::default() }, window: None }
    }

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
            *self.pending.lock().unwrap() = inj;
        }
    }
    struct QuitPicker;
    #[async_trait::async_trait]
    impl Picker for QuitPicker {
        async fn pick(&self) -> Option<Target> { None }
    }

    let pending: Pending = Arc::new(Mutex::new(None));
    let attacher = ScriptAttacher {
        log: Mutex::new(Vec::new()),
        pending: pending.clone(),
        inject: Mutex::new(vec![Some(target("jupiter06", "api")), None]),
    };
    let rc = cockpit_loop(&attacher, &QuitPicker, pending.clone(), Some(target("local", "work"))).await;
    assert_eq!(rc, 0);
    assert_eq!(*attacher.log.lock().unwrap(), vec!["local/work", "jupiter06/api"]);
}

#[tokio::test]
async fn loop_runs_picker_first_when_no_initial_target() {
    use std::sync::{Arc, Mutex};
    use crate::session::Session;
    fn target(source: &str, name: &str) -> Target {
        Target { session: Session { source: source.into(), name: name.into(), ..Default::default() }, window: None }
    }
    struct RecordAttacher { log: Mutex<Vec<String>>, pending: Pending }
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
        async fn pick(&self) -> Option<Target> { self.0.lock().unwrap().remove(0) }
    }
    let pending: Pending = Arc::new(Mutex::new(None));
    let attacher = RecordAttacher { log: Mutex::new(Vec::new()), pending: pending.clone() };
    let picker = ScriptPicker(Mutex::new(vec![Some(target("local", "work")), None]));
    let rc = cockpit_loop(&attacher, &picker, pending.clone(), None).await;
    assert_eq!(rc, 0);
    assert_eq!(*attacher.log.lock().unwrap(), vec!["local/work"]);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `…/cargo.exe test --lib cockpit::tests::loop_`
Expected: FAIL — `Attacher`/`Picker`/`cockpit_loop` not found.

- [ ] **Step 3: Write minimal implementation** (add to `src/cockpit.rs`)

```rust
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

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

/// A recorded cross-server switch, shared between the accept task and the loop.
pub type Pending = Arc<Mutex<Option<Target>>>;

/// The cockpit supervisor loop. Attaches the current target; when the child
/// exits, a recorded switch re-attaches with no picker (the seamless cross-host
/// switch), otherwise the picker chooses the next target (or quits).
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
        let next = pending.lock().unwrap().take();
        target = match next {
            Some(t) => t,
            None => match picker.pick().await {
                Some(t) => t,
                None => return 0,
            },
        };
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `…/cargo.exe test --lib cockpit::tests::loop_`
Expected: PASS (both loop tests).

- [ ] **Step 5: Commit**

```bash
git add src/cockpit.rs
git commit -m "feat(cockpit): supervisor loop — pending re-attaches with no picker"
```

---

### Task 5: Real cockpit wiring (socket server + attach/pick + run_cockpit)

**Files:**
- Modify: `src/control.rs` (generalize `endpoint_name` Windows branch to any `<name>-<pid>.sock` via the file stem; add `cockpit_socket_path`)
- Modify: `src/cockpit.rs` (accept loop, real `Attacher`/`Picker`, `serve_cockpit`, `run_cockpit`)
- Test: `src/control.rs` (cockpit endpoint name), `src/cockpit.rs` (socket round-trip stores pending)

**Interfaces:**
- Consumes: `control::endpoint_name`, `control::write_frame`, `control::Client`, `control::cockpit_socket_path`, `env::Env`, `ui::run::run_switcher`, `attach`, `manage`, `mux`.
- Produces: `pub async fn run_cockpit(env: Arc<Env>) -> i32`; `signal_cockpit_switch(sock: &Path, addr: &str) -> anyhow::Result<bool>` (Task 6 uses it; define the accept side here).

- [ ] **Step 1: Write the failing tests**

In `src/control.rs` tests:

```rust
#[test]
fn endpoint_name_accepts_cockpit_socket() {
    // A cockpit-<pid>.sock must build a valid endpoint (no panic / error) on every
    // platform, just like ctl-<pid>.sock.
    let p = std::path::Path::new("/some/dir/cockpit-1234.sock");
    assert!(endpoint_name(p).is_ok());
}
```

In `src/cockpit.rs` tests:

```rust
#[tokio::test]
async fn cockpit_socket_switch_sets_pending() {
    use std::sync::{Arc, Mutex};
    use interprocess::local_socket::tokio::Listener;
    use interprocess::local_socket::traits::tokio::Listener as _;
    use interprocess::local_socket::ListenerOptions;

    let dir = std::env::temp_dir().join(format!("xmux-cockpit-sock-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let sock = crate::control::cockpit_socket_path(&dir, std::process::id());
    let _ = std::fs::remove_file(&sock);

    let name = crate::control::endpoint_name(&sock).unwrap();
    let listener = ListenerOptions::new().name(name).create_tokio().unwrap();
    #[cfg(windows)]
    let _ = std::fs::write(&sock, b"");

    let pending: Pending = Arc::new(Mutex::new(None));
    let known: Arc<dyn Fn(&str) -> bool + Send + Sync> = Arc::new(|s: &str| s == "jupiter06");
    let task = tokio::spawn(cockpit_accept(listener, pending.clone(), known));

    let mut client = crate::control::Client::dial(&sock).await.unwrap();
    assert_eq!(client.do_cmd("ping").await.unwrap(), "pong");
    assert_eq!(client.do_cmd("switch jupiter06/api").await.unwrap(), "ok");

    // The pending target is set for the loop to consume.
    let got = pending.lock().unwrap().clone().expect("switch sets pending");
    assert_eq!(got.session.address(), "jupiter06/api");

    task.abort();
    let _ = std::fs::remove_dir_all(&dir);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `…/cargo.exe test --lib endpoint_name_accepts_cockpit_socket cockpit_socket_switch_sets_pending`
Expected: FAIL — `cockpit_socket_path` / `cockpit_accept` not found.

- [ ] **Step 3: Write minimal implementation**

In `src/control.rs`, replace the Windows branch of `endpoint_name` so it derives the
namespaced name from the file stem (works for `ctl-*` AND `cockpit-*`):

```rust
    #[cfg(windows)]
    {
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "control: socket path has no usable file stem",
                )
            })?;
        format!("xmux-{stem}").to_ns_name::<GenericNamespaced>()
    }
```

Add to `src/control.rs` next to `socket_path`:

```rust
/// Returns the cockpit control socket path for a given pid in `dir`.
pub fn cockpit_socket_path(dir: &Path, pid: u32) -> PathBuf {
    dir.join(format!("cockpit-{pid}.sock"))
}
```

In `src/cockpit.rs`, add the accept loop, real impls, server, and entry. Imports at top:

```rust
use std::sync::Arc as StdArc; // alias not needed if Arc already imported; keep single Arc import
```
(Use the existing `use std::sync::{Arc, Mutex};` from Task 4 — do not duplicate.)

Accept loop + server + entry:

```rust
use interprocess::local_socket::tokio::{Listener, Stream};
use interprocess::local_socket::traits::tokio::Listener as _;
use interprocess::local_socket::ListenerOptions;
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::attach::{self, OsExecer};
use crate::env::Env;
use crate::manage;
use crate::mux;
use crate::ui::run::run_switcher;

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
            *pending.lock().unwrap() = Some(t);
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
        remove_cockpit_pointer(&self.xmux_dir);
    }
}

/// Binds the cockpit socket, writes the pointer, and serves it. A bind failure
/// returns `None` (the cockpit still runs; cross-host-from-popup is unavailable).
fn serve_cockpit(env: &Arc<Env>, sock: PathBuf, pending: Pending) -> Option<CockpitHandle> {
    let _ = std::fs::remove_file(&sock);
    let name = control::endpoint_name(&sock).ok()?;
    let listener = ListenerOptions::new().name(name).create_tokio().ok()?;
    #[cfg(windows)]
    let _ = std::fs::write(&sock, b"");
    let _ = write_cockpit_pointer(&env.xmux_dir, &sock);
    let by_alias_keys: std::collections::HashSet<String> =
        env.by_alias.keys().cloned().collect();
    let known: KnownSource = Arc::new(move |s: &str| by_alias_keys.contains(s));
    let task = tokio::spawn(cockpit_accept(listener, pending, known));
    Some(CockpitHandle {
        task,
        sock,
        xmux_dir: env.xmux_dir.clone(),
    })
}

/// The real terminal-handover attach (async, so the cockpit can serve its socket
/// concurrently with the child's run).
struct RealAttacher {
    env: Arc<Env>,
}
#[async_trait]
impl Attacher for RealAttacher {
    async fn attach(&self, target: &Target) {
        let Some(src) = self.env.by_alias.get(&target.session.source).cloned() else {
            eprintln!("xmux: unknown source {:?}", target.session.source);
            return;
        };
        if let Err(e) = attach::nest_guard(attach::in_mux()) {
            eprintln!("xmux: {e}");
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
        let status = tokio::process::Command::new(&argv[0])
            .args(&argv[1..])
            .status()
            .await;
        if let Err(e) = status {
            eprintln!("xmux: attach failed: {e}");
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
        let chosen = result.chosen?;
        Some(Target {
            session: chosen,
            window: (result.window >= 0).then_some(result.window),
        })
    }
}

/// The `xmux` (no subcommand) entry: the persistent cockpit.
pub async fn run_cockpit(env: Arc<Env>) -> i32 {
    if attach::in_mux() {
        eprintln!(
            "xmux: warning — inside a mux; attaching is refused here. Run xmux as your terminal entry."
        );
    }
    let _ = std::fs::create_dir_all(&env.xmux_dir);
    let pending: Pending = Arc::new(Mutex::new(None));
    let sock = control::cockpit_socket_path(&env.xmux_dir, std::process::id());
    let _handle = serve_cockpit(&env, sock, pending.clone());

    let ops = env.ops();
    let control = pick_control_path(&env);
    let attacher = RealAttacher { env: env.clone() };
    let picker = RealPicker { ops, control };
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

// Silence the OsExecer import if unused here; OsExecer is used by run_popup, not
// the cockpit attach. Remove the `OsExecer` from the use list if clippy flags it.
```

Note for the implementer: if `OsExecer` is unused in `cockpit.rs`, drop it from the
`use crate::attach::{self, OsExecer};` line (make it `use crate::attach;`).

- [ ] **Step 4: Run tests to verify they pass**

Run: `…/cargo.exe build` then `…/cargo.exe test --lib endpoint_name_accepts_cockpit_socket cockpit_socket_switch_sets_pending`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/control.rs src/cockpit.rs
git commit -m "feat(cockpit): socket server, async attach, picker, run_cockpit"
```

---

### Task 6: Rework run_popup to signal the cockpit

**Files:**
- Modify: `src/cockpit.rs` (add `signal_cockpit_switch`)
- Modify: `src/main.rs` (`run_popup`; add `use xmux::{cockpit, mux};`)
- Test: `src/cockpit.rs` (`signal_cockpit_switch` round-trip via the accept loop)

**Interfaces:**
- Consumes: `control::Client`, `cockpit_accept` (Task 5).
- Produces: `pub async fn signal_cockpit_switch(sock: &Path, addr: &str) -> anyhow::Result<bool>`.

- [ ] **Step 1: Write the failing test** (add to `src/cockpit.rs` tests)

```rust
#[tokio::test]
async fn signal_cockpit_switch_acks_and_sets_pending() {
    use std::sync::{Arc, Mutex};
    use interprocess::local_socket::tokio::Listener;
    use interprocess::local_socket::traits::tokio::Listener as _;
    use interprocess::local_socket::ListenerOptions;

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

    let ok = signal_cockpit_switch(&sock, "jupiter06/api").await.unwrap();
    assert!(ok, "a known target acks ok");
    assert_eq!(pending.lock().unwrap().clone().unwrap().session.address(), "jupiter06/api");

    let rejected = signal_cockpit_switch(&sock, "nope/x").await.unwrap();
    assert!(!rejected, "an unknown source is not ok");

    task.abort();
    let _ = std::fs::remove_dir_all(&dir);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `…/cargo.exe test --lib cockpit::tests::signal_cockpit_switch_acks_and_sets_pending`
Expected: FAIL — `signal_cockpit_switch` not found.

- [ ] **Step 3: Write minimal implementation**

Add to `src/cockpit.rs`:

```rust
/// Tells the cockpit at `sock` to switch to `addr`. Returns `Ok(true)` when the
/// cockpit acks `ok`, `Ok(false)` on any other reply, `Err` if no cockpit answers.
pub async fn signal_cockpit_switch(sock: &Path, addr: &str) -> anyhow::Result<bool> {
    let mut client = control::Client::dial(sock).await?;
    let reply = client.do_cmd(&format!("switch {addr}")).await?;
    Ok(reply.trim() == "ok")
}
```

Then rework `run_popup` in `src/main.rs`. Replace the current body (from `let plan = …`
through the trailing `0`) and add imports `use xmux::cockpit;` and `use xmux::mux;`:

```rust
async fn run_popup(env: Arc<Env>) -> i32 {
    let result = match run_switcher(env.ops(), control_path(&env)).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("xmux: {e}");
            return 1;
        }
    };
    let Some(chosen) = result.chosen else {
        return 0;
    };
    let cockpit_sock = cockpit::read_cockpit_pointer(&env.xmux_dir);
    match cockpit::decide_popup_action(&chosen, session::LOCAL_SOURCE, cockpit_sock.is_some()) {
        cockpit::PopupAction::SwitchClient => {
            if result.window >= 0 {
                if let Some(src) = env.by_alias.get(&chosen.source) {
                    let _ = manage::select_window(src, &chosen.name, result.window).await;
                }
            }
            let argv = mux::switch_client(&env.local_bin, &chosen.name);
            if let Err(e) = attach::run_attach(&OsExecer, &argv) {
                eprintln!("xmux: {e}");
                return 1;
            }
            0
        }
        cockpit::PopupAction::SignalCockpit => {
            let sock = cockpit_sock.expect("SignalCockpit implies a pointer");
            match cockpit::signal_cockpit_switch(&sock, &chosen.address()).await {
                Ok(true) => {
                    let argv = mux::detach_client(&env.local_bin);
                    if let Err(e) = attach::run_attach(&OsExecer, &argv) {
                        eprintln!("xmux: {e}");
                        return 1;
                    }
                    0
                }
                Ok(false) => {
                    eprintln!("xmux: cockpit rejected the switch");
                    1
                }
                Err(_) => {
                    eprintln!("xmux: cross-host switch needs the xmux cockpit; start your terminal with `xmux`");
                    1
                }
            }
        }
        cockpit::PopupAction::NoCockpit => {
            eprintln!("xmux: cross-host switch needs the xmux cockpit; start your terminal with `xmux`");
            1
        }
    }
}
```

Remove now-unused imports in `main.rs` (`jump` is still used by `run_home` until Task 7;
leave it). Add `use xmux::cockpit;` and `use xmux::mux;` to the import block.

- [ ] **Step 4: Run test + build to verify**

Run: `…/cargo.exe test --lib cockpit::tests::signal_cockpit_switch_acks_and_sets_pending` then `…/cargo.exe build`
Expected: test PASS; build OK (warnings about unused `plan_switch` acceptable until Task 7).

- [ ] **Step 5: Commit**

```bash
git add src/cockpit.rs src/main.rs
git commit -m "feat(popup): cross-server pick signals the cockpit then detaches"
```

---

### Task 7: Make `xmux` the cockpit; retire run_home, jump, plan_switch

**Files:**
- Modify: `src/main.rs` (entry `None => run_cockpit`; delete `run_home`; remove `jump`/`OsExecer`-unused/`plan_switch` usage; drop `use xmux::jump;`)
- Delete: `src/jump.rs`
- Modify: `src/lib.rs` (remove `pub mod jump;`)
- Modify: `src/attach.rs` (remove `SwitchPlan` + `plan_switch` + their tests — made unused by this change)

**Interfaces:**
- Consumes: `cockpit::run_cockpit` (Task 5).

- [ ] **Step 1: Switch the entry and delete run_home**

In `src/main.rs` `run()`, change the no-subcommand arm:

```rust
        None => match interactive_env() {
            Ok(env) => cockpit::run_cockpit(Arc::new(env)).await,
            Err(code) => code,
        },
```

Delete the entire `run_home` function. Remove `use xmux::jump;`. If `OsExecer` / `attach`
remain used by `run_popup` and `run_direct_attach`, keep them.

- [ ] **Step 2: Delete jump and its module declaration**

```bash
git rm src/jump.rs
```
In `src/lib.rs`, remove the `pub mod jump;` line.

- [ ] **Step 3: Remove the now-unused plan_switch/SwitchPlan**

In `src/attach.rs`, delete `pub struct SwitchPlan { … }`, `pub fn plan_switch(…)`, and the
two tests `plan_switch_teleport` and `plan_switch_cross_server`. Keep `in_mux`, `nest_guard`,
`Execer`, `OsExecer`, `run_attach` and their tests. Remove the now-unused `use crate::mux;`
and `use crate::session::Session;` from `attach.rs` ONLY if nothing else there uses them
(check: `run_attach`/`Execer` don't; remove both if so).

- [ ] **Step 4: Build + full test suite + verify it fails for nothing**

Run: `…/cargo.exe build` then `…/cargo.exe test`
Expected: build OK; ALL tests PASS (jump tests gone, plan_switch tests gone, cockpit tests present). No compile errors about missing `run_home`/`jump`/`plan_switch`.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat: make xmux the cockpit; retire detach-to-home, pending-jump, plan_switch"
```

---

### Task 8: Gate — clippy, format, full suite

**Files:** none (verification only)

- [ ] **Step 1: Clippy clean**

Run: `…/cargo.exe clippy --all-targets -- -D warnings`
Expected: no warnings. Fix any (e.g. drop unused imports surfaced by Tasks 5–7).

- [ ] **Step 2: Format**

Run: `…/cargo.exe fmt`
Expected: no diff after (or apply the formatting).

- [ ] **Step 3: Full test suite (twice, for flakiness)**

Run: `…/cargo.exe test` (x2)
Expected: identical green both runs.

- [ ] **Step 4: Commit any gate fixes**

```bash
git add -A
git commit -m "chore: clippy/fmt gate for the cockpit switch model"
```

---

## Live verification (post-implementation, headless via control channel)

Run after Task 8. Throwaway sessions on `jupiter06` only; never touch `local/xmux` or `jupiter00/if`.

1. Build the binary: `…/cargo.exe build`.
2. Create a throwaway remote session:
   `ssh jupiter06 'tmux new-session -d -s xmux-probe'`.
3. Start a cockpit headlessly in a psmux drive window:
   `env -u PSMUX_SESSION -u TMUX -u TMUX_PANE psmux -L <socket> new-window -d -n cockpit 'C:/Projects/xmux/target/debug/xmux.exe'`.
4. The cockpit shows the picker (first launch). Drive it via the picker's ctl channel
   (`xmux ctl dump`, `xmux ctl key down`, `xmux ctl key enter`) to attach a LOCAL session.
5. Simulate the cross-host popup signal: `xmux ctl --sock <cockpit-sock>` is the cockpit
   socket — send `switch jupiter06/xmux-probe`, confirm `ok`, then cause the local child to
   detach (`<localmux> detach-client` against the cockpit's client). Confirm via `dump` /
   process inspection that the cockpit re-attached `ssh -t jupiter06 tmux attach` with NO
   intervening picker frame.
6. Tear down: `ssh jupiter06 'tmux kill-session -t xmux-probe'`; kill the drive window.
7. The single step needing a human eye — the on-screen alternate-screen handover looking
   seamless — is recorded for the user to confirm visually; everything else is asserted
   headlessly.
```
