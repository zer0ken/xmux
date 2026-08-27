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

- `Transport` (HOST axis) - the per-host execution trait (local / ssh / wsl); a
  source holds one. It owns where a command runs and how its argv is executed, and
  knows nothing about the mux. "host" is the family/concept; `Transport` is the
  trait.
- `Mux` (MUX axis) - the per-mux behavior trait (tmux / psmux / zellij / abduco /
  screen); a source
  holds one. "mux" is the family/concept; `Mux` is the trait.
- host - a machine that HOSTS muxes and that xmux can reach. The `roster` decides
  the set: of all the machines there are, the hosts are the ones it names. "machine"
  is the plain word for the thing in the world and names no abstraction here; the
  abstraction is the host. A host is never a host paired with one mux - that is a
  `source` - so a host serving several muxes is several sources under ONE host, and
  the host is the half of a source id that survives when the mux half is dropped.
  The host FOR a mux is a host that can run it; the host OF a session is the host
  that session runs on. A `Transport` reaches a host; it is not one, and several
  transports may reach the same host.
- `MuxDriver` - a mux's display driver, which the mux itself builds.
- the app - the runtime that owns the terminal: its loop, its focus state, and
  its input routing.
- `ViewFocus` - which screen region holds focus (nav or terminal).
- `Modal` - the mutually-exclusive focus-grabbing UI (the help and an input
  dialog). A popup is its one focus sub-kind: a draggable centered dialog.

UI elements a user perceives as distinct things:

- split view - the whole two-region layout.
- nav view - the region holding the session cards, ordered by source recency (a left
  column in side layout, a top band in portrait layout, where the same cards run in a
  column flow). Never the
  "sidebar", and never the "tree": the on-screen VIEW is the nav view; "tree" names
  only the internal row-model module, which is still a Source to Session to Window
  structure.
- terminal view - the right region (the selected session's live grid).
- view border - the vertical line between the two views. Modelled on tmux's pane
  border, but it borders views (not panes), so it is a `view border`, never a
  "pane border" or a bare "divider". Its colour is FIXED and the same on every
  source: the palette accent on the lit half, its own `border_inactive` muted tone on
  the other, yellow for the drag-hover cue. The border states which VIEW holds focus,
  which is a fact
  about xmux and not about the mux on the other side of it, so nothing a host or a
  mux reports may move it. Its color config keys are `view-border-style` /
  `view-active-border-style` / `view-border-hover-style`. These keys are OVERRIDES:
  unset (empty), that side keeps the fixed colour; a non-empty key replaces it.
- active view border - the view border half painted the active color to mark which
  view holds focus (tmux `pane-active-border-style`; the top half is the active
  color for nav focus, the bottom half for terminal focus).
- view border lines - the view border's line-drawing style (tmux
  `pane-border-lines`): `single │` (default), `double ║` (auto-hide-nav on),
  `heavy ┃` (hover - the drag-resize grab cue).
- chrome - the furniture around the two views: the view border, the hint bar, and
  the view screens.
- hint bar - the nav's own status line: the bottom row(s) of the nav region, ending
  at the view border rather than spanning the screen, so the terminal view keeps
  every row it owns. At rest it shows only the prefix; while a prefix interaction is
  live (the prefix armed, or its key still held) it shows the keys that interaction
  unlocks. A flash, the scan indicator, and the active
  filter outrank both, in that order. A long flash wraps across as many nav rows as
  it needs instead of clipping. A shown flash paints it in the error style.
