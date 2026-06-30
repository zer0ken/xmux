# Logging Redesign — Implementation Plan

Design: `docs/superpowers/specs/2026-06-30-logging-redesign-design.md`.

Three sequential tasks: (1) stand up the `tracing` infrastructure alongside the
existing `dbg_log`; (2) migrate every existing log site to leveled/structured
`tracing` events and delete `dbg_log`; (3) add the display-lifecycle
observability events that record transition EFFECTS.

## Context

xmux is a Rust TUI (ratatui + tokio) cockpit at branch `feat/logging-redesign`
(base `1588f80`). The display path lives in `src/cockpit.rs` (event loop, the
`dbg_log`/`dbg_ms`/`slow_log_line` helpers near line 234, the `XMUX_DEBUG`
gate), `src/driver.rs` (`MuxDriver` impls `TmuxDriver`/`PsmuxDriver`), and
`src/host.rs` (poll/control metadata, the per-poll enumeration log near line
598). The vt100 grid is `src/proxy/screen.rs` (`Grid`). The attachment registry
is `src/proxy/registry.rs` (`AttachRegistry`: `len`, `addresses`,
`address_of_id`, `grid`). `Env` carries `xmux_dir`.

## Global Constraints (bind every task)

- Dependencies (exact): `tracing = "0.1"`,
  `tracing-subscriber = { version = "0.3", features = ["env-filter"] }`,
  `tracing-appender = "0.2"`. Do NOT add `tracing-throttle` or any other crate.
- Log file: `<xmux_dir>/xmux.log` via `tracing_appender::rolling::daily`
  (directory = `Env::xmux_dir`, basename = `xmux.log`), wrapped in
  `tracing_appender::non_blocking`.
- Env filter variable: `XMUX_LOG`. Default directive when unset/invalid:
  `xmux=info`.
- Logging NEVER writes to stdout or stderr (ratatui owns the terminal). The fmt
  layer's writer is the file appender. `with_ansi(false)`, `with_target(true)`.
- Field style: logfmt `key=value`, snake_case keys. Use `host`, `session`,
  `addr`, `id`, `ms`, `prev`, `next` consistently.
- Level taxonomy (from the design): ERROR=session/app-fatal; WARN=recoverable
  notable; INFO=lifecycle transition (1 line/transition); DEBUG=transition
  detail / slow step ≥10ms; TRACE=per-poll/per-frame.
- Real toolchain only — the rustup shim is blocked. Build/test with
  `~/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin/{cargo,rustc,rustdoc}.exe`
  and `RUSTC`/`RUSTDOC` set. `cargo test` does NOT rebuild the binary. Each task
  ends green: tests pass, `clippy --all-targets` 0 warnings, `fmt --check` clean.
- AS-IS code and comments: describe what the code is, never what changed (no
  "now", "moved", "previously", arrows). Git records change.
- Live log OUTPUT verification (does `display_grid_changed` reveal the swap?) is
  a human gate, NOT a task acceptance criterion. Tasks are accepted on build +
  tests + clippy + fmt + structural correctness.

## Task 1: Stand up the tracing subscriber

Add the three dependencies to `Cargo.toml`. Create `src/logging.rs` (declared in
`src/main.rs`/`src/lib.rs` as appropriate) exposing:

```rust
pub fn init(xmux_dir: &std::path::Path) -> tracing_appender::non_blocking::WorkerGuard
```

`init` builds the subscriber: a daily rolling file appender at
`xmux_dir/xmux.log`, wrapped in `tracing_appender::non_blocking`; a
`tracing_subscriber::fmt` layer with that non-blocking writer,
`with_ansi(false)`, `with_target(true)`,
`with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)`; an `EnvFilter` from
`XMUX_LOG` falling back to `EnvFilter::new("xmux=info")`; `.init()` (or registry
+ `.try_init()`). Return the `WorkerGuard`.

Call `logging::init(&xmux_dir)` in `main()` BEFORE the terminal/ratatui is
initialized, and bind the returned guard to a variable that lives until `main`
returns (e.g. `let _log_guard = logging::init(...)`).

Install a panic hook (after the terminal guard exists) that restores the
terminal via the existing terminal-restore path (find how the cockpit's
`TerminalGuard`/ratatui teardown restores the screen and invoke the same), logs
the panic with `tracing::error!`, then chains to the previous hook.

Leave `dbg_log`/`dbg_ms`/`XMUX_DEBUG` untouched this task — they coexist; Task 2
removes them.

Acceptance: real-toolchain build + `clippy --all-targets` 0 + `fmt --check`
clean; a unit test that `init` returns without panicking given a temp dir and
that `xmux.log` is created in it; nothing in the logging path references stdout
or stderr. Confirm `tracing` resolves (it is a new dependency).

## Task 2: Migrate existing log sites to tracing; remove dbg_log

Replace every `dbg_log`/`dbg_ms`/`slow_log_line` call site with a `tracing`
macro at the correct level and structured fields, then delete `dbg_log`,
`dbg_ms`, `slow_log_line`, and the `XMUX_DEBUG` env gate. Mapping (level — event
name — fields):

- `state.selection -> key=… sess=…` → DEBUG `selection` — `key`, `session`.
- `attach ready key=… seq=… current=… id=…` → INFO `attach_ready` — `key`,
  `seq`, `id`.
- `attach failed key=…: <message>` → WARN `attach_failed` — `key`, `error`.
- `display_tty record id=… addr=… tty=…` → DEBUG `tty_recorded` — `id`, `addr`,
  `tty`.
