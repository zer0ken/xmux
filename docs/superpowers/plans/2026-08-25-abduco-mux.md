# abduco Mux Support Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `abduco` as a new per-session mux family so xmux can enumerate, attach, and create abduco sessions, reusing the existing PerSession path (psmux/zellij).

**Architecture:** `abduco` is a per-session mux (each session is its own server process with its own socket under `~/.abduco`). It has no control stream (polled), no window/pane concept, and no server-socket flag. It joins `known_muxes()` so discovery, factories, `is_recognized`, and `window_label` cover it automatically; a `Mux` impl owns enumeration/attach/create argv and a `MuxDriver` (reattach-on-change, mirroring zellij) owns display. Because abduco has no per-session query, it overrides `poll_once` to emit each session's single window resolution directly.

**Tech Stack:** Rust; the existing `Mux` trait + `MuxDriver` seam in `src/mux/` / `src/driver.rs`.

---

## Verified abduco behavior (WSL, abduco 0.6 built from source)

- `abduco` (no args) lists sessions; header `Active sessions (on host <host>)` then one line per session, tab-separated:
  `* Tue\t2026-08-25 23:04:28\t168265\tsess1`
  - fields: `<status> <Day>` | `YYYY-MM-DD HH:MM:SS` | `<pid>` | `<name>`
  - status: `*` = a client attached, `+` = command terminated while unattached, ` ` = running, unattached.
- `abduco -a <name>` attaches (blocks until detach); good display attach.
- `abduco -n <name>` creates a detached session running abduco's default command; `-n <name> cmd` with an explicit command also works. A name is required (empty → usage error).
- Identity: `-V` (uppercase) is INVALID (exit 1, usage); `-v` (lowercase) prints `abduco-0.6 © …` and exits 0. `help` subcommand does not exist (prints usage, exit non-zero).

Decisions:
- `last_attached = 0` — abduco prints human LOCAL wall-clock time; converting it to the shared epoch scale across hosts requires the host timezone, which is unavailable at the parse site. 0 is the honest "the mux does not report it" value (per `Session` doc), so abduco sessions within a source sort by name (stable).
- `windows = 1` per session (the session is its own single window), but each session emits an EMPTY pane list so its card resolves the loading placeholder and renders as the session alone (no window row — see `render.rs` line2 logic).
- `poll_once` override: enumerate once, emit `Sessions`, then one empty `Panes` per session (no per-session query exists in abduco).

---

### Task 1: Register abduco in the mux registry + identity detection

**Files:**
- Modify: `src/mux/mod.rs`

- [ ] **Step 1: Add the module declaration, re-export, and registry entry**

In `src/mux/mod.rs`, add `mod abduco;`, re-export `Abduco` / `AbducoDriver`, add an `Abduco` entry to `known_muxes()`, and update `detect_backend` to recognize abduco via its `-v` probe.

```rust
mod abduco;
mod control;
mod psmux;
mod tmux;
pub mod vocab;
mod zellij;

pub use abduco::{Abduco, AbducoDriver};
pub use control::{ControlProtocol, Line, Notif};
pub use psmux::Psmux;
pub use tmux::{Tmux, TmuxControl};
pub use zellij::Zellij;
```

```rust
fn known_muxes() -> &'static [MuxKind] {
    &[
        MuxKind {
            name: "abduco",
            make: |bin| Box::new(Abduco { bin }),
        },
        MuxKind {
            name: "psmux",
            make: |bin| Box::new(Psmux { bin }),
        },
        MuxKind {
            name: "zellij",
            make: |bin| Box::new(Zellij { bin }),
        },
    ]
}
```

- [ ] **Step 2: Update `detect_backend`**

abduco rejects `-V` and names itself in `-v` output, so refactor the marker check into a shared helper and add a `-v` stage after the `-V` stage:

