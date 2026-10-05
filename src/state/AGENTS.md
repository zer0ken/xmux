# Working Notes: /src/state

## Purpose

`state` defines the domain state held by the application model: the reachable
inventory plus the selection and display fields, focus, modal, and chrome data.
UI components read it instead of reaching into the row model. The app update
transition owns all mutation and uses this layer's action reducer for domain
intents.

## Mental Model

The state is the app's durable runtime state bag. It owns the inventory and the
active filter, the canonical selection, the confirmed displayed address, the
focus state machine (which view keys go to, and whether a modal is open), the
open modal, the debounced attach deadline with its pending flag and its
dead-display recovery budget, and the last session address persisted to
preferences. It is seeded from either a scan or the configured source list.

Login results are per-machine state separate from reachability and enumeration errors.
A successful SSH connection's reported authentication method is per machine and is
shown on its host information screen. Losing a held password invalidates that
method and closes its metadata and display connections before another explicit
connection attempt.
A later probe cannot replace the login's own reason or its key-registration outcome.
A login's steps belong to one submission on one source. They advance only on reports
carrying that submission, on the answer to the probe the working login started, and on
the first mux answer after it. They outlive the running login's handle because the mux
search runs after the verdict, and leave once a mux answers or a later probe of the
machine makes settled steps describe an older state.

Applying an ACTION is the domain reducer used by the app update transition. It
folds one intent into state and returns effects for the application to unify with
the effects of every other message. It touches only state, never reading the clock
or any registry or source state directly. The clock and runtime attach facts enter
as data on the tick action. A selection
action records a moved selection and marks the attach pending; the trailing tick
re-arms the attach deadline, on every pending selection, so rapid navigation
coalesces into one trailing attach. Once the deadline elapses and the pure attach
gate holds, the attach command is returned, plus a persist command on an address
change. The gate reads the selection against the displayed address; the terminal
view renders the DISPLAYED session, which lags the selection until the fresh attach
has painted or reaches its bounded wait (stale-while-revalidate). Input already targets
the fresh attachment during that wait. Creating a session is
the one session-lifecycle intent, and it is a pure effect emitter: no domain
state is mutated, and a single deferred operation is returned for the run loop to
run off-loop, with the inventory change arriving later as that operation's
result. There is no rename, kill, or window intent; the mux owns editing a
session.

Inbound source events enter the app update transition as messages. That transition
updates domain state, switcher state, and connection tracking together, and emits
ordered backend effects. An exit from a once-connected source is a transient drop
that keeps the last-known inventory. Connection and inventory events carry parsed
sessions for update to fold into the source's own inventory, the single owner.

The modal is ONE optional value: at most one of help, history, host check, command
palette, or input popup (filter, jump, new session, logout).
A single option, rather than independent fields, makes the modals' mutual exclusion
structural, so opening one drops whatever was open. State owns the modal types,
classifiers, input editing, and the read-only popup feed. The UI owns popup geometry
and rendering.

The notifications are the toasts on screen and the bounded history behind them. A
result of work the user started is a toast and a history record; a background event is
a history record only. The clock enters on the tick, which takes down the toasts whose
life is over, so rendering reads remaining life and ages without reading the clock.

## Module Seams

- The state depends on backend layers for inventory groups, login inputs,
  operation results, selection, action, command, effect, and inbound event data.
  It owns the focus state machine and the modal and chrome data consumed by UI.
- It stores domain facts and supplies the action reducer. The app owns message
  handling and unified effect dispatch, including switcher and connection actions,
  inventory application, refetch, probe, reap, sync, scan dispatch, and source
  addition. No IO, spawning, channel sends, presentation behavior, or application
  orchestration happen here.

## Invariants

- The selection is the source / session the display SHOULD show.
- The displayed address is the one whose content is confirmed live on screen; it
  is set only at confirmation, by a synchronous in-place switch or by the display
  becoming painted or reaching its bounded wait. The terminal view always renders the
  displayed grid, so on a switch the prior session stays on screen until that point
  (stale-while-revalidate); there is no transitional placeholder, while input already
  goes to the fresh attachment.
- The focus is the single source of truth for which view owns keys and which
  modal, if any, is open; a modal carries the view it restores to.
- The modal is the single source of truth for WHICH modal is open and its
  content; the focus's modal dimension is reconciled from it at each loop top. At
  most one modal can be open, because it is one option rather than several fields.
- The attach deadline is the debounce gate for settled selection attachment, and
  the pending flag marks a moved selection awaiting its first tick arm. Re-arming
  on every pending selection is the freeze fix; never arm once.
- The tick ARMS on the same condition the gate FIRES on: a display sitting away
  from the selection - the client left for another session, or the confirmed display
  is another session altogether. An arm that only a selection move could set would
  leave the gate true with no deadline, and the two regions would stay split until
  the next move.
- A display PTY that DIED while the selection stands attaches NOTHING. Each attach is a
  fresh connection to that machine, so an attach raised by the death of the attach before
  it is a chain, and a session that is gone makes every attempt die the same way, so the
  chain has no end of its own. The pane keeps the last frame it drew; the user recovers it
  by selecting the card again or re-scanning.
- The last saved session address prevents rewriting preferences on every step
  within the same session.
- This layer branches on nothing mux-specific: the action reducer folds intents
  without a match on mux kind. Per-mux behavior lives behind the mux and driver
  seam the run loop reaches; the mux enters here only as domain data.
- This layer imports backend peers and itself. It has no application or UI
  dependency and no known Layer Direction exception.

## Common Pitfalls

- Do not add fields here just to shorten a function signature; add fields only
  when state ownership is clear.
- Do not perform IO, spawning, channel sends, or registry mutation from this
  module. Return a command for the loop to run instead.
- Do not read the clock or registry and source state inside apply; both enter as
  data on the tick.

## Before Editing

- Check every app site that reads or writes the field.
- Define when the field changes and which event source owns that transition.

## Verification

- Exercise selection sync and the attach debounce end to end: they are where a
  state change most easily desynchronizes from the loop.
