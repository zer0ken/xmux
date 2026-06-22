# Rapid Navigation Freeze Stabilization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make rapid tree navigation unable to freeze the current-thread runtime by proving and isolating blocking display-attach work.

**Architecture:** The immediate target is not a broad actor rewrite. The first target is evidence: instrument the selection and display-attach path, then add a deterministic test seam that injects slow attach work and proves the UI/control loop stays responsive. Attach orchestration moves behind an off-loop display worker only when that evidence points at blocking attach work.

**Tech Stack:** Rust 2021, Tokio current-thread runtime, ratatui, crossterm, portable-pty, std OS threads, tokio mpsc channels, cargo test.

---

## Review Decisions

The stabilization direction has one blocking concern: moving work into an async actor is not enough if the actor still runs synchronous PTY open, process spawn, or teardown work on the current-thread runtime. The plan therefore treats `registry.ensure(...)` and `spawn_attachment(...)` as the first suspect and requires a deterministic slow-attach test as the gate for a structural rewrite.

The revised order is:

1. Add evidence with no behavior change.
2. Add a slow attach seam and a failing responsiveness test.
3. Move display attach work off the runtime thread.
4. Add remote switch serialization only if evidence still shows stale switch races.
5. Extract cockpit responsibilities only when the freeze path is isolated.

---

## File Structure

### Modify: `src/cockpit.rs`

Responsibilities touched:
- Selection timing instrumentation.
- Display attach timing instrumentation.
- Test-only seams for selection storm handling where practical.
- Small call-site changes when display attach moves behind a worker.

Keep this file as the runtime loop until the blocking path is proven. Do not start by splitting the whole cockpit.

### Modify: `src/proxy/registry.rs`

Responsibilities touched:
- Add a narrow spawn injection seam or constructor parameter for tests.
- Add timing instrumentation around `ensure(...)`.
- Preserve existing public behavior for live code.

The registry still owns attachments unless the display worker task explicitly takes ownership in a later task.

### Modify: `src/proxy/run.rs`

Responsibilities touched:
- Expose the minimal type/function boundary needed for injected attach spawning.
- Preserve the existing dedicated PTY control thread model.
- Add tests for slow spawn handling only through public or crate-local seams.

### Create: `src/display.rs`

Responsibilities:
- Own the off-loop display attach worker once Phase 1 starts.
- Define request and event types:
  - `DisplayRequest::Ensure { seq, key, argv, cols, rows }`
  - `DisplayRequest::ResizeAll { cols, rows }`
  - `DisplayRequest::Remove { key }`
  - `DisplayEvent::Ready { seq, key }`
  - `DisplayEvent::Failed { seq, key, message }`
- Keep synchronous PTY work on an OS thread, not on the Tokio current-thread runtime.

Do not add this file until the failing test describes the required behavior.

### Modify: `src/lib.rs`

Responsibilities touched:
- Add `pub mod display;` only when `src/display.rs` exists.

### Test Targets

- `src/proxy/registry.rs` unit tests for the injected slow ensure seam.
- `src/display.rs` unit tests for off-loop responsiveness.
- `src/cockpit.rs` focused tests for selection sequence handling if a seam can be kept small.

---

## Task 1: Add Timing Evidence Around Existing Display Attach Path

**Files:**
- Modify: `src/cockpit.rs`
- Modify: `src/proxy/registry.rs`

- [ ] **Step 1: Add a failing test for timing helper formatting**

Add a small unit test in `src/cockpit.rs` or `src/proxy/registry.rs` for the timing helper that will format elapsed milliseconds consistently. The test should fail because the helper does not exist yet.

```rust
#[test]
fn slow_log_line_includes_label_and_elapsed_ms() {
    let line = slow_log_line("registry.ensure", 42);
    assert_eq!(line, "SLOW registry.ensure 42ms");
}
```

- [ ] **Step 2: Run the focused test and verify RED**

Run:

```powershell
cargo test slow_log_line_includes_label_and_elapsed_ms
```

Expected: compile failure or test failure because `slow_log_line` is not defined.

- [ ] **Step 3: Implement the helper**

