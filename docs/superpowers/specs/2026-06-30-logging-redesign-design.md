# Logging Redesign — Design

## Goal

xmux's logging is a single `dbg_log(dir, freeform_string)` appended to
`~/.xmux/debug.log`, gated by an `XMUX_DEBUG` on/off flag. It has no levels, no
categories, no structured fields, no runtime filtering, and a per-poll line that
floods the file. Worse, it logs DECISIONS ("decided to switch") but never
EFFECTS ("the displayed grid actually changed"), so the central question of the
display subsystem — *is the terminal showing what we selected?* — cannot be
answered from the log.

Redesign all logging onto the Rust/tokio convention (`tracing`), with levels,
structured fields, per-category runtime filtering, source-level noise control,
and — the payoff — events that record the EFFECT of each display transition.

## Library

`tracing` + `tracing-subscriber` (`env-filter` feature) + `tracing-appender`.
All three are Tokio-org first-party crates; `tracing-appender` provides the
non-blocking off-loop file writer and daily rolling that a custom logger would
otherwise have to reimplement. `tracing` is not currently a dependency (tokio's
`tracing` feature is off), so these are three genuinely new dependencies.

`tracing-throttle` (signature-based dedup) is intentionally OUT OF SCOPE: level
gating plus source-level change-only logging removes the only known flood
(per-poll enumeration). Add it later only if a real WARN-storm appears.

## Sink and initialization

- Write to `<xmux_dir>/xmux.log` via `tracing_appender::rolling::daily`, wrapped
  in `tracing_appender::non_blocking` so file I/O never blocks the event loop.
- The `non_blocking` `WorkerGuard` is held for the whole process lifetime (in
  `main`); dropping it early loses buffered lines.
- Filter from the `XMUX_LOG` env var (matches the existing `XMUX_*` namespace),
  default directive `xmux=info`. Never silent, never floods.
- `fmt` layer: `with_ansi(false)` (no escapes in a file), `with_target(true)`
  (the module path is the category), `with_span_events(FmtSpan::NEW | CLOSE)`.
- **TUI hard rule:** logging never writes to stdout/stderr. ratatui owns the
  terminal; any stray write corrupts the alt-screen. The writer is the file
  appender, set before ratatui initializes.
- Panic hook: restore the terminal (leave the alt-screen via the existing
  terminal-guard restore path) BEFORE the default panic output runs, and log the
  panic, so a panic neither corrupts the alt-screen nor vanishes from the log.

## Levels

| Level | Rule |
|---|---|
| ERROR | Session/app-fatal: PTY spawn failure, socket accept error. |
| WARN  | Recoverable but notable: attach retried, host unreachable, attach failed, switch produced no visible change. |
| INFO  | Attach/display lifecycle transitions — one line per transition (selection committed, attach created, display shown, session enumeration changed, PTY reaped). |
| DEBUG | Detail explaining a transition: command sent, reply received, tty probe result, slow synchronous step (≥10ms). |
| TRACE | Per-poll / per-frame activity (unchanged poll enumeration, per-frame render). Default OFF. |

Rule of thumb: fires more than once per user action → DEBUG or TRACE; fires
every poll/frame → TRACE only. Categories are module targets
(`xmux::cockpit`, `xmux::driver`, `xmux::host`), filtered per-target via
`XMUX_LOG` (e.g. `XMUX_LOG=xmux::driver=debug,xmux::host=warn`).

## Format

`tracing-subscriber`'s default text format (logfmt-style: timestamp, LEVEL,
target, message, then `key=value` fields). snake_case keys. Convention:
`host=`, `session=`, `addr=`, `id=`, `ms=`, and `prev=`/`next=` on transitions.

## Noise control

The per-poll enumeration is the only known flood. At its producer, compare the
enumerated session-name set to the last set for that source: log
`sessions_enumerated` at INFO only when the set CHANGED; an unchanged poll logs
at TRACE (off by default). A poll error logs at WARN.

## Display-lifecycle observability (the payoff)

The reason the current log cannot answer "did the displayed terminal actually
swap": it records the decision, not the effect. Add a cheap content fingerprint
and the events that surround a display transition.

- `Grid::fingerprint(&self) -> u64` — a cheap hash of the visible cell contents
  (one lock). Changes iff the rendered content changes. The mechanism that
  distinguishes "switch issued" from "screen actually changed".
- `display_show` (INFO, in each driver's `show`): `host`, `model`,
  `decision` (switch|reattach|warm), `reason`, `session`.
- `attach_created` (INFO, where a display attach is requested): `addr`, `id`,
  `count` (live attachment total).
- `tty_probe` (DEBUG, psmux tty capture): `addr`, `attempt`, `result`.
- `display_inventory` (DEBUG): `count`, `attached` (addr→session list),
  `displayed`, `mismatch` (the displayed selection differs from what the live
  attachment is bound to).
- `display_grid_changed` (INFO, render path): when the displayed grid's
  fingerprint differs from the last rendered fingerprint for its key — `addr`,
  `session`, `fp`. A `display_show decision=switch` that is NOT followed by a
  `display_grid_changed` proves the switch did not change the screen.

These give a one-glance answer to: how many PTYs exist, what is displayed, and
whether a transition actually changed the screen.

## Out of scope

`tracing-throttle`; JSON output; rotation beyond daily; spans on every function
(only the attach/display lifecycle is span-wrapped).