- view screen - what fills the terminal-view region in place of a mux, for a selection
  with no grid to show there. Where a card states the selection's STATE, the screen
  states WHY: it is the one surface with the room to hold a tool's diagnostic whole.
  One screen in three states, so a reader of any of them reads the others: the subject
  as the headline (a host for the two host states, the session address for
  `own session`), under it the state word, then the rows that apply. A row is the key-column row the help also uses - a
  right-aligned cell, the `│` rule, the value - where a bold cell is a key that can be
  pressed here and a muted cell names a datum. No value on a screen is shortened to fit
  its column: one too wide hangs under the same rule, a multi-line one keeps its lines,
  and a control character is written as its escape rather than printed as nothing. The
  UNREACHABLE state states everything known about the failure, in reading order: the
  reason its transport gave, how many failures in a row it is, then what was asked and
  over what (the mux binary, how the machine is addressed and the wait that bounds it,
  the socket, and the session-listing command itself, spelled so it can be run by hand),
  then the provider that put the host on the roster, the ssh stanza it was reached
  through, what the OTHER muxes on that same machine answered, and the log file holding
  the full history - then the rescan key. The EMPTY state's rows are the keys that start a session or rescan. A host
  still scanning gets no screen: an in-flight state is the nav's to show. The
  `own session` state's rows are why it is refused, and no key, because nothing pressed
  here would make it showable.
- nesting - xmux running inside a mux session. Allowed: the app attaches mux clients as
  PTY CHILDREN, so nothing it opens is a terminal handover that a mux would refuse. It
  costs one thing, the `own session`.
- own session - the mux session xmux is ITSELF running in, named once at startup from
  what the mux says (zellij and psmux put it in the environment; tmux is asked). The one
  address the terminal view refuses: mirroring it would attach a second client to the
  session holding xmux, moving the user's own client and painting xmux inside itself. A
  session running a DIFFERENT xmux is not it, and mirrors like any other.
- grid - the live terminal content drawn in the terminal view: xmux's in-memory
  cell mirror of the attached session's screen, fed by the terminal-emulation parser.
- cursor - the real terminal cursor placed over the grid at the mux's cursor cell
  while the terminal view is focused. "cursor" always means this text cursor,
  never the nav selection.
