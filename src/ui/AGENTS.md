# Working Notes: /src/ui

## Purpose

`ui` owns the session switcher: pure row-model transforms over groups and sessions,
the interactive surface in `switcher/`, rendering, the off-loop operation runners, and
off-screen render dumps. The runtime state owns the modal and the chrome data; this
layer reads and paints them and owns only transient popup geometry.

## Module Seams

- The row model holds pure row and group transforms, with no side effects.
- `switcher/` holds selection, the flattened rows, key and mouse handling, operation
  result application, and render state.
- Card layout geometry is pure: it takes card sizes and returns rects, so the paint and
  the mouse hit-test read one answer and rendering never computes a second one.
- The chrome renders the view border, the hint bar, and the view screens, and reads
  inventory from the runtime state, not from the switcher. It also holds the override
  layer of user-named colours over the semantic palette.
- The key list, the help, and the selection hint read every key and its words from the
  model's key table; a surface never spells a key itself.
- Slow mux effects run behind the operations module: a committing key emits a
  deferred-operation command for the run loop to spawn and never calls the mux itself.
- Off-screen dump rendering lives beside the other rendering code.
- `braille_x/` holds the precomputed frame atlas the Braille animation reads.

## Invariants

- The selection is a node (machine, host, or session), not a row; a row is only where
  the node stands. Only the selected half of a section title is highlighted.
- A machine and its mux are one label, and the mux in it is resolved once per card, so a
  session card, its host's card, and the screen behind either cannot spell one mux
  three ways.
- Every in-flight marker reads its glyph from the one spinner helper on the frame the
  chrome advances, so nothing on screen turns out of step.
- Row transforms do not mutate their inputs unless the function name and signature make
  mutation explicit.
- Modal input owns keys while open; they never leak to the terminal view or to global
  shortcuts. The state holds one optional modal, so opening one replaces any open modal.
- A surface that exists to be read never shortens what it states: a value too wide for
  its column continues beneath the same value column, a multi-line value keeps its
  lines, and a control character is written as its escape. Where a datum does not fit,
  the surface grows.
- The words on a screen and the values the code runs come from one place: the ssh
  connect wait is printed from the constant the ssh option is built from, and a status
  word from the one helper the cards read.
- No UI module manages a host process or writes to a PTY.

## Before Editing

- Decide whether the change is pure row data, interactive state, rendering, or a
  side-effecting operation, and find the existing `switcher/` helper for the same
  surface before adding another state path.

## Verification

- Re-check the dump output when its rendering helpers change: it and the live draw
  must agree.
