# Working Notes: /src/mux

## Purpose

`mux` is the mux family home. It defines mux-specific behavior behind the `Backend`
trait AND holds the pure shared vocabulary every backend argv is built from. A
backend knows the mux binary, server model, enumeration behavior, attach command
shape, control-channel availability, event source, death signal, and window/session
operation plans.

`mod.rs` holds the cross-mux surface: the `Backend` trait, identity detection
(`detect_backend`), the factory functions (`for_binary`, `for_kind`), and — via
`control.rs` — the `ControlProtocol` trait that hides a mux's control-mode (`-CC`)
wire details (line framing/classification, the notification→event table, the size
formatter) from `host.rs`. `vocab.rs` is the pure shared vocabulary (the
`SESSION_FORMAT`/`PANE_FORMAT` templates, the argv builders, the row parsers, and
the address utilities); `mod.rs` re-exports it (`pub use vocab::*;`) so
`crate::mux::<fn>` names a vocab builder and the `Backend` factory alike. Each
concrete mux lives in its own sub-directory (owning BOTH its metadata backend AND
its display driver) and is re-exported from `mod.rs`:

- `tmux/mod.rs` — `Tmux` and its `Backend` impl, plus the display-tty file helpers
  and `mux_control_argv`; `tmux/display.rs` — `TmuxDriver` (`MuxDriver` impl) and its
  attach helper; `tmux/control_proto.rs` holds its pure, headlessly-testable `-CC`
  wire functions behind `ControlProtocol`. See `tmux/AGENTS.md`.
- `psmux/mod.rs` — `Psmux` and its `Backend` impl, plus its poll cadence constant
  (`PSMUX_POLL_MS`) and `switch_client_argv`; `psmux/display.rs` — `PsmuxDriver`
  (`MuxDriver` impl) and its tty-capture/refresh helpers; `psmux/registry.rs` is the
  `~/.psmux` per-machine session registry that backs psmux `enumerate` (one server per
  session, no aggregate `list-sessions`). See `psmux/AGENTS.md`.

Sub-modules pull the shared trait, value types, and imports from the parent via
`use super::*;`. `crate::mux::{Tmux, Psmux}` resolve through the re-exports; a
mux's driver is constructed via `Backend::driver()`, so no caller names the concrete
`TmuxDriver`/`PsmuxDriver` type.

## Mental Model

Backends describe mux vocabulary and classification. `Transport` lowers machine
execution. The `MuxDriver` trait (`src/driver.rs`) is the mux-agnostic display seam;
each mux's concrete driver lives in its own family directory and is constructed by
`Backend::driver()`, so a backend owns BOTH its argv/server-model/enumeration AND its
display orchestration. Shared muxes such as tmux use one aggregate server and a
host-level control stream. Per-session muxes such as psmux enumerate differently and
supply a per-session attach plan.

## Module Seams

- `Backend::enumerate` may use `Transport` because enumeration executes on a
  host.
- Plan methods return mux argv or mux intent; they do not decide local versus
  ssh execution. The plan set spans window operations AND session lifecycle
  (`new_session_plan`, `kill_session_plan`, `rename_session_plan`), so `manage`
  builds every mux argv from a `Backend` and lowers it via `Transport`, never off
  a bare binary name.
- Generic `mux::*` command builders (from `vocab.rs`) are called ONLY inside the
  per-mux dirs (`tmux/**`, `psmux/**`) and the shared enumeration helper in `mod.rs`
  (each `*_plan` wraps one); the pure address vocabulary (`mux::window_target`,
  `parse_panes`, `quote_target`) is callable anywhere.
- `ServerModel`, `EventSource`, and `DeathSignal` are the classification values
  callers use instead of branching on backend names. `Backend::driver()` constructs
  the host's `MuxDriver` (each backend builds its OWN — mux selection lives in the mux
  family, never a central `match server_model()`); the thin wrapper `driver_for(host)`
  in `src/driver.rs` is just `host.mux.driver()`. `TmuxDriver` = one PTY per host with
  an in-place `switch-client`; `PsmuxDriver` = in-place client switch or reattach per
  session.

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
- Do not duplicate psmux registry behavior outside the mux/source boundary
  without deciding which module owns it.

## Before Editing

- Identify whether the new behavior is mux semantics, machine transport, or UI
  policy.
- Check both tmux and psmux behavior when changing trait methods.
- Keep trait additions tied to an end-to-end caller.

## Verification

- Run mux and model tests for plan/lowering changes.
- Run host or cockpit tests when event source, death signal, or selection outcome
  changes.
