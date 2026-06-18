---
title: A held process-global StdoutLock deadlocks another thread's terminal draw
date: 2026-06-18
category: runtime-errors
module: PTY proxy (src/proxy/run.rs)
problem_type: runtime_error
component: tooling
symptoms:
  - First overlay hotkey press hangs the whole process; the screen freezes and only SIGKILL recovers it
  - The picker overlay's first draw never appears
  - The full test suite is green and per-task reviews passed, yet the live feature deadlocks on its primary path
root_cause: thread_violation
resolution_type: code_fix
severity: critical
tags: [stdout-lock, deadlock, concurrency, pty-proxy, conpty, terminal, rust]
related_components: ["terminal-handover", "ratatui-crossterm"]
---

# A held process-global StdoutLock deadlocks another thread's terminal draw

## Problem
The PTY-proxy output pump acquired the process-global stdout lock once and held it for the thread's entire lifetime. When the picker overlay (drawing from a different task) tried to write to stdout, it blocked on that held lock forever — the first overlay hotkey press deadlocked the whole process.

## Symptoms
- First `prefix s` (overlay hotkey) hangs the process; screen frozen, only SIGKILL recovers.
- The picker's first `terminal.clear()`/`draw()` never completes.
- 207 passing tests + per-task reviews all green — the deadlock was invisible to them.

## What Didn't Work
- **Trusting the green suite.** Every automated test drove the picker's `event_loop` against a `TestBackend` and never ran `proxy_attach` end-to-end; the only full-path test was `#[ignore]`d (a `cmd.exe` round-trip with no overlay). Nothing ever ran the output pump and the picker against the *real* stdout concurrently, so the contention never occurred under test.
- **Per-task review alone.** The task that introduced the pump and the task that wired the overlay each reviewed clean in isolation; the deadlock only emerges from their *interaction* (pump thread holds the lock + picker thread writes), which a single-task-scoped review does not exercise. The whole-branch review caught it.

## Solution
The pump must never hold the `StdoutLock` across its blocking read, and must not hold it at all while another thread (the picker) owns the terminal. Acquire and release the lock **per write**.

Before (deadlocks):
```rust
let stdout = std::io::stdout();
std::thread::spawn(move || {
    let mut out = stdout.lock();          // held for the whole thread lifetime
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf) {     // ConPTY read never EOFs after child exit
            Ok(0) => break,
            Ok(n) => {
                let chunk = buf[..n].to_vec();
                let _ = pump_tx.send(chunk.clone());
                if !overlay_active.load(Ordering::Acquire) {
                    let _ = out.write_all(&chunk);   // lock still held across the next read
                    let _ = out.flush();
                }
            }
            Err(_) => break,
        }
    }
});
```

After (per-write lock; the picker can draw freely):
```rust
fn pump_write(out: &std::io::Stdout, chunk: &[u8]) {
    let mut o = out.lock();               // acquired and dropped within this call
    let _ = o.write_all(chunk);
    let _ = o.flush();
}
// ...
let stdout = std::io::stdout();
std::thread::spawn(move || {
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let chunk = buf[..n].to_vec();
                let _ = pump_tx.send(chunk.clone());
                if !overlay_active.load(Ordering::Acquire) {
                    pump_write(&stdout, &chunk);   // lock released before the next read
                }
            }
            Err(_) => break,
        }
    }
});
```

## Why This Works
`std::io::Stdout` is a single process-global `ReentrantMutex`. It is re-entrant on the *same* thread, but a `StdoutLock` guard held on one thread blocks `Stdout::write`/`flush` from *any other* thread. The pump thread blocks indefinitely inside `reader.read()` (a ConPTY master read does not return EOF after the child exits — a separate, known finding), so a once-acquired lock is never released. The picker's `CrosstermBackend<Stdout>` write then waits on a lock that will never free. Acquiring per write bounds lock ownership to the microseconds of the actual write, so the picker (and the overlay-close repaint) can take the lock between the pump's writes; the `overlay_active` gate additionally means the pump does not even contend the lock while the picker owns the screen.

## Prevention
- **Never hold a `std::io::Stdout`/`Stderr` lock across a blocking call** (a read, a sleep, a channel recv) when another thread may write to the same stream. Scope the guard to the write itself.
- **A green suite is not coverage for cross-thread contention.** If two code paths write the same global resource from different threads, add a test that exercises them *concurrently*. Here: a timeout-bounded test where a writer thread runs the per-write pattern while a prober thread acquires `std::io::stdout().lock()` and signals over a channel; the main thread asserts the prober succeeds within `recv_timeout(2s)`. A held-lock regression makes the test time out (fail) rather than hang the suite — use a watchdog flag so every thread self-terminates and joins. Example shape:
  ```rust
  // writer thread loops the fixed per-write pattern with EMPTY bytes, then stops on a flag.
  // prober thread: let _g = std::io::stdout().lock(); tx.send(()).unwrap();
  // main: assert!(rx.recv_timeout(Duration::from_secs(2)).is_ok(), "stdout lock held cross-thread");
  ```
- **Lean on a whole-branch review for interaction bugs.** Defects that live in the seam between two independently-correct changes are exactly what a final cross-cutting review (vs. per-task review) is for.

## Related Issues
- Design: `docs/superpowers/specs/2026-06-18-pty-proxy-overlay-design.md` (the cockpit PTY-proxy overlay: single stdin owner, `overlay_active` suppression, vt100 grid restore, pick→`pending` re-attach).
- Plan: `docs/superpowers/plans/2026-06-18-pty-proxy-track1.md`.
- Companion finding: the ConPTY master reader does not return EOF after the child exits, so the output-pump thread is never joined and the master is never dropped at teardown (both would block) — the same property that makes the held lock permanent.
