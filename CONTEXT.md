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
  knows nothing about the mux. "host" is the concept; `Transport` is the
  trait.
- `Mux` (MUX axis) - the per-mux behavior trait (tmux / psmux / zellij / abduco /
  screen / tuios / herdr); a source
  holds one. "mux" is the concept; `Mux` is the trait.
- host - a machine that HOSTS muxes and that xmux can reach. The `roster` decides
  the set: of all the machines there are, the hosts are the ones it names. "machine"
  is the plain word for the thing in the world and names no abstraction here; the
  abstraction is the host. A host is never a host paired with one mux - that is a
  `source` - so a host serving several muxes is several sources under ONE host, and
  the host is the half of a source id that survives when the mux half is dropped.
  The host FOR a mux is a host that can run it; the host OF a session is the host
  that session runs on. A `Transport` reaches a host; it is not one, and several
  transports may reach the same host. A sentence about a host must stay true when one
  host serves two muxes; one that does not is about a source.
- `MuxDriver` - a mux's display driver, which the mux itself builds.
- the app - the runtime that owns the terminal: its loop, its focus state, and
  its input routing.
- `ViewFocus` - which screen region holds focus (nav or terminal).
- `Modal` - the mutually-exclusive focus-grabbing UI a prefix key opens: the inputs
  (filter, jump, new session, logout), the command palette, the help, the hosts to
  check, and the history. Every modal draws as a popup.

UI elements a user perceives as distinct things:

- split view - the whole two-region layout.
- nav view - the region holding the session cards, ordered local→WSL→remote then by
  source name, sessions by name (a left or right column, or a top or bottom band, per the
  nav's attachment; in a band the same cards run in a
  column flow). Never the
  "sidebar", and never the "tree": the on-screen VIEW is the nav view; "tree" names
  only the internal row-model module, which is still a Source to Session
  structure.
- terminal view - the other region: the selected session's live grid, holding the whole
  area the nav's side does not take.
- view border - the line between the nav and the terminal view: vertical beside a left
  or right nav, horizontal between a top or bottom nav and the terminal view. Dragging
  it resizes the nav. The rule uses the palette `primary` across its whole length while
  the nav is focused, `disabled` while the terminal is focused, and the hover colour for
  the drag-hover cue, so it states which view holds focus. `view-border-style`,
  `view-active-border-style`, and `view-border-hover-style` override these colours.
- active view border - the whole view border painted in the active colour while the nav
  holds focus; terminal focus paints it in the inactive colour.
- view border lines - `│` or `─` by default, `║` or `═` with auto-hide-nav, and `┃` or
  `━` while hovered.
- seam thumb - the stretch of a side column's view border drawn heavy (`┃`) beside the
  cards on screen when the list overflows, placed where those cards sit in the whole list.
  It is drawn on the view border rather than in a column of the nav, so the cards keep
  the nav's full width. Nothing is drawn while everything fits.
- chrome - the furniture around the two views: the view border, the hint bar, and
  the view screens.
- hint bar - the nav's prefix indicator: a label on the bottom row of a side column's
  nav region, and at the right end of the view border row in a band, so the terminal
  view keeps every row it owns and every band row holds cards. At rest it shows the
  prefix alone, and it keeps the prefix alone while a prefix interaction is live, since
  the key list beside it names the keys, and while an input is open, since the input's
  popup names its own. It shows one thing at a time, in order: a flash, the prefix while
  the key list or an input is open, the selection hint, the scan indicator, the active
  filter, then the resting prefix. A flash and the selection hint use the whole window's
  bottom rows beside a side column. In a band, the selection hint uses the view border
  beside the prefix, and a flash uses the rows below a top band's seam or above a
  bottom band's seam. With the nav hidden they use the window's bottom rows. A jump
  states its own refusal in its popup, so the bar keeps resting under it. The bar wraps instead of clipping. A flash paints the bar in the
  error style with a `✗` mark.
- key table - the one table of every key xmux binds, with the words that name each key.
  Both focus paths resolve a prefix command through it, and the help, the key list, and
  the selection hint are built from it, so what a surface says a key does and what the
  key does cannot drift apart.
- key list - the rounded box a live prefix opens at once from the prefix indicator
  toward the terminal view (beside a side column against the indicator's row, under a
  top band's seam or over a bottom band's seam at its right end, over the window's bottom
  left when the nav is hidden; the window's whole width when the terminal view beside a
  side column is too narrow for a box), titled with the prefix and naming every prefix key
  under its section title, in as many columns as that room holds. When the keys do not fit it
  first shortens every description, then gives up the keys needed least behind
  `+N more`; it never shows a key without its name. The xmux version sits on its bottom
  border.
- selection hint - what the hint bar says for three seconds after the user moves the
  selection: the selected card's most relevant keys and one fact about it (a session's
  windows, a host's state word and the reason behind it). Any key ends it and the next
  move replaces it. A selection xmux was told to make raises none.
  The first interactive key before the help-seen preference is recorded briefly names
  the configured prefix and its help key; later runs keep the resting indicator.