```rust
/// A known-mux marker in a probe's output names that mux (psmux/zellij in `help`,
/// abduco in `-v`). tmux has no marker anywhere, so a markerless working probe falls
/// through to the tmux fallback.
fn mux_from_marker(text: &str, bin: &str) -> Option<Box<dyn Mux>> {
    for k in known_muxes() {
        if text.contains(k.name) {
            return Some((k.make)(bin.to_string()));
        }
    }
    None
}

pub async fn detect_backend(
    transport: &dyn Transport,
    bin: &str,
    runner: &dyn Runner,
) -> Option<Box<dyn Mux>> {
    // psmux and zellij identify themselves in `help`; check it first because psmux lies in `-V`.
    let (name, args) = transport.exec_argv(false, &[bin.to_string(), "help".to_string()]);
    if let Ok(out) = runner.run(&name, &args).await {
        let low = String::from_utf8_lossy(&out).to_lowercase();
        if let Some(m) = mux_from_marker(&low, bin) {
            return Some(m);
        }
    }
    // `-V` is tmux's version flag: a working one with no marker is a real tmux.
    let (name, args) = transport.exec_argv(false, &[bin.to_string(), "-V".to_string()]);
    if let Ok(out) = runner.run(&name, &args).await {
        let low = String::from_utf8_lossy(&out).to_lowercase();
        if let Some(m) = mux_from_marker(&low, bin) {
            return Some(m);
        }
        return Some(tmux_fallback(bin));
    }
    // `-v` is abduco's version flag (it rejects `-V`). Only reached when `-V` already
    // failed, so the binary is not a tmux that would hang on tmux's verbose `-v`.
    let (name, args) = transport.exec_argv(false, &[bin.to_string(), "-v".to_string()]);
    if let Ok(out) = runner.run(&name, &args).await {
        let low = String::from_utf8_lossy(&out).to_lowercase();
        if let Some(m) = mux_from_marker(&low, bin) {
            return Some(m);
        }
    }
    None
}
```

- [ ] **Step 3: Update `mod.rs` tests**

Update the supported-muxes list, `server_socket_for`, and add abduco detect/behavior tests (see Task 5 for full assertions). At minimum:
- `discovery_only_looks_for_muxes_xmux_can_drive` expects `vec!["tmux", "abduco", "psmux", "zellij"]`.
- `a_socket_reaches_only_a_mux_that_takes_one` expects `server_socket_for("abduco", sock()) == None`.

- [ ] **Step 4: Commit**

```bash
git add src/mux/mod.rs
git commit -m "feat(mux): register abduco family and identity detection"
```

---

### Task 2: Add the abduco mux family

**Files:**
- Create: `src/mux/abduco/mod.rs`
- Create: `src/mux/abduco/display.rs`
- Create: `src/mux/abduco/AGENTS.md`

- [ ] **Step 1: Create `src/mux/abduco/mod.rs`**

The mux impl (see the source in the implementation; exact content below):

