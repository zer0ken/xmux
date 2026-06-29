# Working Notes: /src/state

## Purpose

`state` is the cockpit's single source of truth: the reachable inventory plus the
selection/display runtime fields that need stable ownership outside the main
loop's local variables. UI components read `&State` instead of reaching into the
tree.

## Mental Model

`State` is the cockpit's durable runtime state bag. It owns the inventory
(`groups`/`panes`/`scanning`/`panes_loaded`) and the active `filter`, the
canonical `selection`, the confirmed `displayed` address, the `focus` state
machine (which pane keys go to; whether a modal is open), the open modal
`popup`, the debounced attach deadline, and the last session address persisted
to prefs. `from_scan` / `from_sources` seed the inventory.

`popup` is one `Option<ui::switcher::Popup>` — at most one of help / inline
input / kill confirm / context menu. A single Option (not four independent
fields) makes the modals' mutual exclusion structural: opening one drops
whatever was open. The query helpers `is_modal_popup_open` / `is_inputting` /
`menu_active` / `modal_kind` read it; the switcher owns the modal behavior and
the transient popup geometry (drag offset / drawn rect).

## Module Seams

- `State` depends on `cockpit::Selection` for selected source/session/window,
  `ui::tree::Group` + `session::WindowPanes` for the inventory,
  `proxy::app::Focus` for the focus state machine, and `ui::switcher::Popup`
  for the open modal.
- It stores state facts only; event handling and side effects remain in cockpit
  and related runtime modules.

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