- view screen - what fills the terminal-view region in place of a mux while a selected
  host or source is shown, while a selected source scans or has settled without a
  session to show, or when xmux would mirror its own session. A settled-state card names
  the STATE; its screen has room to state WHY.
  The domain model chooses the screen from the selected node, typed host failure,
  scanning state, empty state, own-session address, and confirmed display. The UI
  renders that choice.
  Each level of the hierarchy has its own screen, whether or not the nav has a card for
  it. The HOST screen carries what belongs to the machine: how it is addressed (address,
  port, user), its reach state, the SSH login method, the public key, its last
  successful reach, the login form while a login is needed, the keys that re-scan it or
  log out of it, and one link per source with that source's session count or state. The
  SOURCE screen carries what belongs to one mux: a `{host}/{mux}` path whose host
  segment links to the host screen, how its list updates and when it was last listed,
  the key that creates a session there, and one link per session.
  A screen link is selectable: in terminal focus `↑`/`↓` (and `Tab`) step through the
  links, and `Enter` or a click opens the linked node's screen. Opening a link selects
  that node; a node with no nav target of its own leaves the nav on its nearest
  ancestor's target (see selection lineage).
  The scanning, settled host, and own-session states share one factual screen grammar,
  so a reader of any of them reads the others: the subject as the headline (a host for
  the host states, the session address for `own session`), under it the state word, then
  the rows that apply. A row is the key-column row the help also uses - a
  left-aligned cell, whitespace, then the value - where a bold cell is a key that can be
  pressed here and a muted cell names a datum. No value on a screen is shortened to fit
  its column: one too wide continues beneath the same value column, a multi-line one keeps its lines,
  and a control character is written as its escape rather than printed as nothing. The
  UNREACHABLE leads with a plain verdict carrying the last successful reach when
  known, then the failure run and the keys to check this host or every host.
  Its details choice unfolds everything known about the failure: the transport reason,
  what was asked and over what (the mux binary, how the machine is addressed and the
  wait that bounds it, the socket, and the session-listing command itself), the roster
  provider, the ssh stanza, what other muxes on that machine answered, and the log path.
  The BLOCKED state states the same failure
  facts and adds the login pane above them; the host stays blocked on any failed login
  and re-probes only itself on a successful one. The EMPTY state leads with the keys
  that start a session or rescan and follows with the latest observation facts. A
  selected host card or section title whose source is scanning shows the SCANNING
  state: `{host}/{mux}` as the headline, `scanning` as the state word, then that host's
  latest observation facts when any exist, with no key offered; its card keeps the
  in-flight spinner. A session grid of another source never shows under a scanning
  host card. The one exception is a full re-scan that turned the selected session card
  into the same source's host card: while the selection has not moved, the view keeps
  that session's confirmed grid, and the first selection move ends the exception. The
  monochrome Braille X rotation alone fills the view during an initial scan before a
  card can be selected. A symbol holds for one second, then turns over 0.4 seconds.
  A scanning or settled screen retains its text and centers the 32-column, 16-row
  animation in the rows below it only when the complete frame fits.
  `[ui] braille-animation = false` hides the central animation on every screen while
  nav activity spinners remain.
  A confirmed session shows its grid. The
  `own session` state's rows are why it is refused, and no key, because nothing pressed
  here would make it showable.
- nesting - xmux running inside a mux session. Allowed: the app attaches mux clients as
  PTY CHILDREN, so nothing it opens is a terminal handover that a mux would refuse. It
  costs one thing, the `own session`.
- own session - the mux session xmux is ITSELF running in, named once at startup from
  what the mux says (herdr, tuios, zellij, abduco, and screen put it in the
  environment; tmux and psmux are asked). The one
  address the terminal view refuses: mirroring it would attach a second client to the
  session holding xmux, moving the user's own client and painting xmux inside itself. A
  session running a DIFFERENT xmux is not it, and mirrors like any other.
- grid - the live terminal content drawn in the terminal view: xmux's in-memory
  cell mirror of the attached session's screen, fed by the terminal-emulation parser.
- cursor - the real terminal cursor placed over the grid at the mux's cursor cell
  while the terminal view is focused. "cursor" always means this text cursor,
  never the nav selection.
- card - one nav entry: a session card is a single row carrying the session name,
  with the `{host}/{mux}` label living on the SECTION TITLE above its group, never on
  the card itself. A host-state card (scanning / unreachable / empty host) is its own
  row naming the host. A host none of whose sources connected (every source is
  unreachable or needs a login, and none is scanning) shows as ONE host card naming the
  machine, at the place of its first source, however many sources it has. A card states WHAT something is; WHY it is that way is the
  screen's, never a card's. One card per SESSION; the mux a source's cards share is
  named once, on the section title, resolved at enumeration so several muxes on one
  host stay distinguishable. The loading card is gone: a session is a plain session
  card from the moment its host resolves.
- section title - the `{host}/{mux}` header row a source's session
  cards hang under, bold in the decoration role with nothing after it, its cards
  indented one cell under it at every nav position. A band column that continues a
  split section repeats it on its top row followed by `…`. It is not a card: it carries
  no number and stays outside ordinary card stepping and number jumps. It has two
  parts: the `{host}` part stands for the host and the `{mux}` part for the source.
  They are where the hierarchy meets the nav (see hierarchy), never cards: only the
  part the selection names takes the highlight, and a click on a part opens that
  node's screen; the indent selects nothing. `n` on one of the session cards
  creates a sibling in the same section.
- card focus - the one thing a card's rendering changes when it gains the selection:
  the number in its address column becomes the `❯` mark. It does not grow a context
  line, it does not change height, and its session name keeps the same column - a name
  that shifts as the cursor passes is what makes a list twitch. The selected look is
  the inverted rect (see selection highlight) plus the mark, nothing more. A section
  title takes the selected mark when one of its parts is selected, but no number, and
  inverts only that part.
- nav size - the nav's live geometry as one value: the width the user SET, the width ON
  SCREEN this frame (0 while auto-hide has taken it and no prefix interaction is live),
  the band height the user set (0 = auto), the side the nav is attached to, and whether
  the nav is collapsed. This shared domain value lives in `src/model/`. All five are settable while
  xmux runs, so every consumer takes the
  whole value rather than picking fields out of the runtime: the effective width
  has one owner, and a resize cannot reach the renderer and miss the PTY sizing. The set
  width and the on-screen width differ while the nav is hidden or a side nav is collapsed,
  and that is exactly why both travel: the regions are cut from what is on screen, while
  the set width is the one the nav returns to when shown and expanded.
- collapsed nav - the nav reduced to its prefix indicator while retaining its natural
  width and height. A left or right nav keeps a column exactly as wide as the prefix, the
  prefix unpadded on its bottom row. Its view border takes no column of its own: it runs
  down the column's terminal-side edge on every row above the prefix, so the prefix keeps
  every character and the terminal view takes every other column. A top or bottom nav
  keeps only its view border row, the prefix at its right end. Cards do not render and
  the view border remains. `prefix z` collapses and
  expands it, a view border drag past the nav's minimum collapses it, and a click
  anywhere on the collapsed nav or keyboard focus into the nav expands it. Auto-hide
  still removes the nav completely and returns it to the collapsed state.
- column flow - how a band lays its rows out: down a column, then right, identical for a
  top and a bottom band. A
  column takes whole SECTIONS (a `{host}/{mux}` title over its session cards), so a
  source's rows stay together under the one title naming them, and the section that
  does not fit opens the next column instead of splitting across the break. A section
  taller than the whole column is the one exception, having nowhere else to go: it
  splits, and the continuation column repeats the title on its top row, followed by `…`,
  with the section's cards under it. A band one row tall runs titles and cards along
  that row instead, unindented, and scrolls sideways.
  A column is as wide as its widest row - the section title, repeated or not, counted among them, so short
  session names cannot narrow a column under the title over it - columns are parted by
  one blank, and the flow is pure geometry, so the paint, the hit-test and the tests read
  one answer. A list would
  show three cards in a band twenty rows wide and leave the rest of every row blank; the
  flow is what makes the band worth its rows.