- `SLOW <label> <ms>ms` (the `dbg_ms` ≥10ms steps: select_attach, grid_lock,
  render, draw, host_drain, registry.ensure) → DEBUG `slow_step` — `label`,
  `ms`. Keep the ≥10ms threshold.
- The any-other existing `dbg_log` site (e.g. the detach/reap path near
  cockpit.rs:1034) → choose the level by the taxonomy (a recoverable host/display
  drop is WARN; a routine transition is INFO) and give it a snake_case event
  name + fields. Do not invent new behavior — only translate the message.

Poll enumeration (`host.rs` ~598, currently logged unconditionally every poll):
track the last enumerated session-name set per source (a small map keyed by
source). On a SUCCESSFUL poll: if the name set CHANGED from the last, INFO
`sessions_enumerated` — `host`, `n`, `names`; if UNCHANGED, TRACE
`sessions_enumerated_unchanged` — `host`, `n`. On a poll error: WARN
`enumeration_failed` — `host`, `error`.

Delete `dbg_log`, `dbg_ms`, `slow_log_line`, and the `XMUX_DEBUG` lookups.
Update or remove the `slow_log_line` unit test (cockpit.rs ~2902) accordingly.

Acceptance: real-toolchain build + clippy 0 + fmt clean; `grep -rn "dbg_log\|dbg_ms\|slow_log_line\|XMUX_DEBUG" src/` returns nothing; existing tests pass
(adjust the one slow-log test); no stdout/stderr writes introduced.

## Task 3: Display-lifecycle observability events

Add `Grid::fingerprint(&self) -> u64` in `src/proxy/screen.rs`: a cheap stable
hash of the visible contents (e.g. hash `self.parser.screen().contents()` with
`std::hash::DefaultHasher`, or fold the rows). One lock, no allocation beyond the
contents string already available via `contents()`.

Add these events (levels/fields per the design):

- `display_show` (INFO, in BOTH `TmuxDriver::show` and `PsmuxDriver::show`, at
  the point each decides its path): `host`, `model`
  (shared|per-session), `decision` (warm|switch|reattach), `reason` (short
  string, e.g. `no-live-client`, `live+tty`, `already-on`), `session`.
- `attach_created` (INFO, where a display attach is requested via
  `request_attach` from a driver): `addr`, `id`, `count` = `registry.len()`.
- `tty_probe` (DEBUG, in the psmux `list-clients` capture task per attempt):
  `addr`, `attempt`, `result` (the parsed tty or `none`).
- `display_inventory` (DEBUG, emitted from `show` after the decision): `count` =
  `registry.len()`, `attached` = the registry addresses joined with the session
  each shows (from `host.display.shows`), `displayed` = the selected
  source/session, `mismatch` = whether the selected session differs from what
  the live attachment under the display key is bound to.
- `display_grid_changed` (INFO, in the render/draw path in `src/cockpit.rs`):
  the cockpit keeps a `HashMap<String, u64>` of the last rendered fingerprint per
  display key; when it renders the displayed grid and the fingerprint differs
  from the stored one, emit `addr`, `session`, `fp` and update the map. This is
  the EFFECT signal: a `display_show decision=switch` not followed by a
  `display_grid_changed` means the switch did not change the screen.

Acceptance: real-toolchain build + clippy 0 + fmt clean; a unit test for
`Grid::fingerprint` (same contents → same hash; fed different content → different
hash); existing tests pass. (Whether the live events reveal the psmux swap
failure is the human live gate, not this task's acceptance.)

## Task 4: AGENTS.md coherence for the logging subsystem

No `src/**/AGENTS.md` currently mentions logging at all, so this is additive
coherence, not stale-fixing: the working notes must describe the logging
subsystem the redesign introduces. Delegate the writing to Codex; verify AS-IS
compliance + scope afterward. Run this AFTER Task 3 lands so it reflects final
code (logging.rs, the events, Grid::fingerprint).

- `src/AGENTS.md`: add `logging.rs` to Module Seams — the `tracing` subscriber
  (file sink `<xmux_dir>/xmux.log`, `XMUX_LOG` env filter, non-blocking
  appender, module-path targets, levels). Add a Common Pitfall: logging never
  writes to stdout/stderr (ratatui owns the terminal — a stray write corrupts
  the alt-screen); events go to the file sink; the panic hook restores the
  terminal before printing. Note the display-lifecycle events
  (`display_show`/`display_inventory`/`display_grid_changed`) as the surface for
  diagnosing whether the displayed terminal actually swapped. Update
  Verification to mention `XMUX_LOG=xmux::driver=debug` for inspecting a switch.
- `src/proxy/AGENTS.md`: if it describes `Grid`, note `fingerprint()` (the
  content hash that `display_grid_changed` compares). Light touch, only if it
  improves coherence.
- Confirm no other `src/**/AGENTS.md` makes a now-inaccurate logging claim.

Constraints: touch ONLY `src/**/AGENTS.md` (not code, not `legacy-go/`, not
`docs/`). AS-IS only — describe what IS, no change-narration ("now", "added",
arrows) in added lines. Match the working-notes voice/structure. Verify every
symbol/path named exists in the code.

Acceptance: the named AGENTS.md files describe the logging subsystem accurately
and AS-IS; scope = `src/**/AGENTS.md` only; `git show --stat` confirms no code /
legacy-go / docs files touched.