Add the minimal helper near the existing debug logging helpers in `src/cockpit.rs`, or in `src/proxy/registry.rs` if the registry owns the log formatting.

```rust
fn slow_log_line(label: &str, ms: u128) -> String {
    format!("SLOW {label} {ms}ms")
}
```

- [ ] **Step 4: Route `dbg_ms` through the helper**

Keep behavior identical and make the formatting test cover the emitted shape.

```rust
fn dbg_ms(dir: &std::path::Path, label: &str, start: std::time::Instant) {
    let ms = start.elapsed().as_millis();
    if ms >= 10 {
        dbg_log(dir, &slow_log_line(label, ms));
    }
}
```

- [ ] **Step 5: Add specific timing probes**

Add probes for:
- `registry.ensure` inside `select_attach(...)`.
- `registry.ensure` inside `sync_source_terminals(...)`.
- `registry.ensure` inside reconnect remote warm path.
- grid lock wait around draw.
- `clear_grid`.

Use existing `XMUX_DEBUG` behavior. Do not change normal runtime behavior.

- [ ] **Step 6: Verify GREEN**

Run:

```powershell
cargo test slow_log_line_includes_label_and_elapsed_ms
cargo test --no-run
```

Expected: the focused test passes and the project compiles.

- [ ] **Step 7: Commit**

```powershell
git add src/cockpit.rs src/proxy/registry.rs
git commit -m "test: add display attach timing probes"
```

---

## Task 2: Add a Deterministic Slow Attach Seam

**Files:**
- Modify: `src/proxy/registry.rs`
- Modify: `src/proxy/run.rs`

- [ ] **Step 1: Write the failing slow-spawn test**

Add a unit test proving `AttachRegistry` can be constructed with an injected spawn function that sleeps and returns a fake attachment. The test should fail because the injection constructor does not exist.

```rust
#[test]
fn registry_ensure_uses_injected_spawner() {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_spawner = calls.clone();
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<crate::proxy::run::PtyEvent>();
    let mut registry = AttachRegistry::with_spawner(
        tx,
        Box::new(move |_argv, _cols, _rows, id, _events| {
            calls_for_spawner.fetch_add(1, Ordering::SeqCst);
            Ok(crate::proxy::run::fake_attachment(id))
        }),
    );

    let argv = vec!["cmd.exe".to_string(), "/c".to_string(), "rem".to_string()];
    assert_eq!(registry.ensure("local/a", &argv, 80, 24).unwrap(), 1);
    assert_eq!(registry.ensure("local/a", &argv, 80, 24).unwrap(), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
```

- [ ] **Step 2: Run the focused test and verify RED**

Run:

```powershell
cargo test registry_ensure_uses_injected_spawner
```

Expected: compile failure because `AttachRegistry::with_spawner` does not exist.

- [ ] **Step 3: Introduce the spawner type**

Add this type alias in `src/proxy/registry.rs`:

```rust
type AttachmentSpawner = Box<
    dyn Fn(
            &[String],
            u16,
            u16,
            u64,
            tokio::sync::mpsc::UnboundedSender<PtyEvent>,
        ) -> anyhow::Result<Attachment>
        + Send
        + Sync,
>;
```

- [ ] **Step 4: Store the spawner in `AttachRegistry`**

Add a `spawner: AttachmentSpawner` field. `new(...)` should use the real `spawn_attachment`.

```rust
pub fn new(events: tokio::sync::mpsc::UnboundedSender<PtyEvent>) -> Self {
    Self::with_spawner(events, Box::new(spawn_attachment))
}
```

- [ ] **Step 5: Add `with_spawner`**

```rust
pub fn with_spawner(
    events: tokio::sync::mpsc::UnboundedSender<PtyEvent>,
    spawner: AttachmentSpawner,
) -> Self {
    AttachRegistry {
        map: HashMap::new(),
        next_id: 1,
        events,
        spawner,
    }
}
```

- [ ] **Step 6: Use the spawner in `ensure`**

```rust
let att = (self.spawner)(argv, cols, rows, id, self.events.clone())?;
```

- [ ] **Step 7: Verify GREEN**

Run:

```powershell
cargo test registry_ensure_uses_injected_spawner
cargo test proxy::registry
```