- source label - how a host and its mux are SHOWN: `{host}/{mux}`, one grammar wherever
  the pair is read (a section title, the screen it selects, the doctor's source
  list). Not the id's own separator, because an id is typed and a label is read, and a
  label parts its levels the way the rest of an address on screen does. Both halves
  always: a host serving one mux carries no mux in its id and still shows one, since a
  host seen with its mux on one title and without it on the next reads as two hosts. The
  name comes from the mux's KIND, not the binary that reached it, so an alias or a path
  cannot put a second spelling on screen. Empty only where nothing knows the mux yet,
  which a card marks with its spinner rather than by dropping the separator.
- nav groups - the nav lists actual session cards under their source titles, then
  reachable hosts with no sessions, then hosts whose connection or inventory is
  unresolved. Each group follows the source order. A blank row parts adjacent groups
  in a side column (the first visible boundary carries a horizontal rule while scrolling),
  and a blank column parts them in a top or bottom band. Content
  starts at the upper-left and unused space stays empty.
  When focus leaves the nav from a session card, only the session group is painted.
  When it leaves from either host group, every group stays painted. Returning focus
  to the nav paints every group. The focus decision holds while the terminal view
  keeps focus, including during prefix and modal interactions. The selected card is
  always painted: while the selection is on a host card, every group is painted.
  Painting fewer groups preserves card numbers and the selected card's identity.
- level color - the per-segment card color, from the palette. Every foreground role
  is ANSI-16, so the terminal theme resolves the hue. There is one TEXT colour, one
  ACCENT, and the section title's quiet header role: a session card reads as one
  neutral line with a single highlighted element - the session name, which takes the
  accent at normal weight. The title is bold and the card number dim, so the hierarchy
  remains visible without colour. The accent belongs to the LOWEST level the card displays:
  the session name on a session card, the mux on a host-state card that has a mux to
  name. A section title uses the decoration role.
  Each state glyph keeps its own colour:
  login needed uses `?` in the warning role, unreachable uses `▲` in the error role,
  and a listing failure uses `✗` in the primary role. The scanning spinner stays in
  the pending role. Every host-state card reserves one cell for its glyph. Only the
  selected host shows its state word; unselected cards retain the glyph alone. In a
  band, the state word floats over neighboring cells with one blank cell on each side
  and does not set the column width. Those blanks join its reverse-video highlight
  while the nav holds focus.
  A host-state card claims a mux only when the mux is CONFIRMED - a settled reachable
  host's enumeration answered through its mux, and a source id that names its own mux
  was resolved from what the machine actually serves; a section title's mux is
  confirmed the same way, because the source's enumeration answered through it. A
  bare-id host that is unreachable
  or still scanning claims none: the card reads the host alone. A scanning card's
  spinner trails its line in one fixed place, whatever the host has or has not resolved.
  The hint bar is two slots as well. Nothing here
  is an RGB value; see "Colour ownership" below for why, and `[ui] selection-style` /
  `[ui] hint-bar-style` for naming one anyway.
- card order - the one order the flat card list follows. It is deterministic: the
  hosts run local, then WSL, then remote, each tier by source name ascending, and
  inside a source its sessions run by name ascending, so one source's cards are
  contiguous and the nav never names a source twice. `rebuild` applies the order on
  every pass, and a re-enumeration reproduces the same order exactly, so the list never
  reshuffles under the user.
- card and section navigation - how the nav itself is walked. The nav is a list of
  CARDS grouped into SECTIONS, not a tree of the hierarchy: card and section are nav
  concepts, independent of session, source, and host. Every card is numbered (a session
  card, a reachable host with no session, an unresolved host), and a section is a
  source's title with its session cards, or the band of host cards. `↑`/`↓` step between
  the numbered cards and never stop on a section title; from a title part they go to the
  adjacent card. `←`/`→` step between sections.
- hierarchy - session, source, and host, the levels a session lives in. The nav does not
  show it as a tree; it is reached in three ways: `Ctrl+↑` walks up from a session to
  its source (its title's `{mux}` part, or its host-state card) and then to its host
  (the `{host}` part, or the host card), and `Ctrl+↓` returns to the child the walk came
  from, else the first child (sources by name, sessions in card order); a click or the
  pointer on a title part; and the links on a host's or a source's view screen.
- selection - the current pick, the HARD selection: arrows and execution move it, and
  the terminal view shows it. It names what it is on by identity, never a row position:
  a session by its address, a source by its id, a host by its machine name. A card is
  selected by card and section navigation; a title part, or a node with no card, only
  through the hierarchy. A re-enumeration or restream never moves it, so neither a
  re-sort nor a host answering late can take it.
- soft selection - what the pointer rests on: a nav target in nav focus, a screen link
  in terminal focus. It previews (the terminal view shows the hovered node's screen or
  grid) without moving the hard selection, and ends when the pointer leaves or focus
  moves to the other region. It paints as an underline, an attribute the terminal theme
  resolves. A click is the same execution as `Enter`: it opens the target's screen,
  focuses the terminal view, and makes the target the hard selection.
- interest - what the user is on or asked for, the one value the selection is resolved
  from on every rebuild (Selection by Interest in `docs/principles.md`).
  Before anything is chosen it is the first session to appear (the launch preselect);
  then it is the selected card; while a session the user asked for has no card yet (the
  session `n` created, the session under the selection when a full re-scan cleared every
  session) it is that session.
- selection lineage - the invariant every path that changes the list obeys. A node
  that loses its target moves the selection to the nearest node up its lineage that
  has one: a session to its source's target, a source to its host's target, a host card
  that resolved into sources to the first source by name, and when nothing of the host
  survives, to the first card after it in the prior card order that survived, else the
  last one before it. A logout or an unreachable host therefore gathers a selection on
  any of its sources or sessions onto its one host card. A node opened from a screen
  link with no target of its own stays selected while the inventory lists it. A card that APPEARS takes the selection only when it is the interest.
  Scans, re-scans, polls, logouts, a session ending, mux discovery, and the filter all
  resolve the selection through this one rule, and none picks a fallback of its own; the
  first card is taken only when no card of the prior list survives.
  A selection that lands on a source card shows that card's screen (information, login,
  unreachable, empty), never another session's grid.
