# Working Notes: /src/backend

## Purpose

`backend` defines mux-specific behavior behind the `Backend` trait. A backend
knows the mux binary, server model, enumeration behavior, attach command shape,
control-channel availability, event source, death signal, and window/session
operation plans.

One mux is one directory. `mod.rs` holds the cross-mux surface: the `Backend`
trait, `SelectOutcome`, identity detection (`detect_backend`), and the factory
functions (`for_binary`, `for_kind`). Each concrete mux lives in its own
sub-directory and is re-exported from `mod.rs`:

- `tmux/mod.rs` — `Tmux` and its `Backend` impl, plus the helpers used only by it
  (`benign_empty`, `mux_control_argv`).
- `psmux/mod.rs` — `Psmux` and its `Backend` impl, plus its poll cadence constant
  (`PSMUX_POLL_MS`).

Sub-modules pull the shared trait, value types, and imports from the parent via
`use super::*;`. `crate::backend::{Tmux, Psmux}` resolve through the re-exports.

## Mental Model

Backends describe mux vocabulary and classification. `Transport` lowers machine
execution. The `MuxDriver` trait (`src/driver.rs`) owns per-host display
orchestration and the concrete attach decision; backends supply the argv, server
model, and enumeration behavior that drivers consume. Shared muxes such as tmux
use one aggregate server and a host-level control stream. Per-session muxes such
as psmux enumerate differently and supply a per-session attach plan.

## Module Seams

- `Backend::enumerate` may use `Transport` because enumeration executes on a
  host.
- Plan methods return mux argv or mux intent; they do not decide local versus
  ssh execution.
- `SelectOutcome`, `ServerModel`, `EventSource`, and `DeathSignal` are the
  classification values callers use instead of branching on backend names.
  `SelectOutcome` classifies the attach pattern a backend implies; the `MuxDriver`
  in `src/driver.rs` acts on that classification (one PTY per host for
  `SharedSwitch`; in-place client switch or reattach for `PerSessionReattach`).

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
