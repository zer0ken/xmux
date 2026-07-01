# Working Notes: /src/state

## Purpose

`state` is the app's single source of truth: the reachable inventory plus the
selection/display runtime fields that need stable ownership outside the main
loop's local variables, and `State::apply` / `State::apply_event` — the two
domain-mutation sites (intent-driven and event-driven). UI components read
`&State` instead of reaching into the tree.

## Mental Model

`State` is the app's durable runtime state bag. It owns the inventory
(`groups`/`panes`/`scanning`/`panes_loaded`) and the active `filter`, the
canonical `selection`, the confirmed `displayed` address, the `focus` state
machine (which view keys go to; whether a modal is open), the open modal
`popup`, the debounced `attach_deadline` + `attach_pending` flag, and the last
session address persisted to prefs. `from_scan` / `from_sources` seed the
inventory.

`State::apply(Action) -> Vec<Command>` is the single domain-mutation site: it
folds one intent into the state and returns the effects for the run loop to
dispatch. `apply` touches only `State` — it never reads the clock or any
registry/host state directly. The clock and the runtime attach facts enter as
DATA on `Action::Tick { now, key_live, in_flight }`. `Action::Select` records a
moved selection and marks `attach_pending`; the trailing `Tick` (re)arms
`attach_deadline = now + 90ms` — re-armed on every pending Select so rapid
navigation coalesces into one trailing attach — and, once the deadline elapses
and the `should_attach` gate holds, returns `Command::Attach` (plus
`Command::PersistLastSession` on an address change). `should_attach` is the pure
gate method that reads `selection` / `displayed`; the terminal view renders and
routes input to `displayed` (the confirmed session), which lags `selection`
until the new attach is confirmed (stale-while-revalidate). The
session-lifecycle intents (`CreateSession` / `NewWindow` /
`SplitWindow` / `RenameSession` / `KillSession` / `KillWindow` / `RenameWindow`)
are pure effect emitters: `apply` mutates no domain state and returns a single
`Command::RunOp(MuxOp)` the run loop runs off-loop (the inventory change arrives
later as the op's result).

`State::apply_event(HostEvent, &mut Switcher, &mut connected) -> Vec<EventEffect>`
is the inbound mirror of `apply`: the single event-driven mutation site. It folds
the arms whose data is SELF-CONTAINED in the event (the `Focus` active-window
marker, a `Panes` subtree, a `Sessions` poll enumeration, the `Exited`
unreachable mark) into `State` through the switcher, and returns the mux
follow-ups it cannot perform itself as `EventEffect`s for the run loop to run
(`ApplyInventory` / `Refetch` / `ProbeActiveWindow` / `ReapHost` /
`ReapDisplayAttach` / `DispatchScanned` / `SyncPollSessions`). The `connected`
once-connected set enters as DATA (like the clock on `Tick`): an `Exited` of a
once-connected host is a transient drop that keeps the last-known tree. The
`Connected`/`Inventory` inventory lives behind the host client's lock the state
layer cannot reach, so its apply is deferred to the loop as `ApplyInventory`.

`popup` is one `Option<ui::switcher::Popup>` — at most one of help / inline
input / kill confirm / context menu. A single Option (not four independent
fields) makes the modals' mutual exclusion structural: opening one drops
whatever was open. The query helpers `is_modal_popup_open` / `is_inputting` /
`menu_active` / `modal_kind` read it; the switcher owns the modal behavior and
the transient popup geometry (drag offset / drawn rect).

## Module Seams

- `State` depends on `app::app::Selection` for selected source/session/window,
  `ui::tree::Group` + `session::WindowPanes` for the inventory,
  `app::focus::Focus` for the focus state machine, `ui::switcher::Popup` for the
  open modal, `model::{Action, Command}` for the `apply` vocabulary, and
  `host::HostEvent` + `model::EventEffect` + `&mut ui::switcher::Switcher` for
  `apply_event` (the switcher rebuilds the tree against `&mut State`).
- It stores state facts + the two mutation sites (`apply` / `apply_event`); the
  run loop owns effect dispatch — for `apply` the synchronous `Command`s (switcher
  cursor move, attach, prefs IO, quit) and for `apply_event` the `EventEffect`
  mux follow-ups (inventory lock apply, refetch, probe, reap, sync,
  scan-dispatch) — and feeds back the runtime attach facts on `Tick`. No
  IO/spawning/channel sends happen here.

## Invariants

- `selection` is the source/session/window the display SHOULD show.
- `displayed` is the address whose content is confirmed live on screen; it is set
  only at confirmation (a synchronous in-place switch/select-window, or
  `DisplayReady`). The terminal view always renders `displayed`'s grid, so on a
  switch the prior session stays on screen until the new one is confirmed
  (stale-while-revalidate); there is no transitional placeholder.
- `focus` is the single source of truth for which view owns keys and which modal
  (if any) is open; a modal carries the view it restores to.
- `popup` is the single source of truth for WHICH modal is open and its content;
  `focus`'s modal dimension is reconciled from it each loop-top via `modal_kind`.
  At most one modal can be open because it is one Option, not four fields.
- `attach_deadline` is the debounce gate for settled selection attachment;
  `attach_pending` marks a moved selection awaiting its first `Tick` arm.
  Re-arming on every pending Select is the freeze fix — never arm-once.
- `last_saved_session` prevents rewriting prefs on every window step within the
  same session.
- This layer branches on nothing mux-specific: `apply` / `apply_event` fold
  intents and events over `State` without a `match` on tmux vs psmux. Per-mux
  behavior lives behind the `Mux`/`MuxDriver` seam the run loop reaches; the
  mux enters here only as domain data (sessions, windows, events).

## Common Pitfalls

- Do not add fields here just to shorten a function signature; add fields only
  when state ownership is clear.
- Do not perform IO, spawning, channel sends, or registry mutation from this
  module — return a `Command` for the loop to run instead.
- Do not read `Instant::now()` or registry/host state inside `apply`; both enter
  as data on `Action::Tick`.

## Before Editing

- Check every app site that reads or writes the field.
- Define when the field changes and which event source owns that transition.

## Verification

- Run `state` tests and the app tests that exercise selection sync and attach
  debounce.