- selection highlight - the selected card's rendering: reverse video filling the whole
  card, the terminal theme's own selected look, in both focus states,
  plus a `❯` mark standing in the address column of the card's row, where
  every other card carries its number. The horizontal view border colour identifies
  focus in a band. The inversion is uniform because the highlight
  pins both foreground and background to the terminal's defaults: inverting per span
  would turn each level color into a background and stripe the card. That same pinning
  is why the mark is an open shape and
  never a solid block: it draws inverted too, so a block fills its cell and disappears
  into the band while an outline keeps a readable silhouette.
  `[ui] selection-style` paints a named background instead. On a section title the
  inversion covers only the selected part.
- offscreen counts - what a band writes on its view border row when columns are off
  screen: `‹ 5` at the left end and `7 ›` before the prefix at the right. Cards, not
  columns, because the reader is hunting a session, not a column. They cost no row and
  say what a thumb cannot: which way the cards went, and how many. A click on one selects
  the hidden card nearest the visible ones. The key list opens off the seam row and
  leaves the counts readable.
- status row fill - how much of its row the hint bar paints. A floating bar (a refusal,
  a selection hint) fills the ROW: a solid bar, legible over whatever it
  covers. The resting prefix
  indicator paints its text plus a cell of padding and stops, leaving the rest of its row
  to the nav or the view border.
- spinner - the braille activity glyph marking the work still in flight. One
  glyph and one frame counter for the whole UI, so every marker on screen turns
  together. It stands on a SCANNING host's card, trailing the line in the same place
  every scanning card uses, on the hint bar's global scan count, and on the login pane's
  running step; a settled session
  card never spins, because a session is a plain session card the moment its host
  resolves.
- status - a host-state card's state: `?` for login needed, `▲` for unreachable,
  `✗` for a listing failure, a blank glyph slot for a reachable empty host, or the
  spinner while scanning. Only the selected card adds the corresponding state word.
  Not to be confused with the hint bar
  (below) or the `chrome`.
- blocked - a host ssh refused for a reason the submitted login answers, a state apart
  from unreachable. ssh's final account-and-host authentication line enters this state,
  and so does a host-key verification failure for a host with no recorded key when the
  effective policy is `ask`, since the submitted login can accept that key. An unknown
  key under a strict policy stays unreachable and names a command that displays its fingerprint. Remote command permissions, name resolution,
  connectivity failures, and a changed host key stay unreachable. A blocked card keeps
  the warning-coloured `?` mark, and
  shows that pane above the same failure facts the unreachable screen states, folded under
  the pane's details choice. A first-seen key opens the login form without a failure
  verdict; a failed login states its verdict over ssh's own sentence. What blocked the
  host is not in its state word. The transport diagnoses the ssh text and the
  inventory group exposes the typed failure.
- login pane - the form a blocked host's panel opens, or that the user opens for an
  unreachable host from the hosts-to-check table or command palette. It holds the three values ssh will
  not ask for and must know before it dials: the address, the port, and the username. A
  masked password field is optional beside them. Address and port start at what ssh WOULD
  use and come from OpenSSH's effective configuration when present,
  and each states that provenance beside its value. Missing values use a provider
  address or host name and port 22. The username comes from an exact host stanza if
  one names it; otherwise it starts empty and must be entered.
  Provisioning resolves those values and the matching ssh stanza before the app
  supplies them to the chrome. A required field is marked in its label; an empty optional one says so in
  the space its value would occupy. One radio choice follows: do nothing, record
  the values in ssh config, or register this machine's public key on the host.
  The connection values and that choice are two
  titled groups.
  The focused text value is reversed while the pane takes keys; section headings and
  whitespace group the inputs, choices, and result. A failure reads as a verdict in
  plain words, a `✗` on the field it concerns, ssh's own last line dimmed, and a details
  choice that unfolds ssh's whole text and the host facts. Enter means one thing
  throughout - submit from the button, pass the focus on from anywhere else - and Space
  picks a choice. It is not a modal and nothing in the nav drives it. The submitted password is held only in xmux
  process memory for that machine and is never logged, rendered, serialized, placed in a
  command argument, child environment, or file. The held credential allocation and the
  current password-field allocation are overwritten in full when released. Transient
  terminal and IPC buffers remain process memory; operating-system crash dump policy is
  outside xmux's control.
- running a login - one ordinary ssh command using a pending credential unavailable to
  every other command until that login succeeds. The ssh child receives a forced askpass
  environment holding an opaque per-command token for a private local broker, not the
  password. The token remains valid until the child is reaped and answers at most once.
  The submitted address, port, and user are included in a bounded effective ssh
  configuration query before the credential becomes available.
  The helper answers an OpenSSH password or keyboard-interactive prompt only when its
  account and host exactly match the held account and the target alias, resolved host name,
  or host-key alias. A destination configured with `ProxyJump` or `ProxyCommand` does
  not enter the password path because the proxy would inherit askpass. The helper
  refuses every other prompt. The submitted login
  accepts a new host key only when the effective ssh policy is `ask`, never weakens `yes`,
  and refuses a changed key; background commands keep the user's host-key policy. The pane says a
  login is under way in place of the button and takes no input but Esc. Below the rule it
  lists the login's steps in the order they run: connect, authenticate, the selected
  recording and key registration, then find mux, the re-probe a working login starts. Each
  step is pending, running (the spinner), done (`✓`), failed (`✗`), or skipped (`·`), and
  moves only on what the login reports: askpass handing over the password ends the connect
  step, the ssh child's verdict, each follow-up's own outcome, and the answer to the
  re-probe the login itself started followed by the first mux answer after it: a mux
  answering, no mux answering, or the search failing. The steps belong to one submission
  on one host card; a report from a replaced submission changes nothing. A key login shows
  no boundary between connecting and authenticating, so its connect step runs until the
  verdict. Its result keeps
  ssh's own sanitized, bounded diagnostic and a failure category. A later probe cannot
  replace that login diagnosis. A refusal that did not receive the held password remains
  visible. A refused password, pending-login cancellation, roster removal, or
  process exit forgets the matching credential, invalidates its outstanding tokens, and
  releases its held plaintext; a working login re-probes its machine and
  every direct ssh started by that running app can use the held password even when connection
  sharing is unavailable. A command removes a held password only when it actually received
  that credential, exits with ssh's connection-failure status, and carries ssh's own
  authentication refusal. A command holding a password still offers a key first. When
  the host drops the connection before a session starts, without refusing authentication
  and before askpass hands over the password, the host accepted the key and could not open
  a session for it, as a Windows sshd does for an Entra account. That command runs once
  more with key authentication off, within the first attempt's time budget. A drop after
  the session started is not retried, because the remote command may have run. Once the
  retry succeeds, every later command holding that password skips the key.
  A probe result tagged with an older credential generation
  cannot reclassify a machine after a newer login. The broker recreates its endpoint with
  backoff after an accept failure; while it is unavailable commands remain non-interactive
  and report that password login is unavailable. The separate command-line attach process has no access to the
  running app's credential and uses keys or ssh's terminal prompt.
