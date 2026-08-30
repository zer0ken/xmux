# Working Notes: /src/state

## Purpose

`state` is the app's single source of truth: the reachable inventory plus the
selection and display runtime fields that need stable ownership outside the main
loop's local variables, and the two domain-mutation sites (intent-driven and
event-driven). UI components read the state instead of reaching into the row
model.

## Mental Model

The state is the app's durable runtime state bag. It owns the inventory and the
active filter, the canonical selection, the confirmed displayed address, the
focus state machine (which view keys go to, and whether a modal is open), the
open modal, the debounced attach deadline with its pending flag, and the last
session address persisted to preferences. It is seeded from either a scan or the
configured source list.

Applying an ACTION is the single domain-mutation site: it folds one intent into
the state and returns the effects for the run loop to dispatch. It touches only
the state, never reading the clock or any registry or source state directly. The
clock and the runtime attach facts enter as DATA on the tick action. A selection
action records a moved selection and marks the attach pending; the trailing tick
re-arms the attach deadline, on every pending selection, so rapid navigation
coalesces into one trailing attach. Once the deadline elapses and the pure attach
gate holds, the attach command is returned, plus a persist command on an address
change. The gate reads the selection against the displayed address; the terminal
view renders and routes input to the DISPLAYED session, which lags the selection
until the new attach is confirmed (stale-while-revalidate). Creating a session is
the one session-lifecycle intent, and it is a pure effect emitter: no domain
state is mutated, and a single deferred operation is returned for the run loop to
run off-loop, with the inventory change arriving later as that operation's
result. There is no rename, kill, or window intent; the mux owns editing a
session.

Applying an EVENT is the inbound mirror: the single event-driven mutation site.
It folds the arms whose data is SELF-CONTAINED in the event (a poll enumeration,
an exit marking a source unreachable)
into the state through the switcher, and returns the mux follow-ups it cannot
perform itself as effects for the run loop. The once-connected set enters as
DATA, like the clock on a tick: an exit from a once-connected source is a transient
drop that keeps the last-known inventory. Connection and inventory events carry
their parsed sessions, which the loop folds into the source's own inventory, the
single owner; that fold needs the source registry the state layer does not hold, so
it is the loop's job.

The modal is ONE optional value: at most one of help or inline input. A single
option, rather than independent fields, makes the modals' mutual exclusion
structural, so opening one drops whatever was open. The query helpers read it,
delegating to the UI modal module, which owns the modal types, classifiers, and
self-contained behavior (the help feed, the popup drag geometry); the switcher
holds the modal state plus its popup geometry and forwards to that module.

## Module Seams

- The state depends on the domain layer for the selection and the action, command,
  and effect sets; on the UI layer for the inventory groups, the open
  modal, and the switcher it rebuilds rows against; on the app layer for the focus
  state machine; and on the connection layer for the inbound events.
- It stores state facts plus the two mutation sites. The run loop owns effect
  dispatch, both the synchronous commands from an action (switcher selection move,
  attach, preferences IO, quit) and the mux follow-ups from an event (inventory
  fold and apply, refetch, probe, reap, sync, scan dispatch, source add), and
  feeds the runtime attach facts back on the tick. No IO, spawning, or channel
  sends happen here.

## Invariants

- The selection is the source / session / window the display SHOULD show.
- The displayed address is the one whose content is confirmed live on screen; it
  is set only at confirmation, by a synchronous in-place switch or by the display
  becoming ready. The terminal view always renders the displayed grid, so on a
  switch the prior session stays on screen until the new one is confirmed
  (stale-while-revalidate); there is no transitional placeholder.
- The focus is the single source of truth for which view owns keys and which
  modal, if any, is open; a modal carries the view it restores to.
- The modal is the single source of truth for WHICH modal is open and its
  content; the focus's modal dimension is reconciled from it at each loop top. At
  most one modal can be open, because it is one option rather than several fields.
- The attach deadline is the debounce gate for settled selection attachment, and
  the pending flag marks a moved selection awaiting its first tick arm. Re-arming
  on every pending selection is the freeze fix; never arm once.
- The tick ARMS on the same condition the gate FIRES on: a display sitting away from
  the selection, whether the client left for another session or the confirmed display
  is another session altogether. An arm that only a selection move could set would
  leave the gate true with no deadline, and the two regions would stay split until
  the next move.
- The last saved session address prevents rewriting preferences on every window
  step within the same session.
- This layer branches on nothing mux-specific: both apply sites fold intents and
  events over the state without a match on mux kind. Per-mux behavior lives behind
  the mux and driver seam the run loop reaches; the mux enters here only as domain
  data (sessions, windows, events).

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
