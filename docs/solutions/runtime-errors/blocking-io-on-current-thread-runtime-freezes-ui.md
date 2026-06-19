---
title: "Blocking I/O on a current_thread async runtime freezes the whole UI"
date: 2026-06-19
category: runtime-errors
module: PTY proxy (src/proxy/run.rs)
problem_type: runtime_error
component: tooling
symptoms:
  - "After the cockpit attaches a remote ssh session, an output flood (scrolling) freezes the whole UI — render, input echo, resize, and the picker hotkey all stop at once"
  - "Only SIGKILL recovers it; the attach child is still alive but the proxy's async loop is wedged"
  - "The freeze tracks a slow consumer — a fast local child never triggers it, a stalled or slow remote does"
root_cause: async_timing
resolution_type: code_fix
severity: high
tags: [blocking-io, current-thread, tokio, pty, conpty, freeze, rust]
---

# Blocking I/O on a current_thread async runtime freezes the whole UI

## Problem
The PTY proxy ran the binary under `#[tokio::main(flavor = "current_thread")]` — one OS thread runs every async task — yet its event loop performed *synchronous, blocking* PTY operations directly on that loop (`pty_writer.write_all()` and `pair.master.resize()`). When the attach child could not drain its input fast enough (a stalled or slow ssh remote), the blocking `write_all` parked the one runtime thread, and everything the loop also drives — rendering, output streaming, terminal resize, the `Ctrl-g` picker hotkey — froze with it.

## Symptoms
- Attaching a remote session and scrolling a burst of output freezes the entire UI; nothing repaints and keystrokes are not echoed.
- The child process is alive; only killing the proxy recovers the terminal.
- Reproducible only against a slow/stalled consumer (remote ssh), never a fast local child — the tell that the loop is blocked on a write, not crashed.

## What Didn't Work
- **Switching to a multi-threaded runtime** (`flavor = "multi_thread"`). A band-aid: a blocking `write_all` would still wrongly stall *its* task, and the real defect — performing blocking syscalls on an async loop — remains. It also adds scheduler overhead and `Send` bounds for no benefit. The fix is to move the blocking off the loop, not to add runtime threads.
- **Leaving the master owned by the loop "to be safe at teardown"** (the prior code held `pair.master` for the whole session and never closed the ConPTY until the process died). This avoided one teardown hazard but never addressed the in-session freeze and leaked the pseudoconsole.

## Solution
Move every blocking PTY operation onto a dedicated OS thread fed by a channel, mirroring the proxy's existing output-pump and stdin-reader threads. The event loop only *sends* (non-blocking); it never writes or resizes the PTY itself.

```rust
// A dedicated PTY control thread owns the writer AND the master.
enum PtyCmd { Input(Vec<u8>), Resize { cols: u16, rows: u16 } }

trait PtySink { fn write_input(&mut self, bytes: &[u8]); fn resize(&mut self, cols: u16, rows: u16); }

fn pty_control_loop(rx: std::sync::mpsc::Receiver<PtyCmd>, mut sink: impl PtySink) {
    while let Ok(cmd) = rx.recv() {            // blocks HERE, on its own thread
        match cmd {
            PtyCmd::Input(b) => sink.write_input(&b),
            PtyCmd::Resize { cols, rows } => sink.resize(cols, rows),
        }
    }
}
```

```rust
// In proxy_attach: spawn the control thread; the loop only sends.
let (pty_tx, pty_rx) = std::sync::mpsc::channel::<PtyCmd>();
std::thread::spawn(move || pty_control_loop(pty_rx, MasterSink { writer: pty_writer, master: pair.master }));

// before (on the async loop — froze it):
//     let _ = pty_writer.write_all(&to_forward); let _ = pty_writer.flush();
// after (non-blocking send):
let _ = pty_tx.send(PtyCmd::Input(std::mem::take(&mut to_forward)));
// resize likewise: let _ = pty_tx.send(PtyCmd::Resize { cols, rows });
```

The control thread also *owns* the master, so dropping it (channel closes at teardown) runs `ClosePseudoConsole` on that thread — never on the runtime thread or the output-read thread.

## Why This Works
The single runtime thread is now never blocked: all blocking I/O lives on dedicated OS threads (this control thread, plus the pre-existing output pump and stdin reader), and the loop does only non-blocking channel sends. Input ordering is preserved because one ordered channel is drained by one reader — input bytes and resizes reach the child in the order they were produced. The runtime is intentionally kept `current_thread`: with the blocking off-loaded, a second runtime thread buys nothing.

The teardown geometry is also *more* correct than before: an adversarial review confirmed that letting the master drop on the control thread runs `ClosePseudoConsole` off the output-read thread, which is exactly Microsoft's documented ConPTY teardown guidance.

## Prevention
- **Rule:** never perform a synchronous blocking call on a `current_thread` tokio loop — `write_all`, `flush`, `resize`, blocking FS, `Command::status()`, any syscall that can park. Off-load it to a dedicated thread + channel, or `tokio::task::spawn_blocking`. On a single-thread runtime, *every* blocking syscall reachable from the event loop is a freeze waiting to happen.
- **Review heuristic:** grep the async loop body for blocking calls (`.write_all`, `.flush`, `.resize`, `.status(`, `std::fs::`). Each is suspect. The safe pattern in this codebase is "dedicated thread owns the blocking resource, loop sends commands."
- **Channel choice:** an unbounded `std::sync::mpsc` is correct here — a *bounded* `send` on a full queue would re-block the runtime thread (reintroducing the freeze). The queue grows only at human typing speed against a wedged child, so unbounded is the right trade.
- **Durable follow-up:** portable-pty 0.9 exposes no async ConPTY-close API (`ReleasePseudoConsole`/timeout), so on Windows older than 24H2 (build < 26100) `ClosePseudoConsole` can block the *control thread itself* (never the runtime thread or `proxy_attach`'s return — the cockpit re-attaches regardless), leaking a thread + pseudoconsole handle per occurrence. The real fix needs a portable-pty dependency bump; a drainer-keepalive workaround was rejected as fragile.

## Related Issues
- [A held process-global StdoutLock deadlocks another thread's terminal draw](held-stdout-lock-deadlocks-cross-thread-draw.md) — same file (`src/proxy/run.rs`) and the same "the proxy hangs the whole process" family, but a DISTINCT bug: that one is a held `StdoutLock` blocking a cross-thread draw (`thread_violation`, fixed by per-write lock scoping); this one is blocking PTY I/O on the async loop (`async_timing`, fixed by the control thread). Read both before touching the proxy's threading.
- [Cockpit model — seamless cross-host mux switching](../architecture-patterns/cockpit-cross-host-switch.md) — the cockpit/proxy architecture this loop lives inside.
