# Working Notes: /src/mux

## Purpose

`mux` is the MUX axis: the `Mux` trait, the mux registry every factory and predicate
reads, identity detection, the control-protocol trait that hides a mux's control-mode
wire details from the connection layer, and the shared argv builders and row parsers.
Each mux owns its metadata mux AND its `MuxDriver`, and constructs that driver itself.
tmux keeps one aggregate server, a source-level control stream, and one PTY per source
moved in place; every other mux reattaches on each session change, because none can
name a client from outside its own session. The command-plan verbs default to
tmux-compatible argv; a mux that shares no argv with tmux overrides every verb and its
listing parse with it, since a plan and the shape of what it prints are one decision.

## Module Seams

- One sub-directory per mux, re-exported from the root, so one path names a shared
  builder and a mux factory alike.
- The generic command builders are called ONLY inside the per-mux directories and the
  shared enumeration helper in the root, each plan wrapping one. The pure address
  helpers are callable anywhere.
- Plan methods return mux argv or mux intent and never decide local versus ssh
  execution. The plan set covers what xmux itself issues: list sessions, attach,
  switch a client in place, the control-mode argv, and start a session. There is no
  kill, rename, or window plan; the mux owns those.
- Drivers reach display resources only through the driver seam's capability port, and
  no code outside the mux tree names a concrete driver type.

## Invariants

- A reachable empty mux enumerates as an empty list; an unreachable source is an
  error, and so is a listing that exceeds the fixed per-command budget.
- A per-session reattach HOLDS the stale attachment, so its grid stays on screen until
  the fresh attachment paints or reaches its bounded wait (stale-while-revalidate).
  Input goes to the fresh attachment while it waits.
- A per-session driver never pre-warms; sync only reaps the source PTY when the source
  has no sessions left.

## Common Pitfalls

- Do not put transport decisions into mux methods that are documented as
  transport-blind, and do not thread remoteness booleans through them.
- Do not duplicate psmux registry behavior outside the mux and source boundary
  without deciding which module owns it.

## Before Editing

- Check tmux, psmux, AND zellij when changing trait methods, and tie any addition to
  an end-to-end caller. A flag question or verb with a tmux-compatible default is
  silently wrong for zellij, which refuses tmux's flags outright.
- The display decision is the highest-risk surface: which client a command reaches
  decides whether xmux moves a terminal the user owns.

## Verification

- Pin the argv a plan emits and the shape it parses back together.
- Re-check the connection and app surfaces when the event source, death signal,
  selection outcome, or display decision changes.
- Set `XMUX_LOG=xmux::mux::<kind>=debug` to trace a driver's decisions.
