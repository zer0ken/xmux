# Working Notes: /src/state

## Purpose

`state` holds explicit cockpit runtime state fields that need stable ownership
outside local variables in the main loop.

## Mental Model

`State` is the cockpit's durable runtime state bag. It tracks the canonical
selection, the last selection that triggered display attachment, the debounced
attach deadline, and the last session address persisted to prefs.

## Module Seams

- `State` depends on `cockpit::Selection` for selected source/session/window.
- It stores state facts only; event handling and side effects remain in cockpit
  and related runtime modules.

## Invariants

- `selection` is the source/session/window the display should show.
- `last_attached_sel` is only updated after an actual attach path accepts the
  selection.
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
