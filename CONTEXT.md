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
  transports may reach the same host.
- `MuxDriver` - a mux's display driver, which the mux itself builds.
- the app - the runtime that owns the terminal: its loop, its focus state, and
  its input routing.
- `ViewFocus` - which screen region holds focus (nav or terminal).
- `Modal` - the mutually-exclusive focus-grabbing UI (the help and the inline
  input). A popup is its one focus sub-kind: a draggable centered dialog, and only
  the help is one.

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
- view border - the line between the two views: vertical between the two columns, horizontal
  between the two bands. Modelled on tmux's pane
  border, but it borders views (not panes), so it is a `view border`, never a
  "pane border" or a bare "divider". Its colour is FIXED and the same on every
  source: the palette `primary` across the whole rule while the nav is focused,
  `disabled` across it while the terminal is focused, and the hover colour for the
  drag-hover cue. The border states which VIEW holds focus,
  which is a fact
  about xmux and not about the mux on the other side of it, so nothing a host or a
  mux reports may move it. Its color config keys are `view-border-style` /
  `view-active-border-style` / `view-border-hover-style`. These keys are OVERRIDES:
  unset (empty), that side keeps the fixed colour; a non-empty key replaces it.
- active view border - the whole view border painted the active color while the nav
  holds focus (tmux `pane-active-border-style`); terminal focus paints the whole rule
  with the inactive color.
- view border lines - the view border's line-drawing style (tmux
  `pane-border-lines`): `single │` (default), `double ║` (auto-hide-nav on),
  `heavy ┃` (hover - the drag-resize grab cue; the seam thumb of an overflowing side nav
  also draws `┃` at rest, in the normal border color).
- chrome - the furniture around the two views: the view border, the hint bar, and
  the view screens.
- hint bar - the nav's prefix indicator: a label on the bottom row of a side column's
  nav region, and at the right end of the view border row in a band, so the terminal
  view keeps every row it owns and every band row holds cards. At rest it shows the
  prefix alone, and it keeps the prefix alone while a prefix interaction is live, since
  the key list beside it names the keys. It shows one thing at a time, in order: a flash,
  an input line, the prefix while the key list is open, the selection hint, the scan
  indicator, the active filter, then the resting prefix. An input line, a flash, and the
  selection hint use the whole window's bottom rows beside a side column. In a band,
  the selection hint uses the view border beside the prefix, while an input or flash
  uses the rows below a top band's seam or above a bottom band's seam. With the nav hidden
  they use the window's bottom rows. The indicator keeps the prefix in a band. The bar
  wraps instead of clipping. A flash paints the bar in the
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
  host scans or has settled without a session to show, or when xmux would mirror its
  own session. A settled-state card names the STATE; its screen has room to state WHY.
  The domain model chooses the screen from the selected address, typed host failure,
  scanning state, empty state, and own-session address. The UI renders that choice.
  The settled host and own-session states share one factual screen grammar, so a reader
  of any of them reads the others: the subject as the headline (a host for the settled
  host states, the session address for `own session`), under it the state word, then
  the rows that apply. A row is the key-column row the help also uses - a
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
  the full history - then the rescan key. The BLOCKED state states the same failure
  facts and adds the login pane above them; the host stays blocked on any failed login
  and re-probes only itself on a successful one. The EMPTY state's rows are the keys that start a session or rescan. A host
  still scanning shows a monochrome Braille X rotation in the terminal view only when
  no session grid has been confirmed; its card keeps the in-flight spinner. A full
  re-scan preserves the confirmed grid while session cards temporarily become host
  cards. The same animation fills the view during an initial scan before a card can
  be selected. Its fixed 32- or 64-column frame is centered and
  clipped in smaller views; one symbol holds for one second and turns in 0.4 seconds.
  A settled screen retains its text and centers the animation in the rows below it
  only when at least a 32-column, 16-row frame fits. A confirmed session shows its grid. The
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
  row naming the host. A card states WHAT something is; WHY it is that way is the
  screen's, never a card's. One card per SESSION; the mux a source's cards share is
  named once, on the section title, resolved at enumeration so several muxes on one
  host stay distinguishable. The loading card is gone: a session is a plain session
  card from the moment its host resolves.