- card - one nav entry: a context line (`{host}/{mux}`, or `{host}` on a
  host-state card) over a detail line (`{session}/{window}` of the focused (active)
  window behind a connector; the settled host state, the state word alone; or a spinner
  in the card's unresolved level). A card states WHAT something is; WHY it is that way
  is the screen's, never a card's.
  The window part is written the way its own mux writes it - see `window label`. The muted connector hangs the detail
  under its context line - on a collapsed card, under the shared context
  above: `├` while a collapsed sibling follows below, `└` on the run's last
  line; the selected card drops the connector (the selection mark and the inverted
  rows already bind its lines).
  One card per SESSION; the mux segment names the mux kind serving it, stamped at
  enumeration, so several muxes on one host stay distinguishable. The kinds are the
  session card, the host-state card (scanning / unreachable / empty host), and the
  loading card.
- card collapse - a session/loading card whose `{host}/{mux}` repeats the
  previous card's drops its context line and renders one row tall, so runs on
  one server read grouped. In the SIDE list the selected card never collapses (focus
  expands it to the full two-row card, so its context is always readable in place), and one
  card-height rule feeds the placement the paint and the hit-test both read, so the
  screen-row mapping never diverges. The column flow collapses by POSITION alone, never by selection: a
  column's first card always states its context, and heights that moved with the selection
  would reflow whole columns as the cursor passed.
- nav size - the nav's live geometry as one value: the width the user SET, the width ON
  SCREEN this frame (0 while auto-hide has taken it and no prefix interaction is live),
  and the portrait band's height the
  user set (0 = auto). All three are settable while xmux runs, so every consumer takes the
  whole value rather than picking two of the three out of the runtime: the effective width
  has one owner, and a resize cannot reach the renderer and miss the PTY sizing. The set
  width and the on-screen width differ only while the nav is hidden, and that is exactly
  why both travel: the regions are cut from what is on screen, the layout turnover is
  measured from what the user set.
- layout turnover - the one test that picks the side column or the top band, measured as
  if the nav kept its side column: the terminal that column would leave is the window
  width less the nav and its border, over the window's full height. Wider than tall keeps
  the side column; square or taller moves the nav to the top band and drops the column.
  Wider than tall in the proportions the user SEES, not in cell counts: a row is about two
  columns tall, so the rows count double and 60 columns over 30 rows is square. Comparing
  the counts directly kept the side column until the terminal was half as wide as it
  looked. The as-if is the point too: going to the band hands those columns back to the
  terminal and takes rows instead, so a test measuring the LIVE terminal would flip its own
  input and the layout would oscillate on one cell of resize. Hiding the nav is not a
  resize either, since the turnover reads the width the user set, so the nav comes back the
  shape it left and the resize keys keep driving the same axis while it is gone.
- column flow - how the portrait band lays its cards out: down a column, then right. A
  column takes whole host/mux RUNS, so a source's cards stay together under the one
  context line naming them, and the run that does not fit opens the next column instead of
  splitting across the break. A run taller than the whole column is the one exception,
  having nowhere else to go: it splits, and the continuation states its context again. A
  column is as wide as its widest card, columns are parted by one blank, and the flow is
  pure geometry, so the paint, the hit-test and the tests read one answer. A list would
  show three cards in a band twenty rows wide and leave the rest of every row blank; the
  flow is what makes the band worth its rows.
- source label - how a host and its mux are SHOWN: `{host}/{mux}`, one grammar wherever
  the pair is read (a card's context line, the screen it selects, the doctor's source
  list). Not the id's own separator, because an id is typed and a label is read, and a
  label parts its levels the way the rest of an address on screen does. Both halves
  always: a host serving one mux carries no mux in its id and still shows one, since a
  host seen with its mux on one card and without it on the next reads as two hosts. The
  name comes from the mux's KIND, not the binary that reached it, so an alias or a path
  cannot put a second spelling on screen. Empty only where nothing knows the mux yet,
  which a card marks with its spinner rather than by dropping the separator.
- nav bands - the two bands the nav's cards fall into: the session cards, then the cards
  of the hosts with no session to show, which sit below every session card whatever order
  the hosts were scanned in. In the side column the parting is the ROOM between them while
  the cards can spare a row for it (the sessions hold the top edge, the host cards the
  bottom), and a rule across the cards once they cannot and the column scrolls as one list,
  because a gap parts only what a reader sees at once. The parting always has a row: the
  column is measured with the rule's row counted in, so a gap of one is the last thing
  before the rule and the bands never meet, at the price of scrolling a row early. Neither
  parting is a card, so a click on one selects nothing.
- level color - the per-segment card color, from the palette. Every foreground role
  is ANSI-16, so the terminal theme resolves the hue: host cyan,
  mux green, session red, the window part bright-black - the quietest
  level, so the session name anchors the detail line. The four read as one code-theme
  palette, and the level a user actually picks (the session) is the one that stands out.
  A spinner is pending yellow wherever it stands. A settled host-state card's detail
  line is colored by state: an unreachable host's is red, and a reachable empty host's
  card is a single host row with no status word (its screen states "no sessions"). The hint bar is two slots as well (black under white, cyan keys). Nothing here
  is an RGB value; see "Colour ownership" below for why, and `[ui] selection-style` /
  `[ui] hint-bar-style` for naming one anyway.
- window label - how a card writes its focused window, in the CONVENTION OF ITS OWN MUX
  rather than one xmux imposes: tmux, psmux, and screen get `{index}:{name}`, which is
  what their own status line and window listing print; zellij gets the tab name alone,
  because zellij's tab bar shows names and nothing else and a tab it names itself is
  already called `Tab #1`. The mux owns the rule, so a reader who knows one mux reads
  its cards without learning a second notation.
- card order - the one order the flat card list follows, held as addresses. Recency is
  measured per SOURCE, not per session: a source's cards are
  contiguous, sources run most-recently-used first, and inside a source its own sessions
  run most-recently-used first. Global session recency would split a source across the
  list, restating its context line and leaving a connector claiming cards that belong to
  another source. One insertion rule carries it: a session lands after the last card
  of its own source, or at the end when its source has none yet - which also keeps a session
  discovered later inside its group. The order is rebuilt while any source is still
  scanning and frozen once they settle, so a routine poll never reshuffles cards under
  the user.
- selection - the nav's current pick, advanced by navigation; a routine poll or
  restream never moves it (only launch / rescan re-sorts). The preselect and the
  reselect are the launch and post-rescan selections.
- selection highlight - the selected card's rendering: reverse video filling the whole
  card, the terminal theme's own selected look,
  plus a `❯` mark standing in the address column of the card's detail line, where
  every other card carries its number. The inversion is uniform because the highlight
  pins both foreground and background to the terminal's defaults: inverting per span
  would turn each level color into a background and stripe the card. That same pinning
  is why the mark is an open shape and
  never a solid block: it draws inverted too, so a block fills its cell and disappears
  into the band while an outline keeps a readable silhouette.
  `[ui] selection-style` paints a named background instead.
- scrollbar strip - the COLUMN the side list's scrollbar takes from the nav region when
  the cards overflow it. Reserved, never overlaid, because the selected card is painted by
  inverting its whole rect and a thumb inside that rect inverts with it into a hole in the
  bar. Nothing is drawn at all while everything fits, so a nav that fits spends no cell on
  furniture. The portrait flow has no strip: it scrolls sideways, and says so in words on
  its status row (see "offscreen counts").
- offscreen counts - what the portrait flow puts on its status row when columns are off
  screen: `<< 5 more` at the left end, `7 more >>` at the right, in the cells the status
  label does not take. Cards, not columns, because the reader is hunting a session, not a
  column. They cost no row (the status row is the band's own last row, never a card's) and
  say what a thumb cannot: which way the cards went, and how many. An ARMED bar takes the
  whole row back, counts included, since a cheatsheet has to be readable over what it
  covers.
- status row fill - how much of its row the hint bar paints. The side column's bar, an
  armed bar and a refusal fill the ROW: a solid bar, legible over whatever it covers. The
  portrait band's resting bar paints its text plus a cell of padding and stops, because it
  shares that row with the offscreen counts and a full-width slab of bar colour across a
  wide window is a lot of paint for one word.
- spinner - the braille activity glyph marking a level that has not resolved. One
  glyph and one frame counter for the whole UI, so every marker on screen turns
  together.
- unresolved level - the first of a card's levels (mux, then session, then window)
  with no answer yet. The spinner stands in exactly that one, and every level of the
  card behind it stays blank: one spinner per card names WHICH answer is outstanding,
  where a second would only say the card is busy.
- loading card - a card standing in for a session whose panes are not yet loaded;
  the window is its unresolved level, so its detail line is `{session}/` + a spinner
  rather than a window part.
- status - a host-state card's detail-line state text once it has SETTLED (`no
  sessions` / `⚠ unreachable`); a card still scanning carries no status word, because
  its spinner already says so. Not to be confused with the hint bar (below) or the
  `chrome`.
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
- source - ONE MUX ON ONE HOST, and the thing every session address names. A
  host running several muxes at once contributes one source per mux, all reached
  through the same `Transport`. A source id is the bare host alias (`local`, `prod`)
  when its host serves a single mux, and `<host>:<mux>` (`local:zellij`) when it
  serves several, so a one-mux setup is spelled exactly as it always was. The two halves
  are read back through accessors; nothing compares a source id to `local` directly. A
  HOST name says which family reaches it wherever the name alone would be ambiguous:
  `local` is this box and `wsl.<distribution>` is a WSL distribution on it, everything
  else being an ssh destination. That is what lets a host named LATER (a mux-discovery
  answer carries a bare host name and nothing else) be reached exactly as one named at
  launch, and it is why an ssh alias spelled either reserved way is refused rather than
  served as the wrong family. The
  nav renders the halves separately (`local/zellij`), so the id
  never appears with its mux twice. A source is held TWICE, once per consumer, and both
  copies resolve its host the same way: the event loop drives a source out of its
  runtime registry, and the off-loop operations resolve one out of the environment's
  source list. Discovery adds to BOTH - a source in only one of them
  paints and scans but refuses every operation, or the reverse.
- mux discovery - how a host's mux list is decided when it named no mux (`mux` unset
  or `auto`): every mux xmux supports is asked whether it is installed there, and each one
  that answers becomes a source. Two halves, in that order: the candidate set is what xmux
  can DRIVE, and the question asked of each candidate is the same identity probe a
  configured mux gets, so a binary carrying a mux's
  name while being another mux is not that mux (where psmux answers, a `tmux` that answers
  is psmux's own alias). A written `mux` value is never probed: it is taken verbatim,
  unreachable and all. Distinct from `roster` (which HOSTS) and `discovery` (scanning a
  source for SESSIONS).
  THIS BOX is resolved before the first paint (a local probe is milliseconds), once, and
  threaded into the construction of both the source list and the runtime registry, so the
  two cannot disagree on which sources exist. A REMOTE host is probed AFTER launch
  instead (one ssh round trip per
  mux, which nothing may wait for): one task per host, the answer arrives as a source
  event, and the loop adds a scanning card for every mux the host does not already
  serve. That add is
  ADD-ONLY: an added source's id is always qualified (`prod:zellij`) and the mux already
  served keeps the id it was painted with, because that id is what the frozen order, the
  persisted selection, and typed ctl targets are keyed to.
- roster - which HOSTS xmux offers, assembled from PROVIDERS, EVERY one on unless
  `[discovery]` turns it off: `~/.ssh/config` aliases, the online peers of this
  machine's tailnet, and this box's WSL distributions. Every provider yields plain ssh
  target names, so nothing downstream BEHAVES differently for one; which provider
  offered a name is kept beside it and shown on the unreachable host's view screen, never read
  to decide anything. The roster is what makes a machine a
  host: a machine no provider names is one xmux has nothing to say about. It is resolved
  at launch and again on every re-scan, and what a re-scan resolves is reconciled by
  MACHINE: a machine that is still named keeps the sources it serves, including the ones
  `mux discovery` found rather than config, and a machine that is not named loses every
  source it served. Distinct
  from `mux discovery`, which asks a host WHICH MUXES it serves, from `discovery`,
  which scans a source for sessions, and from the host axis, which reaches one.
- filter - the type-to-filter input over the nav list.
- flash - a transient notice or error line shown in the hint bar (e.g. a refused
  action's reason). Never a "toast" or "notice".
- scan indicator - the `scanning n/m…` progress shown in the hint bar while host
  probes are in flight, behind the same spinner on the same frame as the cards it
  counts. It counts SOURCES; a card's own spinner names one card's unresolved level.
- armed - the state between pressing the prefix and its command key: the prefix is
  ready for its command, and the hint bar reads it (together with HOLDING) to swap
  from the resting prefix to the cheatsheet, so arming is a visible change and
  redraws the frame.
- holding - the state while the prefix key is physically held down. A held key's
  autorepeat is read as still-holding rather than as a second press, so the status
  bar and the nav stay steady under a hold instead of toggling. A terminal that
  reports key releases (the kitty protocol) is what makes the release observable;
  a terminal that does not leaves the hold latched until a command key or a mouse
  action resolves it, and its second press is read as a hold-repeat rather than as
  the doubled-prefix literal.
- popup - the rounded-bordered, opaque, centered (draggable) dialog a popup modal
  draws, its accent title in the top border. The help and the input dialog are popups.
- prompt - the `❯` entry marker on an input dialog's edit line.

A zellij TAB is a `window` and a zellij SESSION is a `session`: xmux's vocabulary is
one set of words for every mux, so a mux's own naming is translated at its family
boundary and nowhere above it.

`pane` is reserved for a mux window's terminal split (a tmux / psmux pane); it is
never a screen region - screen regions are "views", and the line between them is
the `view border`. A transient hint-bar message is a `flash`, never a "toast" or
"notice". A card's trailing state is a `status`, never a "hint". The reverse-video
selected card is the `selection highlight`; `cursor` names only the grid's text
cursor. The furniture around the views is the `chrome`, never a "status surface".
The switcher's rendered screen is the "switcher screen", never an "overlay".

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

## Documentation is the standard, code is the subject

Durable documentation states the behavior and the design rules the code is
checked against. It is not a mirror of the code, so it never names a test, a
function, a method, a field, or a library API: those move, and a document that
follows them turns every code change into a documentation change. What a
document may name is what the design itself prescribes and what the outside
world already depends on: the two axes and their vocabulary, the directory
layout a new module must fit, config keys, CLI and ctl verbs, socket names, and
the argv of the muxes xmux drives.

## Architecture - the orthogonal design

Two orthogonal axes describe every connection, and no module conflates them:

- HOST - `src/transport/`. Each host family owns its execution behind the
  `Transport` trait; a source builds one at construction, so host selection is
  never a central `match`. Shared shell vocabulary (quoting, remote command
  assembly) lives beside the families. `Transport` owns where a command runs and
  how its argv is executed; it knows nothing about the mux.
- MUX - `src/mux/<kind>/`. Each mux family (`tmux/`, `psmux/`, `zellij/`,
  `abduco/`, `screen/`) owns its metadata and command plans behind the `Mux` trait
  and its display driver beside them. A mux builds its OWN driver, so mux selection
  lives in the mux family,
  never a central `match`. Shared mux vocabulary lives beside the families. The
  trait's command plans default to tmux-compatible argv, so a tmux-compatible mux
  is identity plus a few overrides; a mux that shares no argv (zellij) overrides
  every plan AND the shape of what each plan prints, since a plan and its output
  are one decision.

Attach argv is composed from a source's own mux + transport (the two axes
together), so the two families are combined without either knowing the other.

The supervisor branches on NOTHING mux-specific. `src/app/` (runtime loop,
focus, input routing), `src/ui/` (switcher / rows / chrome / modal / ops
rendering), and `src/state/` (the runtime state and its mutation sites) select
display through the source's own driver and read the grid back from it; per-mux
behavior lives behind that seam. These layers carry no PTY, grid, or
terminal-protocol logic.

The remaining layers each own one concern:

- `src/display/` - the mux- and app-agnostic PTY/grid/input mechanics (attach
  spawning, the grid, input decode, terminal setup, dispatch, the registry, the
  worker).
- `src/link/` - per-source connection management (control-mode reader/writer,
  poll tasks, live client ownership).
- `src/transport/` - the transport axis: the `Transport` trait, the local and ssh
  families, and the shared shell vocabulary.
- `src/provision/` - resolution: the TOML config, the roster of ssh targets, the
  concurrent source probe, and the resolved runtime view over them.
- `src/cli/` - the CLI surface: argument parsing and command dispatch.
- `src/model/` - domain types: sources, selection, actions, commands, event
  effects, and the server model.
- `src/driver.rs` - the mux-agnostic `MuxDriver` trait, the supervisor
  capabilities a driver borrows, and the thin wrapper that resolves a source's
  driver. It names no concrete mux type.

## Colour ownership

**The terminal theme owns every colour xmux paints.** xmux names ANSI-16 slots and
attributes; the terminal resolves them into actual hues. So the whole UI recolours with
whatever scheme the user runs, and xmux never fights a theme it cannot see. This is a
hard invariant, not a preference.

A THEME is a named role→ANSI-slot assignment, and a theme system curates them: the
built-ins are `auto-dark` (the default) and `auto-light`, each an ANSI-only theme for a
dark or a light terminal background. `[ui] theme` names one; an unknown name falls back
to `auto-dark` and `xmux doctor` reports the resolution. The two built-ins are the whole
current set, and adding a theme is adding one registry entry plus its tests - the way
the system keeps growing without loosening the invariant below. Selecting a theme does
not pick colours: the theme IS the slot mapping, and both ends (the accent on the
cards, the `bar_accent` on the hint bar) stay within the slots.

The `[ui]` presentation settings - theme / selection-style / hint-bar-style /
view-border styles - are re-applied LIVE when `config.toml` changes: the redraw
cadence stats the file (a cheap poll, no watch dependency) and a changed mtime
reloads just that section, keeping the previous settings on a malformed edit. The
roster and hosts are not part of it - re-scanning sources is the `rescan` key's job
and a config edit must not reset the user's sessions.

- The vocabulary is the sixteen slots (one per UI role) plus ATTRIBUTES: reverse
  video, bold. Nothing else. An RGB colour, or an indexed colour above 15, is a hue
  xmux chose for somebody else's terminal, and it is wrong on every
  theme it was not chosen for. The palette is guarded so one cannot reach it.
- Anything the sixteen slots cannot say is said with an attribute instead. "One step off
  the background" is the case that keeps coming up, and it is not a slot: so the selected
  card is REVERSE VIDEO, the terminal swapping its own pair, which is what a theme itself
  means by "selected". Not a computed surface - computing one needs the terminal's
  background, and a terminal is free to answer no colour query at all (Windows Terminal
  answers none), which leaves a fixed fallback as the permanent state rather than a rare
  one.
- The exceptions are colours the USER names: `[ui] selection-style`,
  `[ui] hint-bar-style`, and the view-border colours. Their terminal, their choice.
  The chrome's colour mapping is that vocabulary and the only place a `#rrggbb` may
  enter.
- A colour a CHILD program emits passes through untouched: it is that program's own
  choice against the same theme, and xmux is not in it.

A new colour goes into the palette as a slot, or it does not go in.

## Adding a module

At creation time, place a new source file by the axis it belongs to:

- Host-specific → a new host family is a new module under `src/transport/`
  implementing `Transport` (plus its factory); new per-host execution goes in
  the existing local or ssh family.
- Mux-specific (a new mux family or per-mux behavior) → `src/mux/<kind>/`.
- PTY / grid / terminal-protocol mechanics → `src/display/`.
- Orchestration (runtime loop, focus) → `src/app/`.
- Per-source connection management → `src/link/`.
- Domain types → `src/model/`.
- Provisioning (config / roster / discovery / resolved env) → `src/provision/`.
- CLI command surface → `src/cli/`.
- Switcher / nav rows / status UI → `src/ui/`.
- Runtime state → `src/state/`.

Then, if the module introduces a new directory, create that directory's
`AGENTS.md` using the Working Notes Format above (all seven sections). Follow the
AS-IS rule: describe the current state only, with refactoring direction expressed
as invariants, seams, and pitfalls - never as change history or phase narrative.

## Improvement Notes

- Per-source session/window inventory has a single owner: the source's own
  inventory. Both metadata paths feed it through source events - the control reader
  carries its parsed sessions and pane subtrees, and the poll task carries the same
  - the run loop folds them in and rebuilds the nav rows from it. The source
  manager owns the live mechanisms (control clients and poll tasks). Keep live
  process/task ownership out of the source domain type, and do not add a third
  per-source registry.
- A source definition is thin per-source config/data. The CLI, the scan, and the
  off-loop operations assemble a runtime source from it and drive
  enumerate/manage/attach through the source, mux, and transport APIs; the host
  boundary (argv assembly, ssh transport) lives entirely in the transport, and the
  psmux registry helpers live in the psmux family. The runtime source registry is
  the app loop's (every source keyed by id, in display order); the environment
  keeps the source list and its alias index for the CLI, the scan, and the off-loop
  operations. The remaining direction: shrink the definition further by folding its
  assembly into runtime-source construction and backing the off-loop operations
  with the runtime registry too, then reshape the source manager as a runtime
  manager if it outgrows its metadata-client role. New local/ssh execution belongs
  in the transport, new mux behavior in the mux.
- The control socket has a useful module seam: public ctl verbs resolve to domain
  actions, while raw key and text injection stays behind the unstable `raw:`
  namespace. Working Notes should tell agents to add user-facing automation
  through semantic actions first, and reserve raw input for low-level
  compatibility.
