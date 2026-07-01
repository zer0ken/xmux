# Context

## Glossary

### Working Notes

A directory-local guide for people and agents that explains why the code exists,
how to reason about its module seams, what invariants must hold, and what to
verify before and after editing. Working Notes are stored in `AGENTS.md` files
and titled as `Working Notes: <path>`.

### Module Seam

The place where a module's interface lives: what callers may rely on, what the
module hides, and which dependencies are allowed to cross into it.

## Working Notes Format

Working Notes use these sections:

- `Purpose`
- `Mental Model`
- `Module Seams`
- `Invariants`
- `Common Pitfalls`
- `Before Editing`
- `Verification`

Working Notes describe the current codebase state. Active refactoring direction
is expressed as invariants, module seams, and pitfalls rather than as change
history or phase narrative.

Repository documentation is written in English when it is committed to the
project. Temporary files outside the repository may use another language.
The `docs/superpowers/` tree is not part of the public documentation surface and
must be excluded before release.

## Improvement Notes

- `src/ui/switcher.rs` is the current aggregate UI module. It owns row
  flattening, cursor state, modal/menu/input state, key and mouse handling, op
  result application, and rendering. This is unfinished rearchitecture work. The
  intended shape keeps pure tree transforms in `ui/tree.rs`, slow side-effecting
  operations in `ui/ops.rs`, and moves smaller UI surfaces behind clearer module
  seams as the TUI is decomposed.
- Per-host metadata ownership is not fully settled. `model::Host` owns domain
  state for a host and also contains a `control` slot plus
  `ensure_control_client`, while `host::HostManager` currently owns live control
  clients and poll tasks. Before changing either area, confirm which module is
  meant to own live metadata mechanisms and avoid adding a third per-host
  registry.
- The migration boundary between `Env`/`Source` and `Hosts`/`Host` is not fully
  specified. `Hosts` is intended to become the per-machine owner, but `Env` and
  `Source` still drive CLI commands, discovery, `Ops`, and cockpit source lookup.
  Before moving source or host logic, decide whether `Env` remains a CLI/config
  assembly layer or whether `Hosts` also becomes the runtime source registry.
- Preferred direction: keep `Env` as the config and CLI assembly layer, make
  `Hosts` the runtime source registry, keep live process and task ownership out
  of `model::Host`, and let `host::HostManager` own metadata mechanisms until it
  is renamed or reshaped as a runtime manager. `Source` should shrink toward a
  compatibility adapter as `Backend + Transport` cover its command-building and
  execution roles.
- `Source` and `model::Transport` currently duplicate machine-execution
  responsibilities. Treat `Source` as live compatibility plumbing, not the
  preferred home for new execution semantics. New local/ssh execution behavior
  belongs in `Transport`; new mux behavior belongs in `Backend`. A future
  source-compatibility-shrink phase should move psmux registry helpers into
  `backend/psmux`, port `manage` and `EnvOps` toward `Host + Backend +
  Transport`, and then remove or minimize `Source`.
- `docs/superpowers/` contains working planning material and is not intended for
  the public open source documentation surface. Before release, remove it from
  the published repository state or replace any still-useful content with
  current English documentation elsewhere.
- The control socket has a useful module seam: public ctl verbs parse to
  `model::Action`, while raw key and text injection stays behind the
  unstable `raw:` namespace. Working Notes should tell agents to add
  user-facing automation through semantic actions first, and reserve raw
  input for tests or low-level compatibility.
- Some Rust module comments still contain planning-only language.
  Durable comments and docs should describe current behavior and invariants
  only, with refactoring direction kept in Working Notes or improvement notes.
