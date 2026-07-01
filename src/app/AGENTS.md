# Working Notes: /src/app

## Purpose

`app` is the application orchestration layer: the cockpit runtime that owns the
terminal for the whole session (`cockpit.rs`) plus the application UI-state
machine it folds through (`focus.rs`). The cockpit is the coordinator; `focus`
is the focus/modal routing state it reads and mutates.

## Mental Model

`cockpit.rs` is a persistent supervisor. It keeps ONE real attached mux client
per session (a `tmux attach` / `psmux attach` in a `portable-pty` PTY, via the
`AttachRegistry`) alive across selections and renders the SELECTED session's live
`Grid` on the right. A separate control-mode client per remote host
(`HostManager`) supplies the sidebar inventory, mux-side change events, and
programmatic `select-window`; local psmux is enumerated/polled with plain
commands. One `select!` loop interleaves stdin, host events, PTY events, the
control socket, terminal resize, and an animation tick. It folds domain
`Action`s through `State::apply` and inbound `HostEvent`s through
`State::apply_event`, dispatches the returned `Command`s/`EventEffect`s, keeps
`state::State` in sync with the switcher cursor, drives the debounced attach, and
draws the split view.

`focus.rs` tracks which pane holds focus (tree vs terminal) and which modal is
open, and exposes the transitions the cockpit and `State` fold through. It is UI
state, not display mechanics: it decides where input is routed, not how a PTY is
pumped or a grid is rendered.

## Module Seams

- `cockpit.rs` coordinates the runtime modules and owns the main event loop. It
  calls `driver_for(host).show(sel, ctx)` for display and reads back the grid via
  `driver.grid`; it branches on nothing mux-specific. `Selection` (the canonical
  `source`/`session`/`window`) lives here and is the value the display reads.
- `focus.rs` holds the focus/modal state (`Focus`, `PaneFocus`, `ModalKind`) and
  the focus-transition helpers. `state::State` embeds a `Focus`; the cockpit reads
  and mutates it through these types.
- The display mechanics (PTY/grid/input) live in `src/display`; host connection
  management lives in `src/host`; the domain vocabulary (`Action`, `Command`,
  `EventEffect`) lives in `src/model`; the durable runtime state bag lives in
  `src/state`.

## Invariants

- The cockpit does not decompose into components here: `run_cockpit` is the whole
  runtime; splitting it into a thin runtime plus components is out of scope.
- `Selection` is the canonical selected source/session/window value consumed by
  display selection and rendering.
- The per-mux display decision lives in the `MuxDriver` implementation, never in
  `cockpit.rs`.
- `focus` is the single source of truth for which pane owns keys and which modal
  (if any) is open; focus and modal transitions stay in `focus.rs` — the cockpit
  and `State` call into them rather than open-coding pane/modal bookkeeping.
- This layer carries no PTY, grid, or terminal-protocol logic (that is `display`).

## Common Pitfalls

- Do not block the cockpit loop on process spawn, PTY close, pipe reads, writes,
  or resize operations.
- Logging macros must never write to stdout or stderr: ratatui owns the terminal
  in alt-screen mode, and a stray byte corrupts the display. The panic hook
  restores the terminal before printing the panic message.
- Do not reintroduce display mechanics into `focus.rs`; PTY/grid/input belongs in
  `display`. Do not scatter pane-focus or modal-kind decisions across the cockpit;
  route them through `focus.rs`.

## Before Editing

- For cockpit changes, locate the event source and the state it owns before
  adding fields or channels.
- For focus changes, identify whether the change is a focus/modal state transition
  (here) or a display mechanic (`src/display`).

## Verification

- Run `app::cockpit` and `state` tests when changing selection sync, attach
  debounce, or focus/modal routing.
- Set `XMUX_LOG=xmux::mux=debug` to emit `display_show`, `tty_probe`, and
  `display_inventory` at debug verbosity; the log file is at `<xmux_dir>/xmux.log`.
