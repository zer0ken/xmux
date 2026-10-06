# Working Notes: /src/app/runtime

## Purpose

`runtime` is the app's event loop: one async select turns stdin, host, PTY, ctl,
resize, and tick input into update messages and executes the returned effects in order.

## Module Seams

- The runtime owns the I/O resources (receivers, timers, the terminal, the attach
  registry, the host connection manager) and one application model; every select
  arm and stateful helper is a method on it, so each takes a small argument list.
- Discovery's async half runs here (the per-machine reachability probe, then
  detection, metadata channels, and mux discovery), because only the loop holds the
  host registry and the manager that starts a new host's first scan. The off-loop
  operations read the hosts the registry publishes, so adding a host to the
  registry is the whole of adding it.

## Invariants

- The nav selection and xmux's own display client name the same session, held by a
  comparison made on every pass and never by a recorded switch; focus alone decides
  which of the two moves.
- Only the selection's own attach confirms the display; any other stays warm.
- A logout clears nothing of its machine until its key steps and the removal of the
  ssh config stanza its login recorded settle, and a login's follow-ups and a logout's
  key search take one per-machine gate in turn.
