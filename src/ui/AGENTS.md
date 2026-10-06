# Working Notes: /src/ui

## Purpose

`ui` owns the session switcher: pure row-model transforms, side-effecting UI
operations, interactive behavior, rendering, and off-screen render dumps.

## Mental Model

The row model is side-effect-free logic over groups and sessions. `switcher/` is
the aggregate interactive TUI surface: selection, flattened rows, modal and input
BEHAVIOR and rendering, key and mouse handling, operation result application, and
render state. The runtime state owns the modal type and open-modal value; the
switcher reads and writes it and owns only transient popup geometry.

## Module Seams

- Pure row and group transforms belong in the row model.
- Every word a surface says about a key (the help, the key list, the selection hint)
  is read from the model's one key table. A surface never spells a key or its
  description itself, so it cannot drift from what the key does.
- UI colours come from the semantic palette, so the theme changes in one place. The
  chrome holds only the override layer over it (the per-role `[ui]` colour keys).
- A colour the USER named is parsed in the chrome, never in the palette: the
  palette holds xmux's own choices, which are slots only.
- Chrome rendering and its view-local state belong in the chrome; it reads
  inventory from the runtime state, not from the switcher.
- Slow (network) mux effects belong behind the operations module; a committing key
  emits a deferred-operation command for the run loop to spawn, and does not call
  the mux itself.
- Off-screen dump rendering belongs beside the other rendering code.
- Card LAYOUT geometry belongs in the column-flow module and stays pure: it takes card
  widths and run boundaries and returns rects, so the paint, the mouse hit-test and the
  tests all read one answer. Rendering reads that answer; it does not compute a second
  one.
- Other interaction state and rendering live in `switcher/` until a smaller seam
  exists for the specific surface being changed.

## Invariants

- Selecting and executing are separate inputs (`docs/principles.md`): the arrow keys move the hard
  selection, hovering sets the soft selection, and Enter on the hard selection and a
  click on the soft selection are one shared execution. Either selection shows its
  target (a hovered nav card shows its screen) but never moves the focus or runs
  anything. The soft selection never moves the hard selection, and the hard selection
  shows again when the pointer leaves.
- The selection is a node (host, source, session), not a row: a row is only where the
  node stands. The nav stays a list of numbered cards in sections: the card step and the
  section step never stop on a title, and a title part never takes a number. A section
  title's host part and source part are targets of the hierarchy only (`Ctrl+↑/↓`, the
  pointer, a click), and only the selected part inverts. A node with no nav target of its own (a source of a
  down host opened from a link) stays the selection while the nav stands on its
  nearest ancestor's target.
- Every colour xmux itself paints is an ANSI-16 slot or an attribute (reverse
  video, bold, dim), so the terminal theme resolves it, never an RGB value. The
  palette module documentation holds the rule and its exceptions; the palette is
  guarded so a stray RGB colour cannot reach it.
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
- Every in-flight marker in this layer reads its glyph from the one spinner helper on
  the frame the chrome advances, cards and the hint bar's scan progress alike, so
  nothing on screen turns out of step with anything else.
- Row transforms do not mutate their inputs unless the function name and
  signature make mutation explicit.
- Modal input owns keys while open; those keys must not leak to the terminal view
  or global shortcuts. At most one modal is open, because the state holds one
  optional modal, so opening any modal drops whatever was open.
- UI actions resolve to domain intents or application messages. The app update
  transition applies them; runtime and rendering code do not mutate UI state.
- This layer branches on nothing mux-specific: the switcher renders rows and emits
  domain intents, never a match on mux kind. Per-mux behavior lives behind the mux
  and driver seam, reached through the operations trait, not decided here.
- A rebuild resolves the selection from the interest alone, by the selection lineage
  (`CONTEXT.md`, Selection by Interest in `docs/principles.md`): the selected
  card holds by identity while it survives, a vanished card hands the selection to the
  nearest surviving card of its lineage, and an appearing card takes it only when it is
  the interest. No path picks a fallback row of its own. The rows are re-derived on
  every answer of the scan, so a selection re-picked from the top would walk from host
  to host as they arrive; the launch interest is the first session to appear, and once
  it lands it stays until the user or the mux moves it.
- Selection and drag helpers are invoked only inside the app update transition.
  The runtime observes the updated application model and executes emitted effects.
- A surface that exists to be READ never shortens what it states. A value too wide for
  its column continues beneath the same value column, a multi-line value keeps its lines, and a
  control character is written as its escape rather than printed as nothing: where a
  datum does not fit, the surface grows, and the datum is never the thing that gives
  way. This is why the reason, the probe command and the ssh stanza are on a screen and
  not on a card - the card had the room for none of them.
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

- Decide whether the change is pure row data, interactive state, rendering, or
  a side-effecting operation.
- For `switcher/`, find the existing helper for the same surface before adding
  another state path.
- Check focus and modal ownership before changing key handling.

## Verification

- Exercise the pure row transforms directly, and drive keys, mouse, modals, and
  rendering through the switcher for interactive changes.
- Re-check the dump output when its rendering helpers change: it and the main
  draw path must agree.
