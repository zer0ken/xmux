# Working Notes: /src/app

## Purpose

`app` is the application orchestration layer: the app runtime that owns the
terminal for the whole session, plus the application UI-state machine it folds
through. The app is the coordinator; focus is the focus/modal routing state it
reads and mutates.

## Mental Model

The runtime is a persistent supervisor. It keeps ONE real attached mux client
per session alive in a PTY across selections and renders the SELECTED session's
live grid on the right. A separate control-mode client per remote source supplies
the nav view inventory, mux-side change events, and display-driver
selection; a local mux is enumerated or polled with plain commands. One async
loop interleaves stdin, source events, PTY events, the control socket, terminal
resize, and an animation tick. It folds domain actions and inbound source events
through the runtime state, dispatches the returned commands and effects, keeps
the state in sync with the switcher selection, drives the debounced attach, and
draws the split view.

Focus tracks which view holds focus (nav or terminal) and which modal is open,
and exposes the transitions the app and the state fold through. It is UI state,
not display mechanics: it decides where input is routed, not how a PTY is pumped
or a grid is rendered.

The input path keeps ONE prefix-interaction signal that both the hint bar and the
auto-hide nav width read: ready, meaning a prefix interaction is live. A prefix key
sets it; it clears when the FUNCTION the prefix started ends, or on a focus switch /
mouse action (a cancel). Most functions end with their command key, so ready usually
clears there; an input row's function ends when the row closes, and a resize's ends
when its repeat window lapses, so ready spans those. The window lapses on the clock
rather than on an event, so the loop top compares ready against the stored value and
marks the frame dirty when it goes idle on its own. Under auto-hide the nav comes
back for a live prefix interaction and hides again when it ends, so a jump can read
the card numbers it needs.

## Module Seams

- `runtime/` owns the main event loop as one struct: the entry point builds it,
  keeps the loop's receivers, timers, and terminal as loop-locals, and drives a
  select where each arm is one method on that struct. It resolves the source's own
  driver for display and reads the grid back from it; it branches on nothing
  mux-specific. The canonical selection it reads lives in `src/model`.
- `runtime/` owns holding the nav selection and xmux's own display client to ONE
  session. It learns where the client is in whichever way the mux offers - pushed
  over a control channel, or read off the live client for a mux that pushes
  nothing - and records it; the read runs on the animation beat and is mux-blind
  in both directions, since each mux answers whether its client can be read at
  all. What follows from the two naming different sessions is one comparison made
  on every loop pass, the same for every mux.
- `runtime/` also owns MUX DISCOVERY's async half: one fire-and-forget probe per
  remote host that named no mux, spawned right after the startup scans, whose
  answers become new sources through an effect. It is the loop's job because only
  the loop holds the source registry (what a host already serves, and where a
  new source goes) and the manager that kicks the new source's first scan.
- Input routing has a pure, stateless core (key resolution, mouse chains, the
  predicates, the input outcome types); the stateful handlers are runtime methods
  that call into it. The prefix is tracked as ready (an interaction is live): the end
  of the function it started, or a focus switch / mouse action (a cancel), clears it.
- Focus holds the focus and modal state plus the transition helpers. The runtime
  state embeds it; the app reads and mutates it through those helpers.
- The display mechanics (PTY, grid, input) live in `src/display`; per-source connection
  management lives in `src/link`; the domain types live in `src/model`; the
  durable runtime state bag lives in `src/state`.

## Invariants

- The entry point is thin: the runtime struct owns the loop's world state, and
  every select arm and stateful helper is a method on it, so each takes a small
  argument list rather than a large loose-parameter bundle.
- The app loop is not a second writer of the runtime state. The display truth,
  the attach debounce, and the focus all change only inside the state's apply,
  routed there as actions. The loop makes the decision (a live grid exists, a
  deadline elapsed, a click landed) and folds the result through apply, so domain
  mutation stays at one site.
- Only the attach the SELECTION is displayed through confirms the display truth. A
  landed attach for any other key installs and stays warm without claiming the
  terminal view, so a host warming a PTY on its own inventory cannot move the view to
  a machine nobody selected.
- The selection, defined in `src/model`, is the canonical selected source /
  session value consumed by display selection and rendering.
- The per-mux display decision lives in the driver implementation, never here.
- The nav selection and the session xmux's own display client is on must name the
  same session, and which of the two moves is decided by the FOCUS and by nothing
  else. In terminal focus the user is driving the mux, so the selection goes to the
  client; in nav focus the selection is the user's own, so the client is attached
  back to it. Exactly one of the two may act at a time, which is what keeps them
  from undoing each other.
- That is a COMPARISON, evaluated on every pass, never an event that is recorded
  and replayed. Nothing anywhere holds a switch that happened, a move that is owed,
  or a moment at which to pay one, so there is no policy for when such a record
  would be paid and none for when it would be cancelled. Where the client is is the
  only thing kept, because it is a standing fact rather than a pending action, and
  a pass that stops seeing a difference stops asking for anything.
- A move the nav cannot make is simply not made. A session created moments ago has
  no card to move to; the client is still on it, so the next pass asks again and
  the move lands on the first pass after the enumeration that brings the card in.
- Reading the live client for its session is skipped while a reattach is in flight
  for the display key. The stale client is deliberately kept on screen and still
  sits on the session the selection just left, so reading it then would report the
  old session as where the display is and send the reconcile after a client that is
  already on its way elsewhere. A switch the mux pushed is a fresh fact rather than
  a re-reading of a stale one, so it is not skipped.
- Carrying the client back is armed only while the debounce is idle and nothing is
  in flight. A navigation burst still coalesces into one trailing attach, and the
  attach already carrying the display is never restarted under itself.
- Focus is the single source of truth for which view owns keys and which modal,
  if any, is open. Focus and modal transitions stay in the focus module; the app
  and the state call into it rather than open-coding view or modal bookkeeping.
- This layer carries no PTY, grid, or terminal-protocol logic; that is `display`.
- The effective nav width under auto-hide is reconciled at the loop top against the
  one prefix-interaction signal the hint bar also reads, so a held prefix cannot
  make the nav and the bar disagree.

## Common Pitfalls

- Do not block the app loop on process spawn, PTY close, pipe reads, writes, or
  resize operations.
- Logging must never write to stdout or stderr: the renderer owns the terminal in
  alt-screen mode, and a stray byte corrupts the display. The panic hook restores
  the terminal before printing the panic message.
- Do not reintroduce display mechanics into the focus module; PTY, grid, and
  input belong in `display`. Do not scatter view-focus or modal-kind decisions
  across the app; route them through focus.

## Before Editing

- For app changes, locate the event source and the state it owns before adding
  fields or channels.
- For focus changes, identify whether the change is a focus or modal state
  transition (here) or a display mechanic (`src/display`).

## Verification

- Drive the behavior end to end when changing selection sync, attach debounce, or
  focus and modal routing: those three are where the loop and the state can
  silently disagree.
- Set `XMUX_LOG=xmux::mux=debug` to raise the display events to debug verbosity;
  the log file is at `<xmux_dir>/xmux.log`.
