# Working Notes: /src/model

## Purpose

`model` holds runtime domain values shared across backend, transport, host,
control, and cockpit code: host state, host collections, operations, transport
lowering results, server models, plans, and death-signal helpers.

## Mental Model

The model layer carries facts and intent, not live process ownership. A `Host`
combines machine transport and mux backend state. `Operation` is the semantic
command language shared by key handling and ctl.

## Module Seams

- `operation.rs` defines the domain actions exposed to automation and UI input.
- `transport.rs` lowers backend intent into executable argv for local or remote
  hosts.
- `host.rs` and `hosts.rs` store per-host domain state and collections.
- `death.rs`, `plan.rs`, and `server_model.rs` provide value types used by
  cockpit, backend, and host management.

## Invariants

- `Operation` variants should represent user-visible domain actions, not key
  strokes.
- Live control clients, polling tasks, and PTY attachments are owned outside
  `model`.
- Transport lowering should preserve backend intent without introducing mux
  policy.

## Common Pitfalls

- Do not put task lifecycle or process handles into domain model values.
- Do not add an `Operation` for behavior that is only a test hook; raw ctl input
  already covers low-level injection.
- Do not split host state between new registries without checking `Hosts`,
  `Host`, and `HostManager` ownership.

## Before Editing

- Confirm whether a new field is durable domain state or live runtime machinery.
- Check whether an existing plan/value type can express the behavior.
- Keep parsing aliases close to the value type they construct.

## Verification

- Run model tests for equality, parsing, lowering, and collection behavior.
- Run ctl and cockpit tests when changing `Operation` or `FocusTarget`.
