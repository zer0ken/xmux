# Working Notes: /src/app

## Purpose

`app` is the application orchestration layer: the app runtime that owns the
terminal for the whole session (`runtime.rs`) plus the application UI-state
machine it folds through (`focus.rs`). The app is the coordinator; `focus`
is the focus/modal routing state it reads and mutates.

## Mental Model

`runtime.rs` is a persistent supervisor. It keeps ONE real attached mux client
per session (a `tmux attach` / `psmux attach` in a `portable-pty` PTY, via the
`AttachRegistry`) alive across selections and renders the SELECTED session's live
`Grid` on the right. A separate control-mode client per remote host
(`HostManager`) supplies the tree view inventory, mux-side change events, and
programmatic `select-window`; local psmux is enumerated/polled with plain
commands. One `select!` loop interleaves stdin, host events, PTY events, the
control socket, terminal resize, and an animation tick. It folds domain
`Action`s through `State::apply` and inbound `HostEvent`s through
`State::apply_event`, dispatches the returned `Command`s/`EventEffect`s, keeps
`state::State` in sync with the switcher selection, drives the debounced attach, and
draws the split view.

`focus.rs` tracks which view holds focus (tree vs terminal) and which modal is
open, and exposes the transitions the app and `State` fold through. It is UI
state, not display mechanics: it decides where input is routed, not how a PTY is
pumped or a grid is rendered.

## Module Seams

- `runtime.rs` owns the main event loop as a `Runtime` struct: `run_app` builds it,
  keeps the `select!` receivers/timers and the ratatui terminal as loop-locals, and
  drives a `select!` where each arm is one `&mut self` method. It calls
  `driver_for(host).show(sel, ctx)` for display and reads back the grid via
  `driver.grid`; it branches on nothing mux-specific. The canonical `Selection`
  (`source`/`session`/`window`) it reads lives in `src/model`.
- `input.rs` holds the pure, stateless input-routing core (`resolve_tree_key`,
  `resolve_mouse_chain`, the predicates, `MouseState`/`StdinOutcome`); the stateful
  handlers are `Runtime` methods that call into it.
- `focus.rs` holds the focus/modal state (`Focus`, `ViewFocus`, `ModalKind`) and
  the focus-transition helpers. `state::State` embeds a `Focus`; the app reads
  and mutates it through these types.
- The display mechanics (PTY/grid/input) live in `src/display`; host connection
  management lives in `src/host`; the domain vocabulary (`Action`, `Command`,
  `EventEffect`) lives in `src/model`; the durable runtime state bag lives in
  `src/state`.

## Invariants

- `run_app` is a thin entry point: the `Runtime` struct owns the loop's world state,
  and every `select!` arm plus every stateful helper is a `&mut self` method, so each
  takes a small argument list rather than a large loose-parameter bundle.
- The app loop is not a second State-writer: the display truth (`displayed`), the
  attach debounce (`attach_deadline`), and `focus` all change only inside
  `State::apply`, routed there as an `Action` (`ConfirmDisplay`/`ClearDisplay`,
  `RearmAttach`/`RearmAttachNow`/`Tick`, `Focus`/`FocusToggle`). The loop makes the
  decision — a live grid exists, a deadline elapsed, a click landed — and folds the
  result through `apply`, so domain mutation stays at one site.
- `Selection` (defined in `src/model`) is the canonical selected
  source/session/window value consumed by display selection and rendering.
- The per-mux display decision lives in the `MuxDriver` implementation, never in
  `runtime.rs`.
- `focus` is the single source of truth for which view owns keys and which modal
  (if any) is open; focus and modal transitions stay in `focus.rs` — the app
  and `State` call into them rather than open-coding view/modal bookkeeping.
- This layer carries no PTY, grid, or terminal-protocol logic (that is `display`).

## Common Pitfalls

- Do not block the app loop on process spawn, PTY close, pipe reads, writes,
  or resize operations.
- Logging macros must never write to stdout or stderr: ratatui owns the terminal
  in alt-screen mode, and a stray byte corrupts the display. The panic hook
  restores the terminal before printing the panic message.
- Do not reintroduce display mechanics into `focus.rs`; PTY/grid/input belongs in
  `display`. Do not scatter view-focus or modal-kind decisions across the app;
  route them through `focus.rs`.

## Before Editing

- For app changes, locate the event source and the state it owns before
  adding fields or channels.
- For focus changes, identify whether the change is a focus/modal state transition
  (here) or a display mechanic (`src/display`).

## Verification

- Run `app::app` and `state` tests when changing selection sync, attach
  debounce, or focus/modal routing.
- Set `XMUX_LOG=xmux::mux=debug` to emit `display_show`, `tty_probe`, and
  `display_inventory` at debug verbosity; the log file is at `<xmux_dir>/xmux.log`.
