# Working Notes: /src/mux/psmux

## Purpose

`mux/psmux` is the psmux family: everything mux-specific to psmux lives here so
no psmux code sits at the `src` root. It owns BOTH sides of the mux:

- the metadata mux: binary name, a per-session server model, registry-merge
  enumeration for a LOCAL host (a plain session listing over ssh for a REMOTE
  one), attach argv, poll cadence, death signal, the environment variable its own
  client carries its session in, and the window and session operation plans;
- the display driver: the per-source display orchestration for a per-session mux.

The mux constructs its own driver, so psmux selection lives in this family and
never in a central match on server model. psmux has no control stream; it is
polled.

## Mental Model

psmux is a PER-SESSION mux: one server per session on its own port, recorded in a
per-host registry under the user's home directory and coordinated over the
default socket. The display driver holds ONE per-source PTY and REATTACHES it
with `new-session -A -s <name>` on every session change, which routes to that
session's OWN server. A bare `attach -t` lands on a warm clone instead, which is
why it is not used.

There is no in-place switch, because psmux can name no client from outside its
own session. A command that carries no session name reaches whichever server was
most recently active on the machine, and `switch-client` honors no client
selector: a `-c <tty>` naming a real client exits 0 and moves nothing, and one
naming a client that command cannot resolve exits 0 and moves the client it
reached instead, which is a separate psmux terminal of the user's. A reattach is
addressed by session NAME, so it can only ever land on xmux's own PTY.

psmux moves its own client without any server seeing it: the client detaches from
one session's server and the same process reconnects to another, keeping its pid
and the argv it started with. What it rewrites each time it lands is
`PSMUX_SESSION_NAME` in its own environment, which it also sets on the first
attach, so that value always names the session the client is on right now. It
belongs to the process, so it names xmux's own client and can never report a
separate psmux terminal of the user's. Reading it needs the client to be a process
on THIS machine, so a remote psmux source has no such witness and behaves as
though there were none to read.

A remote psmux source is enumerated and displayed the generic way.

The mux supplies mux vocabulary (argv, model, enumeration); the driver consumes it
and owns the concrete display decision. The transport lowers the host execution.

## Module Seams

- The family root holds the mux itself and the poll cadence.
- The driver sits beside it and owns the per-source display orchestration.
- The session registry backs local enumeration: an existence set merged with one
  detail row from a session listing.
- The driver pulls the mux-agnostic display seam from `src/driver.rs` and the
  supervisor capabilities from the app runtime. The seam does NOT import the
  driver; the dependency is one-way, so there is no cycle.

## Invariants

- A per-session attach uses `new-session -A -s <name>`, which routes to that
  session's own server, never a bare `attach -t` on the default socket, which
  yields a warm clone with the wrong content.
- A session change ALWAYS reattaches, at any client tty on record and whatever
  the display bookkeeping says. psmux honors no client selector, so a switch
  cannot be aimed at xmux's own client and would move somebody else's. The ONE
  thing that suspends the reattach is the live client's own report that it is
  already on the selected session, which is a fact read from the running process
  and never an inference from bookkeeping: bookkeeping is exactly what goes stale
  when psmux moves the client itself. With no such report, for any reason at all,
  the reattach happens.
- On a reattach the stale attachment is HELD, not removed, so its grid stays on
  screen until the fresh one is ready (stale-while-revalidate).
- Sync never pre-warms, since attaches are selected on demand when a session is
  shown; it only reaps the source PTY when the source has no sessions left.
- A LOCAL psmux source reads the per-host registry; a REMOTE one enumerates over
  ssh and never touches the local registry.

## Common Pitfalls

- Do not name the concrete driver outside the mux tree; the supervisor resolves it
  through the mux, never through a match on server model.
- Do not reach for a client-addressed command (a switch, a refresh, a detach of
  one client). psmux accepts the client selector, ignores it, and acts on the
  client its own default route reached, with a success exit either way.
- Do not let the reattach guard rest on anything but the live client's own report.
  A guard that trusts the display bookkeeping holds when the client is not there,
  which leaves nothing to confirm and nothing to respawn, and the terminal view
  stays blank until the user navigates away and back.
- Do not fold the local registry into a REMOTE source: it would inject local session
  names as phantoms and swallow an ssh failure into a fake empty list.

## Before Editing

- Decide whether the behavior is psmux mux vocabulary, display orchestration, or
  registry enumeration.
- Keep the driver's behavior identical unless the change is explicitly a behavior
  change; which client a command reaches is the highest-risk surface, since the
  wrong answer moves a terminal the user owns.
- Check tmux for parity when changing the shared driver or mux trait shape.

## Verification

- Exercise the plan argv, the registry merge, and the driver decision for the
  seam you touched, and re-check the app and connection surfaces when the event source,
  death signal, or display decision changes.
- Set `XMUX_LOG=xmux::mux::psmux=debug` to trace the driver's show and inventory
  decisions.
