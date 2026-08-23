# Working Notes: /src/ui

## Purpose

`ui` owns the session switcher: pure row-model transforms, side-effecting UI
operations, control socket serving helpers, interactive switcher state, and
rendering.

## Mental Model

The row model is side-effect-free logic over groups and sessions. `switcher/` is
the aggregate interactive TUI surface: selection, flattened rows, modal and input
BEHAVIOR and rendering, key and mouse handling, operation result application, and
render state. The open modal itself lives in the runtime state, though the modal
type is defined here; the switcher reads and writes it and owns only the
transient popup geometry.

The chrome is the view border, the hint bar, and the host screens, plus its
view-local state (flash, spinner, view border colours, prefix, armed). The hint
bar is the NAV's bottom row or rows, not a full-width strip, and shows the prefix
alone until it is armed. The chrome instance itself lives in the runtime
state, fed by the app each frame and rendered from it.

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
- UI colours come from the semantic palette (accent and muted tiers, the hint
  bar's pair, the per-level card colours, and the selection style), so the theme
  changes in one place. The one exception is the view border: its stock defaults
  deliberately mirror tmux's pane-border defaults and live with their three-tier
  resolve logic in the chrome.
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
- A card that is waiting turns ONE spinner, in the first of its levels that has not
  resolved (mux, then session, then window); every level behind it stays blank. A
  second spinner on one card would say two separate things are in flight, when the
  card is waiting on exactly one answer. A level that has settled shows its value, and
  a card that has settled entirely shows a status word - never both a word and a
  spinner for the same state.
- Every in-flight marker in this layer reads its glyph from the one spinner helper on
  the frame the chrome advances, cards and the hint bar's scan progress alike, so
  nothing on screen turns out of step with anything else.
- Row transforms do not mutate their inputs unless the function name and
  signature make mutation explicit.
- The dump should reflect the same split view the main draw path renders.
- A scrollbar is RESERVED a column of the nav region, never overlaid on the cards: the
  selected card is painted by inverting its rect, so a thumb drawn inside one inverts with
  it and reads as a hole in the bar. The portrait flow scrolls sideways and puts its cue on
  the status row instead, which is the band's own last row and never a card's, so the flow
  keeps every row of the band.
- In the portrait column flow, what a card collapses under is decided by POSITION alone,
  never by the selection: a card height that moved with the cursor would reflow whole
  columns as the selection passed over them. The side list keeps its
  selection-expands-the-card rule, where a height change only shifts rows.
- A pending prefix is dropped by the next INPUT, mouse included. The mouse path has to say
  so itself, because mouse bytes never reach either focus path's key handling. Bare hover
  is exempt: it is the pointer sitting there, not an action.
- An arrow key points AT the view it focuses, in both focus paths (one for nav focus, one
  for terminal focus): right and down name the terminal, left and up name the nav. A change
  to one path is a change to both.
- Modal input owns keys while open; those keys must not leak to the terminal view
  or global shortcuts. At most one modal is open, because the state holds one
  optional modal, so opening any modal drops whatever was open.
- UI actions that become domain intents should resolve to a domain action, applied
  at the state's single apply site.
- This layer branches on nothing mux-specific: the switcher renders rows and emits
  domain intents, never a match on mux kind. Per-mux behavior lives behind the mux
  and driver seam, reached through the operations trait, not decided here.
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
