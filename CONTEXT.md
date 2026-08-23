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

- `Transport` (MACHINE axis) - the per-machine execution trait (impls `Local` /
  `Ssh`); a host's `transport` field is `Box<dyn Transport>`. It owns where a
  command runs and how its argv is executed, and knows nothing about the mux.
  "machine" is the family/concept; `Transport` is the trait.
- `Mux` (MUX axis) - the per-mux behavior trait (impls `Tmux` / `Psmux` /
  `Zellij`); a host's `mux` field is `Box<dyn Mux>`. "mux" is the family/concept; `Mux` is
  the trait.
- `MuxDriver` - a mux's display driver, built by `Mux::driver()`.
- the app - the runtime that owns the terminal (`crate::app`; the `Runtime` struct
  and its loop in `app/runtime/`, entry `run_app`).
- `ViewFocus` - which screen region holds focus (`Nav` / `Terminal`).
- `Modal` - the mutually-exclusive focus-grabbing UI (`Help` and an input
  dialog). `ModalKind::Popup` is its one focus sub-kind: a draggable centered
  dialog.

UI elements a user perceives as distinct things:

- split view - the whole two-region layout.
- nav view - the region holding the session cards, ordered by source recency (a left
  column in `Side`, a top band in portrait `Top`, where the same cards run in a column
  flow). Never the "sidebar", and
  never the "tree": the on-screen VIEW is the nav view; `tree` names only the
  internal row-model module (`ui::tree`), which is still a Host→Session→Window
  structure.
- terminal view - the right region (the selected session's live grid).
- view border - the vertical line between the two views. Modelled on tmux's pane
  border, but it borders views (not panes), so it is a `view border`, never a
  "pane border" or a bare "divider". Its color config keys are `view-border-style`
  / `view-active-border-style` / `view-border-hover-style`. These keys are
  OVERRIDES: unset (empty), each colour comes from the displayed host's live mux
  `pane-*-border-style` (queried per displayed host), falling back to the stock
  default (`green` / terminal-default / `yellow`). A non-empty key wins over both.
- active view border - the view border half painted the active color to mark which
  view holds focus (tmux `pane-active-border-style`; the top half is the active
  color for nav focus, the bottom half for terminal focus).
- view border lines - the view border's line-drawing style (tmux
  `pane-border-lines`): `single │` (default), `double ║` (auto-hide-nav on),
  `heavy ┃` (hover - the drag-resize grab cue).
- chrome - the furniture around the two views, owned by the `Chrome` type: the
  view border, the hint bar, and the host info.
- hint bar - the nav's own status line: the bottom row(s) of the nav region, ending
  at the view border rather than spanning the screen, so the terminal view keeps
  every row it owns. At rest it shows only the prefix; while the prefix is ARMED it
  shows the keys that prefix unlocks. A flash, the scan indicator, and the active
  filter outrank both, in that order. A long flash wraps across as many nav rows as
  it needs instead of clipping. A shown flash paints it in the error style.
- host info - the unreachable-host detail shown in the terminal-view region.
- landing - the empty-state panel shown in the terminal-view region for a selected
  reachable host that has no sessions yet (its name + the keys to start one).
- grid - the live terminal content drawn in the terminal view: xmux's in-memory
  cell mirror of the attached session's screen, fed by the vt100 parser.
- cursor - the real terminal cursor placed over the grid at the mux's cursor cell
  while the terminal view is focused. "cursor" always means this text cursor,
  never the nav selection.
- card - one nav entry: a context line (`{host}/{mux}`, or `{host}` on a
  host-state card) over a detail line (`{session}/{window}` of the focused (active)
  window behind a connector; the host state; or the session name + a loading spinner).
  The window part is written the way its own mux writes it - see `window label`. The muted connector hangs the detail
  under its context line - on a collapsed card, under the shared context
  above: `├` while a collapsed sibling follows below, `└` on the run's last
  line; the selected card drops the connector (the selection mark and the inverted
  rows already bind its lines).
  One card per SESSION; the mux segment names the mux kind serving it
  (`Session.mux`, stamped at enumeration), so several muxes on one host stay
  distinguishable. The kinds are the session card, the host-state card
  (scanning / unreachable / empty host), and the loading card.
