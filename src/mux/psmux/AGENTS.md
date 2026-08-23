# Working Notes: /src/mux/psmux

## Purpose

`mux/psmux` is the psmux family: everything mux-specific to psmux lives here so
no psmux code sits at the `src` root. It owns BOTH sides of the mux:

- the metadata mux: binary name, a per-session server model, registry-merge
  enumeration for a LOCAL host (a plain session listing over ssh for a REMOTE
  one), attach argv, poll cadence, death signal, and the window and session
  operation plans;
- the display driver: the per-host display orchestration for a per-session mux.

The mux constructs its own driver, so psmux selection lives in this family and
never in a central match on server model. psmux has no control stream; it is
polled.

## Mental Model

psmux is a PER-SESSION mux: one server per session on its own port, recorded in a
per-machine registry under the user's home directory and coordinated over the
default socket. The display driver holds ONE per-host PTY and, on a session
change, either:

- SWITCHES it in place (`switch-client -c <tty> -t <session>`) when a live client
  AND its captured tty are known, with no teardown, so the terminal view never
  goes blank, followed by a `refresh-client` to force a full repaint; or
- REATTACHES with `new-session -A -s <name>`, which routes to that session's OWN
  server, when there is no live client or no captured tty. A bare `attach -t`
  lands on a warm clone instead, which is why it is not used.

The tty is captured off-loop by a read-only `list-clients` probe, correlating the
client by the session it shows: with one server per session, the client showing a
session is on that session's own server. A remote psmux host is enumerated and
displayed the generic way, and the local probe is skipped there.

The mux supplies mux vocabulary (argv, model, enumeration); the driver consumes it
and owns the concrete switch-or-reattach decision. The transport lowers the
machine execution.

## Module Seams

- The family root holds the mux itself, the poll cadence, and the in-place switch
  plan (the switch followed by the refresh) that the driver runs.
- The driver sits beside it with the psmux-only client-tty parsing and the
  off-loop tty capture.
- The session registry backs local enumeration: an existence set merged with one
  detail row from a session listing.
- The driver pulls the mux-agnostic display seam from `src/driver.rs` and the
  supervisor capabilities from the app runtime. The seam does NOT import the
  driver; the dependency is one-way, so there is no cycle.

## Invariants

- A per-session attach uses `new-session -A -s <name>`, which routes to that
  session's own server, never a bare `attach -t` on the default socket, which
  yields a warm clone with the wrong content.
- An in-place switch runs ONLY with a live client AND a non-empty captured tty;
  otherwise it reattaches, so a box where the tty is never captured still lands
  on the right session. An in-place switch with a guessed tty would move somebody
  else's client.
- On a reattach the stale attachment is HELD, not removed, so its grid stays on
  screen until the fresh one is ready (stale-while-revalidate).
- Sync never pre-warms, since attaches are selected on demand when a session is
  shown; it only reaps the host PTY when the host has no sessions left.
- A LOCAL psmux host reads the per-machine registry; a REMOTE one enumerates over
  ssh and never touches the local registry.

## Common Pitfalls

- Do not name the concrete driver outside the mux tree; the supervisor resolves it
  through the mux, never through a match on server model.
- Do not run a switch with an empty client tty. The capture is guarded, and an
  empty or absent tty must fall back to reattach.
- Do not fold the local registry into a REMOTE host: it would inject local session
  names as phantoms and swallow an ssh failure into a fake empty list.

## Before Editing

- Decide whether the behavior is psmux mux vocabulary, display orchestration, or
  registry enumeration.
- Keep the driver's behavior identical unless the change is explicitly a behavior
  change; the switch-or-reattach decision is the highest-risk surface.
- Check tmux for parity when changing the shared driver or mux trait shape.

## Verification

- Exercise the plan argv, the registry merge, and the driver decision for the
  seam you touched, and re-check the app and host surfaces when the event source,
  death signal, or display decision changes.
- Set `XMUX_LOG=xmux::mux::psmux=debug` to trace the driver's show, tty probe, and
  inventory decisions.
