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
- `runtime/` owns FOLLOWING a session change the mux made to xmux's own display
  client, in one place for every way that change is learned: pushed over a
  control channel, or read off the live client for a mux that pushes nothing.
  The read runs on the animation beat and is mux-blind in both directions, since
  each mux answers whether its client can be read at all.
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
  management lives in `src/link`; the domain vocabulary lives in `src/model`; the
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
- The selection, defined in `src/model`, is the canonical selected source /
  session value consumed by display selection and rendering.
- The per-mux display decision lives in the driver implementation, never here.
- The nav moves for a mux-side client switch only in terminal focus. In nav focus
  the selection is the user's and the mux does not move it, though where the
  client actually is is still recorded, because that is a fact rather than a
  claim about what the user picked.
- Reading the live client for a switch is skipped while a reattach is in flight
  for the display key. The stale client is deliberately kept on screen and still
  sits on the session the selection just left, so reading it then would report the
  old session as a fresh switch and drag the nav backwards. A switch the mux
  pushed is a fresh fact rather than a re-reading of a stale one, so it is not
  skipped, and neither is a move already owed, which carries a fact learned before
  the reattach began.
- A follow lands as soon as the nav can hold it and the user is not driving the
  nav. Its two halves settle on different schedules: where the client is, is
  recorded at once and unconditionally, because it is a fact and because it is
  what stops a driver from reattaching the client the user just moved; the nav
  move waits on a card, which a session enumerated after its switch was learned
  has not got yet, and on the terminal focus, which a detour into the nav takes
  away. A move that cannot land is remembered and retried on the animation beat
  and on the sweeps that grow the nav, so a latched belief can never answer later
  probes with "already there" while the nav names another session. Passing through
  the nav defers the move rather than cancelling it: the two regions must not
  settle on different sessions because the user looked at the list on the way.
- One owed move per source, holding the latest session the client reported. It is
  dropped when it lands, when the host's display belief no longer names its
  session (a later switch, or the user settling the display on a session of their
  own, both of which write that belief), and when the client that reported it
  dies, because its session may never get a card and nothing is on it any more.
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