- card collapse - a session/loading card whose `{host}/{mux}` repeats the
  previous card's drops its context line and renders one row tall, so runs on
  one server read grouped. In the `Side` list the SELECTED card never collapses (focus
  expands it to the full two-row card, so its context is always readable in place) and
  the renderer and mouse hit-testing share one `card_height` so the screen-row mapping
  never diverges. The column flow collapses by position alone, never by selection: a
  column's first card always states its context, and heights that moved with the
  selection would reflow whole columns as the cursor passed.
- layout turnover - the one test that picks `Side` or `Top` (`view_layout`), measured as
  if the nav kept its side column: the terminal that column would leave is
  `w - nav_width - 1` wide over the window's full height. Wider than tall keeps the side
  column; square or taller moves the nav to the top band and drops the column. The
  as-if is the point - going `Top` hands those columns back to the terminal and takes the
  band's rows instead, so a test measuring the LIVE terminal would flip its own input and
  the layout would oscillate on one cell of resize.
- column flow - how the portrait `Top` band lays its cards out (`ui::switcher::columns`):
  down a column, then right. A column takes whole host/mux RUNS, so a source's cards stay
  together under the one context line naming them and the run that does not fit opens the
  next column instead of splitting across the break. A run taller than the whole column
  is the one exception, having nowhere else to go: it splits, and the continuation states
  its context again. A column is as wide as its widest card, columns are parted by one
  blank, and the flow is pure geometry, so the paint, the hit-test and the tests read one
  answer. A list would show three cards in a band twenty rows wide and leave the rest of
  every row blank; the flow is what makes the band worth its rows.
- level color - the per-segment card color, from the palette (`ui::palette`).
  Every foreground role is ANSI-16, so the terminal theme resolves the hue: host blue,
  mux green, session red, the window part bright-black - the quietest
  level, so the session name anchors the detail line. The four read as one code-theme
  palette, and the level a user actually picks (the session) is the one that stands out.
  A host-state card's detail line is
  colored by state - scanning yellow, unreachable red, settled "no sessions"
  muted. The hint bar is two slots as well (black under white, blue keys). Nothing here
  is an RGB value; see "Colour ownership" below for why, and `[ui] selection-style` /
  `[ui] hint-bar-style` for naming one anyway.
- window label - how a card writes its focused window, in the CONVENTION OF ITS OWN MUX
  rather than one xmux imposes: tmux and psmux get `{index}:{name}`, which is what
  their own status line and `list-windows` print; zellij gets the tab name alone,
  because zellij's tab bar shows names and nothing else and a tab it names itself is
  already called `Tab #1`. The mux owns the rule (`Mux::window_label`), so a reader who
  knows one mux reads its cards without learning a second notation.
- card order - the one order the flat card list follows (`Switcher::nav_order`, held as
  addresses). Recency is measured per SOURCE, not per session: a source's cards are
  contiguous, sources run most-recently-used first, and inside a source its own sessions
  run most-recently-used first. Global session recency would split a source across the
  list, restating its context line and leaving a connector claiming cards that belong to
  another host. One insertion rule carries it: a session lands after the last card of its
  own source, or at the end when its source has none yet - which also keeps a session
  discovered later inside its group. The order is rebuilt while any host is still
  scanning and frozen once they settle, so a routine poll never reshuffles cards under
  the user.
- selection - the nav's current pick (its card index is `selected`), advanced by
  navigation; a routine poll or restream never moves it (only launch / rescan
  re-sorts). `preselect` / `reselect` are the launch and post-rescan selections.
