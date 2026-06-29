# Working Notes: /src

## Purpose

`src/` contains the runtime application: CLI/config assembly, mux discovery,
the cockpit event loop, host metadata management, display attachment spawning,
the control socket, and the UI/proxy/model submodules.

## Mental Model

The cockpit is the coordinator. It receives stdin, control socket commands,
host metadata events, display worker events, PTY output events, resize events,
and ticks. It folds domain `Action`s in through `State::apply` and dispatches the
returned `Command`s, keeps `state::State` in sync with the switcher cursor (via
`Action::Select`), drives the debounced attach (via `Action::Tick`), and renders
the live split view.

## Module Seams

- `control.rs` owns ctl wire parsing, framing, endpoint naming, and the ctl
  client. Semantic ctl verbs parse to a domain `model::Action`; `raw:` verbs are
  low-level injection.
- `display.rs` owns the off-runtime worker that spawns PTY attachments and
  returns `DisplayEvent`s. It never owns the registry.
- `host.rs` owns control-mode reader/writer machinery, poll task management,
  host inventory, and `HostEvent`s. It is a metadata path only.
- `cockpit.rs` coordinates these modules and owns the main event loop.
- `source.rs` and `env.rs` are compatibility and config assembly plumbing for
  source definitions and command construction.

## Invariants

- `State::apply(Action) -> Vec<Command>` is the single domain-mutation site;
  `dispatch_action` runs its synchronous commands for key- and ctl-derived
  actions, and the loop-top `Tick` runs the settled-attach commands. The two
  surfaces (keys, ctl) can never diverge because both flow through `apply`.
- `Selection` is the canonical selected source/session/window value consumed by
  display selection and rendering.
- `DisplayWorker` spawns attachments and hands them back; `AttachRegistry`
  stores and tears them down.
- Host metadata events update inventory and selection aids, not display grids.

## Common Pitfalls

- Do not make ctl public verbs depend on internal key names.
- Do not block the cockpit loop on process spawn, PTY close, pipe reads, writes,
  or resize operations.
- Do not treat `Source` as the preferred place for new execution semantics;
  prefer `model::Transport` for machine execution and `backend::Backend` for
  mux behavior.

## Before Editing

- For ctl changes, add a `model::Action` variant (and its `Command`/`apply` arm)
  only when the behavior is a real domain action.
- For cockpit changes, locate the event source and the state it owns before
  adding fields or channels.
- For host/display changes, decide whether the behavior belongs to metadata,
  display PTY, or transport lowering.

## Verification

- Run module tests for `control`, `host`, `display`, `cockpit`, and any touched
  submodule.
- Exercise ctl parser tests when adding or renaming control verbs.
- Check redraw and blocking behavior when moving work into the cockpit loop.
