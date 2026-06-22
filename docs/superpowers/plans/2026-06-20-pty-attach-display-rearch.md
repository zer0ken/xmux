# PTY-attach display re-architecture — Implementation Plan

**Goal:** Replace the control-mode screen RECONSTRUCTION display with REAL per-session
attached mux clients. xmux keeps one live `tmux attach`/`psmux attach` PTY per session
across all hosts, renders the selected session's PTY `Grid` on the right, drives window
switches programmatically, and syncs mux-side structure changes into the sidebar.

**Core skeleton (user mandate):** the mux is actually USED inside xmux — a real attached
client process per session in a `portable-pty` PTY — not a `%output` reconstruction.

## Architecture (hybrid; Codex-designed, reconciled)

```
control mode (-CC, remote only)  = inventory, %window-*/%session-* events, select-window
local psmux                      = plain list-sessions/list-panes POLL (one-server-per-session)
per-session PTY attach           = pixels (Grid) + raw user input   ← the live terminal
explicit Selection {source,session,window} = canonical state (UI renders from it)
Switcher                         = tree + cursor + inline ops (kept; not rebuilt)
```

Per-host topology:
- remote tmux: 1× `tmux -CC attach` (metadata, `refresh-client -f no-output`) + N× `ssh -t … tmux attach -t S` PTYs.
- local psmux: poll `list-sessions`/`list-panes` (no host -CC) + N× `psmux attach -t S` PTYs; window switch = one-shot `psmux select-window`.

Display = ratatui renders the selected Attachment's `Grid` in BOTH states (Overlay split,
Passthrough fullscreen). NO raw-stdout LiveOwner pump (deferred — highest-risk handoff; the
real-attach mandate is met by the connection model, not the pixel transport). Confirmed by
Codex's `PtyTerminal{grid,connecting,cmd_tx}` + `PtyEvent::Output` shape.

## Reuse (recovered from git 7d37a95^)

- `src/proxy/run.rs` — recover `Attachment`, `spawn_attachment`, `PtyCmd`, `PtySink`,
  `MasterSink`, `pty_control_loop`, `DummyChild`/`fake_attachment`. STRIP `LiveOwner`,
  `pump_write`, `scan_clear`, `status_bar`, `RawGuard`, duplicate `parse_prefix` (the
  raw-pump path is dropped; pump now: read→`Grid::feed`→`PtyEvent::Output`, EOF→`Exited`).
- `src/proxy/registry.rs` — recover `AttachRegistry` (= Codex `TerminalPool`). Add `PtyEvent`,
  pre-attach-all default (high `keep_cap`), keep LRU+protect as the optional cap valve.

## Phases (each: TDD red→green, then `cargo test` + `cargo clippy --all-targets` = 0 warnings)

Toolchain: real rustup bin (shim blocked) — PATH-prepend
`C:\Users\hrlee\.rustup\toolchains\stable-x86_64-pc-windows-msvc\bin`, set RUSTC/RUSTDOC;
never pipe cargo through tail/head (masks exit); `cargo build` before exercising the binary.

### P1 — PTY attachment unit (`proxy/run.rs`)
Recover + strip. `spawn_attachment(argv, cols, rows, id, ev_tx) -> Attachment`; pump feeds Grid,
emits `PtyEvent::Output{id}` (coalesced) + `PtyEvent::Exited{id}` on EOF. `Attachment` =
`{grid, control_tx, size, connecting, child, id}` with `input/resize/teardown`.
Verify: `pty_control_loop_writes_in_order_resizes_and_exits` + connecting/Output unit tests green.

### P2 — Attachment pool (`proxy/registry.rs`)
Recover `AttachRegistry`. Methods: `ensure(addr, argv, cols, rows, protect)->id`, `reap(id)`,
`resize_all`, `teardown_all`, `get(addr)->&Attachment`, `grid(addr)`, `input(addr,bytes)`,
`contains`. Default cap = large (pre-attach-all); LRU+protect retained.
Verify: recovered LRU/protect/reap tests green.

### P3 — Selection state (`cockpit.rs` or small new type)
`Selection { source, session, window: Option<i64> }` + `address()`; canonical, owned by the
cockpit, set from `switcher.terminal_view_target()`/`current_attach_target()` after each key.
Render reads Selection (not switcher internals) for the grid key.
Verify: selection-from-target mapping unit tests (window-row vs session-row).

### P4 — host.rs metadata-only additions
Add `refresh-client -f no-output` to the remote connect sequence (non-fatal). Add a host-level
`select_window(target)` command method (generic; `select_window_on` exists — reuse/rename).
Keep inventory + event parsing untouched. (Dead display code removed in P8 simplify.)
Verify: writer emits the no-output + select-window lines; existing host tests stay green.

### P5 — cockpit integration (the rewrite)
- Add `(pty_tx, pty_rx)` + `AttachRegistry`; drain `pty_rx` in `select!` (coalesced like host_rx).
- As inventory streams in (HostEvent::Inventory, LocalEnum::Sessions): diff sessions →
  `registry.ensure(addr, src.attach_command(name, win), view_cols, view_rows, protect)` for new,
  `registry.reap`/teardown for removed (#1, #2, #5).
- Render: grid from `registry.grid(selection.address())` (replaces `mgr.get(tgt_key).grid`).
- Input (terminal focus): `TermAction::Forward(f)` → `registry.input(sel_addr, f)` (replaces send_keys).
- Window switch (#4): on window-row select, remote → control `select_window("S:N")`; local →
  one-shot `src.run(mux::select_window(...))` off-loop; the PTY follows server-side.
- Resize: `registry.resize_all(view_cols, view_rows)` alongside control resize.
- Quit: `registry.teardown_all()`.
- Local poll (#5): periodic re-`spawn_local_enumeration` (~1.5s) → diff → ensure/reap PTYs + tree.
Verify: headless `select!`-logic tests (ensure-on-inventory, reap-on-exit, input→pool,
window-switch dispatch) green; `select_attach`/control-display path removed.

### P6 — control socket parity (`ui/run.rs`)
`Cmd::Keys`/`Cmd::Dump` route through the pool (dump renders the selected Attachment grid).
Verify: dump reflects pool grid; ctl key path drives Selection.

### P7 — build + clippy + full suite green (task #7)

### P8 — whole-codebase review + simplify (task #9): remove now-dead control-mode display
(grid/feed_grid/capture_screen/CaptureScreen/switch_client/send_keys/resume_pane on the control
client), dedupe, altitude. Then Codex re-review (task #10).

## Risks (Codex + mine) → mitigations
- N ssh handshakes at startup → bounded warm concurrency (semaphore), selected jumps queue.
- Windows no ControlMaster → each PTY = independent ssh; accepted (only way to "real attach all").
- two clients/session size fight → size control + PTYs identically to the view pane.
- ConPTY teardown stall → channel-shutdown + kill + master dropped on its control thread (bounded).
- PTY flood starves current-thread runtime → coalesced Output drain with a budget (like HOST_EVENT_DRAIN_BUDGET).
- self-mirror → nest_guard at entry (xmux not in a mux ⇒ no local session runs xmux); keep guard.
- remote auth prompt in PTY → metadata stays BatchMode; mark Attachment `connecting` until first output.
