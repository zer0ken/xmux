# Working Notes: /src/model

## Purpose

`model` holds the runtime domain values that the axes, `link`, `provision`, `state`,
and `app` share. It carries facts and intent, never live process ownership: a host
combines one transport with one mux, an action is the domain intent that key handling
and ctl share, a command is the matching effect, and an event effect is the I/O that
remains after the update transition folds an inbound source event into the model.

## Module Seams

- The action module holds the action, command, and event-effect sets, the focus
  target, and the slow-operation descriptor a deferred command carries for the UI to
  run off-loop.
- The source registry holds every source's host with its domain state and display
  bookkeeping, and publishes the source definitions the CLI, the scan, and the off-loop
  operations read; the PTYs themselves stay in `display/`. It also names every host on
  the roster, including one whose muxes are not known yet and so has no source, and a
  reconcile reports the hosts it added and dropped apart from the sources.
- Inventory groups, their deterministic session order, and the typed failure of a
  source are domain values.
- View screen selection is pure policy over the selection, the typed failure, the
  scanning and empty states, the own session, and the confirmed display.
- The operation port carries slow host operations and their plain results; execution
  policy and completion messages stay in `ui/`.
- Login values carry the after-login choice, the login progress, and a bounded secret.
- The key table is the one list of every key xmux binds and the words that name it,
  per section, independent of focus.
- Nav geometry, the selection, the server model, the plans, and the death-signal
  helpers are value types read by `app`, `mux`, and `link`.

## Invariants

- Action variants are user-visible domain intents, not key strokes; command variants
  are effects the update transition normalizes; event-effect variants are the mux I/O
  an inbound source event still requires after update folded its model changes.
- Live control clients, polling tasks, PTY attachments, and task handles are owned
  outside `model`.
- Transport dispatch preserves mux intent without introducing mux policy.
- An action or command exists only for a real domain intent; low-level injection is
  the `raw:` ctl namespace.
- A source exists in the source registry or nowhere. Its definition is derived from the
  registry's host and republished on every change to the sources, never assembled
  beside the registry, so no consumer can hold a source another lacks.

## Common Pitfalls

- Source state already has owners: the host, the source registry, and the source
  manager in `link/`. Check them before adding another registry.

## Before Editing

- Decide whether a new field is durable domain state or live runtime machinery.
- Check whether an existing plan or value type already expresses the behavior, and
  keep parsing aliases next to the value type they construct.

## Verification

- When the intent or effect set changes, re-check `state/`, the ctl surface, and
  `app/`: all three read it, and the update transition owns their integration.
