# Working Notes: /

## Purpose

This repository is a Rust terminal multiplexer switcher. The running app owns the
terminal, keeps mux display attachments alive, renders the split view (a nav view
of session cards plus the selected session's live PTY grid), and exposes a local
control socket for headless driving. Each instance is addressed by NAME, owning
`ctl-<name>.sock`, which is what `xmux send <name> <command>` dials.

## Mental Model

Two orthogonal axes describe every connection and no module conflates them:
`Transport` (MACHINE, local versus ssh) and `Mux` (MUX, the per-mux behavior
trait). Attach and command argv are composed from a host's own transport and mux,
so the two families combine without either knowing the other.

There are two mux-facing paths:

- Metadata path: `src/host/` runs control-mode or poll enumeration, tracks
  inventory, and emits host events.
- Display path: `src/display/` runs real PTY attachments and feeds grids; the
  driver seam owns the per-host display decision (which PTY to use and whether to
  switch in place or reattach) and keeps input and resize work off the async
  runtime.

The app ties those paths together and branches on nothing mux-specific. Domain
intent converges on a single action vocabulary applied at one site in the runtime
state; raw key and text injection is an unstable low-level surface.

## Module Seams

- `src/app/` - the app: the runtime loop that owns the terminal, plus the focus
  and modal routing state.
- `src/machine/` - the MACHINE axis: the `Transport` trait, the per-machine
  families, and the shared shell vocabulary. A host builds one at construction.
- `src/mux/` - the MUX axis: the `Mux` trait, the per-mux families (`tmux/`,
  `psmux/`, `zellij/`) owning metadata, command plans, and a display driver, and
  the shared mux vocabulary.
- `src/model/` - runtime domain values: hosts, the action vocabulary, and the
  command vocabulary.
- `src/driver.rs` - the mux-agnostic `MuxDriver` trait and the thin wrapper that
  resolves a host's driver; it names no concrete mux type.
- `src/display/` - PTY attachment, the grid, terminal input, and low-level input
  protocol mechanics.
- `src/host/` - host connection management (control-mode reader and writer, poll
  tasks, live client ownership).
- `src/ui/` - nav row transforms, interaction state, and rendering.
- `src/state/` - the explicit app runtime state and its two mutation sites.

## Invariants

- The public control surface should speak semantic operations before raw keys.
- Metadata and control clients do not own display pixels.
- Display attachments are real mux clients, not reconstructed output streams.
- Blocking process, PTY, and pipe operations must stay off the single-threaded
  runtime path.

## Common Pitfalls

- Do not add another per-host live-process registry without reconciling it with
  the host manager.
- Do not put transport decisions into mux methods that are documented as
  transport-blind.
- Do not document work history in code comments or durable docs; describe the
  current invariant instead.
- Do not describe code in a durable doc. A document states behavior and design
  rules, and names no test, function, method, field, or library API, so a rename
  in the source is never a documentation change.

## Before Editing

- Identify whether the change touches metadata, display, UI interaction, domain
  operations, or transport lowering.
- Follow the existing seam first; only widen a seam when the current interface
  cannot represent the behavior.
- Check `CONTEXT.md` for the vocabulary and open architecture notes before moving
  responsibilities.

## Verification

- Exercise the behavior the touched module is responsible for.
- For app, host, or display changes, run the whole suite when feasible, because
  cross-module behavior is heavily coupled.