- selection highlight - the selected card's rendering: reverse video (ratatui's
  `highlight_style`, filling the whole card), the terminal theme's own selected look,
  plus a `❯` mark standing in the address column of the card's detail line, where
  every other card carries its number. The inversion is uniform because the highlight
  pins fg and bg to `Reset`: inverting per span would turn each level color into a
  background and stripe the card. That same pinning is why the mark is an open shape and
  never a solid block: it draws inverted too, so a block fills its cell and disappears
  into the band while an outline keeps a readable silhouette.
  `[ui] selection-style` paints a named background instead. `selected` + `highlight`
  follow ratatui's list vocabulary.
- scrollbar strip - the row or column a scrollbar takes from the nav region when the
  cards overflow it: the bottom ROW under the portrait column flow (which scrolls
  sideways, by column), the right COLUMN beside the side list (which scrolls down, by
  card). Reserved, never overlaid, because the selected card is painted by inverting its
  whole rect and a thumb inside that rect inverts with it into a hole in the bar. The
  thumb is proportional and reaches both ends of its track; nothing is drawn at all while
  everything fits, so a nav that fits spends no cell on furniture.
- spinner - the braille activity glyph on a loading card (and, historically, a
  connecting session).
- loading card - a card standing in for a session whose panes are not yet loaded;
  its detail line is `{session}/` + a spinner rather than a window part.
- status - a host-state card's detail-line state text (`scanning…` / `no sessions` /
  `⚠ unreachable`). Not to be confused with the hint bar (below) or the `chrome`.
- address column - the leftmost column set of every card, holding the one thing that
  answers "where is this": the dim 0-based number `prefix <digit>` jumps to, or, on the
  SELECTED card, the selection mark - the number there would be the address of where you
  already are. One column carries both, so a card's name never moves as the selection
  passes over it. It is written on the DETAIL line, beside the session it addresses, so a
  collapsed card puts it in the same place as an expanded one; a context line spends the
  same width blank. The column is one width per frame, so the names stay aligned and the
  numbers line up by units place as the count crosses 10.
- jump - the digits-only popup `prefix <digit>` opens. It acts WHILE open: each edit
  moves the selection, so Enter only closes it and Esc restores where it started. It
  accepts only a digit that keeps the number addressing a real card, so one-, two-,
  and three-digit numbers behave identically and the buffer never shows a number you
  cannot land on. User-facing text calls this "jump to a session" (see the naming rule
  below).
- instance name - a running app's identity: an auto-generated `<adjective>-<noun>`
  (or `--name`), owning `ctl-<name>.sock` for its lifetime. `xmux send <name>` and
  `xmux instances` address instances by it; a unique name prefix resolves, and `-`
  means the sole live instance.
- source - ONE MUX ON ONE MACHINE, and the thing every session address names. A
  machine running several muxes at once contributes one source per mux, all reached
  through the same `Transport`. A source id is the bare machine alias (`local`, `prod`)
  when its machine serves a single mux, and `<machine>:<mux>` (`local:zellij`) when it
  serves several, so a one-mux setup is spelled exactly as it always was. `machine_of`
  / `mux_of` / `is_local_source` read the two halves back; nothing compares a source id
  to `local` directly. The nav renders the halves separately (`local/zellij`), so the id
  never appears with its mux twice. A source is held TWICE, once per consumer, and both
  copies are built from `machine::kind_for` so they reach the machine the same way: the
  event loop drives a `model::Host` out of `Hosts`, and the off-loop ops resolve a
  `source::Source` out of `Env`. Discovery adds to BOTH - a source in only one of them
  paints and scans but refuses every operation, or the reverse.
