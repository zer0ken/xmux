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

- `Transport` (MACHINE axis) — the per-machine execution trait (impls `Local` /
  `Ssh`); a host's `transport` field is `Box<dyn Transport>`. It owns where a
  command runs and how its argv is executed, and knows nothing about the mux.
  "machine" is the family/concept; `Transport` is the trait.
- `Mux` (MUX axis) — the per-mux behavior trait (impls `Tmux` / `Psmux`); a
  host's `mux` field is `Box<dyn Mux>`. "mux" is the family/concept; `Mux` is
  the trait.
- `MuxDriver` — a mux's display driver, built by `Mux::driver()`.
- the app — the runtime that owns the terminal (`crate::app`; loop in
  `app/runtime.rs`, entry `run_app`). There is no `App`/`Cockpit` struct yet:
  the app is the module and its run function until the runtime is decomposed.
- `ViewFocus` — which screen region holds focus (`Nav` / `Terminal`).
- `Modal` — the mutually-exclusive focus-grabbing UI (`Help`, an input dialog,
  a kill confirm, a context `Menu`). `ModalKind::{Popup, Menu}` are its two
  focus sub-kinds: popup = a draggable centered dialog, menu = the context menu.

UI elements a user perceives as distinct things:

- split view — the whole two-region layout.
- nav view — the region holding the flat, MRU-ordered list of window cards (a
  left column in `Side`, a top band in portrait `Top`). Never the "sidebar", and
  never the "tree": the on-screen VIEW is the nav view; `tree` names only the
  internal row-model module (`ui::tree`), which is still a Host→Session→Window
  structure.
- terminal view — the right region (the selected session's live grid).
- view border — the vertical line between the two views. Modelled on tmux's pane
  border, but it borders views (not panes), so it is a `view border`, never a
  "pane border" or a bare "divider". Its color config keys are `view-border-style`
  / `view-active-border-style` / `view-border-hover-style`. These keys are
  OVERRIDES: unset (empty), each colour comes from the displayed host's live mux
  `pane-*-border-style` (queried per displayed host), falling back to the stock
  default (`green` / terminal-default / `yellow`). A non-empty key wins over both.
- active view border — the view border half painted the active color to mark which
  view holds focus (tmux `pane-active-border-style`; the top half is the active
  color for nav focus, the bottom half for terminal focus).
- view border lines — the view border's line-drawing style (tmux
  `pane-border-lines`): `single │` (default), `double ║` (auto-hide-nav on),
  `heavy ┃` (hover — the drag-resize grab cue).
- chrome — the furniture around the two views, owned by the `Chrome` type: the
  view border, the hint bar, and the host info.
- hint bar — the bottom line spanning the full terminal width (key hints, flash,
  the scan indicator, filter text), not just the nav column, so a long flash wraps
  across it instead of clipping. A shown flash paints it in the error style.
- host info — the unreachable-host detail shown in the terminal-view region.
- landing — the empty-state panel shown in the terminal-view region for a selected
  reachable host that has no sessions yet (its name + the keys to start one).
- grid — the live terminal content drawn in the terminal view: xmux's in-memory
  cell mirror of the attached session's screen, fed by the vt100 parser.
- cursor — the real terminal cursor placed over the grid at the mux's cursor cell
  while the terminal view is focused. "cursor" always means this text cursor,
  never the nav selection.
- card — one nav entry, two screen rows: line1 `{host}/{session}` (or `{host}` on
  a host-state card), line2 `{window-number}:{window-name}` (or the host state / a
  loading spinner). The kinds are the window card, the host-state card (scanning /
  unreachable / empty host), and the loading card.
- level color — the fixed per-kind card color: host yellow, session green, window
  magenta, status / loading grey.
- selection — the nav's current pick (its card index is `selected`), advanced by
  navigation; a routine poll or restream never moves it (only launch / rescan
  re-sorts). `preselect` / `reselect` are the launch and post-rescan selections.
- selection highlight — the reverse-video rendering of the selected card (ratatui's
  `highlight_style`, filling the whole card). `selected` + `highlight` follow
  ratatui's list vocabulary.
- active marker — the bold + italic styling on a window card whose window is the
  displayed (active) one of its session.
- spinner — the braille activity glyph on a loading card (and, historically, a
  connecting session).
- loading card — a card standing in for a session whose panes are not yet loaded;
  its line2 is a spinner rather than a `{window}:{name}`.
- status — a host-state card's line2 state text (`scanning…` / `no sessions` /
  `⚠ unreachable`). Not to be confused with the hint bar (below) or the `chrome`.
