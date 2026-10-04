# Working Notes: /src/model

## Purpose

`model` holds runtime domain values shared across the mux and transport axes,
connection management (`link`), provisioning, state, and app code: the source
definition and source state, source collections, inventory groups and typed failures,
view screen policy, login input values, nav geometry, the operation port and its
exchanged results, the action / command / event-effect unidirectional-flow set,
transport dispatch results, server models, plans, and death-signal helpers.

## Mental Model

The model layer carries facts and intent, not live process ownership. A source
combines host transport and mux state. An action is the domain intent set shared
by key handling and ctl; a command is the matching domain effect set. The
application update transition invokes the state action reducer, normalizes its
commands into runtime-facing effects, and owns every application-state change.
Event effects carry the I/O follow-ups (refetch, probe, reap, sync, scan dispatch,
source add) that remain after update folds an inbound source event into the model.

## Module Seams

- The action module defines the domain intent and effect sets, the focus
  target, the slow-operation descriptor a deferred command carries for the UI to
  run off-loop, and the event effect returned for a source event. The raw-byte
  input action from `src/display` projects INTO the domain action; the two are
  distinct types in separate modules. The event effect carries a boxed mux, so it
  is neither cloneable nor comparable and has a hand-written debug form.
- Inventory groups and their deterministic session ordering are domain values.
  Each group classifies its diagnostic as blocked or unreachable through the ssh
  diagnostic owned by the transport layer. Presentation filtering and row construction
  consume the typed result without owning the classification.
- View screen selection is pure domain policy over the selected source and address,
  typed failure, scanning state, empty state, own-session address, and confirmed
  display. Rendering consumes the selected screen without choosing it. A scan
  cannot replace a previously confirmed session grid.
- The operation port carries slow host operations and their plain results.
  Execution policy and user-facing completion messages stay in the UI layer.
- Login input values carry the remember choice and bounded secret. The secret
  keeps its allocation bounded, redacts debug output, and zeroes its storage.
- The HOST axis lives in `src/transport`, not here; a source holds one transport
  from it.
- The key table is the one list of every key xmux binds and the words that name it:
  its section, its keys, a help description, a full and a short key list description,
  and how readily the key list gives it up. It names what each prefix chord does
  independent of focus; each focus path turns that into its own input action.
- Source state and source collections store per-source domain state. A source
  carries no control client, no display-key derivation, and no attach or reap plan:
  the live control client belongs to the source manager, the live warm and reap to
  the driver, and the display-key authority to the driver capability port.
- The death signal, the plans, and the server model are value types used by app,
  mux, and connection management. The server model is just the shared-versus-
  per-session discriminant the supervisor reads to shape the attach fan-out.

## Invariants

- Action variants represent user-visible domain intents, not key strokes; command
  variants represent effects the application update transition normalizes;
  event-effect variants represent the remaining mux I/O an inbound source event
  requires after update has folded its model changes.
- Live control clients, polling tasks, and PTY attachments are owned outside
  `model`.
- Transport dispatch should preserve mux intent without introducing mux policy.

## Common Pitfalls

- Do not put task lifecycle or process handles into domain model values.
- Do not add an action or command for behavior that is only a low-level hook; the
  raw ctl namespace already covers low-level injection.
- Do not split source state between new registries without checking who already
  owns it: the source, the source collection, and the source manager.

## Before Editing

- Confirm whether a new field is durable domain state or live runtime machinery.
- Check whether an existing plan or value type can express the behavior.
- Keep parsing aliases close to the value type they construct.

## Verification

- Check equality, parsing, dispatch, and collection behavior for the value you
  touched.
- Re-check the state, ctl, and app surfaces when the intent or effect set changes:
  all three read it, and the application update transition owns their integration.
