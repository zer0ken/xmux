# Working Notes: /src/host

## Purpose

`host` owns host connection management: the control-mode reader/writer machinery,
poll task lifecycle, per-host session/window inventory, and the `HostEvent`s the
app folds into `State`. It is a METADATA channel only — the per-session PTY
attachments (in `src/display`) own the pixels.

## Mental Model

Each remote host gets ONE `-CC` control-mode client (`HostClient`), owned and
reaped by `HostManager`. A reader thread parses control-mode notifications into
`HostEvent`s (via `run_reader`); a writer thread turns queued `HostCmd`s into the
exact bytes to send (via `run_writer`). The reader holds no inventory of its own:
it parses each list-sessions / list-panes block and carries the result on a
`HostEvent` (`Connected`/`Inventory` carry sessions; `Panes` carries a subtree —
the same carriers the poll path uses). `PendingReply` correlates a control command
with its reply so the right `HostEvent` is emitted. The app folds those events
through `State::apply_event` into `model::Host.inventory` — the single owner of
per-host session/window inventory — and (re)builds the tree from it.

## Module Seams

- The module is split by role: `inventory.rs` (the shared vocabulary — inventory
  data plus the command/event/reply types the threads exchange), `reader.rs` (the
  `-CC` stdout line state machine → `HostEvent`s), `writer.rs` (drains `HostCmd`s to
  the child, one in-flight correlation per line), `client.rs` (`HostClient`: one
  control-mode child plus its reader/writer/stderr threads and command API),
  `poll.rs` (a POLL host's self-looping enumeration task for muxes with no control
  stream), and `manager.rs` (`HostManager`: owns each host's metadata channel and
  the composed `control_argv`).
- `HostManager::ensure` spawns the `-CC` control child with an argv composed
  across the two orthogonal axes — `Transport::control_argv(&Mux::control_argv())`
  (the mux supplies the control payload, the transport wraps it for local `-S` /
  `ssh -tt`). It never hardcodes a mux verb or hand-rolls ssh.
- `HostManager` owns the map of `HostClient`s plus `ensure`/reap and poll-task
  management; `HostClient` owns one host's reader/writer threads and channels.
- `HostEvent` is the outbound vocabulary consumed by `State::apply_event`; the
  app's `run_event_effect` runs the returned `EventEffect`s back against these
  clients, the registry, and the display worker.
- Depends on `crate::mux` for control-protocol parsing (`parse_panes`,
  `parse_sessions`, `ControlProtocol`, `Line`, `Notif`) and `crate::session` for
  `Session` / `WindowPanes`.

## Invariants

- This is a metadata path only: host events update inventory and selection aids,
  not display grids.
- `HostManager::ensure` is idempotent: re-ensuring a live host is a no-op.
- The control argv is composed from the transport and mux axes; no mux verb
  or ssh invocation is hardcoded here.

## Common Pitfalls

- Do not do display/PTY work here; that belongs to `src/display`.
- Do not block: the reader and writer run on their own threads and communicate
  with the app loop over channels.

## Before Editing

- Decide whether the change is metadata (here), display PTY (`src/display`), or
  transport lowering (`machine::Transport`).
- For a new event, add the `HostEvent` variant and its `State::apply_event` arm
  and `EventEffect` follow-up together.

## Verification

- Run `host` tests (manager ensure/reap idempotency) and the app/state tests
  that exercise `apply_event`.