Expected: registry tests pass.

- [ ] **Step 8: Commit**

```powershell
git add src/proxy/registry.rs
git commit -m "test: inject display attachment spawner"
```

---

## Task 3: Prove Slow Attach Blocks the Runtime in the Current Design

**Files:**
- Modify: `src/proxy/registry.rs`
- Modify: `src/cockpit.rs` if a small helper is needed

- [ ] **Step 1: Write the failing responsiveness test**

Add a test that injects a spawner sleeping for 500 ms, calls the same synchronous ensure path, and demonstrates the current call does not return within a short bound. This test documents the current hazard and should be marked ignored only if it would slow normal runs. Prefer a fast 100 ms sleep if stable.

```rust
#[test]
fn synchronous_ensure_with_slow_spawner_exceeds_responsiveness_budget() {
    use std::time::{Duration, Instant};

    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<crate::proxy::run::PtyEvent>();
    let mut registry = AttachRegistry::with_spawner(
        tx,
        Box::new(move |_argv, _cols, _rows, id, _events| {
            std::thread::sleep(Duration::from_millis(100));
            Ok(crate::proxy::run::fake_attachment(id))
        }),
    );

    let argv = vec!["cmd.exe".to_string(), "/c".to_string(), "rem".to_string()];
    let started = Instant::now();
    let _ = registry.ensure("local/slow", &argv, 80, 24).unwrap();
    assert!(
        started.elapsed() >= Duration::from_millis(100),
        "current synchronous ensure blocks the caller"
    );
}
```

- [ ] **Step 2: Run and verify the characterization test**

Run:

```powershell
cargo test synchronous_ensure_with_slow_spawner_exceeds_responsiveness_budget
```

Expected: pass. This is a characterization test, not the final desired behavior.

- [ ] **Step 3: Write the desired off-loop responsiveness test**

Create the failing test for the new worker API. It should fail because `display::DisplayWorker` does not exist.

```rust
#[tokio::test(flavor = "current_thread")]
async fn display_worker_slow_ensure_does_not_block_runtime_ticks() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_spawner = calls.clone();
    let worker = crate::display::DisplayWorker::with_spawner(Box::new(
        move |_argv, _cols, _rows, id, _events| {
            calls_for_spawner.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(100));
            Ok(crate::proxy::run::fake_attachment(id))
        },
    ));

    worker.ensure(
        crate::display::DisplayEnsure {
            seq: 7,
            key: "local/slow".to_string(),
            argv: vec!["cmd.exe".to_string(), "/c".to_string(), "rem".to_string()],
            cols: 80,
            rows: 24,
        },
    );

    tokio::time::timeout(Duration::from_millis(20), tokio::time::sleep(Duration::from_millis(1)))
        .await
        .expect("runtime tick must not be blocked by slow ensure");

    let event = tokio::time::timeout(Duration::from_millis(300), worker.recv())
        .await
        .expect("worker returns an event")
        .expect("event channel remains open");

    assert!(matches!(
        event,
        crate::display::DisplayEvent::Ready { seq: 7, ref key } if key == "local/slow"
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
```

- [ ] **Step 4: Run and verify RED**

Run:

```powershell
cargo test display_worker_slow_ensure_does_not_block_runtime_ticks
```

Expected: compile failure because `crate::display` does not exist.

- [ ] **Step 5: Commit the tests only if the repository policy accepts red commits**

If red commits are not desired, keep the failing test in the working tree and proceed directly to Task 4.

---

## Task 4: Add the Off-Loop Display Worker

**Files:**
- Create: `src/display.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: Add the module declaration**

In `src/lib.rs`:

```rust
pub mod display;
```

- [ ] **Step 2: Implement request and event types**

In `src/display.rs`:

```rust
use crate::proxy::registry::AttachRegistry;
use crate::proxy::run::{Attachment, PtyEvent};

pub struct DisplayEnsure {
    pub seq: u64,
    pub key: String,
    pub argv: Vec<String>,
    pub cols: u16,
    pub rows: u16,
}

pub enum DisplayEvent {
    Ready { seq: u64, key: String },
    Failed { seq: u64, key: String, message: String },
}

