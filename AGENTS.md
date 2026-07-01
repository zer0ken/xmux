# Working Notes: /

## Purpose

This repository is a Rust terminal multiplexer switcher. The running cockpit owns
the terminal, keeps mux display attachments alive, renders a tree plus the
selected live PTY grid, and exposes a local control socket for headless driving.

## Mental Model

There are two mux-facing paths:

- Metadata path: `host.rs` runs control-mode or poll enumeration, tracks
  inventory, and emits `HostEvent`s.
- Display path: `proxy/` runs real PTY attachments and feeds grids; `driver.rs`
  (`MuxDriver`) owns the per-host display decision — which PTY to use and whether
  to `switch-client` or reattach — and keeps input and resize work off the async
  runtime.

The cockpit ties those paths together. Domain intent converges on `model::Action`
applied at `State::apply`; raw key/text injection is an unstable low-level surface.

## Module Seams

- `src/backend/` defines mux behavior.
- `src/driver.rs` defines the `MuxDriver` trait and owns per-host display
  orchestration (which PTY, `switch-client` vs reattach); `cockpit.rs` branches on
  nothing mux-specific for display.
- `src/model/` defines runtime domain values exchanged by backend, transport,
  host, control, and cockpit code.
- `src/proxy/` owns PTY attachment, grid, terminal input, and low-level input
  protocol concerns.
- `src/ui/` owns switcher tree transforms, interaction state, and rendering.
- `src/state/` owns explicit cockpit runtime state fields.

## Invariants

- The public control surface should speak semantic operations before raw keys.
- Metadata/control clients do not own display pixels.
- Display attachments are real mux clients, not reconstructed `%output` streams.
- Blocking process, PTY, and pipe operations must stay off the single-threaded
  runtime path.

## Common Pitfalls

- Do not add another per-host live-process registry without reconciling it with
  `HostManager`.
- Do not put transport decisions into mux backend methods that are documented as
  transport-blind.
- Do not document work history in code comments or durable docs; describe the
  current invariant instead.

## Before Editing

- Identify whether the change touches metadata, display, UI interaction,
  domain operations, or transport lowering.
- Follow the existing seam first; only widen a seam when the current interface
  cannot represent the behavior.
- Check `CONTEXT.md` for open architecture notes before moving responsibilities.

## Verification

- Run the narrow unit tests for the touched module.
- For cockpit, host, display, or proxy changes, prefer `cargo test` when
  feasible because cross-module behavior is heavily coupled.
