# Working Notes: /src/model

## Purpose

`model` holds runtime domain values shared across backend, transport, host,
control, and cockpit code: host state, host collections, the `Action`/`Command`
unidirectional-flow vocabulary, transport lowering results, server models,
plans, and death-signal helpers.

## Mental Model

The model layer carries facts and intent, not live process ownership. A `Host`
combines machine transport and mux backend state. `Action` is the single domain
intent vocabulary shared by key handling and ctl; `Command` is the matching
effect vocabulary the run loop dispatches. `State::apply(Action) -> Vec<Command>`
(in `crate::state`) is the one site that turns an `Action` into state changes +
`Command`s.

## Module Seams

- `action.rs` defines the domain `Action` (intent) and `Command` (effect)
  enums plus `FocusTarget`. The cockpit's raw-byte input `Action`
  (`proxy::dispatch`) projects INTO this via `as_action`; the two are distinct
  types in separate modules.
- `transport.rs` lowers backend intent into executable argv for local or remote
  hosts.
- `host.rs` and `hosts.rs` store per-host domain state and collections.
- `death.rs`, `plan.rs`, and `server_model.rs` provide value types used by
  cockpit, backend, and host management.

## Invariants

- `Action` variants represent user-visible domain intents, not key strokes;
  `Command` variants represent effects the run loop carries out.
- Live control clients, polling tasks, and PTY attachments are owned outside
  `model`.
- Transport lowering should preserve backend intent without introducing mux
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
- Run state, ctl, and cockpit tests when changing `Action`, `Command`, or
  `FocusTarget` (`State::apply` and its debounce live in `crate::state`).
