# Working Notes: /src/app

## Purpose

`app` is the application orchestration layer: the app runtime that owns the
terminal for the whole session, plus the application UI-state machine it folds
through. The app is the coordinator; focus is the focus/modal routing state it
reads and mutates.

## Mental Model

The runtime is a persistent supervisor. It keeps ONE real attached mux client
per session alive in a PTY across selections and renders the SELECTED session's
live grid on the right. A separate control-mode client per remote host supplies
the nav view inventory, mux-side change events, and programmatic window
selection; a local mux is enumerated or polled with plain commands. One async
loop interleaves stdin, host events, PTY events, the control socket, terminal
resize, and an animation tick. It folds domain actions and inbound host events
through the runtime state, dispatches the returned commands and effects, keeps
the state in sync with the switcher selection, drives the debounced attach, and
draws the split view.

Focus tracks which view holds focus (nav or terminal) and which modal is open,
and exposes the transitions the app and the state fold through. It is UI state,
not display mechanics: it decides where input is routed, not how a PTY is pumped
or a grid is rendered.

## Module Seams

- `runtime/` owns the main event loop as one struct: the entry point builds it,
  keeps the loop's receivers, timers, and terminal as loop-locals, and drives a
  select where each arm is one method on that struct. It resolves the host's own
  driver for display and reads the grid back from it; it branches on nothing
  mux-specific. The canonical selection it reads lives in `src/model`.
- `runtime/` also owns MUX DISCOVERY's async half: one fire-and-forget probe per
  remote machine that named no mux, spawned right after the startup scans, whose
  answers become new sources through an effect. It is the loop's job because only
  the loop holds the host registry (what a machine already serves, and where a
  new host goes) and the manager that kicks the new source's first scan.
- Input routing has a pure, stateless core (key resolution, mouse chains, the
  predicates, the input outcome types); the stateful handlers are runtime methods
  that call into it.
- Focus holds the focus and modal state plus the transition helpers. The runtime
  state embeds it; the app reads and mutates it through those helpers.
- The display mechanics (PTY, grid, input) live in `src/display`; host connection
  management lives in `src/host`; the domain vocabulary lives in `src/model`; the
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
  session / window value consumed by display selection and rendering.
- The per-mux display decision lives in the driver implementation, never here.
- Focus is the single source of truth for which view owns keys and which modal,
  if any, is open. Focus and modal transitions stay in the focus module; the app
  and the state call into it rather than open-coding view or modal bookkeeping.
- This layer carries no PTY, grid, or terminal-protocol logic; that is `display`.

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