- SSH authentication record - the information screen states the method OpenSSH
  reported for the selected session's live display connection: public key or
  username and password. A host card states the last method reported for its
  machine. A connection reused through an existing master may report no method,
  so the screen says `not observed` for that connection. When a held password
  disappears, or the user logs out, the machine's metadata and display clients close.
  A login or an explicit re-scan permits another connection.
- recording a login - what the pane's ssh config choice does once the connection works: an
  xmux-marked stanza naming the host, holding the values that reached it, written at the
  TOP of `~/.ssh/config` because ssh keeps the FIRST value it obtains for a keyword. The
  marker is what makes a second login replace the stanza instead of stacking, and what
  tells a reader which lines are xmux's. Nothing the user wrote is touched. A password is
  never recorded, because ssh config has nowhere to put one; the public-key choice is
  what stops the host asking again.
- logging out - `prefix L` on an SSH host names the selected session's observed
  authentication method and the affected machine, then requires typing `logout`.
  Before anything closes, it takes this machine's public key off the host over the
  machine's current connection: one command lists the lines of the host's key files whose
  key type and body equal one of this machine's public keys, ignoring options and the
  comment, and a second removes the chosen ones. Lines carrying the registration mark
  are chosen at once. A matching line without the mark is a key xmux did not add, and
  removing it also stops ssh outside xmux from using it, so a second confirmation asks
  first; confirming chooses it too, and closing the confirmation any other way keeps it.
  A host that cannot be reached or a removal that fails reports that the key remains and
  why, and the logout goes on. Then it discards that machine's in-memory password and
  closes its metadata and display connections and shared SSH master where present. SSH
  config is not changed. A pending login on that machine is cancelled when the logout
  starts, and its result cannot reopen it. The next requested connection can use a key
  the host still accepts; otherwise a login is needed.
- registering a key - what the pane's key choice does once the connection works. The
  login command reads the host's shell family, because the registration is a command for
  one family and a locked host's family is unknown until someone gets in. Registration is
  an ordinary ssh command over the machine's in-memory authentication and reports
  registered, skipped with a reason, or failed with ssh's reason in the login's toast,
  the log, and host information. A POSIX host gets the
  line in `~/.ssh/authorized_keys`; a Windows host gets it there too, and in
  `administrators_authorized_keys` when its sshd reads an Administrators member's keys
  from that file. The line it appends ends its comment with the mark `xmux-registered`,
  which sshd reads as free text and a logout reads as "xmux added this". It adds the line
  only when no key line in the file holds the same key type and body, so a second login
  changes nothing and a line the user added stays unmarked, and it makes this machine an
  ed25519 pair first when it has none.
  Registration then logs in once with the key alone, over a connection of its own that
  shares no master, never prompts, and runs a command that does nothing. Only that
  command running makes the result registered. A host that accepts the key and then
  cannot open a session would refuse every later command from this machine, which offers
  the key first, so the result is failed with the server's error and the marked line
  this registration added is removed; a line that was already there stays. A key login that
  fails before authentication finishes proves nothing about the key, so the result is
  failed as not verified and the line stays.
- address column - the leftmost column set of every card, holding the one thing that
  answers "where is this": the dim card number `prefix <digit>` jumps to, or, on the
  SELECTED card, the selection mark - the number there would be the address of where you
  already are. One column carries both, so a card's name never moves as the selection
  passes over it. It is written on the card's single row, beside the session it
  addresses; a section title is not a card and spends no number there at all (its
  `{host}/{mux}` label is flush left). The column is one width per frame, so the names
  stay aligned and the numbers line up by units place as the highest number crosses 10.
- card number - with `[ui] renumbering = true` (the default), the card's 1-based
  position in the current sorted nav list. Adding or removing a card, filtering,
  and scanning can change that number. A section title has no
  number. With `renumbering = false`, a card keeps the number it first takes for the
  run; an ended card leaves its number vacant, a new card takes the next number, and
  a full scan deals numbers again in list order. The list order, not the numbers,
  decides where a card sits.
- jump - the digits-only input `prefix <digit>` opens in a popup holding the
  digit, beside the name of the card the number names. It acts WHILE open: each edit moves the selection while the number names a
  card on the list, and a number no card carries (0, a vacant number, one past the
  highest) leaves the selection alone. Enter closes the popup when the number names a
  card and states in its popup that no card carries the number while leaving it open otherwise;
  Esc restores where it started. User-facing text calls this "jump to a session" (see
  the naming rule below).
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
  HOST name says which kind reaches it wherever the name alone would be ambiguous:
  `local` is this machine and `wsl.<distribution>` is a WSL distribution on it, everything
  else being an ssh destination. That is what lets a host named LATER (a mux-discovery
  answer carries a bare host name and nothing else) be reached exactly as one named at
  launch, and it is why an ssh alias spelled either reserved way is refused rather than
  served as the wrong kind. The
  nav renders the halves separately (`local/zellij`), so the id
  never appears with its mux twice. A source is held TWICE, once per consumer, and both
  copies resolve its host the same way: the event loop drives a source out of its
  runtime registry, and the off-loop operations resolve one out of the environment's
  source list. Discovery adds to BOTH - a source in only one of them
  paints and scans but refuses every operation, or the reverse.