- mux discovery - how a machine's mux list is decided when it named no mux (`mux` unset
  or `auto`): every mux xmux supports is asked whether it is installed there, and each one
  that answers becomes a source. Two halves, in that order: the candidate set is what xmux
  can DRIVE (`mux::supported_muxes`), and the question asked of each candidate is the same
  identity probe a configured mux gets (`detect_backend`), so a binary carrying a mux's
  name while being another mux is not that mux (where psmux answers, a `tmux` that answers
  is psmux's own alias). A written `mux` value is never probed: it is taken verbatim,
  unreachable and all. Distinct from `roster` (which MACHINES) and `discovery` (scanning a
  source for SESSIONS).
  THIS BOX is resolved before the first paint (a local probe is milliseconds), once, in
  `Env`, and threaded into `source::build` / `Hosts::build` so source ids and host ids
  cannot disagree. A REMOTE machine is probed AFTER launch instead (one ssh round trip per
  mux, which nothing may wait for): `discover_machine_muxes` spawns one task per machine,
  the answer arrives as `HostEvent::MuxesFound`, and `EventEffect::AddDiscoveredSources`
  adds a scanning card for every mux the machine does not already serve. That add is
  ADD-ONLY: an added source's id is always qualified (`prod:zellij`) and the mux already
  served keeps the id it was painted with, because that id is what the frozen order, the
  persisted selection, and typed ctl targets are keyed to.
- roster - the list of ssh targets xmux offers as sources, assembled from
  PROVIDERS: `~/.ssh/config` aliases and, when `[discovery]` enables it, the online
  peers of this machine's tailnet. Every provider yields plain ssh target names, so
  nothing downstream can tell where a name came from. Distinct from `discovery`, which
  scans a source for sessions, and from `machine/`, which reaches one.
- filter - the type-to-filter input over the nav list.
- flash - a transient notice or error line shown in the hint bar (e.g. a refused
  action's reason). Never a "toast" or "notice".
- scan indicator - the `⟳ scanning hosts n/m…` progress shown in the hint bar
  while host probes are in flight; distinct from a row's `scanning…` status.
- armed - the state between pressing the prefix and its command key. The hint bar
  reads it to swap from the resting prefix to the cheatsheet, so arming is a
  visible change and redraws the frame.
- popup - the rounded-bordered, opaque, centered (draggable) dialog a
  `ModalKind::Popup` draws, its accent title in the top border. The help and the
  input dialog are popups.
- prompt - the `❯` entry marker on an input dialog's edit line.

A zellij TAB is a `window` and a zellij SESSION is a `session`: xmux's vocabulary is
one set of words for every mux, so a mux's own naming is translated at its family
boundary and nowhere above it.

`pane` is reserved for a mux window's terminal split (a tmux / psmux pane); it is
never a screen region - screen regions are "views", and the line between them is
the `view border`. A transient hint-bar message is a `flash`, never a "toast" or
"notice". A card's trailing state is a `status`, never a "hint". The reverse-video
selected card is the `selection highlight`; `cursor` names only the grid's text
cursor. The furniture around the views is the `chrome`
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

## Architecture - the orthogonal design

Two orthogonal axes describe every connection, and no module conflates them:

- MACHINE - `src/machine/`. Each machine family (`local.rs`, `ssh.rs`) owns its
  execution behind the `Transport` trait; a host builds one via `machine::local()`
  / `machine::ssh()`, so machine selection lives at construction, never a central
  `match`. Shared shell vocabulary (`quote` / `remote_command`) lives in
  `src/machine/vocab.rs`. `Transport` owns where a command runs and how its argv is
  executed; it knows nothing about the mux.
- MUX - `src/mux/<kind>/`. Each mux family (`tmux/`, `psmux/`, `zellij/`) owns its
  metadata and command plans in `mod.rs` (behind the `Mux` trait) and its display
  driver in `display.rs`. A mux builds its OWN driver via `Mux::driver()`,
  so mux selection lives in the mux family, never a central `match`. Shared mux
  vocabulary lives in `src/mux/vocab.rs`. The trait's command plans default to
  tmux-compatible argv, so a tmux-compatible mux is identity plus a few methods; a
  mux that shares no argv (zellij) overrides every plan AND the shape of what each
  plan prints, since a plan and its output are one decision.

Attach argv is composed from a host's own `mux` + `transport` (the two axes
together), so the two families are combined without either knowing the other.

The supervisor branches on NOTHING mux-specific. `src/app/` (runtime loop,
focus, input routing), `src/ui/` (switcher / tree / chrome / modal / ops
rendering), and `src/state/` (runtime `State` + the `apply` / `apply_event`
mutation sites) select display through `driver_for(host).show(...)` - i.e.
`host.mux.driver()` - and read the grid back via `MuxDriver::grid`; per-mux
behavior lives behind that seam. These layers carry no PTY, grid, or
terminal-protocol logic.

The remaining layers each own one concern:

- `src/display/` - the mux- and app-agnostic PTY/grid/input mechanics (attach
  spawning, the `Grid`, input decode, `term`, `dispatch`, the registry, worker).
- `src/host/` - host connection management (control-mode reader/writer, poll
  tasks, live client ownership).
- `src/machine/` - the machine axis: the `Transport` trait, the `Local`/`Ssh`
  families, and the shared shell vocab (`vocab.rs`).
- `src/model/` - domain types (`Host`, `Hosts`, `Selection`, `Action`,
  `Command`, `EventEffect`, server model).
- `src/driver.rs` - the mux-agnostic `MuxDriver` trait + `DriverCtx` (the
  supervisor capabilities a driver borrows) + the thin `driver_for` wrapper. It
  names no concrete mux type.

## Colour ownership

**The terminal theme owns every colour xmux paints.** xmux names ANSI-16 slots and
attributes; the terminal resolves them into actual hues. So the whole UI recolours with
whatever scheme the user runs, and xmux never fights a theme it cannot see. This is a
hard invariant, not a preference.

- The vocabulary is the sixteen slots (`ui::palette`, one field per UI role) plus
  ATTRIBUTES: reverse video, bold. Nothing else. A `Color::Rgb`, or a `Color::Indexed`
  above 15, is a hue xmux chose for somebody else's terminal, and it is wrong on every
  theme it was not chosen for. `every_colour_xmux_chooses_is_an_ansi_slot` fails if one
  reaches the palette.
- Anything the sixteen slots cannot say is said with an attribute instead. "One step off
  the background" is the case that keeps coming up, and it is not a slot: so the selected
  card is REVERSE VIDEO, the terminal swapping its own pair, which is what a theme itself
  means by "selected". Not a computed surface - computing one needs the terminal's
  background, and a terminal is free to answer no colour query at all (Windows Terminal
  answers none), which leaves a fixed fallback as the permanent state rather than a rare
  one.
- The exceptions are colours the USER names: `[ui] selection-style`,
  `[ui] hint-bar-style`, and the view-border colours. Their terminal, their choice.
  `chrome::map_color` is that vocabulary and the only place a `#rrggbb` may enter.
- A colour a CHILD program emits passes through untouched (`display::grid`): it is that
  program's own choice against the same theme, and xmux is not in it.

A new colour goes into `ui::palette` as a slot, or it does not go in.

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
- Switcher / nav rows / status UI → `src/ui/`.
- Runtime `State` → `src/state/`.

Then, if the module introduces a new directory, create that directory's
`AGENTS.md` using the Working Notes Format above (all seven sections). Follow the
AS-IS rule: describe the current state only, with refactoring direction expressed
as invariants, seams, and pitfalls - never as change history or phase narrative.

## Improvement Notes

- Per-host session/window inventory has a single owner: `model::Host.inventory`.
  Both metadata paths feed it through `HostEvent`s - the control reader carries
  its parsed sessions on `Connected`/`Inventory` and pane subtrees on `Panes`, and
  the poll task carries `Sessions`/`Panes`; the run loop folds them in and rebuilds
  the nav rows from it. `host::HostManager` owns the live mechanisms (control clients
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
