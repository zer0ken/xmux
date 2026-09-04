# Working Notes: /src/ui

## Purpose

`ui` owns the session switcher: pure row-model transforms, side-effecting UI
operations, control socket serving helpers, interactive switcher state, rendering,
and the lightweight preferences that persist UI hints (last-selected address, nav
width and height, auto-hide-nav, nav position) across runs.

## Mental Model

The row model is side-effect-free logic over groups and sessions. `switcher/` is
the aggregate interactive TUI surface: selection, flattened rows, modal and input
BEHAVIOR and rendering, key and mouse handling, operation result application, and
render state. The open modal itself lives in the runtime state, though the modal
type is defined here; the switcher reads and writes it and owns only the
transient popup geometry.

The chrome is the view border, the hint bar, and the host screens, plus its
view-local state (flash, spinner, view border colours, prefix, ready). The hint
bar is the NAV's bottom row or rows, not a full-width strip, and shows the prefix
alone until a prefix interaction is live (the prefix ready), when it lists the keys
that interaction unlocks. The chrome instance
itself lives in the runtime state, fed by the app each frame and rendered from it.

The operations module holds the off-loop mux-action boundary: the trait over the
live mux (one mutating method, starting a session; the rest read), the outcome
values, and the runner that executes one deferred operation against that trait in
a detached task. A switcher key that COMMITS a slow action resolves it through
the state's apply into a deferred-operation command it RETURNS up; the run loop
spawns the runner and folds the outcome back through the operation channel, so
the switcher holds no pending-operation queue of its own.

The control bridge turns control socket requests into app commands and can
flatten renders for the dump verb.

## Module Seams

- Pure row and group transforms belong in the row model.
- UI colours come from the semantic palette (the seven roles - primary, secondary,
  accent, decoration, warning, error, disabled - plus the hint bar's own pair and
  the selection style), so the theme changes in one place. A theme is a named
  role→ANSI-slot assignment; the palette holds the registry (`auto-dark`,
  `auto-light`) and `[ui] theme` selects one. `decoration` is the CONTENT furniture
  (card number, `/`, the rules); the view border's dim half is the separate
  `disabled` role because it states focus, not a card mark. The hint
  bar reads its OWN accent (`bar_accent`) because it sits on a different surface than
  the cards - a slot that reads on one may not read on the other. What lives in the
  chrome is only the override layer over these (the per-role `[ui]` colour keys).
- A colour the USER named is parsed in the chrome, never in the palette: the
  palette holds xmux's own choices, which are slots only.
- Chrome rendering and its view-local state belong in the chrome; it reads
  inventory from the runtime state, not from the switcher.
- Slow (network) mux effects belong behind the operations module; a committing key
  emits a deferred-operation command for the run loop to spawn, and does not call
  the mux itself.
- Control socket serving and dump rendering belong in the control bridge.
- Card LAYOUT geometry belongs in the column-flow module and stays pure: it takes card
  widths and run boundaries and returns rects, so the paint, the mouse hit-test and the
  tests all read one answer. Rendering reads that answer; it does not compute a second
  one.
- Other interaction state and rendering live in `switcher/` until a smaller seam
  exists for the specific surface being changed.

## Invariants

- Every colour xmux itself paints is an ANSI-16 slot or an attribute (reverse
  video, bold), so the terminal theme resolves it, never an RGB value. A
  background with no slot for it is an attribute instead: the selected card is
  reverse video, not a computed surface. See "Colour ownership" in `CONTEXT.md`;
  the palette is guarded so a stray RGB colour cannot reach it.
- The view screens are ONE screen in several states, not a panel each: one builder lays
  them all out, so the headline, the state word, and the key rows cannot drift apart. A
  state added later joins that grammar rather than bringing its own.
- The terminal view refuses exactly one address, the session xmux is running in, and it
  refuses it by emptying the view TARGET rather than at each place that would attach.
  The target is what the display reconcile, the attach and the mux-side switch all read,
  so a refusal anywhere else would leave the other paths open.
- A settled host's status word has one source, so the word on a card and the word on the
  screen reached from it are the same word.