- section title - the non-selectable `{host}/{mux}` header row a source's session
  cards hang under, dim (the decoration role) with nothing after it, its cards
  indented one cell under it at every nav position. A band column that continues a
  split section repeats it on its top row followed by `…`. It is not a card: it carries
  no number, the selection can never land on it, and a click on it or on the indent
  selects nothing. `n` on one of its session cards creates a sibling in the same
  section.
- card focus - the one thing a card's rendering changes when it gains the selection:
  the number in its address column becomes the `❯` mark. It does not grow a context
  line, it does not change height, and its session name keeps the same column - a name
  that shifts as the cursor passes is what makes a list twitch. The selected look is
  the inverted rect (see selection highlight) plus the mark, nothing more. A section
  title never takes either.
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
  width and height. A left or right nav keeps a column as wide as the prefix with a cell
  either side; a top or bottom nav keeps only its view border row, the prefix at its
  right end. Cards do not render and the view border remains. `prefix z` collapses and
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
- nav bands - the two bands the nav's rows fall into: the session cards (each under
  its section title), then the cards of the hosts with no session to show, which sit
  below every session card whatever order the hosts were scanned in. In a column
  the parting is the ROOM between them while the cards can spare a row for it (the
  sessions hold the top edge, the host cards the bottom), and a rule across the cards
  once they cannot and the column scrolls as one list, because a gap parts only what a
  reader sees at once. The parting always has a row: the column is measured with the
  rule's row counted in, so a gap of one is the last thing before the rule and the bands
  never meet, at the price of scrolling a row early. In a band the parting is
  horizontal: the session columns hold the left edge, the host band is pushed to the
  right while a blank column parts them, and a vertical rule takes the boundary's column
  once they cannot (the run scrolls a column early for the same reason). Neither parting
  is a card, so a click on one selects nothing. A list with NOTHING but host cards is
  the host band alone, and it still takes its side of the split: anchored to the
  bottom (column) / right edge (band), the blank rows or columns opposite being where
  the sessions that will be found land, so a scan reads as the pending hosts draining
  toward the sessions they become. The host band is hidden while the terminal view
  holds the focus when a session card was selected on the move into it, and shown
  again on the move back into the nav or once the selection reaches a host card; a
  host card selected on the move keeps it. A live prefix paints the band while it lasts,
  since its hint bar offers a jump to any card by number, and the band is hidden again
  when the prefix ends. Hidden cards leave the screen, not the list, so card numbers do
  not shift.
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
  band, the state word floats over neighboring cells and does not set the column width.
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
- selection - the nav's current pick, advanced by navigation; a re-enumeration or
  restream never moves it: the order is identical on every rebuild, and the session
  under the cursor is held by identity across one, so neither a re-sort nor a host
  answering late can take it. The preselect and the
  reselect are the launch and post-rescan selections.
- selection highlight - the selected card's rendering: reverse video filling the whole
  card, the terminal theme's own selected look, while the nav holds focus,
  plus a `❯` mark standing in the address column of the card's row, where
  every other card carries its number. While the terminal holds focus the card keeps the
  mark alone, so the selection and the view border colour say the same thing about the
  focus. The inversion is uniform because the highlight
  pins both foreground and background to the terminal's defaults: inverting per span
  would turn each level color into a background and stripe the card. That same pinning
  is why the mark is an open shape and
  never a solid block: it draws inverted too, so a block fills its cell and disappears
  into the band while an outline keeps a readable silhouette.
  `[ui] selection-style` paints a named background instead.
- seam thumb - the stretch of a side column's view border drawn heavy (`┃`) beside the
  cards on screen when the list overflows, placed where those cards sit in the whole list.
  It is drawn on the view border rather than in a column of the nav, so the cards keep
  the nav's full width and the selected card's inverted rect never runs under a thumb.
  Nothing is drawn while everything fits.
