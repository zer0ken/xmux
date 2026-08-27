# Working Notes: /src/mux/abduco

## Purpose

`mux/abduco` is the abduco family: everything mux-specific to abduco lives here so
no abduco code sits at the `src` root. It owns BOTH sides of the mux:

- the metadata mux: binary name, a per-session server model, listing
  enumeration (the bare binary IS the listing), attach argv, create argv, poll
  cadence, death signal, and the one-card-per-session rule;
- the display driver: the per-source display orchestration for a per-session mux.

The mux constructs its own driver, so abduco selection lives in this family and
never in a central match on server model. abduco has no control stream; it is polled.

## Mental Model

abduco is a PER-SESSION mux: each session is its own server process owning its own
unix socket under `~/.abduco`. It is the simplest mux xmux drives — it has no
windows (a session is one PTY running one command), no control-mode channel, and no
server-socket flag. The display driver holds ONE per-source PTY and REATTACHES it
with `abduco -a <name>` on every session change, which attaches to that session's
own server.

Because there is no per-session query, a poll sweep enumerates ONCE and resolves
every session as a plain session card (the session alone). `last_attached` is 0:
abduco prints human local
wall-clock time, which cannot be converted to the shared epoch scale across hosts
without the host's timezone, so the mux "does not report" it.

The mux supplies mux vocabulary (argv, model, enumeration); the driver consumes it
and owns the concrete display decision. The transport lowers the host execution.

## Module Seams

- The family root holds the mux itself, the poll cadence, and the listing parser.
- The driver sits beside it and owns the per-source display orchestration.
- The driver pulls the mux-agnostic display seam from `src/driver.rs` and the
  supervisor capabilities from the app runtime. The seam does NOT import the
  driver; the dependency is one-way, so there is no cycle.
- Identity detection (`-v` naming itself, `-V` being rejected) lives at the mux
  root beside the other families' probes; this family only answers the `Mux`
  surface the probe constructs.

## Invariants

- A per-session attach uses `abduco -a <name>`, which reaches that session's own
  server.
- A session change ALWAYS reattaches; on a reattach the stale attachment is HELD,
  not removed, so its grid stays on screen until the fresh one is ready
  (stale-while-revalidate).
- Sync never pre-warms; it only reaps the source PTY when the source has no
  sessions left.
- A session resolves as the session alone, never with a per-session command
  that cannot exist.
- `last_attached` is always 0: the mux reports no comparable value.

## Common Pitfalls

- Do not invent a per-session query: abduco has none, and a bogus command
  would run every poll and fail.
- Do not use `-V` (uppercase) anywhere: abduco rejects it; its version flag is
  `-v` (lowercase).
- Do not rely on dvtm: abduco's default session command is the user's tool inside
  the session and is out of xmux's scope; xmux only creates the session.
- Do not name the concrete driver outside the mux tree; the supervisor resolves it
  through the mux, never through a match on server model.

## Before Editing

- Decide whether the behavior is abduco mux vocabulary, display orchestration, or
  listing enumeration.
- Check tmux, psmux, AND zellij behavior when changing trait methods; abduco is
  the simplest family and inherits nothing from tmux's argv.

## Verification

- Pin the plan argv and the shape it parses back together; they are one decision.
- Re-check the connection and app surfaces when the event source, death signal, or
  display decision changes.
- Set `XMUX_LOG=xmux::mux::abduco=debug` to trace the driver's show and inventory
  decisions.
