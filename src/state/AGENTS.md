# Working Notes: /src/state

## Purpose

`state` defines the domain state held by the application model: the inventory and
active filter, the canonical selection, the confirmed displayed address, focus, the
open modal, chrome data, notifications, login and authentication state, the attach
debounce, and the last session address persisted to preferences. It is seeded from a
scan or the configured source list. The app update transition owns all mutation and
folds domain intents through this layer's action reducer, which touches only state
and returns commands; the clock and runtime attach facts enter as data on the tick.

## Module Seams

- The state depends on backend layers for inventory groups, login inputs, operation
  results, the selection, and the action, command, effect, and event types.
- The app owns message handling and unified effect dispatch: switcher and connection
  actions, inventory application, refetch, probe, reap, sync, scan dispatch, and
  source addition.
- State owns the focus state machine, the modal types, classifiers, input editing,
  and the read-only popup feed; the UI owns popup geometry and rendering.
- Notifications hold the toasts on screen and the bounded history behind them; the
  tick takes down expired toasts, so rendering reads remaining life and ages without
  reading the clock.

## Invariants

- The selection is the session the display should show; the displayed address is
  the one confirmed live on screen, set only at confirmation, and the terminal view
  always renders it.
- Focus is the single source of truth for which view owns keys and which modal is
  open, and a modal carries the view it restores to. The modal value is the single
  source of truth for which modal is open and its content; the focus's modal
  dimension is reconciled from it on every loop pass.
- The attach debounce re-arms on every pending selection and arms on the same
  condition the attach gate fires on.
- A display PTY that died while the selection stands attaches nothing.
- No IO, spawning, channel sends, registry mutation, or clock reads happen here;
  return a command for the loop to run instead.
- The action reducer folds intents without a match on mux kind; the mux enters here
  only as domain data.

## Common Pitfalls

- Do not add fields here just to shorten a function signature; add a field only when
  its owner and the event that changes it are clear, and check every app site that
  reads or writes it.

## Verification

- Exercise selection sync and the attach debounce end to end: they are where a state
  change most easily desynchronizes from the loop.