type AttachmentSpawner = Box<
    dyn Fn(
            &[String],
            u16,
            u16,
            u64,
            tokio::sync::mpsc::UnboundedSender<PtyEvent>,
        ) -> anyhow::Result<Attachment>
        + Send
        + Sync
        + 'static,
>;
```

- [ ] **Step 3: Implement the worker shell**

Use one OS thread that owns `AttachRegistry`.

```rust
pub struct DisplayWorker {
    tx: std::sync::mpsc::Sender<DisplayEnsure>,
    rx: tokio::sync::mpsc::UnboundedReceiver<DisplayEvent>,
}
```

- [ ] **Step 4: Implement `with_spawner`**

The worker thread receives ensure requests, calls `registry.ensure(...)` on that OS thread, and sends `DisplayEvent` through a Tokio unbounded sender.

```rust
impl DisplayWorker {
    pub fn with_spawner(spawner: AttachmentSpawner) -> Self {
        let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<DisplayEnsure>();
        std::thread::spawn(move || {
            let (pty_tx, _pty_rx) = tokio::sync::mpsc::unbounded_channel::<PtyEvent>();
            let mut registry = AttachRegistry::with_spawner(pty_tx, spawner);
            while let Ok(req) = cmd_rx.recv() {
                let result = registry.ensure(&req.key, &req.argv, req.cols, req.rows);
                let event = match result {
                    Ok(_) => DisplayEvent::Ready {
                        seq: req.seq,
                        key: req.key,
                    },
                    Err(e) => DisplayEvent::Failed {
                        seq: req.seq,
                        key: req.key,
                        message: e.to_string(),
                    },
                };
                let _ = event_tx.send(event);
            }
            registry.teardown_all();
        });
        DisplayWorker { tx: cmd_tx, rx: event_rx }
    }

    pub fn ensure(&self, req: DisplayEnsure) {
        let _ = self.tx.send(req);
    }