- A host and its mux are shown as ONE label, and the mux in it is resolved ONCE per card,
  so a session card, its host's card and the screen behind either cannot spell one mux
  three ways. A source id's own separator never reaches a surface: an id is typed, a label
  is read, and the two grammars are not interchangeable.
- Honesty is the rule the whole layer serves: a card shows only what it can back
  with an answer, and says so when it cannot. A mux is named on a host-state card
  only when it is confirmed (the enumeration answered through it, or the source id
  resolves it), never when the host is unreachable or still scanning with only a
  bare id to go on.
- A card states a STATE, never a REASON: the status word is all a settled host card
  carries, and the message behind it (the diagnostic its transport gave, the provider
  that offered the host, the config stanza it was reached through) is stated on the
  screen that card selects. A card is only as wide as the nav, so a reason on it is a
  cut-down copy of one the screen already holds whole.
- A card that is waiting turns ONE spinner, trailing the line of a scanning
  host card in the same place whatever the host has or has not resolved, so all
  scanning cards read as the same thing loading. A settled card shows its value and
  no spinner - a session is never waiting once its host has resolved.
- Every in-flight marker in this layer reads its glyph from the one spinner helper on
  the frame the chrome advances, cards and the hint bar's scan progress alike, so
  nothing on screen turns out of step with anything else.
- Row transforms do not mutate their inputs unless the function name and
  signature make mutation explicit.
- The nav hides a settled unreachable host's card unless the filter names that host
  (`[ui] hide-unreachable`, default on): the named card is the one entry to its
  unreachable screen, so the hiding must leave it reachable. A reachable empty host and
  a host still scanning never hide, and the prune runs before the filter, so the no-match
  fallback cannot resurrect a host the filter does not name.
- A LOCKED host (the network answered, the credentials refused) never hides, whatever
  hide-unreachable says: its card is the one entry to the unlock, so the prune keeps it
  alongside a filter-named card. The classification reads ONLY ssh's own failure line
  (`Permission denied (publickey,…)`), never a generic "permission denied" or a reach
  failure, so a host that merely died stays unreachable.
- The unlock's credentials are exactly what the user submits: the two-step input (the
  username, then the masked password) carries the id across without looking it up, and
  a locked host's switch and create are refused. The password is never rendered (its
  field draws bullets), never stored, and the rendered frame carries no plaintext.
- The dump should reflect the same split view the main draw path renders.
- The nav's two bands are parted by the ROOM between them while the cards can spare a row
  for it, and by a rule once they cannot: a gap that scrolls out of view parts nothing a
  reader can see. The parting is measured as part of the run, so the bands never meet with
  nothing between them and the list scrolls a row before the cards alone would fill it.
  Which parting applies is decided in the side list's placement, and the boundary itself is
  one question asked once (the first host-state card), so the paint, the hit-test and the
  scrollbar cannot part the list in three places.
- A list with NOTHING but host-state cards (no session has a session to show) is the host
  band alone, and it still takes its side of the split: anchored to the BOTTOM in a
  column, to the RIGHT edge in a band, with the blank rows/columns opposite being
  where the sessions that will be found land. As each source resolves, its section and
  cards move to the top / left, so a scan reads as the pending hosts draining toward the
  sessions they become.
- A card's rect is decided by the PAINT and read back from it, in both layouts. Neither
  layout puts cards on a fixed pitch the paint ignores (a column parts its bands, a
  band runs columns), so a hit-test that measured its own pitch would land clicks
  on cards the renderer put elsewhere.
- Nav FURNITURE is never inside a card's rect. The selected card is painted by inverting
  that rect, so anything drawn inside one inverts with it; a band's connector
  therefore stands in a strip the column reserves left of the card, and the rect the paint
  records - what the selection inverts and what the hit-test reads - starts past it. The
  strip is reserved on every session card the band flows, painted or not, because the card
  widths are measured before the flow decides columns.
- A scrollbar is RESERVED a column of the nav region, never overlaid on the cards: the
  selected card is painted by inverting its rect, so a thumb drawn inside one inverts with
  it and reads as a hole in the bar. A band scrolls sideways and puts its cue on
  the status row instead, which is the band's own last row and never a card's, so the flow
  keeps every row of the band.
