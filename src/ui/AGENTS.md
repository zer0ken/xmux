# Working Notes: /src/ui

## Purpose

`ui` owns the session switcher: pure tree model transforms, side-effecting UI
operations, control socket serving helpers, interactive switcher state, and
ratatui rendering.

## Mental Model

`tree.rs` is side-effect-free model logic over groups and sessions.
`switcher.rs` is the aggregate interactive TUI surface: cursor, flattened rows,
menus, popups, inline input, key/mouse handling, operation result application,
and render state. `ops.rs` performs slower side-effecting operations behind the
switcher interface. `run.rs` bridges the control socket into cockpit commands
and can flatten renders for `dump`.

## Module Seams

- Pure row/group transforms belong in `tree.rs`.
- External effects initiated by UI actions belong behind `ops.rs`.
- Control socket serving and dump rendering belong in `run.rs`.
- Interaction state and ratatui rendering live in `switcher.rs` until a smaller
  seam exists for the specific surface being changed.

## Invariants

- Tree transforms do not mutate their inputs unless the function name and
  signature make mutation explicit.
- `dump` should reflect the same split view the main draw path renders.
- Modal and menu input owns keys while open; those keys must not leak to mux
  passthrough or global shortcuts.
- UI actions that become domain commands should resolve through `Operation`.

## Common Pitfalls

- Do not put host process management or PTY writes in UI modules.
- Do not add side effects to `tree.rs`.
- Do not route public ctl behavior through internal switcher key names.

## Before Editing

- Decide whether the change is pure tree data, interactive state, rendering,
  side-effecting operation, or control/dump plumbing.
- For `switcher.rs`, find the existing helper for the same surface before adding
  another state path.
- Check focus/modal ownership before changing key handling.

## Verification

- Run `ui::tree` tests for pure model transforms.
- Run switcher/cockpit tests for key, mouse, modal, menu, and rendering changes.
- Run control dump tests when changing `ui::run` rendering helpers.
