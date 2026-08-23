# Working Notes: /src/mux/tmux

## Purpose

`mux/tmux` is the tmux family: everything mux-specific to tmux lives here so no
tmux code sits at the `src` root. It owns BOTH sides of the mux:

- the metadata mux: binary name, a shared server model, aggregate session-listing
  enumeration, attach argv, the control-mode argv, event source, death signal,
  and the window and session operation plans;
- the display driver: the per-source display orchestration for a shared-server mux.

The mux constructs its own driver, so tmux selection lives in this family and
never in a central match on server model. The control-mode wire protocol lives
beside them, behind the shared control-protocol trait.

## Mental Model

tmux is a SHARED-server mux: one aggregate server holds every session. The display
driver keeps ONE PTY per source, warmed on the first session and MOVED to another
session with `switch-client`, an in-place move with no teardown. A remote shared
attach records its OWN controlling tty to a per-source file before exec, so a later
switch targets xmux's own display client and never the user's own attached client.
A LOCAL shared source has no remote shell to record or read the tty, so it
reattaches instead.

The mux supplies mux vocabulary (argv, model, enumeration, control payload); the
driver consumes it and owns the concrete attach-or-switch decision. The transport
lowers the host execution, and the tmux family never hardcodes ssh.

## Module Seams

- The family root holds the mux itself, the per-source display-tty file helpers
  (the path, the family-private record prefix, and the in-place switch plan that
  reads the recorded tty), the control argv, and the control-protocol
  implementation.
- The driver sits beside it with the tmux-only attach helper that wraps the
  tty record.
- The control-mode wire module holds the pure, headlessly-testable line
  classification, the notification-to-event table, and the command-line builders.
- The driver pulls the mux-agnostic display seam from `src/driver.rs` and the
  supervisor capabilities from the app runtime. The seam does NOT import the
  driver; the dependency is one-way, so there is no cycle.

## Invariants

- A shared source keeps ONE PTY, keyed by source id; a session change MOVES it rather
  than tearing it down.
- A remote in-place switch reads the tty the attach recorded to its per-source file,
  so it moves xmux's own display client and never the user's. It never runs with
  an empty client tty.
- Sync warms the source PTY on the first session and reaps it when the source has no
  sessions. The driver value itself carries no state, so constructing a fresh one
  per call is fine: the state lives on the source and the attachment registry.
- A reachable empty tmux enumerates as an empty list; unreachable is an error.

## Common Pitfalls

- Do not name the concrete driver outside the mux tree; the supervisor resolves it
  through the mux, never through a match on server model.
- Do not fold the display-tty record prefix into a LOCAL attach. There is no shell
  to run it, and it would corrupt the argv's session-name argument.
- Do not thread a remoteness boolean through the mux. The driver reads the
  transport's capability predicate for whether an attach runs through a host
  shell, which is what gates the tty record, and the mux stays transport-blind.

## Before Editing

- Decide whether the behavior is tmux mux vocabulary, display orchestration, or
  the control-mode wire protocol.
- Keep the driver's behavior identical unless the change is explicitly a behavior
  change; the display decision is the highest-risk surface.
- Check psmux for parity when changing the shared driver or mux trait shape.

## Verification

- Exercise the plan argv, the control-mode line classification, and the driver
  decision for the seam you touched, and re-check the app and connection surfaces when
  the event source, death signal, or display decision changes.
- Set `XMUX_LOG=xmux::mux::tmux=debug` to trace the driver's show, inventory, and
  attach decisions.
