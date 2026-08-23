# Working Notes: /src/ui

## Purpose

`ui` owns the session switcher: pure row-model transforms, side-effecting UI
operations, control socket serving helpers, interactive switcher state, and
ratatui rendering.

## Mental Model

`tree.rs` is side-effect-free model logic over groups and sessions.
`switcher/` is the aggregate interactive TUI surface: selection, flattened rows,
modal/input BEHAVIOR and rendering, key/mouse handling, operation result
application, and render state. The open modal itself lives in `State.modal`
(the `Modal` enum is defined here); the switcher reads/writes it and owns only
the transient popup geometry (drag offset / drawn rect). `chrome.rs` defines the
chrome - the view border, the hint bar, and the unreachable-host info - and its
view-local state (flash, spinner, view border colours, ui prefix, armed);
the hint bar is the NAV's bottom row(s), not a full-width strip, and shows the
prefix alone until it is armed; the `Chrome`
instance itself lives in `State.chrome` (like `State.modal`), fed by the app each
frame and rendered from `&State`. `ops.rs`
holds the off-loop mux-action boundary: the `Ops` trait over the live mux (one
mutating method, `new_session`; the rest read), the
`OpResult` outcomes, and `run_op` which executes one `MuxOp` against `Ops` in a
detached task. A switcher key that COMMITS a slow action (Enter on the new-session
input) resolves it through `State::apply` into a
`Command::RunOp(MuxOp)` it RETURNS up; the run loop spawns `run_op` and folds the
`OpResult` back through the op channel (the switcher no longer holds a pending-op
queue). `run.rs` bridges the control socket into app commands and can flatten
renders for `dump`.

## Module Seams

- Pure row/group transforms belong in `tree.rs`.
- UI colours come from the semantic palette in `palette.rs` (accent / muted tiers,
  the hint bar's pair, the per-level card colours, and `selection_style`), so the
  theme changes in one place. The one exception is the view border: its stock defaults
  deliberately mirror tmux's pane-border defaults and live with their three-tier
  resolve logic in `chrome.rs`.
- A colour the USER named is parsed in `chrome.rs` (`map_color` and the
  `parse_*_style` helpers), never in `palette.rs`: the palette holds xmux's own
  choices, which are slots only.
- Chrome rendering (view border, hint bar, host-info) and its view-local state
  belong in `chrome.rs`; it reads inventory from `&State`, not the switcher.
- Slow (network) mux effects belong behind `ops.rs` (`Ops`/`run_op`/`OpResult`);
  a committing key emits `Command::RunOp(MuxOp)` for the run loop to spawn, it
  does not call the mux itself.
- Control socket serving and dump rendering belong in `run.rs`.
- Card LAYOUT geometry belongs in `switcher/columns.rs` and stays pure: it takes card
  widths and run boundaries and returns rects, so the paint, the mouse hit-test and the
  tests all read one answer. Rendering reads that answer; it does not compute a second
  one.
- Other interaction state and ratatui rendering live in `switcher/` until a
  smaller seam exists for the specific surface being changed.

## Invariants

- Every colour xmux itself paints is an ANSI-16 slot or an attribute (reverse video,
  bold), so the terminal theme resolves it - never an RGB value. A background with no
  slot for it is an attribute instead: the selected card is reverse video, not a
  computed surface. See "Colour ownership" in `CONTEXT.md`; the palette's own test
  fails on a stray `Color::Rgb`.
- Tree transforms do not mutate their inputs unless the function name and
  signature make mutation explicit.
- `dump` should reflect the same split view the main draw path renders.
- A scrollbar is RESERVED a row or a column of the nav region, never overlaid on the
  cards: the selected card is painted by inverting its rect, so a thumb drawn inside one
  inverts with it and reads as a hole in the bar.
- In the portrait column flow, what a card collapses under is decided by POSITION alone,
  never by the selection: a card height that moved with the cursor would reflow whole
  columns as the selection passed over them. The side list keeps its
  selection-expands-the-card rule, where a height change only shifts rows.
- An arrow key points AT the view it focuses, in both focus paths (`app::input` for nav
  focus, `display::input` for terminal focus): right and down name the terminal, left and
  up name the nav. A change to one path is a change to both.
- Modal input owns keys while open; those keys must not leak to the terminal view
  or global shortcuts. At most one modal is open: `State.modal` is
  one Option, so opening any modal drops whatever was open.
- UI actions that become domain intents should resolve to a `model::Action`
  (the app input `Action` projects via `as_action`), applied at
  `State::apply`.
- This layer branches on nothing mux-specific: the switcher renders rows and
  emits domain intents, never a `match` on tmux vs psmux vs zellij. Per-mux behavior lives
  behind the `Mux`/`MuxDriver` seam, reached via `Ops`, not decided here.
- Selection/drag mutators (`select_address` / `set_active_window` /
  `begin_popup_drag`)
  return a `bool` - "did it actually move / grab?" - by accepted convention: the
  app gates its follow-up (attach, event consumption) on that signal. This
  mutate-and-return-bool shape is deliberate; it is not split into a pure
  command/query pair (the churn would exceed the value).

## Common Pitfalls

- Do not put host process management or PTY writes in UI modules.
- Do not reach for a `Color::Rgb` to get a tone the sixteen slots lack (a raised
  surface, a slightly-off bar). There is no theme-safe way to pick one, and a terminal
  may answer no colour query at all - use an attribute, or take the colour from config.
- Do not add side effects to `tree.rs`.
- Do not route public ctl behavior through internal switcher key names.

## Before Editing

- Decide whether the change is pure row data, interactive state, rendering,
  side-effecting operation, or control/dump plumbing.
- For `switcher/`, find the existing helper for the same surface before adding
  another state path.
- Check focus/modal ownership before changing key handling.

## Verification

- Run `ui::tree` tests for pure model transforms.
- Run switcher/app tests for key, mouse, modal, and rendering changes.
- Run control dump tests when changing `ui::run` rendering helpers.