    pub async fn recv(&mut self) -> Option<DisplayEvent> {
        self.rx.recv().await
    }
}
```

If the test needs `worker.recv()` while `worker.ensure(...)` borrows immutably, make `recv` take `&mut self` and bind the worker as mutable in the test.

- [ ] **Step 5: Verify GREEN**

Run:

```powershell
cargo test display_worker_slow_ensure_does_not_block_runtime_ticks
cargo test proxy::registry
cargo test --no-run
```

Expected: focused display worker test passes, registry tests pass, project compiles.

- [ ] **Step 6: Commit**

```powershell
git add src/display.rs src/lib.rs src/proxy/registry.rs
git commit -m "feat: run display attach ensure off loop"
```

---

## Task 5: Integrate Latest-Sequence Selection With the Worker

**Files:**
- Modify: `src/cockpit.rs`
- Modify: `src/display.rs`

- [ ] **Step 1: Write the failing sequence test**

Add a test for a small pure helper that accepts current sequence and an incoming event sequence.

```rust
#[test]
fn display_event_is_current_only_when_sequence_matches() {
    assert!(display_event_is_current(9, 9));
    assert!(!display_event_is_current(9, 8));
    assert!(!display_event_is_current(9, 10));
}
```

- [ ] **Step 2: Run and verify RED**

Run:

```powershell
cargo test display_event_is_current_only_when_sequence_matches
```

Expected: compile failure because `display_event_is_current` does not exist.

- [ ] **Step 3: Implement the helper**

Place the helper near the selection code in `src/cockpit.rs`.

```rust
fn display_event_is_current(current_seq: u64, event_seq: u64) -> bool {
    current_seq == event_seq
}
```

- [ ] **Step 4: Add selection sequence state**

In `run_cockpit`, add:

```rust
let mut selection_seq: u64 = 0;
```

When `new_sel != selection`, increment it with saturating behavior:

```rust
selection_seq = selection_seq.saturating_add(1);
```

- [ ] **Step 5: Send debounced ensure requests to the worker**

Replace direct display attach ensure calls on the selection hot path with worker requests. Keep remote `switch-client` serialization out of this task unless the test requires it.

- [ ] **Step 6: Consume display events**

Add a select arm for worker events. Apply only events where `display_event_is_current(selection_seq, seq)` is true. Stale ready events must not move selection or clear current state.

- [ ] **Step 7: Verify GREEN**

Run:

```powershell
cargo test display_event_is_current_only_when_sequence_matches
cargo test --no-run
```

Expected: focused test passes and project compiles.

- [ ] **Step 8: Commit**

```powershell
git add src/cockpit.rs src/display.rs
git commit -m "feat: gate display attach by selection sequence"
```

---

## Task 6: Add Remote Switch Serialization Only If Evidence Requires It

**Files:**
- Modify: `src/cockpit.rs`
- Create: `src/host_switch.rs` if a focused type is needed
- Modify: `src/lib.rs` if `src/host_switch.rs` is created

- [ ] **Step 1: Check Phase 0 logs**

Review `XMUX_DEBUG` logs from rapid navigation. Continue this task only if logs show stale or overlapping remote `switch-client` commands once display attach work is off-loop.

- [ ] **Step 2: Write the failing coalescing test**

If needed, create a pure host-switch queue test:

```rust
#[test]
fn host_switch_queue_keeps_only_latest_target_per_host() {
    let mut q = HostSwitchQueue::default();
    q.push("jup", 1, "api");
    q.push("jup", 2, "logs");
    q.push("jup", 3, "shell");
    assert_eq!(q.next("jup"), Some(HostSwitch { seq: 3, session: "shell".into() }));
}
```

- [ ] **Step 3: Run and verify RED**

Run:

```powershell
cargo test host_switch_queue_keeps_only_latest_target_per_host
```

Expected: compile failure because the queue type does not exist.

- [ ] **Step 4: Implement the smallest queue**

Use `HashMap<String, HostSwitch>` keyed by host. Do not add a general actor if a plain queue type covers the observed race.

- [ ] **Step 5: Verify GREEN**

Run:

```powershell
cargo test host_switch_queue_keeps_only_latest_target_per_host
cargo test --no-run
```

Expected: focused test passes and project compiles.

- [ ] **Step 6: Commit**

```powershell
git add src/cockpit.rs src/host_switch.rs src/lib.rs
git commit -m "feat: serialize remote session switches"
```

---

## Task 7: Rapid Navigation Verification

**Files:**
- Modify: existing tests only if needed
- Create: `tests/rapid_nav.rs` only if an integration harness can run without a real terminal

- [ ] **Step 1: Run focused unit tests**

```powershell
cargo test display_worker_slow_ensure_does_not_block_runtime_ticks
cargo test registry_ensure_uses_injected_spawner
cargo test display_event_is_current_only_when_sequence_matches
```

Expected: all focused tests pass.

- [ ] **Step 2: Run project test compile**

```powershell
cargo test --no-run
```

Expected: test binaries build.

- [ ] **Step 3: Run full automated tests**

```powershell
cargo test
```

Expected: all non-ignored tests pass.

- [ ] **Step 4: Run live debug gate**

In a real non-nested terminal:

```powershell
$env:XMUX_DEBUG='1'
cargo run --bin xmux
```

Manual exercise:
- Hold rapid `j` and `k` navigation across sessions and windows.
- Pause repeatedly to trigger debounced attach.
- Open another terminal and inspect the debug log under the xmux directory.

Expected:
- No full-process freeze.
- `registry.ensure` and display attach spans do not block the event loop.
- Draw spans remain bounded.
- Stale display events do not change the current selection.

- [ ] **Step 5: Commit verification-only adjustments**

If only test or log tweaks were needed:

```powershell
git add src tests
git commit -m "test: verify rapid navigation responsiveness"
```

---

## Stop Conditions

Stop and reassess if:
- Timing logs show draw grid lock wait, not attach ensure, is the dominant stall.
- `HostManager::ensure(...)` blocks the loop ahead of display attach work.
- The off-loop worker needs ownership of live grid handles in a way that breaks current rendering.
- The control-socket harness cannot exercise the same path as real rapid navigation.

In those cases, write the observed evidence into `docs/solutions/runtime-errors/` instead of adding more architecture.
