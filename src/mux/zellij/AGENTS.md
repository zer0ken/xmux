# Working Notes: /src/mux/zellij

## Purpose

`mux/zellij` is the zellij implementation: everything mux-specific to zellij lives here so
no zellij code sits at the `src` root. It owns BOTH sides of the mux:

- the metadata mux: binary name, a per-session server model, session-listing
  enumeration, attach argv, poll cadence, death signal, the environment variable
  its own client carries its session in, and the command plans, none of which are
  tmux-compatible;
- the display driver: the per-source display orchestration for a per-session mux
  with no in-place switch;
- the output shapes: the session listing (a human line), as pure functions.

The mux constructs its own driver, so zellij selection lives in this implementation and
never in a central match on server model. zellij has no control stream; it is
polled.

## Mental Model

zellij is a PER-SESSION mux, and it is the one mux implementation that shares NO argv with
tmux: every command plan is overridden rather than inherited, and its listing is
parsed here rather than by the shared builders.

Its CLI is one process per query, and every query is addressed with
`--session <name>`, because zellij's actions otherwise target the session the
caller is inside, which xmux never is. Vocabulary maps as: a zellij TAB is what
xmux calls a window, and its position is the window index.

The display driver holds ONE per-source PTY and REATTACHES it on every session
change. There is no in-place switch to make: `switch-session` moves whichever
client runs it, and a client cannot be named from outside its own session, so
xmux has no way to aim a switch at its own display client the way it does for
tmux and psmux.

The same `switch-session` is how the USER moves xmux's own display client, and it
moves it INSIDE the client process: the client detaches from one session's server
and the same process attaches to another, keeping its pid and the argv it started
with. No server sees that move, so the poll cannot ask for it, and the session
listing cannot answer it either, since its current-session marker names the session
the listing itself ran inside and xmux polls from outside every session. What the
client rewrites each time it lands is `ZELLIJ_SESSION_NAME` in its own environment,
which it also sets on the first attach, so that value always names the session the
client is on right now. It belongs to the process, so it names xmux's own client
and never another zellij client of the user's. Reading it needs the client to be a
process on THIS machine, so a remote or WSL zellij source has no such source of truth and
behaves as though there were none to read.

## Module Seams

- The implementation root holds the mux itself, the poll cadence, the
  `--session <name> action <verb>` argv builder, and the clock the reported
  session age is subtracted from.
- The parsing module holds the session-line grammar and the human-readable duration
  zellij prints an age in. Pure and total: anything that does not
  fit is skipped.
- The driver sits beside them.
- The driver pulls the mux-agnostic display seam from `src/driver.rs` and the
  supervisor capabilities from the app runtime. The seam does NOT import the
  driver; the dependency is one-way, so there is no cycle.

## Invariants

- Every action argv carries `--session <name>`. An action without it targets the
  caller's own session, and xmux is outside every session.
- `go-to-tab` counts tabs from ONE while xmux and zellij's own position count from
  zero. The shift happens in the window-selection plan and nowhere else.
- The attach is plain `attach <name>`, never `attach -c`: showing a session must
  not create or resurrect one. A session that died between the scan and the attach
  fails the attach, which is the end-of-stream the death signal is waiting for.
- A session listed as exited is a resurrectable record, not a session, and is
  dropped during enumeration.
- The session's last-attached value carries its CREATION instant. zellij reports
  no attach time, and the shared session model carries a value on the same epoch scale
  tmux reports.
- A session change reaches another session by a fresh attach, and the display
  belief is what suspends it: an attachment already recorded as showing the
  selected session is left alone. Following a client switch records the client's
  own report as that belief BEFORE moving the nav, so the selection arriving at
  the display decision finds a belief the client backs, and the client the user
  just moved is never torn down to reach the session it is already in.
- On a reattach the stale attachment is HELD, not removed, so its grid stays on
  screen until the fresh one is ready (stale-while-revalidate).
- Sync never pre-warms, since attaches are selected on demand when a session is
  shown; it only reaps the source PTY when the source has no sessions left.

## Common Pitfalls

- Do not name the concrete driver outside the mux tree; the supervisor resolves it
  through the mux, never through a match on server model.
- Do not inherit a tmux-compatible command plan by leaving a verb unoverridden.
  zellij refuses tmux's flags outright (a session listing with tmux's format flag
  is an unexpected-argument error), so a missed override surfaces as an
  unreachable source, not a degraded one.
- Do not parse the session listing by splitting on whitespace. zellij forbids only
  `/` in a session name, so a name may hold spaces; the split is on the
  ` [Created ` marker.
- Do not parse a window or tab list at all: a session's cards name the session
  alone, so no `list-tabs` / `list-panes` query exists here, and the display
  mirrors whatever tab the attached client lands on.
- Do not treat a non-zero session-listing exit as an unreachable source. An idle
  zellij writes `No active zellij sessions found.` to stderr and exits 1.
- Do not read the listing's current-session marker as where xmux's display client
  is. It marks the session the LISTING COMMAND ITSELF ran inside, so xmux, which
  polls from outside every session, never sees it on any line.
- Do not assume an action always answers. On WINDOWS an action addressed at a
  stale session never returns, which would freeze that source's whole poll loop; the
  mux layer's per-command poll budget is what bounds it. Verify zellij behavior on
  Linux, where the same queries answer immediately.

## Before Editing

- Decide whether the behavior is zellij argv, an output shape, or
  display orchestration.
- Verify a new argv against a live zellij before shipping it. zellij's flags move
  between versions, and several of its subcommands accept a flag that changes only
  the TABLE columns while the JSON flag always dumps every field.
- Check tmux and psmux for parity when changing the shared driver or mux trait
  shape.

## Verification

- Exercise the plan argv, both parsers, and the driver decision for the seam you
  touched, and re-check the app and connection surfaces when the event source, death
  signal, or display decision changes.
- Set `XMUX_LOG=xmux::mux::zellij=debug` to trace the driver's show and inventory
  decisions.
- A live check of the client-switch follow needs a real LOCAL zellij on Windows:
  attach xmux's terminal view to one session, run
  `zellij action switch-session <other>` inside it, and confirm the nav selection
  lands on the other session's card while the same client stays on screen.
- A live check needs a real zellij host: create a detached session with
  `zellij attach -b <name>` and let xmux attach to it. Do NOT create tabs from
  outside a clientless session first: zellij 0.45.0 then panics its server on the
  next client attach, which looks like an xmux attach failure and is not one.