- filter — the type-to-filter input over the nav list.
- flash — a transient notice or error line shown in the hint bar (e.g. a refused
  action's reason). Never a "toast" or "notice".
- scan indicator — the `⟳ scanning hosts n/m…` progress shown in the hint bar
  while host probes are in flight; distinct from a row's `scanning…` status.
- popup — the bordered, opaque, centered (draggable) dialog a `ModalKind::Popup`
  draws, its title in the top border. The help, input dialog, and kill confirm
  are popups.
- prompt — the `❯` entry marker on an input dialog's edit line.
- confirm — the red `[y]es / [n]o` kill-confirmation prompt.
- menu highlight — the reverse-video context-menu entry under the pointer (the
  menu's selection highlight).

`pane` is reserved for a mux window's terminal split (a tmux / psmux pane); it is
never a screen region — screen regions are "views", and the line between them is
the `view border`. A transient hint-bar message is a `flash`, never a "toast" or
"notice". A card's trailing state is a `status`, never a "hint". The reverse-video
selection is the `selection highlight` (nav) or `menu highlight` (menu); `cursor`
names only the grid's text cursor. The furniture around the views is the `chrome`
(owned by `Chrome`), never a "status surface". The switcher's rendered screen is
the "switcher screen" (`dump_screen`), never an "overlay".

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

- MACHINE — `src/machine/`. Each machine family (`local.rs`, `ssh.rs`) owns its
  execution behind the `Transport` trait; a host builds one via `machine::local()`
  / `machine::ssh()`, so machine selection lives at construction, never a central
  `match`. Shared shell vocabulary (`quote` / `remote_command`) lives in
  `src/machine/vocab.rs`. `Transport` owns where a command runs and how its argv is
  executed; it knows nothing about the mux.
- MUX — `src/mux/<kind>/`. Each mux family (`tmux/`, `psmux/`) owns its metadata
  and command plans in `mod.rs` (behind the `Mux` trait) and its display
  driver in `display.rs`. A mux builds its OWN driver via `Mux::driver()`,
  so mux selection lives in the mux family, never a central `match`. Shared mux
  vocabulary lives in `src/mux/vocab.rs`.

Attach argv is composed from a host's own `mux` + `transport` (the two axes
together), so the two families are combined without either knowing the other.

The supervisor branches on NOTHING mux-specific. `src/app/` (runtime loop,
focus, input routing), `src/ui/` (switcher / tree / chrome / modal / ops
rendering), and `src/state/` (runtime `State` + the `apply` / `apply_event`
mutation sites) select display through `driver_for(host).show(...)` — i.e.
`host.mux.driver()` — and read the grid back via `MuxDriver::grid`; per-mux
behavior lives behind that seam. These layers carry no PTY, grid, or
terminal-protocol logic.

The remaining layers each own one concern:

- `src/display/` — the mux- and app-agnostic PTY/grid/input mechanics (attach
  spawning, the `Grid`, input decode, `term`, `dispatch`, the registry, worker).
- `src/host/` — host connection management (control-mode reader/writer, poll
  tasks, live client ownership).
- `src/machine/` — the machine axis: the `Transport` trait, the `Local`/`Ssh`
  families, and the shared shell vocab (`vocab.rs`).
- `src/model/` — domain types (`Host`, `Hosts`, `Selection`, `Action`,
  `Command`, `EventEffect`, server model).
- `src/driver.rs` — the mux-agnostic `MuxDriver` trait + `DriverCtx` (the
  supervisor capabilities a driver borrows) + the thin `driver_for` wrapper. It
  names no concrete mux type.

## Adding a module

At creation time, place a new source file by the axis it belongs to:

- Machine-specific → a new machine family is a new `src/machine/<kind>.rs`
  implementing `Transport` (+ a `machine::<kind>()` factory); new per-machine
  execution goes in the existing `local.rs`/`ssh.rs`.
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

- Per-host session/window inventory has a single owner: `model::Host.inventory`.
  Both metadata paths feed it through `HostEvent`s — the control reader carries
  its parsed sessions on `Connected`/`Inventory` and pane subtrees on `Panes`, and
  the poll task carries `Sessions`/`Panes`; the run loop folds them in and rebuilds
  the tree from it. `host::HostManager` owns the live mechanisms (control clients
  and poll tasks). Keep live process/task ownership out of `model::Host`, and do
  not add a third per-host registry.
- `Source` is thin per-source config/data. The CLI, the `ls` scan, and the
  off-loop `Ops`/`manage` paths assemble a value `Host` from it (`Source::host`)
  and drive enumerate/manage/attach through the `Host`/`Mux`/`Transport` APIs; the
  machine boundary (argv assembly, ssh transport) lives entirely in `Transport`,
  and the psmux registry helpers live in `mux/psmux`. `Hosts` is the app loop's
  runtime host registry (every `Host` keyed by id, in display order); `Env` keeps
  the source list + `by_alias` for the CLI, the scan, and `EnvOps`. The remaining
  direction: shrink `Source` further by folding its `Host` assembly into `Host`
  construction and backing the off-loop `Ops` with `Hosts` too, then reshape
  `host::HostManager` as a runtime manager if it outgrows its metadata-client role.
  New local/ssh execution belongs in `Transport`, new mux behavior in `Mux`.
- `docs/superpowers/` contains working planning material and is not intended for
  the public open source documentation surface. Before release, remove it from
  the published repository state or replace any still-useful content with
  current English documentation elsewhere.
- The control socket has a useful module seam: public ctl verbs parse to
  `model::Action`, while raw key and text injection stays behind the
  unstable `raw:` namespace. Working Notes should tell agents to add
  user-facing automation through semantic actions first, and reserve raw
  input for tests or low-level compatibility.
