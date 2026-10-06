# Working Notes: /src/app

## Purpose

`app` is the application orchestration layer. It holds the application model and its
one update transition, the runtime that owns the terminal for the whole session, the
ctl socket server, and preference persistence. The runtime is a persistent supervisor:
it keeps one attached mux client per session alive in a PTY across selections and
draws the selected session's live grid beside the nav. The update transition keeps
domain state, switcher state, geometry, interaction state, host tracking, and the
render plan coherent, then returns ordered effects for the runtime to execute.

## Module Seams

- The application model owns domain state, switcher interaction state, nav geometry
  and preference values, mouse state, the connected and detecting host sets, the
  re-scan and logout in flight, the running logins, and the last render plan.
- `runtime/` owns the event loop, the I/O resources, and the effect executor; its
  own Working Notes describe it.
- Input routing has a pure, stateless core (key resolution, mouse chains, gesture
  predicates, outcome types); the stateful handlers are runtime methods that call it.
- The ctl server owns binding, endpoint permissions, stale endpoint replacement,
  request dispatch, and connection lifetime. Wire parsing and client discovery stay in
  `src/link`.
- Preference persistence owns the small best-effort files under the xmux directory
  that restore UI hints across runs; a missing, stale, or unparsable value falls back
  to the runtime default.
- Focus and modal values and their reducers live in `src/state`; display mechanics
  (PTY, grid, terminal input) live in `src/display`; the selection and the other
  domain types live in `src/model`.

## Invariants

- A prefix command in either focus resolves through the one key table, so neither
  focus binds a key the table does not name.
- The nav width, band height, and attached side are reconciled once per loop pass,
  against the same prefix-interaction signal the key list reads, and the mux
  terminals are resized whenever any of them moves.
- The update transition is where operation results become toasts and history
  records; a re-scan reports once, against the inventory the user saw when asking.
- This layer carries no PTY, grid, or terminal-protocol logic, and focus or modal
  decisions are not open-coded here: they go through `src/state`.

## Before Editing

- Locate the event source and the state it owns before adding a field or a channel.

## Verification

- Drive the behavior end to end when changing selection sync, the attach debounce,
  or focus and modal routing: those are where the loop and the state can silently
  disagree.
- `XMUX_LOG=xmux::mux=debug` raises the display events to debug verbosity; the log
  file is `xmux.log` in the xmux directory.