```rust
//! abduco: one server per session, no control mode, no windows, every session a
//! single PTY. Enumerated from `abduco`'s own listing and polled for change.

use super::*;
use crate::link::HostEvent;
use crate::model::source::RunError;
use crate::session::{Pane, Session, WindowPanes};
use crate::transport::Transport;

pub mod display;

pub use display::AbducoDriver;

/// The abduco poll cadence. abduco pushes no change events, so the session list is
/// re-enumerated; one sweep costs one `abduco` process spawn (no per-session query
/// exists), so the cadence sits between psmux's local registry read and zellij's
/// per-session ssh round trips.
const ABDUCO_POLL_MS: u64 = 2000;

/// abduco: one server per session, enumerated from its listing, polled for change,
/// each session displayed through its own attachment.
pub struct Abduco {
    pub bin: String,
}

#[async_trait]
impl Mux for Abduco {
    /// abduco has no server-socket flag; sessions live under `~/.abduco`.
    fn takes_server_socket(&self) -> bool {
        false
    }

    fn kind(&self) -> &str {
        "abduco"
    }

    fn bin(&self) -> &str {
        &self.bin
    }

    fn server_model(&self) -> ServerModel {
        ServerModel::PerSession
    }

    fn driver(&self) -> Box<dyn crate::driver::MuxDriver> {
        Box::new(AbducoDriver)
    }

    fn clone_box(&self) -> Box<dyn Mux> {
        Box::new(Self {
            bin: self.bin.clone(),
        })
    }

    /// The bare binary with no arguments IS the listing.
    fn list_sessions_plan(&self) -> Vec<String> {
        vec![self.bin.clone()]
    }

    async fn enumerate(
        &self,
        transport: &dyn Transport,
        runner: &dyn Runner,
    ) -> Result<Vec<Session>, RunError> {
        let argv = self.list_sessions_plan();
        let (name, args) = transport.exec_argv(false, &argv);
        match runner.run(&name, &args).await {
            Ok(out) => Ok(parse_sessions(
                transport.host_id(),
                self.kind(),
                &String::from_utf8_lossy(&out),
            )),
            Err(e) if crate::mux::is_no_sessions(&e) => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    fn attach_plan(&self, session: &str) -> Vec<String> {
        vec![
            self.bin.clone(),
            "-a".to_string(),
            session.to_string(),
        ]
    }

    fn control_argv(&self) -> Option<Vec<String>> {
        // abduco has no control-mode channel.
        None
    }

    fn death_signal(&self) -> DeathSignal {
        // One server per session, so the attachment dying IS the session dying.
        DeathSignal::Eof
    }

    fn event_source(&self) -> EventSource {
        EventSource::Poll {
            interval_ms: ABDUCO_POLL_MS,
        }
    }

    /// abduco has no per-session query and no windows: each session is the whole
    /// session. Polling therefore enumerates once and resolves every session's card
    /// directly with an empty pane list (the session alone, no window row) instead of
    /// running a per-session command that cannot exist.
    async fn poll_once(
        &self,
        source: &str,
        transport: &dyn Transport,
        runner: &dyn Runner,
        emit: &mut (dyn FnMut(HostEvent) + Send),
    ) {
        let (sessions, err) =
            match within_poll_budget("list-sessions", self.enumerate(transport, runner)).await {
                Ok(s) => (s, None),
                Err(e) => (Vec::new(), Some(e.to_string())),
            };
        let addresses: Vec<String> = sessions.iter().map(|s| s.address()).collect();
        emit(HostEvent::Sessions {
            source: source.to_string(),
            sessions,
            err,
        });
        for address in addresses {
            emit(HostEvent::Panes {
                address,
                panes: Vec::new(),
            });
        }
    }

    fn new_session_plan(&self, name: &str) -> Vec<String> {
        // `-n` creates a session without attaching, running abduco's default command
        // (typically dvtm, the user's tool inside the session — out of xmux's scope).
        // abduco cannot auto-name, so an empty name fails like zellij's detached create.
        vec![
            self.bin.clone(),
            "-n".to_string(),
            name.to_string(),
        ]
    }

    fn window_label(&self, _index: i64, name: &str) -> String {
        // abduco has no windows: the session is its own single window, named itself.
        name.to_string()
    }
}

/// Parses `abduco`'s listing into sessions tagged with `source`/`mux`. Lines are
/// `<status> <Day>\t<YYYY-MM-DD HH:MM:SS>\t<pid>\t<name>`; the header and any banner
/// carry no tabs and are skipped. `attached` reads the leading status char (`*`).
/// abduco prints human local time, not a host-timezone-free epoch, so `last_attached`
/// is 0 (the mux does not report it).
pub fn parse_sessions(source: &str, mux: &str, out: &str) -> Vec<Session> {
    let mut sessions = Vec::new();
    for ln in out.split('\n') {
        let ln = ln.strip_suffix('\r').unwrap_or(ln);
        let fields: Vec<&str> = ln.split('\t').collect();
        if fields.len() < 4 {
            continue;
        }
        let status = fields[0].chars().next().unwrap_or(' ');
        let name = fields[3..].join("\t");
        if name.is_empty() {
            continue;
        }
        sessions.push(Session {
            source: source.to_string(),
            name,
            mux: mux.to_string(),
            windows: 1,
            attached: status == '*',
            last_attached: 0,
        });
    }
    sessions
}
```

- [ ] **Step 2: Create `src/mux/abduco/display.rs`**

A per-session driver identical in shape to `ZellijDriver` (reattach on every change, keep the stale attachment until the fresh one is ready, no in-place switch, sync reaps only when empty):

```rust
//! The abduco display driver: a per-session mux (one server per session) displayed
//! through ONE per-host PTY that is REATTACHED whenever the selected session changes.
//! `Abduco::driver` constructs it, so mux selection lives in the abduco family.

use std::sync::{Arc, Mutex};

use crate::app::runtime::{host_selection_key, request_attach, terminal_view_size};
use crate::display::grid::Grid;
use crate::driver::{DriverCtx, MuxDriver};
use crate::model::Selection;

/// Per-session mux (abduco): one server per session, displayed through ONE per-host
/// PTY that is REATTACHED whenever the selected session changes (`abduco -a <name>`
/// attaches to that session's own server).
pub struct AbducoDriver;

impl MuxDriver for AbducoDriver {
    fn kind(&self) -> &str {
        "abduco"
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

        // REATTACH, always: the only way to move abduco's display. The stale attachment
        // is KEPT (not removed) so its grid stays on screen until DisplayReady swaps in
        // the new one and tears the stale one down (stale-while-revalidate).
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
            crate::driver::lower_select_window(host, control, &sel.session, win);
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
```

