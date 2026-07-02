# Working Notes: /src/ui

## Purpose

`ui` owns the session switcher: pure tree model transforms, side-effecting UI
operations, control socket serving helpers, interactive switcher state, and
ratatui rendering.

## Mental Model

`tree.rs` is side-effect-free model logic over groups and sessions.
`switcher.rs` is the aggregate interactive TUI surface: selection, flattened rows,
modal/menu/input BEHAVIOR and rendering, key/mouse handling, operation result
application, and render state. The open modal itself lives in `State.modal`
(the `Modal` enum is defined here); the switcher reads/writes it and owns only
the transient popup geometry (drag offset / drawn rect). `chrome.rs` owns the
chrome — the view border,
the hint bar, and the unreachable-host info — plus its view-local state
(flash, spinner, view border colours, ui prefix), rendering from `&State`. `ops.rs`
holds the off-loop mux-action boundary: the `Ops` trait over the live mux, the
`OpResult` outcomes, and `run_op` which executes one `MuxOp` against `Ops` in a
detached task. A switcher key that COMMITS a slow action (Enter on an input, `y`
on a kill confirm) resolves it through `State::apply` into a
`Command::RunOp(MuxOp)` it RETURNS up; the run loop spawns `run_op` and folds the
`OpResult` back through the op channel (the switcher no longer holds a pending-op
queue). `run.rs` bridges the control socket into app commands and can flatten
renders for `dump`.

## Module Seams

- Pure row/group transforms belong in `tree.rs`.
- Chrome rendering (view border, hint bar, host-info) and its view-local state
  belong in `chrome.rs`; it reads inventory from `&State`, not the switcher.
- Slow (network) mux effects belong behind `ops.rs` (`Ops`/`run_op`/`OpResult`);
  a committing key emits `Command::RunOp(MuxOp)` for the run loop to spawn, it
  does not call the mux itself.
- Control socket serving and dump rendering belong in `run.rs`.
- Other interaction state and ratatui rendering live in `switcher.rs` until a
  smaller seam exists for the specific surface being changed.

## Invariants

- Tree transforms do not mutate their inputs unless the function name and
  signature make mutation explicit.
- `dump` should reflect the same split view the main draw path renders.
- Modal and menu input owns keys while open; those keys must not leak to the terminal view
  or global shortcuts. At most one modal is open: `State.modal` is
  one Option, so opening any modal drops whatever was open.
- UI actions that become domain intents should resolve to a `model::Action`
  (the app input `Action` projects via `as_action`), applied at
  `State::apply`.
- This layer branches on nothing mux-specific: the switcher renders a tree and
  emits domain intents, never a `match` on tmux vs psmux. Per-mux behavior lives
  behind the `Mux`/`MuxDriver` seam, reached via `Ops`, not decided here.

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
- Run switcher/app tests for key, mouse, modal, menu, and rendering changes.
- Run control dump tests when changing `ui::run` rendering helpers.
