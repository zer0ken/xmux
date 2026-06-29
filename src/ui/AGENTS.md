# Working Notes: /src/ui

## Purpose

`ui` owns the session switcher: pure tree model transforms, side-effecting UI
operations, control socket serving helpers, interactive switcher state, and
ratatui rendering.

## Mental Model

`tree.rs` is side-effect-free model logic over groups and sessions.
`switcher.rs` is the aggregate interactive TUI surface: cursor, flattened rows,
modal/menu/input BEHAVIOR and rendering, key/mouse handling, operation result
application, and render state. The open modal itself lives in `State.popup`
(the `Popup` enum is defined here); the switcher reads/writes it and owns only
the transient popup geometry (drag offset / drawn rect). `status.rs` owns the
status-bar surface — the divider rule,
the footer, and the unreachable-host info panel — plus its view-local state
(flash, spinner, divider colours, ui prefix), rendering from `&State`. `ops.rs`
performs slower side-effecting operations behind the switcher interface.
`run.rs` bridges the control socket into cockpit commands and can flatten
renders for `dump`.

## Module Seams

- Pure row/group transforms belong in `tree.rs`.
- Status-bar rendering (divider, footer, host-info) and its view-local state
  belong in `status.rs`; it reads inventory from `&State`, not the switcher.
- External effects initiated by UI actions belong behind `ops.rs`.
- Control socket serving and dump rendering belong in `run.rs`.
- Other interaction state and ratatui rendering live in `switcher.rs` until a
  smaller seam exists for the specific surface being changed.

## Invariants

- Tree transforms do not mutate their inputs unless the function name and
  signature make mutation explicit.
- `dump` should reflect the same split view the main draw path renders.
- Modal and menu input owns keys while open; those keys must not leak to mux
  passthrough or global shortcuts. At most one modal is open: `State.popup` is
  one Option, so opening any modal drops whatever was open.
- UI actions that become domain intents should resolve to a `model::Action`
  (the cockpit input `Action` projects via `as_action`), applied at
  `State::apply`.

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
