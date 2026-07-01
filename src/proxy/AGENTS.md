# Working Notes: /src/proxy

## Purpose

`proxy` owns the application UI-state layer: the cockpit's focus and modal
routing state machine. The PTY/grid/input display mechanics live in `src/display`;
this directory holds only `app.rs`.

## Mental Model

`app.rs` tracks which pane holds focus (tree vs terminal) and which modal is open,
and exposes the transitions the cockpit and `State` fold through. It is UI state,
not display mechanics: it decides where input is routed, not how a PTY is pumped or
a grid is rendered.

## Module Seams

- `app.rs` holds the focus/modal state (`Focus`, `PaneFocus`, `ModalKind`) and the
  focus-transition helpers. `state::State` embeds a `Focus`; the cockpit reads and
  mutates it through these types.

## Invariants

- Focus and modal transitions stay in `app.rs`; the cockpit and `State` call into
  them rather than open-coding pane/modal bookkeeping.
- This layer carries no PTY, grid, or terminal-protocol logic (that is `display`).

## Common Pitfalls

- Do not reintroduce display mechanics here; PTY/grid/input belongs in `display`.
- Do not scatter pane-focus or modal-kind decisions across the cockpit; route them
  through `app.rs`.

## Before Editing

- Identify whether the change is a focus/modal state transition (here) or a display
  mechanic (`src/display`).

## Verification

- Run cockpit and state tests when changing focus routing or modal routing.