- offscreen counts - what a band writes on its view border row when columns are off
  screen: `‹ 5` at the left end and `7 ›` before the prefix at the right. Cards, not
  columns, because the reader is hunting a session, not a column. They cost no row and
  say what a thumb cannot: which way the cards went, and how many. A click on one selects
  the hidden card nearest the visible ones. The key list opens off the seam row and
  leaves the counts readable.
- status row fill - how much of its row the hint bar paints. A floating bar (an input
  line, a refusal, a selection hint) fills the ROW: a solid bar, legible over whatever it
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
  the warning-coloured `?` mark, is never hidden by hide-unreachable (it is the one entry to the login pane), and
  shows that pane above the same failure facts the unreachable screen states, folded under
  the pane's details choice. What it was blocked ON is not in its state word: the pane
  states a verdict over ssh's own sentence. The transport diagnoses the ssh text and the
  inventory group exposes the typed failure.
- login pane - the form a blocked host's panel opens, holding the three values ssh will
  not ask for and must know before it dials: the address, the port, and the username. A
  masked password field is optional beside them. Every value starts at what ssh WOULD
  use. Address, port, and user come from OpenSSH's effective configuration when present,
  and each field states that provenance beside its value. Missing values use a provider
  address or host name, port 22, and this machine's account
  name. Provisioning resolves those values and the matching ssh stanza before the app
  supplies them to the chrome. A required field is marked in its label; an empty optional one says so in
  the space its value would occupy. Two choices follow: whether to record the values, and
  whether to register this machine's public key on the host. The record choice appears
  only once a value differs from what ssh would have used, since a stanza repeating what
  ssh already resolves records nothing. The connection values and the two choices are two
  titled groups. A recent list between them offers successful connection values from
  this run without passwords; selecting an entry fills the three connection fields.
  The focused stop's name is reversed while the pane takes keys, and a rule
  parts the inputs from the login's steps and its failure. A failure reads as a verdict in
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
  authentication refusal. A probe result tagged with an older credential generation
  cannot reclassify a machine after a newer login. The broker recreates its endpoint with
  backoff after an accept failure; while it is unavailable commands remain non-interactive
  and report that password login is unavailable. The separate command-line attach process has no access to the
  running app's credential and uses keys or ssh's terminal prompt.
- remembering a login - what the pane's record choice does once the connection works: an
  xmux-marked stanza naming the host, holding the values that reached it, written at the
  TOP of `~/.ssh/config` because ssh keeps the FIRST value it obtains for a keyword. The
  marker is what makes a second login replace the stanza instead of stacking, and what
  tells a reader which lines are xmux's. Nothing the user wrote is touched. A password is
  never recorded, because ssh config has nowhere to put one; the public-key choice is
  what stops the host asking again.
- registering a key - what the pane's key choice does once the connection works. The
  login command reads the host's shell family, because the registration is a command for
  one family and a locked host's family is unknown until someone gets in. Registration is
  an ordinary ssh command over the machine's in-memory authentication and reports
  registered, skipped with a reason, or failed with ssh's reason in the login's toast,
  the log, and host information. A POSIX host gets the
  line in `~/.ssh/authorized_keys`; a Windows host gets it there too, and in
  `administrators_authorized_keys` when its sshd reads an Administrators member's keys
  from that file. It adds the line only when that line is absent, so a second login
  changes nothing, and it makes this machine an ed25519 pair first when it has none.
- address column - the leftmost column set of every card, holding the one thing that
  answers "where is this": the dim card number `prefix <digit>` jumps to, or, on the
  SELECTED card, the selection mark - the number there would be the address of where you
  already are. One column carries both, so a card's name never moves as the selection
  passes over it. It is written on the card's single row, beside the session it
  addresses; a section title is not a card and spends no number there at all (its
  `{host}/{mux}` label is flush left). The column is one width per frame, so the names
  stay aligned and the numbers line up by units place as the highest number crosses 10.
