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

### Vocabulary

One concept, one word. The two axes and the runtime:

- `Transport` (MACHINE axis) — local vs ssh execution; knows nothing about the
  mux.
- `Mux` (MUX axis) — the per-mux behavior trait (impls `Tmux` / `Psmux`); a
  host's `mux` field is `Box<dyn Mux>`. "mux" is the family/concept; `Mux` is
  the trait.
- `MuxDriver` — a mux's display driver, built by `Mux::driver()`.
- the app — the runtime that owns the terminal (`crate::app`; loop in
  `app/runtime.rs`, entry `run_app`). There is no `App`/`Cockpit` struct yet:
  the app is the module and its run function until the runtime is decomposed.
- `ViewFocus` — which screen region holds focus (`Tree` / `Terminal`).
- `Modal` — the mutually-exclusive focus-grabbing UI (`Help`, an input dialog,
  a kill confirm, a context `Menu`). `ModalKind::{Popup, Menu}` are its two
  focus sub-kinds: popup = a draggable centered dialog, menu = the context menu.

UI elements a user perceives as distinct things:

- split view — the whole two-region layout.
- tree view — the left region (the host / session / window / pane tree).
- terminal view — the right region (the selected session's live grid).
- divider — the vertical rule between the views; carries the focus, auto-hide,
  and hover cues.
- status surface — the `Status`-owned chrome (divider + hint bar + host info).
- hint bar — the bottom line of the tree column (key hints, flash, scanning,
  filter text). Scoped to the tree column, so not a full-width status bar.
- grid — the live terminal content drawn in the terminal view.
- host info — the unreachable-host detail shown in the terminal-view region.
- row — one tree line (host / session / window / pane / loading row).
- hint — a row's trailing state annotation (`scanning…` / `(empty)` /
  `⚠ unreachable`).
- spinner — the braille activity glyph on a connecting session row.
- filter — the type-to-filter input over the tree.

`pane` is reserved for a mux window's terminal split (a tmux / psmux pane); it
is never a screen region — screen regions are "views". The switcher's rendered
screen is the "switcher screen" (`dump_screen`), never an "overlay".

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

## Architecture — the orthogonal design

Two orthogonal axes describe every connection, and no module conflates them:

- MACHINE — `src/model/transport.rs`. `Transport` (`Local` / `Ssh`) owns where a
  command runs and how its argv is executed; it knows nothing about the mux.
- MUX — `src/mux/<kind>/`. Each mux family (`tmux/`, `psmux/`) owns its metadata
  and command plans in `mod.rs` (behind the `Mux` trait) and its display
  driver in `display.rs`. A mux builds its OWN driver via `Mux::driver()`,
  so mux selection lives in the mux family, never a central `match`. Shared mux
  vocabulary lives in `src/mux/vocab.rs`.

Attach argv is composed from a host's own `mux` + `transport` (the two axes
together), so the two families are combined without either knowing the other.

The supervisor branches on NOTHING mux-specific. `src/app/` (runtime +
focus state), `src/ui/` (switcher/tree/status/ops rendering), and `src/state/`
(runtime `State` + the `apply` / `apply_event` mutation sites) select display
through `driver_for(host).show(...)` — i.e. `host.mux.driver()` — and read the
grid back via `MuxDriver::grid`; per-mux behavior lives behind that seam. These
layers carry no PTY, grid, or terminal-protocol logic.

The remaining layers each own one concern:

- `src/display/` — the mux- and app-agnostic PTY/grid/input mechanics (attach
  spawning, the `Grid`, input decode, `term`, `dispatch`, the registry, worker).
- `src/host/` — host connection management (control-mode reader/writer, poll
  tasks, live client ownership).
- `src/model/` — domain types (`Host`, `Hosts`, `Transport`, `Action`,
  `Command`, `EventEffect`, server model).
- `src/driver.rs` — the mux-agnostic `MuxDriver` trait + `DriverCtx` (the
  supervisor capabilities a driver borrows) + the thin `driver_for` wrapper. It
  names no concrete mux type.

## Adding a module

At creation time, place a new source file by the axis it belongs to:

- Machine-specific (a new transport / execution behavior) → `src/model/transport.rs`.
- Mux-specific (a new mux family or per-mux behavior) → `src/mux/<kind>/`.
- PTY / grid / terminal-protocol mechanics → `src/display/`.
- Orchestration (runtime loop, focus) → `src/app/`.
- Host connection management → `src/host/`.
- Domain types → `src/model/`.
- Switcher / tree / status UI → `src/ui/`.
- Runtime `State` → `src/state/`.

Then, if the module introduces a new directory, create that directory's
`AGENTS.md` using the Working Notes Format above (all seven sections). Follow the
AS-IS rule: describe the current state only, with refactoring direction expressed
as invariants, seams, and pitfalls — never as change history or phase narrative.

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
  `Source` still drive CLI commands, discovery, `Ops`, and app source lookup.
  Before moving source or host logic, decide whether `Env` remains a CLI/config
  assembly layer or whether `Hosts` also becomes the runtime source registry.
- Preferred direction: keep `Env` as the config and CLI assembly layer, make
  `Hosts` the runtime source registry, keep live process and task ownership out
  of `model::Host`, and let `host::HostManager` own metadata mechanisms until it
  is renamed or reshaped as a runtime manager. `Source` should shrink toward a
  compatibility adapter as `Mux + Transport` cover its command-building and
  execution roles.
- `Source` and `model::Transport` currently duplicate machine-execution
  responsibilities. Treat `Source` as live compatibility plumbing, not the
  preferred home for new execution semantics. New local/ssh execution behavior
  belongs in `Transport`; new mux behavior belongs in `Mux`. A future
  source-compatibility-shrink phase should move psmux registry helpers into
  `mux/psmux`, port `manage` and `EnvOps` toward `Host + Mux +
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