- mux discovery - how a host's mux list is decided when it named no mux (`mux` unset
  or `auto`): every mux xmux supports is asked whether it is installed there, and each one
  that answers becomes a source. No mux is assumed for such a host: until it answers it
  serves no source and its card reads the host alone, and a host where nothing answers
  has no card. Two halves, in that order: the candidate set is what xmux
  can DRIVE, and the question asked of each candidate is the same identity probe a
  configured mux gets, so a binary carrying a mux's
  name while being another mux is not that mux (where psmux answers, a `tmux` that answers
  is psmux's own alias). A written `mux` value is never probed: it is taken verbatim,
  unreachable and all. Distinct from `roster` (which HOSTS) and `discovery` (scanning a
  source for SESSIONS).
  THIS BOX is resolved once off the runtime loop after the config-only first paint, and
  the answer is applied to both the source list and the runtime registry, so the two
  cannot disagree on which sources exist. A REMOTE machine is asked only AFTER it is
  found to CONNECT: discovery leads with a bounded machine `reachability` probe, and mux
  discovery (one task per machine, which nothing may wait for) fires only for a connected
  one, over the machine's own transport, which carries what its reachability probe and
  login established into every source found on it. The answer arrives as a source event,
  and the loop adds a scanning card for every mux the machine does not already serve. A
  machine that served nothing yet names its sources as a written list would (one mux
  takes the bare host alias, several are each qualified). Otherwise the add is
  ADD-ONLY: an added source's id is always qualified (`prod:zellij`) and the mux already
  served keeps the id it was painted with, because that id is what the deterministic
  order, the
  persisted selection, and typed ctl targets are keyed to.
- reachability - a machine's connect state, decided by ONE probe (`ssh <machine>
  true`, or an inline connect for this box) that leads discovery, bounded so a large
  roster never floods the network at once. Its three outcomes gate everything after:
  `connected` goes on to `mux discovery`, detection, and the metadata channels;
  `blocked` (a failure the login pane's values could fix) and `unreachable` (any other
  failure) classify the machine's cards and open no channel. The probe reads
  ssh's own failure text, so the reason a card shows is the machine's, not a guess; a
  connected probe also warms the shared ControlMaster its later channels reuse. Distinct
  from `mux discovery` (which muxes a connected machine serves) and `discovery` (a
  source's sessions).
- neighbour - a machine this box already reaches in ONE hop and that answers ssh, found
  in the operating system's own network state rather than from any VPN's client. Two
  records name the directly reachable: the routes to single machines, which a mesh VPN
  writes one of per peer (and sometimes one for two of them, so a route naming a handful
  of addresses is read as those addresses and a wider one as a network), and the
  neighbour table, which holds the machines on this link this box has exchanged frames
  with. A tunnel appears only in the first (it carries no ARP) and a switch only in the
  second, so both are read. Where the OS refuses the neighbour table outright, the link
  this box holds an address in is asked address by address instead. Neither record is a
  list of hosts, so what they give is narrowed: an entry that resolved to nothing names
  nobody, one hardware address answering for many addresses is a router rather than a
  machine, and what survives has to answer ssh, because a printer on the same switch is
  a neighbour and not a host. The name comes from whoever knows it - the system
  resolver, which holds what someone registered, then the machine itself over mDNS,
  which knows what it calls itself - and is adopted only when this box can resolve it
  back, since the name is also the ssh destination; the address stands in as the name
  when nothing resolvable answers. This box is told from its neighbours by the
  connection itself, whose two ends carry the same address only when it reached here.
- roster - which HOSTS xmux offers, assembled from PROVIDERS, EVERY one on unless
  `[discovery]` turns it off: `~/.ssh/config` aliases, this machine's NEIGHBOURS, and
  this machine's WSL distributions. Every provider yields plain ssh
  target names, so nothing downstream BEHAVES differently for one; which provider
  offered a name is kept beside it and shown on the unreachable host's view screen, never read
  to decide anything. The roster is what makes a machine a
  host: a machine no provider names is one xmux has nothing to say about. The first frame
  uses config-only host skeletons while every provider resolves off the runtime loop; the
  completed roster is then applied through the same reconciliation used on every re-scan.
  A re-scan result is reconciled by
  MACHINE: a machine that is still named keeps the sources it serves, including the ones
  `mux discovery` found rather than config, and a machine that is not named loses every
  source it served. Distinct
  from `mux discovery`, which asks a host WHICH MUXES it serves, from `discovery`,
  which scans a source for sessions, and from the host axis, which reaches one.
- filter - the type-to-filter input over the nav list. It applies as you type: each
  edit re-filters the cards, the selection holds its card while that survives and
  moves along its selection lineage within the visible cards otherwise. Its popup's top border counts the cards
  kept of the cards listed, and matching characters are bold.
  Esc restores the filter the input opened with; with the input closed, Esc clears an
  active filter. The hosts-to-check table and command palette can select a host by name.
- hosts to check - the table `prefix h` opens: every host in a problem state grouped by
  cause, each with its reason.
  Enter or a click on a row selects that host's card
  and opens the login pane for a blocked or unreachable host.
- command palette - the searchable popup `prefix :` opens. It lists named actions
  from the key table and login entries for blocked or unreachable hosts. Enter runs
  the selected action and a click runs the clicked one; Esc closes it.
- one-host re-scan - `prefix R`: the selected card's machine asked again alone, its
  reachability probe and then every source it serves, reported in its own summary toast.
  A full re-scan (`prefix r`) asked meanwhile takes over.
- a source scan has one ten-second budget shared by first contact and the session
  listing; a slow first contact leaves only the remaining time for enumeration.
  A card still scanning after ten seconds reports a timeout and stops spinning.
- flash - the reason a key did nothing, shown in the hint bar (a new session on an
  unreachable host, a logout confirm without the word). A jump number no card carries
  is stated in the jump's popup instead. It goes away on the next tree key, and
  after ten seconds for a user who presses nothing, since it is about something that
  already happened. A flash is a refusal, never the result of work: that is a toast.
- toast - the result of work the user started (a login and what it registered, a new
  session, a re-scan's summary of what changed), or the release notice at launch, in a
  rounded box floating in the terminal view's corner nearest the hint, at most 40% of
  the window wide. It avoids the prefix key list and floating hint. A toast of successes
  and facts leaves after five seconds and fills its bottom border with a bold accent
  line for the share of that life still ahead. A login result leaves after five seconds
  even when it contains a warning or failure, since the login pane and history keep its
  details. Other toasts carrying a warning `▲` or a failure `✗` stay until a click on
  them or opening the history dismisses them.
  `[ui] notifications` turns toasts off.
- history - the bounded record of every toast and every background event, opened with
  `prefix m`, newest first. A background event is one nobody asked about (a host that
  stops answering outside a re-scan) and is recorded without a toast. When full, it drops
  its oldest success or info record before any warning or failure.
- scan indicator - the `scanning hosts n/m…` progress shown in the hint bar while
  host probes are in flight (a narrow row shortens it to `scanning n/m…`, then to the
  bare `n/m`), behind the same spinner on the same frame as the cards it counts. It
  counts SOURCES; a scanning host's card spinner trails that host's card.
- ready - the state while a prefix interaction is live. A prefix key sets it; it
  clears when the interaction's FUNCTION ENDS, or on a focus switch / mouse action
  (a CANCEL). Most functions end with their command key (even a no-op like focusing
  the already-focused view); an input row's function ends when Enter or Esc closes
  the row; a resize's function ends when its repeat window lapses. A second prefix
  is the doubled-prefix command (one literal prefix byte reaches the pane). The
  key list reads ready to open beside the prefix indicator, so becoming ready is a
  visible change and redraws the frame; the list closes the moment ready clears.
- popup - the rounded-bordered, opaque box a modal draws, moved by a drag from anywhere
  on it, in the key list's
  grammar: its accent title and a muted count or machine in the top border, its keys on
  the bottom border, and a text field's caret as a reversed cell, where the terminal's
  own cursor also sits so an input method composes in the field. Every popup opens
  where the key list opens, growing the way it does, so a prefix key replaces the key
  list with its popup in the same place in every nav layout. Every row of a popup wraps
  to its width, a description under its own column, so no size cuts a word; only a text
  field stays on one row. The help lists the key table section by section and then the
  glyph legend as one document, a blank row between two sections, searched by typing and
  scrolled by the arrows. Its tab row names the sections: `←`/`→` or a click moves the
  active tab and scrolls that section to the top, and a scroll moves the active tab to
  the section at the top. A popup's pickable items (the help's tabs, the hosts to check,
  the palette's commands) follow Separate Selection and Execution
  (`docs/principles.md`): the arrows move the hard selection, the
  pointer over an item is the soft selection, underlined, and a click on an item
  executes it as Enter on it would. A hovered tab shows its section until the pointer
  leaves the tab row. A key ends the soft selection. The whole box, its items included,
  is its drag handle: a press that moves is a drag, a press released where it was
  pressed is a click.

A zellij TAB is a `window` and a zellij SESSION is a `session`: xmux uses
one set of words for every mux, so a mux's own naming is translated at its implementation
boundary and nowhere above it.

### Prefix interaction

The prefix is a single `ready` state. It is set by the prefix key and cleared when
the function it started ends:

| Event | ready |
| --- | --- |
| prefix key | set |
| a command key whose function ends with it | clear |
| a command key that opens an input row | held until Enter / Esc closes the row |
| a resize command | held until its repeat window lapses |
| a focus switch or a mouse action | clear (canceled) |
| a second prefix (terminal view) | clear, one literal prefix byte to the pane |

The key list and the auto-hide nav show for the whole time ready is set, the key list
giving way to the input's popup while an input row is open. Because ready spans the
function rather than the keystroke, the list stays up across a resize burst, and it
closes once by itself.

A terminal reports no key-up, so a held prefix's autorepeat is byte-identical to
repeated taps and takes the doubled-prefix path: it streams literals to the pane
and blinks the key list. That is accepted rather than fixed; reading a key-up would
mean depending on the kitty keyboard protocol, which every terminal and every
enclosing mux in the chain would have to pass through.

`pane` is reserved for a mux window's terminal split (a tmux / psmux pane); it is
never a screen region - screen regions are "views", and the line between them is
the `view border`. A refused key's reason in the hint bar is a `flash`; the result of
work the user started is a `toast`, never a "notice". A card's trailing state is a `status`, never a "hint". The reverse-video
selected card is the `selection highlight`; `cursor` names only the terminal's text
cursor (the grid's, or the one on a focused field's caret). The furniture around the views is the `chrome`, never a "status surface".
The switcher's rendered screen is the "switcher screen", never an "overlay".

## Architecture - the orthogonal design

Two orthogonal axes describe every connection, and no module conflates them:

- HOST - `src/transport/`. Each host implementation owns its execution behind the
  `Transport` trait; a source builds one at construction, so host selection is
  never a central `match`. Shared shell helpers (quoting, remote command
  assembly) lives beside the implementations. `Transport` owns where a command runs and
  how its argv is executed; it knows nothing about the mux.
- MUX - `src/mux/<kind>/`. Each mux implementation (`tmux/`, `psmux/`, `zellij/`,
  `abduco/`, `screen/`, `tuios/`, `herdr/`) owns its metadata and command plans behind the `Mux` trait
  and its display driver beside them. A mux builds its OWN driver, so mux selection
  lives in the mux implementation,
  never a central `match`. Shared mux builders live beside the implementations. The
  trait's command plans default to tmux-compatible argv, so a tmux-compatible mux
  is identity plus a few overrides; a mux that shares no argv (zellij) overrides
  every plan AND the shape of what each plan prints, since a plan and its output
  are one decision.

Attach argv is composed from a source's own mux + transport (the two axes
together), so the two implementations are combined without either knowing the other.

The supervisor branches on NOTHING mux-specific. `src/app/` (runtime loop,
input routing, ctl serving, preference persistence), `src/ui/` (switcher / rows /
chrome and modal rendering), and `src/state/` (the runtime state, focus, modal and
chrome data, and its domain reducers) select display through the source's own driver and read the
grid back from it; per-mux behavior lives behind that seam. These layers carry
no PTY, grid, or terminal-protocol logic.

The remaining layers each own one concern:

- `src/display/` - the mux- and app-agnostic PTY/grid/input mechanics (attach
  spawning, the grid, input decode, terminal setup, dispatch, the registry, the
  worker).
- `src/link/` - per-source connection management (control-mode reader/writer,
  poll tasks, live client ownership).
- `src/transport/` - the transport axis: the `Transport` trait, the local and ssh
  implementations, and the shared shell helpers.
- `src/provision/` - resolution: the TOML config, the roster of ssh targets, the
  concurrent source probe, and the resolved runtime view over them.
- `src/cli/` - the CLI surface: argument parsing and command dispatch.
- `src/model/` - domain types: sources, selection, nav geometry, actions,
  commands, event effects, and the server model.
- `src/driver.rs` - the mux-agnostic `MuxDriver` trait, the supervisor
  capabilities a driver borrows, and the thin wrapper that resolves a source's
  driver. It names no concrete mux type.

### Layer Direction

Backend code depends sideways or downward and never reaches into application,
presentation, or application-state ownership. State depends on backend code and
itself, never on application or presentation code. A source file belongs to the
directory directly below `src/`, or to the root module named by its `.rs` file.

| Importing module | Allowed target modules |
| --- | --- |
| `display`, `driver`, `link`, `logging`, `model`, `mux`, `provision`, `session`, `transport` | `display`, `driver`, `link`, `logging`, `model`, `mux`, `provision`, `session`, `transport` |
| `state` | `display`, `driver`, `link`, `logging`, `model`, `mux`, `provision`, `session`, `state`, `transport` |
| `app`, `cli`, `lib`, `main`, `ui` | `app`, `cli`, `display`, `driver`, `link`, `logging`, `model`, `mux`, `provision`, `session`, `state`, `transport`, `ui` |

The repository architecture check scans production source and `#[cfg(test)]`
modules. Its known-exception list is empty, and any disallowed edge fails the
check.

### View Purity

The View Purity rule requires rendering to read the application model and write
only the frame. Its required data direction is application model, immutable
`RenderPlan`, frame. Paint and input hit-testing must consume the same plan
without either owning or mutating it.

### Single Update Owner

The Single Update Owner rule permits only the update transition to mutate
application state. One application model owns domain state, switcher interaction
state, navigation geometry and preferences, mouse state, connected and detecting
source sets, and the last immutable render plan. Key, mouse, semantic ctl, source
event, operation result, tick, resize, and configuration inputs are messages to
the transition. The transition applies domain actions as one part of the same
flow and emits a single effect type. One exhaustive runtime executor handles
command, source, persistence, attachment, and login effects in their emitted
order. Raw terminal bytes are the sole direct path because they are payload for
the selected terminal display rather than an application-state transition.

## Colour ownership

The principle is Terminal-Owned Colour in `docs/principles.md`. This section is how
the palette keeps it.

A THEME is a named role→ANSI-slot assignment, and a theme system curates them: the
built-ins are `auto-dark` (the default) and `auto-light`, each an ANSI-only theme for a
dark or a light terminal background. `[ui] theme` names one; an unknown name falls back
to `auto-dark` and `xmux doctor` reports the resolution. The two built-ins are the whole
current set, and adding a theme is adding one registry entry plus its tests - the way
the system keeps growing without loosening the invariant below. Selecting a theme does
not pick colours: the theme IS the slot mapping, and both ends (the accent on the
cards, the `bar_accent` on the hint bar) stay within the slots.

The `[ui]` presentation settings - theme / selection-style / hint-bar-style /
view-border styles / notifications / braille-animation / renumbering - are re-applied LIVE when `config.toml` changes: the redraw
cadence stats the file (a cheap poll, no watch dependency) and a changed mtime
reloads just that section, keeping the previous settings on a malformed edit. The
roster and hosts are not part of it - re-scanning sources is the `rescan` key's job
and a config edit must not reset the user's sessions.

- The palette is the sixteen slots (one per UI role) plus ATTRIBUTES: reverse
  video, bold, and dim. Nothing else. An RGB colour, or an indexed colour above 15, is a hue
  xmux chose for somebody else's terminal, and it is wrong on every
  theme it was not chosen for. The palette is guarded so one cannot reach it.
  A nonempty `NO_COLOR` resets xmux's palette and configured chrome colours;
  selection remains visible through reverse video.
- Anything the sixteen slots cannot say is said with an attribute instead. "One step off
  the background" is the case that keeps coming up, and it is not a slot: so the selected
  card is REVERSE VIDEO, the terminal swapping its own pair, which is what a theme itself
  means by "selected". Not a computed surface - computing one needs the terminal's
  background, and a terminal is free to answer no colour query at all (Windows Terminal
  answers none), which leaves a fixed fallback as the permanent state rather than a rare
  one.
- The exceptions are colours the USER names: the per-role keys (`[ui] primary`,
  `secondary`, `accent`, `decoration`, `warning`, `error`, `disabled`, and the hint
  bar's `bar-bg`/`bar-fg`/`bar-accent`), plus `[ui] selection-style`,
  `[ui] hint-bar-style`, and the view-border colours. Their terminal, their choice.
  The chrome's colour mapping is that palette and the only place a `#rrggbb` may
  enter.
- A colour a CHILD program emits passes through untouched: it is that program's own
  choice against the same theme, and xmux is not in it.

A new colour goes into the palette as a slot, or it does not go in.

## Adding a module

At creation time, place a new source file by the axis it belongs to:

- Host-specific → a new host implementation is a new module under `src/transport/`
  implementing `Transport` (plus its factory); new per-host execution goes in
  the existing local or ssh implementation.
- Mux-specific (a new mux implementation or per-mux behavior) → `src/mux/<kind>/`.
- PTY / grid / terminal-protocol mechanics → `src/display/`.
- Orchestration (runtime loop) → `src/app/`.
- Per-source connection management → `src/link/`.
- Domain types → `src/model/`.
- Provisioning (config / roster / discovery / resolved env) → `src/provision/`.
- CLI command surface → `src/cli/`.
- Switcher / nav rows / status UI → `src/ui/`.
- Runtime state, focus, modal data, and chrome data → `src/state/`.

Then, if the module introduces a new directory, create that directory's
`AGENTS.md` in the Working Notes format `docs/AGENTS.md` defines.

## Improvement Notes

- Per-source session inventory has a single owner: the source's own
  inventory. Both metadata paths feed it through source events - the control reader
  carries its parsed sessions, and the poll task carries the same
  - the run loop folds them in and rebuilds the nav rows from it. The source
  manager owns the live mechanisms (control clients and poll tasks). Keep live
  process/task ownership out of the source domain type, and do not add a third
  per-source registry.
- A source definition is thin per-source config/data. The CLI, the scan, and the
  off-loop operations assemble a runtime source from it and drive
  enumerate/manage/attach through the source, mux, and transport APIs; the host
  boundary (argv assembly, ssh transport) lives entirely in the transport, and the
  psmux registry helpers live in the psmux implementation. The runtime source registry is
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
