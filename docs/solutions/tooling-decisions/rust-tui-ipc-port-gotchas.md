---
title: Porting a Go tview/tcell TUI to Rust ratatui — crate gotchas and approach
date: 2026-06-16
category: tooling-decisions
module: xmux
problem_type: tooling_decision
component: tooling
severity: medium
applies_when:
  - "Porting a Go terminal UI (tview/tcell) to Rust (ratatui/crossterm/tokio)"
  - "Building a cross-platform local-socket IPC channel in Rust (Windows + unix)"
  - "Live-verifying a real-terminal TUI from a headless/automated session"
tags: [rust, ratatui, crossterm, tokio, interprocess, tui, ipc, windows, porting]
---

# Porting a Go tview/tcell TUI to Rust ratatui — crate gotchas and approach

## Context

xmux (a stateless cross-environment tmux/psmux session switcher) was ported from
Go to Rust on branch `feat/rust-rewrite`. The Go stack was tview/tcell (retained-
mode TUI) + cobra (CLI) + goroutines. The Rust stack is ratatui 0.30 + crossterm
0.29 + tokio (current-thread) + clap + serde/toml + interprocess 2.4. Several
crate behaviors differ enough from the Go equivalents that they cost real time;
capturing them here so the next Rust TUI/IPC port is faster.

## Guidance

**1. `interprocess` uses NAMED PIPES on Windows, not AF_UNIX.** A filesystem-path
endpoint (`path.to_fs_name::<GenericFilePath>()`) fails on Windows with
`Custom { kind: Unsupported, error: "not a named pipe path" }`. Go's
`net.Listen("unix", path)` works on Windows (Go uses AF_UNIX there); the Rust
`interprocess` crate does not — it routes local sockets through named pipes on
Windows. Make the endpoint name platform-specific, and keep a separate filesystem
marker file so a filesystem-glob discovery still works on both platforms:

```rust
pub fn endpoint_name(path: &Path) -> std::io::Result<Name<'static>> {
    #[cfg(unix)] // the path IS the AF_UNIX socket AND the discovery marker
    { path.to_owned().into_os_string().to_fs_name::<GenericFilePath>() }
    #[cfg(windows)] // named pipe has no fs presence; derive a stable name from the pid
    {
        let pid = pid_from_sock(path).ok_or_else(/* ... */)?;
        format!("xmux-ctl-{pid}").to_ns_name::<GenericNamespaced>()
    }
}
```

On Windows also write an empty `ctl-<pid>.sock` marker file next to where the
unix socket would live, so a `read_dir` + glob `discover()` finds running
instances by pid on both OSes. Remove it on shutdown.

**2. A control/automation client must STREAM commands over ONE connection.** When
driving the async event loop over the socket, separate process invocations
(`ctl key /`, then `ctl text x`, then `ctl key enter`) race against the loop and
do not reliably preserve order, even when issued sequentially in the shell. Send
ordered command sequences over a single connection (stdin streaming):

```sh
printf 'key /\ntext xmux\nkey enter\ndump\n' | xmux ctl
```

**3. `cargo test` does NOT rebuild the `[[bin]]` target.** After a library
change, `target/debug/<bin>.exe` is stale. Live-testing the binary then exercises
the OLD code — a feature whose unit test passes can appear "not to render live."
Always `cargo build` before driving the real binary in a verification step.

**4. ratatui is immediate-mode; there is no TreeView/focus/event-handler system**
like tview. Reimplement the UI as a state machine over a flattened row model, and
write ONE render pass that draws to both the live `CrosstermBackend` and a
headless `TestBackend`. `TestBackend` (an in-memory `Buffer`) is the analog of
tcell's `SimulationScreen`: the same render fn backs the control-channel `dump`
and all headless render tests.

**5. Async event loop shape:** a tokio `select!` over a unified `Cmd` mpsc
channel (terminal `EventStream` keys/mouse + control-socket injections + preview
results + dump requests) and a poll interval. Keep the core loop backend-generic
(`Terminal<B: Backend>` with `B::Error: Error + Send + Sync + 'static`) so it is
driveable headlessly; put real-terminal setup (raw mode, alt screen, mouse,
EventStream reader) only in the outer entry point, behind a RAII restore guard.

## Why This Matters

Each of these is a silent or misleading failure: the interprocess one produces a
cryptic error that doesn't mention "use a different name type"; the streaming and
stale-binary ones produce *wrong behavior with passing unit tests*, which is the
most expensive kind of bug to chase because the code looks correct. Knowing them
up front turns hours of confusion into a one-line decision.

## When to Apply

- Any Rust port of a Go (or other) terminal UI to ratatui/crossterm.
- Any Rust IPC channel that must work on both Windows and unix with a
  filesystem-discoverable, per-process socket.
- Any time you verify a real-terminal TUI headlessly (drive it inside a
  tmux/psmux PTY via a control socket — `env -u TMUX -u PSMUX_SESSION psmux -L
  <sock> new-session -d '<bin>'`, then inject keys + `dump` over the socket).

## Examples

Headless live-verification that confirmed the real-terminal path (render, live
preview capture, navigation, create, kill) without a human at the keyboard:

```sh
# Launch the TUI in a fresh psmux server (its own PTY), dodging the nesting guard:
env -u PSMUX_SESSION -u TMUX -u TMUX_PANE psmux -L xt new-session -d -x 120 -y 40 \
  'C:\path\to\xmux.exe'
sleep 9                                   # let the concurrent ssh scan finish
printf 'key n\ntext rusttest\nkey enter\ndump\n' | xmux ctl   # create, see it render
# ...verify the session exists on the real server, then kill it via x / y.
```

The module-by-module approach: port each Go package to a Rust module **with its
Go test contract translated first** (TDD), keeping `cargo build`/`cargo test`/
`cargo clippy` green and committing after each. The pure/logic layers (parsers,
config, quoting, tree model, ANSI→styled-text) port almost mechanically; the
divergence concentrates in the TUI and IPC layers above.

## Related

- Branch `feat/rust-rewrite`; the Go sources are preserved under `legacy-go/`.
- PROGRESS.md in the repo root holds the full module enumeration, port order, and
  live-verification log.