- [ ] **Step 3: Create `src/mux/abduco/AGENTS.md`** — Working Notes following the seven-section format, modeled on `psmux/AGENTS.md` but for abduco's shape (per-session, no windows, no control stream, no server-socket flag, single-window cards).

- [ ] **Step 4: Commit**

```bash
git add src/mux/abduco
git commit -m "feat(mux): add abduco per-session mux family"
```

---

### Task 3: Add abduco unit tests

**Files:**
- Modify: `src/mux/abduco/mod.rs` (tests module)

Add tests mirroring zellij's: kind/model/event/death, attach argv, create argv, listing plan, enumerate parse (with a real captured listing), idle/empty and unreachable classification, and the poll_once single-window resolution.

- [ ] **Step 1: Write tests** (see implementation for full assertions; key cases below)

```rust
#[tokio::test]
async fn enumerate_reads_the_listing() {
    let m = abduco();
    let out = "Active sessions (on host localhost)\n* Tue\t2026-08-25 23:04:28\t168265\tsess1\n  Tue\t2026-08-25 23:03:45\t168097\tbuild\n";
    let runner = CannedRunner::ok(out);
    let got = m.enumerate(&ssh("jup"), &runner).await.unwrap();
    let names: Vec<&str> = got.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["sess1", "build"]);
    assert!(got[0].attached, "the * marker means a client is attached");
    assert!(!got[1].attached);
    assert!(got.iter().all(|s| s.source == "jup" && s.mux == "abduco"));
    assert_eq!(got[0].windows, 1);
    assert_eq!(got[0].last_attached, 0);
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test --lib mux::abduco
```

- [ ] **Step 3: Commit**

```bash
git add src/mux/abduco/mod.rs
git commit -m "test(mux): cover abduco enumeration, argv, and poll"
```

---

### Task 4: Update config validation text

**Files:**
- Modify: `src/provision/config.rs`

- [ ] **Step 1: Update the three warning strings** `(tmux/psmux/zellij)` → `(tmux/psmux/zellij/abduco)`.
- [ ] **Step 2: Run config tests**

```bash
cargo test --lib provision::config
```

- [ ] **Step 3: Commit**

```bash
git add src/provision/config.rs
git commit -m "feat(config): recognize abduco in mux validation"
```

---

### Task 5: Full-suite verification (Windows)

- [ ] `cargo build` — compiles.
- [ ] `cargo test` — full suite passes (the psmux/zellij/abduco tests all use injected runners, so no real mux needed on Windows).

---

### Task 6: Live verification in WSL against real abduco

- [ ] Build xmux for Linux in WSL (`/root/xmux-wsl`) with the worktree source.
- [ ] Create real abduco sessions in WSL, run xmux's enumeration against them, and confirm names/attached parse correctly.
- [ ] Confirm `abduco -n` / `abduco -a` argv are accepted by the real binary.

---

### Task 7: Documentation propagation

**Files:**
- Modify: `CONTEXT.md` — glossary `Mux` axis and architecture `src/mux/<kind>/` family list to include abduco.
- Modify: `src/mux/AGENTS.md` — add the abduco family bullet and mental-model note.
- Modify: `README.md` / `README.ko.md` — supported-mux lists and the config example.
- Modify: `docs/requirements.md` — FR-A6/FR-A9 supported-mux phrasing.
- Create: `docs/superpowers/plans/2026-08-25-abduco-mux.md` (this file, already created).

Follow the project rule: documents state behavior/design rules and name no test/function/method/field/library API.

- [ ] Commit:

```bash
git add -A
git commit -m "docs: propagate abduco mux family across working notes and docs"
```

---

### Task 8: Final review + PR

- [ ] `cargo test` green; `cargo build --release` green.
- [ ] Request review, incorporate feedback.
- [ ] Simplify pass, then a second review.
- [ ] Open a PR from `feat/abduco-mux` to `main`.
