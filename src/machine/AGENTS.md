# Working Notes: /src/machine

## Purpose

`machine/` is the MACHINE axis: how a mux argv reaches the server it runs on,
SEPARATE from which mux runs there (that is `crate::mux`). It owns argv assembly
and the ssh wrapping only — never a server model, never a mux verb.

## Mental Model

A machine family is a `Transport` implementation. `Local` runs a command on this
machine (injecting `-S <socket>` for a non-default mux server); `Ssh` wraps the
command in an ssh connection with the right tty / BatchMode / ControlMaster
options. A host holds one as `Box<dyn Transport>` and never branches on which
family it is — it calls trait methods. This mirrors the MUX axis: `Transport` is
to `machine/` what `Mux` is to `mux/`.

## Module Seams

- `mod.rs` holds the `Transport` trait, the `MachineKind` enum + its `transport()`
  method (the single construction-time match that maps a kind to a concrete transport),
  the `machine::local()` / `machine::ssh()` factory functions `transport()` delegates to,
  the `LoweredSwitch` execution-shape enum, the `Clone for Box<dyn Transport>`
  impl (via `clone_box`), and the blanket `impl Transport for Box<dyn Transport>`
  (so a stored box passes where `&dyn Transport` is expected).
- `local.rs` = `Local` (the local machine). Issues no remote shell command, so it
  uses none of `vocab`.
- `ssh.rs` = `Ssh` (a remote over ssh). Owns the private `ssh_opts` (tty/batch/
  ControlMaster) and is the sole consumer of `vocab::remote_command`.
- `vocab.rs` = the shared shell vocabulary (`quote`, `remote_command`) that renders
  an argv injection-safe for the POSIX shell ssh hands its command to. Peer of
  `mux/vocab.rs`.

The dependency is one-way: `ssh.rs` imports `vocab`; nothing in `machine/`
imports a mux type or a `Source`.

## Invariants

- `Transport` names no mux and no server model. `is_remote()` is a semantic ssh-vs-local marker exercised by tests, read on no production path;
  the capability predicates `runs_through_shell()` (a display attach runs through a host
  shell — the tty-record gate) and `local_registry_scope()` (this box's mux registry is
  authoritative — the registry-merge / local `list-clients` gate) express what the mux
  sites need. None of the three derives from another, and no code reads them to pick a
  server model (that is `ServerModel`).
- Machine selection is a single construction-time match — `MachineKind::transport()` maps
  a kind to a concrete transport (via the `machine::local` / `machine::ssh` factories),
  never a match scattered across call sites. The trait object then carries the choice.
- `exec_argv` lowers a non-interactive command; `interactive_attach_argv` lowers an
  attach into the terminal handover (local `-S` injection, or `ssh -t` running
  `[<select-window> ;] exec <attach>` — the `exec`/window-fold lives here, never in
  the mux or the caller); `control_argv` lowers a `-CC` child; `raw_ssh_argv` wraps
  a raw remote command (default `None`; only `Ssh` returns `Some`).
- The mux argv always comes from a `Mux::*_plan` method; a `Transport` only decides
  HOW to run it, never WHAT.
- Every untrusted argv element crossing into a remote shell passes through
  `vocab::quote` — the single injection-safe boundary.

## Common Pitfalls

- Do not add mux-kind knowledge here. If a decision needs the mux, it belongs in
  `mux/` or the caller, not the transport.
- `&Box<dyn Transport>` does not coerce to `&dyn Transport` on its own; the blanket
  impl in `mod.rs` is what lets `&host.transport` be passed to a `&dyn Transport`
  parameter. If you remove it, every such call site needs an explicit `&*`.
- `remote_command`/`quote` assume a POSIX remote shell. A cmd.exe remote is NOT a
  supported target (see the `remote_command` doc). Do not weaken the quoting to
  accommodate one without an explicit per-host shell feature.

## Before Editing

- Adding a machine family (e.g. wsl): add `src/machine/<kind>.rs` with a struct
  implementing `Transport` — override the capability predicates for its combination
  (WSL is `runs_through_shell() = true`, `local_registry_scope() = false`) rather than
  deriving from `is_remote` — add a `machine::<kind>()` factory, and add a
  `MachineKind::<Kind>` variant plus one arm in `MachineKind::transport()` (the single
  selection site). No other `match`/`if` on kind changes.
- Adding per-machine execution behavior to an existing family: edit `local.rs` /
  `ssh.rs`; keep the shared shell vocab in `vocab.rs`.

## Verification

- Run the module tests (`cargo test`): `machine::local`, `machine::ssh`, and
  `machine::vocab` each carry unit tests pinning the exact argv each method emits.
- When touching quoting, exercise `vocab::quote` against shell metacharacters and
  confirm `remote_command` joins quoted (the injection-safety tests).