- No card's height or shape moves with the selection: focus changes only the address
  column (the number becomes the mark), so a row that gained a line under the cursor
  would reflow the list and the columns as the cursor passed. A section title is a
  fixed non-selectable row, and in a band the host band never shares a
  column with session cards.
- A pending prefix is dropped by the next INPUT, mouse included. The mouse path has to say
  so itself, because mouse bytes never reach either focus path's key handling. Bare hover
  is exempt: it is the pointer sitting there, not an action.
- An arrow PAIR names the view it focuses, keyed on the nav's attachment, in both focus
  paths (one for nav focus, one for terminal focus): the pair facing the terminal's side
  names the terminal (right and down with the nav on the left or above, left and up with
  the nav on the right or below), the other pair names the nav. A change
  to one path is a change to both.
- Modal input owns keys while open; those keys must not leak to the terminal view
  or global shortcuts. At most one modal is open, because the state holds one
  optional modal, so opening any modal drops whatever was open.
- UI actions that become domain intents should resolve to a domain action, applied
  at the state's single apply site.
- This layer branches on nothing mux-specific: the switcher renders rows and emits
  domain intents, never a match on mux kind. Per-mux behavior lives behind the mux
  and driver seam, reached through the operations trait, not decided here.
- A rebuild holds the selection on its session whenever that session survives the
  rebuild, whoever put the selection there. The rows are re-derived on every answer
  of the scan, so a selection re-picked from the top would walk from host to host as
  they arrive; it lands on the first session to appear and stays until the user or
  the mux moves it.
- The nav's two navigation steps name the two things its list is made of: one walks the
  cards, the other walks the categories, landing on a category's first card. A category
  is a source with sessions to show, or the whole host band at once. Neither step is
  defined by where a card sits on screen, so both mean the same thing in a column and in
  a band.
- A selection xmux is TOLD to make - a ctl switch, a create landing on its new
  card, the nav following the session the mux moved its own display client onto -
  names the card and moves to it through one entry point. Nothing downstream tells
  those callers apart, so the switcher does not either.
- The selection and drag mutators return a boolean, "did it actually move or
  grab", by accepted convention: the app gates its follow-up (attach, event
  consumption) on that signal. This mutate-and-return-bool shape is deliberate; it
  is not split into a pure command and query pair, because the churn would exceed
  the value.
- A surface that exists to be READ never shortens what it states. A value too wide for
  its column hangs under the same rule, a multi-line value keeps its lines, and a
  control character is written as its escape rather than printed as nothing: where a
  datum does not fit, the surface grows, and the datum is never the thing that gives
  way. This is why the reason, the probe command and the ssh stanza are on a screen and
  not on a card - the card had the room for none of them.
- A state screen states everything known about the state it explains, not the minimum
  that identifies it. The user reached it because the one-line state word was not
  enough, so the screen carries what failed, what was asked and over what, who put the
  thing on the list, what else nearby answered, and where the full history is written. A
  datum nothing recorded is an ABSENT row, never a blank one.
- The words on a screen and the values the code runs come from one place: the ssh
  connect wait is printed from the same constant the ssh option is built from, and a
  status word from the one helper the cards read. Two spellings of one fact drift.

## Common Pitfalls

- Do not put source process management or PTY writes in UI modules.
- Do not reach for an RGB colour to get a tone the sixteen slots lack (a raised
  surface, a slightly-off bar). There is no theme-safe way to pick one, and a
  terminal may answer no colour query at all. Use an attribute, or take the colour
  from config.
- Do not add side effects to the row model.
- Do not route public ctl behavior through internal switcher key names.

## Before Editing

- Decide whether the change is pure row data, interactive state, rendering, a
  side-effecting operation, or control and dump plumbing.
- For `switcher/`, find the existing helper for the same surface before adding
  another state path.
- Check focus and modal ownership before changing key handling.

## Verification

- Exercise the pure row transforms directly, and drive keys, mouse, modals, and
  rendering through the switcher for interactive changes.
- Re-check the dump output when the control bridge's rendering helpers change: it
  and the main draw path must agree.
