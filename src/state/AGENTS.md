# Working Notes: /src/state

## Purpose

`state` is the cockpit's single source of truth: the reachable inventory plus the
selection/display runtime fields that need stable ownership outside the main
loop's local variables. UI components read `&State` instead of reaching into the
tree.

## Mental Model

`State` is the cockpit's durable runtime state bag. It owns the inventory
(`groups`/`panes`/`scanning`/`panes_loaded`) and the active `filter`, plus the
canonical `selection`, the confirmed `displayed` address, the debounced attach
deadline, and the last session address persisted to prefs. `from_scan` /
`from_sources` seed the inventory.

## Module Seams

- `State` depends on `cockpit::Selection` for selected source/session/window, and
  on `ui::tree::Group` + `session::WindowPanes` for the inventory.
- It stores state facts only; event handling and side effects remain in cockpit
  and related runtime modules.

## Invariants

- `selection` is the source/session/window the display SHOULD show.
- `displayed` is the address whose content is confirmed live on screen; it is set
  only at confirmation (a synchronous switch/select-window, or `DisplayReady`).
  The grid renders only while `displayed` matches `selection`'s session, so a
  stale attachment shows "(attaching…)" rather than the previous session.
- `attach_deadline` is the debounce gate for settled selection attachment.
- `last_saved_session` prevents rewriting prefs on every window step within the
  same session.

## Common Pitfalls

- Do not add fields here just to shorten a function signature; add fields only
  when state ownership is clear.
- Do not perform IO, spawning, channel sends, or registry mutation from this
  module.

## Before Editing

- Check every cockpit site that reads or writes the field.
- Define when the field changes and which event source owns that transition.

## Verification

- Run `state` tests and the cockpit tests that exercise selection sync and attach
  debounce.
