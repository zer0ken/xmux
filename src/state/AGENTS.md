# Working Notes: /src/state

## Purpose

`state` is the cockpit's single source of truth: the reachable inventory plus the
selection/display runtime fields that need stable ownership outside the main
loop's local variables, and `State::apply` — the single domain-mutation site. UI
components read `&State` instead of reaching into the tree.

## Mental Model

`State` is the cockpit's durable runtime state bag. It owns the inventory
(`groups`/`panes`/`scanning`/`panes_loaded`) and the active `filter`, the
canonical `selection`, the confirmed `displayed` address, the `focus` state
machine (which pane keys go to; whether a modal is open), the open modal
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
`Command::PersistLastSession` on an address change). `should_attach` and
`display_matches_selection` are the pure gate methods that read `selection` /
`displayed`.

`popup` is one `Option<ui::switcher::Popup>` — at most one of help / inline
input / kill confirm / context menu. A single Option (not four independent
fields) makes the modals' mutual exclusion structural: opening one drops
whatever was open. The query helpers `is_modal_popup_open` / `is_inputting` /
`menu_active` / `modal_kind` read it; the switcher owns the modal behavior and
the transient popup geometry (drag offset / drawn rect).

## Module Seams

- `State` depends on `cockpit::Selection` for selected source/session/window,
  `ui::tree::Group` + `session::WindowPanes` for the inventory,
  `proxy::app::Focus` for the focus state machine, `ui::switcher::Popup` for the
  open modal, and `model::{Action, Command}` for the `apply` vocabulary.
- It stores state facts + the single mutation site (`apply`); the run loop owns
  effect dispatch (switcher cursor move, attach, prefs IO, quit) and feeds back
  the runtime attach facts on `Tick`. No IO/spawning/channel sends happen here.

## Invariants

- `selection` is the source/session/window the display SHOULD show.
- `displayed` is the address whose content is confirmed live on screen; it is set
  only at confirmation (a synchronous switch/select-window, or `DisplayReady`).
  The grid renders only while `displayed` matches `selection`'s session, so a
  stale attachment shows "(attaching…)" rather than the previous session.
- `focus` is the single source of truth for which pane owns keys and which modal
  (if any) is open; a modal carries the pane it restores to.
- `popup` is the single source of truth for WHICH modal is open and its content;
  `focus`'s modal dimension is reconciled from it each loop-top via `modal_kind`.
  At most one modal can be open because it is one Option, not four fields.
- `attach_deadline` is the debounce gate for settled selection attachment;
  `attach_pending` marks a moved selection awaiting its first `Tick` arm.
  Re-arming on every pending Select is the freeze fix — never arm-once.
- `last_saved_session` prevents rewriting prefs on every window step within the
  same session.

## Common Pitfalls

- Do not add fields here just to shorten a function signature; add fields only
  when state ownership is clear.
- Do not perform IO, spawning, channel sends, or registry mutation from this
  module — return a `Command` for the loop to run instead.
- Do not read `Instant::now()` or registry/host state inside `apply`; both enter
  as data on `Action::Tick`.

## Before Editing

- Check every cockpit site that reads or writes the field.
- Define when the field changes and which event source owns that transition.

## Verification

- Run `state` tests and the cockpit tests that exercise selection sync and attach
  debounce.
