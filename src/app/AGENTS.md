# Working Notes: /src/app

## Purpose

`app` is the application orchestration layer: the app runtime that owns the
terminal for the whole session, the ctl socket server, and preference
persistence. The app coordinates the focus and modal state owned by `state`.

## Mental Model

The runtime is a persistent supervisor. It keeps ONE real attached mux client
per session alive in a PTY across selections and renders the SELECTED session's
live grid on the right. A separate control-mode client per remote source supplies
the nav view inventory, mux-side change events, and display-driver
selection; a local mux is enumerated or polled with plain commands. One async loop
interleaves stdin, source events, PTY events, the control socket, terminal resize,
and an animation tick. Every application input becomes a message to the one update
transition over the application model. The transition keeps domain state,
switcher state, geometry, interaction state, source tracking, and the render plan
coherent, then returns ordered effects to the runtime's one executor. The loop
drives I/O and draws the split view from that model.

The ctl server owns each instance endpoint and translates wire requests into app
commands. Preference persistence stores lightweight UI hints as best-effort files
under the xmux directory.

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

- The application model owns domain state, switcher interaction state,
  navigation geometry and preference values, mouse state, connected and
  detecting source sets, and the last render plan. Its update transition is the
  only writer of those values.
- `runtime/` owns only the main event loop and I/O resources: the entry point
  keeps receivers, timers, and the terminal as loop-locals, and drives a select
  where each arm turns its input into a message or performs direct terminal byte
  forwarding. It resolves the source's own driver for display and reads the grid
  back from it; it branches on nothing mux-specific.
- `runtime/` owns holding the nav selection and xmux's own display client to ONE
  session. It learns where the client is in whichever way the mux offers - pushed
  over a control channel, or read off the live client for a mux that pushes
  nothing - and records it; the read runs on the animation beat and is mux-blind
  in both directions, since each mux answers whether its client can be read at
  all. What follows from the two naming different sessions is one comparison made
  on every loop pass, the same for every mux.
- `runtime/` also owns DISCOVERY's async half. It leads with a per-machine
  REACHABILITY probe (bounded): a machine that connects goes on to detection, its
  metadata channels, and, when it named no mux, MUX DISCOVERY (a fire-and-forget probe
  whose answers become new sources through an effect); the model classifies a failed
  machine probe for every card on that machine, and no channel opens. A host that named no mux is
  held by name and transport with no source until its answer arrives, so the probe,
  the login, and mux discovery address a HOST, never a source it may not have yet. It is
  the loop's job because only the loop holds the source registry (what a host already
  serves, and where a new source goes) and the manager that kicks the new source's first
  scan.
- Every runtime host, including one discovered or reconciled after login, shares the
  environment's machine credential store before it can spawn. A probe refusal removes a
  held password only when askpass actually supplied it and ssh then refused it. A probe
  carries the credential generation from spawn, so an older probe result cannot reclassify a
  machine after a newer login.
- Input routing has a pure, stateless core (key resolution, mouse chains, the
  predicates, the input outcome types); the stateful handlers are runtime methods
  that call into it. The prefix is tracked as ready (an interaction is live): the end
  of the function it started, or a focus switch / mouse action (a cancel), clears it.
- Domain state owns focus and modal values plus their reducers. The application
  update transition is the only caller that mutates them during a running app.
- The ctl server owns binding, endpoint permissions, stale endpoint replacement,
  request dispatch, and connection lifetime. Wire parsing and client discovery
  remain in `src/link`.
- Preference persistence owns the small files that restore UI hints across runs.
  Missing, stale, unreadable, or unparsable values fall back to runtime defaults.
- The display mechanics (PTY, grid, input) live in `src/display`; per-source connection
  management lives in `src/link`; the domain types live in `src/model`; the
  durable runtime state bag lives in `src/state`.

## Invariants

- The entry point is thin: the runtime struct owns I/O handles and one application
  model. Every select arm and stateful helper is a method on the runtime, so each
  takes a small argument list rather than a large loose-parameter bundle.
- One exhaustive runtime executor handles every effect from domain actions,
  switcher input, source events, login input, ticks, persistence, and attachment
  bookkeeping. It has no ignored effect variant.
- The app loop is not a second writer of application state. It supplies I/O facts
  as message data, runs update, and executes the returned effects in order. Raw
  terminal bytes forwarded to the selected display are the only direct path.
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
- The effective nav width is reconciled at the loop top against the one prefix-interaction
  signal the hint bar also reads, so a held prefix cannot make the nav and the bar
  disagree. The band height comes from the resize keys and border drags, and the nav's
  attachment side is resolved at the loop top from the settings and the `prefix p` pin;
  the loop-top reconcile resizes the mux terminals and repaints when any dimension moves.

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