- card number - the number a card takes the first time it appears and keeps for the
  whole run. A card that ends leaves its number VACANT: no other card's number shifts,
  and a new card takes the next number past the highest one given. A full scan (the
  launch scan and every `prefix r`) deals the numbers again from 1 in list order while it
  runs, so it ends with numbers that read in list order; a one-host re-scan, a filter,
  and a nav scope change leave every number where it is. A session that returns under
  its own name takes its number back. The list order, not the numbers, decides where a
  card sits.
- jump - the digits-only input `prefix <digit>` opens in the hint bar holding the
  digit. It acts WHILE open: each edit moves the selection while the number names a
  card on the list, and a number no card carries (0, a vacant number, one past the
  highest) leaves the selection alone. Enter closes the popup when the number names a
  card and flashes the range up to the highest number while leaving it open otherwise;
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
  lands on the first remaining card otherwise. The input states the total matches and
  how many matching hosts are normally hidden, and matching characters are bold.
  Esc restores the filter the input opened with; with the input closed, Esc clears an
  active filter. A host hidden from the nav (`[ui] hide-unreachable`) shows its card
  while the filter names it.
- nav scope - which cards the nav lists: `sessions` (the default, with the hidden hosts
  left out), `all hosts` (nothing hidden), or `needs attention` (only the hosts in a
  settled problem state, no session). `prefix s` steps it, it is named only while the
  user interacts (the key list's bottom border, a toast when it steps), and it is
  remembered across runs.
- hosts to check - the table `prefix h` opens: every host in a problem state grouped by
  cause, each with its reason and a mark on the ones the hiding leaves without a card.
  Enter on a row selects that host's card, bringing a hidden host back through the
  filter, and focuses the terminal view for a host whose login pane answers it.
- one-host re-scan - `prefix R`: the selected card's machine asked again alone, its
  reachability probe and then every source it serves, reported in its own summary toast.
  A full re-scan (`prefix r`) asked meanwhile takes over.
- flash - the reason a key did nothing, shown in the hint bar (a jump number no card
  carries, a new session on an unreachable host). It goes away on the next tree key, and
  after ten seconds for a user who presses nothing, since it is about something that
  already happened. A flash is a refusal, never the result of work: that is a toast.
- toast - the result of work the user started (a login and what it registered, a new
  session, a re-scan's summary of what changed), or the release notice at launch, in a
  rounded box floating in the terminal view's top corner farthest from the nav (bottom
  right when the nav rides on top), at most 40% of the window wide. A toast of successes
  and facts leaves after five seconds and underlines its first line for the share of that
  life still ahead; one carrying a warning `▲` or a failure `✗` stays until a click on it
  or opening the history dismisses it. `[ui] notifications` turns toasts off.
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
- popup - the rounded-bordered, opaque, centered (draggable) dialog a popup modal
  draws, its accent title in the top border. The help and the history are popups; an
  input renders in the hint bar instead, reading `[feature] guide: <buffer>` with a
  reversed-block caret at the edit position. The help lists the key table section by
  section and then the glyph legend, searched by typing and scrolled by the arrows.

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
giving way to the input line while an input row is open. Because ready spans the
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
world already depends on: the two axes and their terms, the directory
layout a new module must fit, config keys, CLI and ctl verbs, socket names, and
the argv of the muxes xmux drives.

## Honesty

xmux is honest by design: it shows only what it can back with an answer,
and it says so when it cannot. Honesty is the core rule every presentation
decision is checked against, before colour, before layout, before any
value on a card.

- A mux is named only when it is CONFIRMED. A settled host's enumeration
  answered through its mux, and a source id that names its own mux was
  resolved from what the machine actually serves. No mux is assumed for a
  host that named none, and an unreachable host's written mux stays off its
  card: the card reads the host alone rather than claim a mux the failed
  probe never confirmed.
- An answer that has not arrived is shown as in flight, never as a value.
  A scanning host's card turns the spinner trailing its line, and no card spins
  for a session once its host has resolved.
- A failure is shown as a failure, never dressed as a value. The
  unreachable mark and the refusal keep their own state colour, and the
  reason is stated on the screen, where it fits whole, never cut down to
  fit a card.
- A card states WHAT something is; WHY it is that way is the screen's. A
  card that cannot back a word omits it, and a value that was never
  confirmed is never presented as one.

### Minimal Persistent Surface

The always-visible card surface contains names, numbers, one state glyph, the selection
mark, and the resting `C-g` prefix only. Long names preserve their beginning and end
with a middle ellipsis rather than displacing state or navigation cells.

### Helpful Interaction Surface

An interaction surface spends the available space on state words, counts, the next
key, and complete reasons or solutions. The selected card names its state, an open
filter names total matches and matches from hidden hosts, the key list's bottom border
names the nav scope and the hidden host count, and a host screen keeps the failure
reason whole. A nav left with no card is the one exception at rest: its body says in one
line why it is empty and which key answers it. A live prefix names every key it unlocks, and a selection move names
the selected card's next keys and its state for three seconds; both read the one key table,
and the help adds the glyph legend.

### Four-Position Grammar

The nav uses the same card, status, selection, filter, and key grammar at left, top,
right, and bottom. Placement changes geometry, not vocabulary or interaction shape.

### Terminal-Safe Shape Vocabulary

Persistent UI symbols are conventional one-cell glyphs rendered by OS-default terminal
fonts without emoji presentation. The allowed vocabulary includes `❯`, `✓`, `✗`,
braille spinner frames led by `⠋`, box drawing led by `╭`, `▲`, `?`, and `…`.

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

## Asked-for requests

**xmux reaches a machine only when something asked it to.** Every request traces to one
of three things: the launch scan, a user action, or a push stream that is already open.
No failure raises its own retry.

A user action means a re-scan, a login, selecting a card, or an operation on a session.
A push stream is one connection that stays open while the far side speaks over it, which
is not a repeated request however much it carries. A POLL source that answered is kept
current on a cadence over a path the machine already holds open: the local box and a WSL
distribution are a local process, and an ssh machine is reached over the one master this
side shares across runs, so a repeat there opens no connection and the nav shows what the
mux is doing now. Where every repeat would be a fresh login (an ssh side that cannot
multiplex), a POLL source is enumerated only when something asked for it - the launch
scan or an explicit re-scan. The first enumeration that fails ends the cadence either way.

The rule exists because a request that answers a failed request cannot stop. A machine
that refuses one connection refuses the next identically, so a client that reconnects on
every refusal reconnects without end, and the machine's own defences are built to read
exactly that as an attack. So a host that does not answer is not asked again - the
re-scan is what asks it. The consequences are deliberate and they are what the user
sees: a channel that dropped stays dropped, a display whose client died keeps the last
frame it drew, and a card that is unreachable stays unreachable, each until the user asks
for it again.

A DETACH is not a drop. A mux that ends a control client which had already listed
sessions, with a notice that names no reason, says so over the open push stream and
keeps serving its other sessions: tmux detaches that way a control client whose attached
session was destroyed. The host answered, so its card stands as the mux last reported it
and the channel is opened once more. Only a reopened channel that lists sessions again
earns another reopen on its next detach, so a reopen that fails ends like any other
channel: an empty host when the mux says it has no sessions or no server, and unreachable
otherwise. A notice that names a reason is an orderly end and reopens nothing; a server
that exited leaves the host empty.

Concurrency follows from the same fact. A machine counts the connections that have not
authenticated yet, so work fans out ACROSS machines and never within one: a machine is
asked one thing at a time, however many things there are to ask it.

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
`AGENTS.md` using the Working Notes Format above (all seven sections). Follow the
AS-IS rule: describe the current state only, with refactoring direction expressed
as invariants, seams, and pitfalls - never as change history or phase narrative.

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
