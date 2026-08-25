# Working Notes: /src/model

## Purpose

`model` holds runtime domain values shared across mux, transport, source, control,
and app code: source state, source collections, the action / command / event-effect
unidirectional-flow vocabulary, transport lowering results, server models, plans,
and death-signal helpers.

## Mental Model

The model layer carries facts and intent, not live process ownership. A source
combines host transport and mux state. An action is the single domain intent
vocabulary shared by key handling and ctl; a command is the matching effect
vocabulary the run loop dispatches. Applying an action, in `src/state`, is the
one site that turns intent into state changes plus commands. An event effect is
the inbound mirror: applying a source event folds that event's self-contained state
mutation and returns the mux follow-ups (refetch, probe, reap, sync, scan
dispatch, source add) the run loop runs against the source clients and the
registry.

## Module Seams

- The action module defines the domain intent and effect vocabularies, the focus
  target, the slow-operation descriptor a deferred command carries for the UI to
  run off-loop, and the event effect returned for a source event. The raw-byte
  input action from `src/display` projects INTO the domain action; the two are
  distinct types in separate modules. The event effect carries a boxed mux, so it
  is neither cloneable nor comparable and has a hand-written debug form.
- The HOST axis lives in `src/transport`, not here; a source holds one transport
  from it.
- Source state and source collections store per-source domain state. A source
  carries no control client, no display-key derivation, and no attach or reap plan:
  the live control client belongs to the source manager, the live warm and reap to
  the driver, and the display-key authority to the app, which uses the source id
  for both server models.
- The death signal, the plans, and the server model are value types used by app,
  mux, and connection management. The server model is just the shared-versus-
  per-session discriminant the supervisor reads to shape the attach fan-out.

## Invariants

- Action variants represent user-visible domain intents, not key strokes; command
  variants represent effects the run loop carries out; event-effect variants
  represent the mux I/O an inbound source event requires after its state mutation
  has been folded.
- Live control clients, polling tasks, and PTY attachments are owned outside
  `model`.
- Transport lowering should preserve mux intent without introducing mux policy.

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

- Check equality, parsing, lowering, and collection behavior for the value you
  touched.
- Re-check the state, ctl, and app surfaces when the intent or effect vocabulary
  changes: all three read it, and the apply site is in `src/state`.
