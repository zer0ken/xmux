# Working Notes: /

## Purpose

This repository is a Rust terminal multiplexer switcher. The running app owns
the terminal, keeps mux display attachments alive, renders the split view (a
tree view plus the selected session's live PTY grid), and exposes a local
control socket for headless driving.

## Mental Model

Two orthogonal axes describe every connection and no module conflates them:
`Transport` (MACHINE — local vs ssh) and `Mux` (MUX — the per-mux behavior
trait). Attach and command argv are composed from a host's own `transport` and
`mux`, so the two families combine without either knowing the other.

There are two mux-facing paths:

- Metadata path: `host/` runs control-mode or poll enumeration, tracks
  inventory, and emits `HostEvent`s.
- Display path: `display/` runs real PTY attachments and feeds grids; `driver.rs`
  (`MuxDriver`, built by `Mux::driver()`) owns the per-host display decision —
  which PTY to use and whether to `switch-client` or reattach — and keeps input
  and resize work off the async runtime.

The app (`app/runtime.rs`) ties those paths together and branches on nothing
mux-specific. Domain intent converges on `model::Action` applied at
`State::apply`; raw key/text injection is an unstable low-level surface.

## Module Seams

- `src/app/` — the app: the runtime loop (`runtime.rs`, entry `run_app`) that
  owns the terminal, plus focus state (`focus.rs`).
- `src/machine/` — the MACHINE axis: the `Transport` trait, per-machine families
  (`local.rs`, `ssh.rs`), and shared shell vocabulary (`vocab.rs`). A host builds
  one via `machine::local()` / `machine::ssh()`.
- `src/mux/` — the MUX axis: the `Mux` trait, per-mux families (`tmux/`,
  `psmux/`) owning metadata + command plans + a display driver, and shared mux
  vocabulary (`vocab.rs`).
- `src/model/` — runtime domain values: `Host`, `Action`, and `Command`.
- `src/driver.rs` — the mux-agnostic `MuxDriver` trait and the thin `driver_for`
  wrapper; names no concrete mux type.
- `src/display/` — PTY attachment, the `Grid`, terminal input, and low-level
  input protocol mechanics.
- `src/host/` — host connection management (control-mode reader/writer, poll
  tasks, live client ownership).
- `src/ui/` — switcher tree transforms, interaction state, and rendering.
- `src/state/` — the explicit app runtime `State` and its `apply` /
  `apply_event` mutation sites.

## Invariants

- The public control surface should speak semantic operations before raw keys.
- Metadata/control clients do not own display pixels.
- Display attachments are real mux clients, not reconstructed `%output` streams.
- Blocking process, PTY, and pipe operations must stay off the single-threaded
  runtime path.

## Common Pitfalls

- Do not add another per-host live-process registry without reconciling it with
  `HostManager`.
- Do not put transport decisions into `Mux` methods that are documented as
  transport-blind.
- Do not document work history in code comments or durable docs; describe the
  current invariant instead.

## Before Editing

- Identify whether the change touches metadata, display, UI interaction,
  domain operations, or transport lowering.
- Follow the existing seam first; only widen a seam when the current interface
  cannot represent the behavior.
- Check `CONTEXT.md` for the vocabulary and open architecture notes before
  moving responsibilities.

## Verification

- Run the narrow unit tests for the touched module.
- For app, host, or display changes, prefer `cargo test` when feasible because
  cross-module behavior is heavily coupled.
