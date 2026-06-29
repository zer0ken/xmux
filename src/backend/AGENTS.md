# Working Notes: /src/backend

## Purpose

`backend` defines mux-specific behavior behind the `Backend` trait. A backend
knows the mux binary, server model, enumeration behavior, attach command shape,
control-channel availability, event source, death signal, and window/session
operation plans.

## Mental Model

Backends describe mux intent. `Transport` lowers machine execution. Shared muxes
such as tmux use one aggregate server and a host-level control stream. Per-session
muxes such as psmux enumerate differently and keep selection as a per-session
reattach concern.

## Module Seams

- `Backend::enumerate` may use `Transport` because enumeration executes on a
  host.
- Plan methods return mux argv or mux intent; they do not decide local versus
  ssh execution.
- `SelectOutcome`, `ServerModel`, `EventSource`, and `DeathSignal` are the
  values callers use instead of branching on backend names.

## Invariants

- A reachable empty mux enumerates as `Ok(vec![])`; unreachable hosts return an
  error.
- Transport-specific command wrapping belongs in `model::Transport`.
- Backend methods should stay at the exact behavior surface used by cockpit,
  host metadata, and manage code.

## Common Pitfalls

- Do not add a broad capability catalog when only one caller needs a concrete
  plan.
- Do not thread `remote` booleans through backend methods.
- Do not duplicate psmux registry behavior outside the backend/source boundary
  without deciding which module owns it.

## Before Editing

- Identify whether the new behavior is mux semantics, machine transport, or UI
  policy.
- Check both tmux and psmux behavior when changing trait methods.
- Keep trait additions tied to an end-to-end caller.

## Verification

- Run backend and model tests for plan/lowering changes.
- Run host or cockpit tests when event source, death signal, or selection outcome
  changes.
