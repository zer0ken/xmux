# Working Notes: /src/model

## Purpose

`model` holds runtime domain values shared across mux, transport, host,
control, and app code: host state, host collections, the
`Action`/`Command`/`EventEffect` unidirectional-flow vocabulary, transport
lowering results, server models, plans, and death-signal helpers.

## Mental Model

The model layer carries facts and intent, not live process ownership. A `Host`
combines machine transport and mux mux state. `Action` is the single domain
intent vocabulary shared by key handling and ctl; `Command` is the matching
effect vocabulary the run loop dispatches. `State::apply(Action) -> Vec<Command>`
(in `crate::state`) is the one site that turns an `Action` into state changes +
`Command`s. `EventEffect` is the inbound mirror: `State::apply_event(HostEvent)
-> Vec<EventEffect>` (in `crate::state`) folds a mux event's self-contained
state mutation and returns the mux follow-ups (refetch / probe / reap / sync
/ scan-dispatch) the run loop runs against the host clients + registry.

## Module Seams

- `action.rs` defines the domain `Action` (intent) and `Command` (effect)
  enums, `FocusTarget`, `MuxOp` (the slow-mux-action descriptor `Command::RunOp`
  carries and `ui::ops::run_op` runs off-loop), and `EventEffect` (the mux
  follow-up `State::apply_event` returns for a `HostEvent`). The app's
  raw-byte input `Action` (`display::dispatch`) projects INTO this via `as_action`;
  the two are distinct types in separate modules. `EventEffect` is not
  `Clone`/`Eq` (its `DispatchScanned` carries a `Box<dyn Mux>`) and has a
  hand-written `Debug`.
- `transport.rs` is the MACHINE axis: it lowers a mux argv into executable argv for
  local or remote hosts and OWNS all local/ssh/socket/exec/pty wrapping. `exec_argv`
  lowers a non-interactive command; `interactive_attach_argv` lowers an attach into the
  terminal handover (local `-S` injection, or `ssh -t` running `[<select-window> ;] exec
  <attach>` — the `exec`/window-fold is here, never in the mux or caller); `control_argv`
  lowers a `-CC` child; `raw_ssh_argv` wraps a raw remote command (the driver's remote
  in-place switch). The mux argv comes from a `Mux::*_plan` method; the transport only
  decides HOW to run it.
- `host.rs` and `hosts.rs` store per-host domain state and collections. A `Host`
  carries no control client, no display-key derivation, and no attach/reap plan:
  the live control client is owned by `HostManager`, the live warm/reap by
  `MuxDriver::sync`, and the live display-key authority is
  `app::host_selection_key` (the host id for BOTH server models).
- `death.rs`, `plan.rs`, and `server_model.rs` provide value types used by
  app, mux, and host management. `server_model.rs` is just the
  `Shared`/`PerSession` discriminant the supervisor reads to shape the attach
  fan-out.

## Invariants

- `Action` variants represent user-visible domain intents, not key strokes;
  `Command` variants represent effects the run loop carries out; `EventEffect`
  variants represent the mux I/O an inbound `HostEvent` requires after its
  state mutation has been folded.
- Live control clients, polling tasks, and PTY attachments are owned outside
  `model`.
- Transport lowering should preserve mux intent without introducing mux
  policy.

## Common Pitfalls

- Do not put task lifecycle or process handles into domain model values.
- Do not add an `Action`/`Command` for behavior that is only a test hook; raw
  ctl input already covers low-level injection.
- Do not split host state between new registries without checking `Hosts`,
  `Host`, and `HostManager` ownership.

## Before Editing

- Confirm whether a new field is durable domain state or live runtime machinery.
- Check whether an existing plan/value type can express the behavior.
- Keep parsing aliases close to the value type they construct.

## Verification

- Run model tests for equality, parsing, lowering, and collection behavior.
- Run state, ctl, and app tests when changing `Action`, `Command`, or
  `FocusTarget` (`State::apply` and its debounce live in `crate::state`).
