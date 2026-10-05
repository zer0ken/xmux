# xmux: functional requirements & use cases

xmux is a stateless cross-environment session switcher: one terminal that sees and
moves between every reachable tmux/psmux/zellij/screen/tuios/herdr session, local and
over ssh, regardless of OS or mux kind. Its reason to exist is to deliver tmux's `prefix + s`
(choose-tree / switch-client) experience **across hosts**: instant, in-place
switching to any host's session.

Each requirement has a stable ID and states one behavior the implementation is
checked against. A requirement describes behavior only: it names no source file,
no function, and no test, so renaming code is never a documentation change.

---

## A. Discovery & inventory

- **FR-A1** - `xmux ls` lists every reachable session across all sources as
  `<source>/<name>` lines.
- **FR-A2** - A reachable mux with zero sessions is reported as empty, not failed;
  a source on a dead host is reported unreachable; "every source unreachable" is
  distinguished.
- **FR-A3** - `xmux doctor` reports config health, ssh availability, and per-source
  reachability with session counts.
- **FR-A4** - Sessions are ordered deterministically: the hosts run local, then WSL,
  then remote, and within each tier by source name ascending; inside a source its
  sessions run by name ascending. A re-enumeration reproduces the same order, so one
  source's cards are contiguous and the nav never names a source twice.
- **FR-A5** - The roster (which HOSTS are offered) comes from providers the
  `[discovery]` table selects: `~/.ssh/config` aliases and this machine's neighbours,
  both on by default. A neighbour is a machine the OS already reaches in one hop - by a
  route naming a machine, or by the neighbour table, or by the link this machine holds an
  address in where the OS refuses that table - and that answers ssh; it is offered under
  the name the system resolver gives it, or the name the machine gives for itself when
  the resolver gives none, or its address when neither yields a name this machine can
  resolve back. Those two records are read from the OS itself wherever it has an
  interface for them rather than through a command, so a machine whose command-line
  network tools are missing, or refused as they are to an app on Android, still lists its
  neighbours. This box is skipped, since it is reached without ssh. A provider that
  cannot answer contributes nothing instead of failing the run, so a machine whose
  network state cannot be read reaches an empty list rather than an error, and
  ssh-config names keep their position when a provider repeats them. The roster is resolved again on every re-scan, so a machine that has
  come online, and an edit to the `[discovery]` table, both take effect without a
  restart. Absence from a re-resolved roster means opposite things for the two kinds of
  evidence a provider offers a machine from. A RECORD is authoritative both ways, so a
  machine an ssh-config alias, a `[[hosts]]` entry, or the distribution list stops naming
  is dropped along with every source it served and everything on screen for it. A PROBE is
  authoritative one way only: it proves the machine is there, and its silence proves
  nothing, because a neighbour is offered by reaching it inside a bounded budget that one
  slow hop misses with nothing wrong. A machine only a probe offered is therefore carried
  into a roster that lost it, with its sources, the provider its card names, and the
  address the login pane offers; a machine that really left keeps its card and reports
  itself unreachable, exactly as a recorded machine does when it goes offline. A machine
  the roster still names keeps the sources it is serving, including any that were found by
  asking the machine rather than by configuration.
- **FR-A6** - A host's mux is identified by what its binary answers as, not by the
  name it was invoked under, so tmux, psmux, zellij, abduco, screen, tuios, and herdr mix freely
  across hosts with no configuration. Each mux is one implementation behind the mux axis: the
  command plans default to tmux-compatible argv (so a tmux-compatible mux is identity
  plus a few overrides), and a mux that shares no argv with tmux overrides every plan
  together with the shape of what each plan prints. zellij is that case: it is
  enumerated from its
  session listing, its windows come from its tab listing, and its sessions are polled
  because it offers no push channel. abduco is the simplest case: one server per
  session, no windows, no control stream, and no per-session query, so its sessions are
  polled from the bare listing and each resolves as the session alone.
  tuios has one daemon for all sessions but the same display behavior: its JSON listing
  answers the whole poll in one command, and every selected session is shown through a
  fresh attachment because no external command can retarget a named client.
  herdr has one persistent server per session and the same display behavior. Its JSON
  listing offers only running, reachable sessions, and the first attachment creates a
  session because herdr has no detached-create command.
- **FR-A7** - A SOURCE is one mux on one host, so a host running several
  muxes at once contributes one source per mux and every one of them is listed. A `mux`
  value is a name or a LIST of names, in `[local]` and in `[[hosts]]` alike. A host
  given several muxes has its sources named `<host>:<mux>`; a host given one keeps
  the bare host alias, so an existing setup's ids, addresses, and typed targets do
  not move. `exclude` names hosts, so it drops every mux on one. A listed mux that is
  not installed there surfaces as unreachable rather than being dropped, because a name
  the user wrote is a name they meant.
- **FR-A8** - A polled source cannot wedge on one unanswered command. Every command in an
  enumeration runs under a fixed per-command budget, so a timed-out listing surfaces as
  that source's error (the nav shows it unreachable) instead of holding the source open.
- **FR-A9** - No mux list needs configuring, on any host. A host that named no
  mux is asked which of the ones xmux SUPPORTS it has, and each one that answers becomes a
  source. The candidate set is what xmux can drive, and each candidate is asked with the
  same identity probe a configured mux gets, so a binary carrying a mux's name while being
  another mux is not counted as that mux: where psmux answers, a `tmux` that also answers
  is psmux's own alias of itself (which names itself by the name it was invoked under, so
  no probe can tell it apart) and is dropped. A WRITTEN value is never probed, keeping
  FR-A7's rule that a name the user wrote stays visible even when it is missing. No mux
  is ever assumed for a host that named none: it serves exactly what answered, and a
  host where nothing answers serves nothing and has no card, as this machine has no
  local card when nothing is installed here.
- **FR-A10** - A REMOTE host is discovered AFTER launch, asynchronously, and its
  answer only ADDS. The app paints the sources the config names first (a remote probe is
  an ssh round trip per mux, and nothing may wait for that), and a host that named no mux
  as one card that reads the host alone and turns a spinner. Each host's answer then
  arrives and every mux it reports that the host does not already serve becomes a
  scanning card on the spot. A host that served nothing yet names its sources as a
  written list would: one mux takes the bare host alias, which is the card the host
  was already showing, and several are each qualified, replacing that card. On a host
  that already serves a source, an added source's id is always qualified
  (`prod:zellij`) while the mux already served keeps the id it was painted with: that
  id is what the deterministic order, the persisted selection, and anything the user
  typed are keyed to, so
  nothing is renamed and nothing is removed. A new card sorts into its name position,
  so the deterministic order holds while a card the user is looking at does not move
  because another host answered. An added source is
  OPERABLE, not merely visible: creating a session on it works
  exactly as on a configured source.
- **FR-A11** - A mux running inside a WSL distribution is a source like any other. A
  distribution is a HOST of its own, named `wsl.<distribution>` so which kind it
  belongs to is readable in the id and in every address typed at it, and no ssh alias may
  claim a name spelled that way. Distributions are offered either by the `[discovery] wsl`
  provider (on by default, like every provider: a box without WSL costs an empty list
  rather than an error) or by naming one in a `[[wsl]]` entry, which also overrides its
  mux list. A distribution that runs no mux at all, which is what Docker Desktop installs,
  surfaces as unreachable like any other host with nothing to serve, and `exclude` drops
  it by name. Everything FR-A7 to FR-A10 say then holds unchanged: several
  muxes in one distribution are several sources, `exclude` names the host, an unlisted
  mux surfaces as unreachable, and the distribution is asked which muxes it has after
  launch. The WSL implementation is added at the end of the source list, so every id an existing
  install already had keeps the position it had.

## B. The switcher: "see the list, decide whether & where to move"

- **FR-B1** - The nav renders ONE CARD PER SESSION across every reachable source,
  in the deterministic display order (local sources first, then WSL distros, then
  remote hosts, each tier by source name, sessions by name): a session card is a
  single row naming the session, hung
  under a non-selectable `{host}/{mux}` SECTION TITLE that names the whole group once.
  The list is flat, with
  no window or pane rows: xmux aggregates and switches, and the mux itself already shows
  its own windows, so a card has nothing to add below the session name. A name that
  exceeds its card keeps both its beginning and end with a middle ellipsis.
- **FR-B2** - Render-first: the source skeleton paints instantly; each source's
  sessions stream in independently.
- **FR-B3** - The terminal view shows the confirmed session's live grid and follows
  the cursor. A switch keeps the prior grid on screen until the fresh attachment
  shows a visible frame and its output then settles for 50 ms, continuous output after
  that frame reaches 400 ms, or 3 s pass without a visible frame
  (stale-while-revalidate). Input targets the fresh attachment during that wait, and
  resize reaches both attachments. A selected host card whose source is scanning
  shows its scanning screen: the `{host}/{mux}` headline, the `scanning` state word,
  and the host's latest observation facts when any exist. A session grid of another
  source never shows under it. A full re-scan that turned the selected session card
  into the same source's host card keeps that session's confirmed grid until the
  selection moves. The initial scan before a card is selected shows the monochrome
  Braille X animation alone, in a centered 32-column, 16-row frame. A scanning or
  settled host screen keeps its content and centers the animation in the remaining
  rows when a complete frame fits; a confirmed session shows its grid. An
  attachment a host warms on a session of its own choosing is kept
  live, because that is what makes its host instant to reach, but it is never
  confirmed and so cannot take the view. Whenever the confirmed session is not the
  one the cursor names, the view is carried back to the cursor for as long as the two
  differ. Connection and unreachable state hints remain in the nav. `[ui]
  braille-animation` defaults to true; false hides the central animation in both
  scanning and settled host screens while preserving nav activity spinners. Config
  changes apply live.
- **FR-B4** - Navigation: up/down/home/end/pgup/pgdn; fuzzy filter over
  `<source>/<name>`; manual `prefix r` rescan. Up/down and left/right name the two
  things the list is made of: up/down step one card, left/right step one CATEGORY,
  landing on its first card. A category is a source that has sessions, entered at its first session,
  or the whole host band (FR-B21) at once, entered at its first host card: a list of
  machines with nothing running on them is one thing to reach past, not a run of places
  to be carried into one at a time, and the card step still reaches each of them. The
  category is left from any card of it, so a selection deep inside the band steps
  straight out. Both steps wrap, and both mean the same thing in either layout, since
  neither is defined by where a card sits on screen. While filter input is open, its
  popup's top border counts the cards kept of the cards listed, and the matching
  characters on cards are bold. Enter keeps the filter; Esc restores the
  opening filter. With input closed, Esc clears an active filter.
- **FR-B5** - Surveying without committing is first-class: xmux is a switcher, not a
  session owner. Quitting (`prefix q`, or the ctl `quit` verb) leaves the current
  mux session untouched: it is never killed or altered by exiting.
- **FR-B6** - Under a filter, `Enter` attaches the **visible (filtered)** session,
  never a filtered-out one, even when a host row is selected.
- **FR-B7** - Every host-state card reserves one fixed-width state-glyph slot. A card
  that is WAITING turns one spinner there, so the animation never shifts the name. The
  nav's scan progress turns the same spinner on the same frame. A session is a plain
  session card from the moment its host resolves. Settled states use `?` for login
  needed, `▲` for unreachable, `✗` for a listing failure, and a blank slot for a
  reachable empty host. These three failure glyphs use the warning, error, and primary
  ANSI-16 palette roles respectively. An unselected card carries only the glyph; the
  selected card adds `login needed`, `unreachable`, `list failed`, `no sessions`, or
  `scanning`. A host-state card claims a
  mux only when the mux is CONFIRMED: a settled reachable host's enumeration answered
  through its mux, and a source id that names its own mux was resolved from what the
  machine actually serves. A bare-id host that is unreachable or still scanning claims
  none - the card reads the host alone.
  WHY a host failed is stated on its view screen, which has the room to keep a tool's
  diagnostic whole, while a card is only as wide as the nav and could carry no more than
  a cut-down copy of it.
- **FR-B8** - The session xmux is ITSELF running in is never mirrored into the terminal
  view: showing it attaches a second client to the session that HOLDS xmux, which moves
  the user's own client and paints xmux inside itself. The refusal is on the terminal-view
  TARGET, the one value the display reconcile, the attach, and the mux-side switch all
  read, so none of them reaches that session by another path; the card stays selectable
  and killable, and a screen stands in place of the grid to say why. A session running a
  DIFFERENT xmux is not refused - it mirrors like any other session, showing that xmux's
  screen. A session xmux cannot name (the mux does not say, and cannot be asked) is not
  refused either, because a refusal keyed to a guess would hide a session at random.
- **FR-B9** - The nav carries a prefix indicator, not a screen-wide footer: on the bottom
  row of a side column, and at the right end of the seam row in a top or bottom band. At
  rest it names the prefix alone; the states that outrank it (a refusal, scan progress,
  an active filter) take its place while they apply. Arming the prefix opens the key
  list (FR-B36) from the indicator toward the terminal view while the indicator keeps the
  prefix: beside a side column against the indicator's row, below a top band's seam, and
  above a bottom band's seam. Only the PAINT moves, floating over the live grid and
  leaving the layout alone so no card shifts. When the nav is auto-hidden, a live
  prefix interaction brings the nav back for the moment it needs it (a jump reads the
  card numbers), and it hides again when the interaction ends.
- **FR-B10** - Every unselected card carries a number in its address column, on the row
  of the session it addresses, and `prefix <digit>` jumps to it. With `[ui]
  renumbering = true` (the default), the current sorted nav list receives contiguous
  numbers from 1 whenever it changes, including filtering and
  scans. With `renumbering = false`, cards keep their numbers until a full scan deals
  them again in list order; ended cards leave vacant numbers and new cards take the
  next number. The order of the cards on screen is the list order under either setting. The
  selected card holds the selection mark in that same column instead. Selecting a card
  changes nothing else on the card (the address column keeps its width), so a name holds
  its column as the selection passes over it. The input stays open in its popup so
  the number can grow, and every digit is taken as typed: the number only has to name a
  card on the list at Enter. Each edit moves the selection while the number names a card
  and leaves it alone otherwise; `Enter` closes when the number names a card and, for a
  vacant number or one past the highest, states that no card carries it beside the range
  up to the highest number on the list while leaving the input open; `Esc` returns to where the jump started.
- **FR-B11** - Every colour xmux paints is an ANSI-16 slot, so the TERMINAL THEME
  resolves the hue and the whole UI recolours with the user's own scheme. A THEME is a
  named role→ANSI-slot assignment curated in a registry: the built-ins are
  `auto-dark` (the default) and `auto-light`, one for a dark and one for a light
  terminal background, and `[ui] theme` selects one (an unknown name falls back to
  `auto-dark`, reported by `xmux doctor`). The session level reads BOLD so the level a
  user actually picks stands off the text parts of the same line; the
  hint bar keys read its own `bar_accent` slot, because a slot that reads on the cards
  may not read on the bar's own background. Every interaction screen renders key tokens
  in the same bold shape. A section title reads bold in the `decoration` slot, while
  card numbers stay dim, so the group remains clear without colour.
  What the sixteen slots cannot say is said with an attribute: the selected card is
  REVERSE VIDEO in both focus states, the terminal swapping its own pair, which is
  what a theme itself means by "selected". The view border identifies the focused view.
  A background xmux picked instead would be wrong on every theme it was not picked for,
  and it cannot be computed from the terminal's own background either, since a terminal
  is free to answer no colour query at all. `[ui] selection-style` names a background
  anyway, in the same colour slots as the view border, and `xmux doctor` reports
  which of the two is in effect because it is invisible on a screenshot. The view
  border uses one slot across the whole rule: `primary` while the nav holds focus and
  `disabled` while the terminal holds focus. What the border states is which VIEW holds
  focus, a fact about xmux, so no host and no mux may recolour it and a selection moving
  between hosts leaves it exactly as it was.
- **FR-B12** - A group is drawn the same way at every nav position: a dim
  `{host}/{mux}` title over its session cards, each card indented two cells under it.
  The indent is the
  title's and NOT part of the card: it stands left of the card's rect, so the selection,
  which paints a card by inverting that rect, leaves it blank, and a click on it is a
  click on no card.
  On a portrait screen the nav is a wide, short band, and its rows flow into COLUMNS:
  down a column, then right. A column takes whole SECTIONS, so a source's rows stay
  together under the one title naming them and the section that does not fit opens the
  next column rather than splitting across the break; only a section taller than the
  whole column splits, having nowhere else to go, and the continuation column repeats the
  title on its top row, dim and followed by `…`, with the section's cards under it, so a
  column read alone still says whose cards it holds. Card order does not change, so the
  numbers still count in reading order. The paint records each card's rect and the
  hit-test reads it back, so a click cannot land on a card the renderer put elsewhere. A
  column is as wide as the widest thing standing in it and the title, repeated or not,
  is one of those things, so sessions named in one character do not shrink the column
  under the `{host}/{mux}` above them: a label with more name than room is answered in
  the column's WIDTH, never by carrying the name onto a second row.
  A band one row tall has no row to spare for a title over its cards, so it writes each
  title and its cards along the one row, with no indent, and scrolls sideways to keep the
  selected card in view.
- **FR-B13** - The nav says what is off screen on the seam, the one line it draws
  between itself and the terminal view, so no row or column of the nav is spent on it.
  A side list that overflows thickens the stretch of the seam beside the cards on screen
  to `┃`, placed where those cards sit in the whole list, and the cards keep the nav's
  full width. A band scrolls sideways and writes its counts on the seam row: `‹ 5` at the
  left end and `7 ›` at the right end before the prefix, counting CARDS behind the
  columns the window does not reach. A click on a count selects the hidden card nearest
  the visible ones, so the band scrolls to it. Every band row holds cards. Nothing is
  drawn while everything fits, and a floating bar covering the seam row hides the counts
  while it is up.
- **FR-B14** - The arrow PAIR facing the terminal's side names the terminal, and the
  other pair names the nav, identically on both focus paths. With the nav on the left or
  above, `prefix right` and `prefix down` focus the terminal while `prefix left` and
  `prefix up` focus the nav; with the nav on the right or below the whole pair flips. An
  arrow naming the view that already has focus does
  nothing. Bare arrows belong to the cards instead: up and down step one card, left and
  right step one category (FR-B4). Neither is by column, because the band
  puts the next card below in one place and one column over in another, and a key that
  moved by column would mean two different things in the two layouts.
- **FR-B15** - Which side the nav rides on is resolved each frame in one order: a
  placement pinned at runtime wins outright, else the `[ui] nav-position` default applies.
  The nav never moves on its own; only `prefix p` (a pin, saved to `~/.xmux/nav_position`
  the moment it changes) or a config change moves it, and a pin change resizes the mux
  terminals at the next loop top.
- **FR-B16** - The nav's width, the band's height, and the side it rides on are all
  live: the saved prefs seed them, the resize keys and `prefix p` step them, a border drag
  sets the size, and auto-hide takes
  the width away while no prefix interaction is live (a live one brings the nav back). An
  expanded side nav is never narrower than a card's indent, a two-digit number, and eight
  cells of name, and always wider than the prefix with a cell either side, so a wider
  configured prefix can raise that floor; a band is never less than one row. The values therefore travel as
  ONE value carrying
  the width the user set, the width on screen, the band height, the attachment side, and
  the collapsed state,
  so the renderer, the PTY sizing and mouse hit-testing cannot read different answers,
  and the effective width keeps its single owner. Hiding the nav does not move the
  layout: the side travels with the hidden nav, so the nav returns the shape it left.
  `prefix z` collapses and expands it from either view,
  and dragging the view border past the nav's minimum width or height collapses it;
  dragging back out within the same drag expands it at the size the pointer reached. A
  collapsed side nav is a column exactly as wide as the resting prefix, the prefix
  unpadded on its bottom row. Its view border shares the column: it runs down the
  column's edge beside the terminal view on every row above the prefix, so the prefix
  keeps every character and the terminal view takes every column beside the prefix's own;
  a collapsed band is the seam row alone, the prefix at
  its right end. A collapsed nav renders no cards, keeps the view border, and preserves the
  natural width and height for expansion. A click anywhere on it, the view border
  included, expands it without moving the focus or starting a drag, and focusing the nav
  by keyboard expands it too. Auto-hide wins while active and restores the prior
  collapsed state when the nav returns. The collapsed state is persisted.
- **FR-B17** - The resting prefix indicator is a label, not a bar: it paints its text plus
  a cell of padding on the bar's background and leaves the rest of its row to the nav (a
  side column's bottom row) or to the seam and its offscreen counts (a band's seam row). A
  ready or flashing bar fills its whole row, because it has to be readable over what it
  covers. A flash is the reason a key did nothing, a refusal rather than the result of
  work. It comes down
  on the next tree key and, for a user who presses nothing, after ten seconds of its own:
  it reports something that already happened, so holding one indefinitely would keep the
  nav's own help text off screen over a message that has stopped being news.
- **FR-B18** - A prefix lasts as long as the FUNCTION it starts, not as long as the
  keystroke that names it. Most commands end with their key. A command that opens an
  input row ends when Enter or Esc closes the row. A resize ends when its repeat window
  lapses, so a whole burst of arrows is one interaction. The key list and the
  auto-hidden nav show for exactly that span, so neither drops out from under an
  interaction still running.
- **FR-B19** - A prefix waits for the next INPUT, and a mouse action is input: a click, a
  release, a wheel or a drag cancels the prefix chord (the ready wait) in either focus,
  because mouse bytes are scanned
  out of the stream before either focus path's key handling sees them and a chord left
  half-open keeps its key list on screen and then eats the next key. Bare hover is not
  an action: the pointer drifting must not break a chord being typed. A left press on the
  key list, and the drag it starts up to its release, is not an action on anything
  behind the box: it moves the key list and keeps the prefix.
- **FR-B20** - Input is read as key presses only, because a terminal's byte stream
  carries no key-up. A held prefix is therefore indistinguishable from repeated taps and
  is treated as such: each repeat sends the doubled-prefix literal to the pane and blinks
  the key list for as long as the key is down. Recovering the key-up would mean
  requiring the kitty keyboard protocol from the terminal and from every mux enclosing
  xmux, which would make behaviour depend on what that chain passes through; a uniform
  input path everywhere is worth more than this one case.
- **FR-B21** - The nav has three groups in order: actual session cards under their
  source titles, no-session cards for reachable hosts, and cards for hosts whose
  connection or inventory is unresolved. Adjacent groups are separated by one blank
  row in a side column or one blank column in a top or bottom band. The first visible
  boundary can carry a horizontal rule while the side list scrolls. Each group starts
  at the upper-left of its available area. When focus leaves the nav from a session
  card, only the session group is painted. When it leaves from either host group,
  every group remains painted. Returning focus to the nav shows every group.
  Prefix and modal interactions preserve this decision while the terminal view
  keeps focus. While the selection is on a host card, every group is painted, so the
  selected card is always painted. Card numbers and selection identity remain stable
  across focus changes.
- **FR-B22** - A host and its mux are SHOWN as one label, `{host}/{mux}`, wherever the pair
  is read: a nav section title, the screen a card selects, the doctor's source list.
  Always that separator, never the one a source id parts its two halves with, because an id
  is typed and a label is read. And always both halves: a host serving a single mux carries
  no mux in its id, but its label still names one, since a host that appears with its mux on
  one card and without it on the next reads as two hosts. The mux a source's title names is
  resolved once, from the kind the enumeration stamped or from the host's own configured
  mux where no session carries one, so a card and its source's title cannot name it two
  ways. The
  one thing that omits it is a mux nothing knows yet: there is no name to write, and the
  card reads the host alone with its trailing spinner for the work still in flight. A
  session's own ADDRESS is unaffected - it
  is what the user types and what xmux is sent, so its grammar is the id's.

- **FR-B23** - The nav FOLLOWS the mux when the mux moves xmux's own display client to
  another session, so the two regions never name different sessions. A mux moves it
  whenever the user drives the mux itself rather than the nav (`prefix`+`s` and
  `switch-client` under tmux and psmux, `switch-session` under zellij). Which region yields
  is decided by focus, and one of the two always does: in TERMINAL focus the user is
  driving the mux, so the nav selection moves to the session the client is on; in NAV focus
  the selection is the user's own, so it stays and the client is carried back to it
  instead. A session the client reaches before the nav has enumerated it is followed as
  soon as its card appears. The client's session is read where the mux carries it and
  nowhere else: a control channel that pushes the change, or the client process's own
  environment on this machine. A mux that offers no such reading, and a host whose client
  runs on the far side of ssh or a WSL distribution, are not guessed at, and a mux that
  cannot move a client between sessions at all (screen, abduco) has nothing to follow.
  A session RENAMED under the selection is not a move: the selection and the record of
  what is on screen take the new name, so neither region moves. A listing carries names
  only, so a rename is read off one session leaving the list as exactly one other joins
  it, and any other difference is read as sessions made or ended.

- **FR-B24** - `prefix h` opens the table of hosts to check, grouped by cause:
  login needed, unreachable, and inventory failure. Each host carries its latest
  reason. Enter, or a click on a host, selects that host's card and opens its login pane
  when a login is needed or the host is unreachable; the pointer over a host underlines
  it without moving the row the arrows are on. The command palette opens these hosts by
  name, and a click on a palette entry runs it as Enter on it would.
  The nav keeps every host available whenever it holds focus. A nav with no hosts
  names the re-scan key in its body.
- **FR-B25** - The nav attaches on one of FOUR sides of the terminal view - a left or
  right column, a top or bottom band - and the placement is a user choice at two layers:
  a single `[ui] nav-position` setting (default `left`) names the placement when nothing
  is pinned, and `prefix p` moves the nav one side clockwise (left → top → right →
  bottom → default), saving the pinned value to `~/.xmux/nav_position` the moment it
  changes, where it beats the setting until the key cycles back to the default. The nav
  never moves on its own: a pinned side wins outright, else the default applies. The
  mirror symmetry is shape-only: the layout INSIDE the
  nav region is identical at all four placements (a right column is the left column's
  list, a bottom band the top band's down-then-right flow), only what sits on which side
  of the view border flips, and the prefix indicator sits on the bottom row of a side
  column and on the seam row of a top or bottom band. The view border
  drag mirrors its math per side (a right border measures the width from the right edge,
  a bottom border the height from the bottom edge), and the resize keys follow the same
  rule: the key moves the border the way it points, so the nav size follows the placement
  (it grows on a left or top nav and shrinks on a right or bottom one). A position change
  leaves the terminal view the remainder whole, with the selection and the focus kept, resizes the mux terminals
  for the new split, and repaints the whole screen, since the border jumps to the
  opposite side. The focus arrow pairs follow the placement (FR-B14), and the key list
  and the help name the pair the current placement makes active.
- **FR-B26** - BLOCKED means ssh refused the host for a reason the submitted login
  answers. Two of ssh's own refusals enter this state: its final account-and-host
  authentication line, and a host-key verification failure for a host with no recorded
  key when the effective ssh policy is `ask`, which the submitted login resolves. An
  unknown key under a strict policy stays unreachable and gives a command that displays
  its fingerprint. Remote command
  permissions, name resolution, connectivity failures, and a changed host key stay
  unreachable. A
  blocked host carries a card, renders the `?` mark, and
  shows the pane above the same failure facts the unreachable screen states. What it was
  blocked on is not in its state word: the pane states a plain-language verdict, marks
  with `✗` the input field the failure concerns (the address for a name that does not
  resolve or a host key, the port for a refused connection, the password for a refused
  password, the username and password for a refused authentication), and shows ssh's
  own last line dimmed under it. A details choice unfolds ssh's whole sanitized text
  together with the failure facts, which stay folded until it is picked.
- **FR-B27** - The LOGIN PANE holds the three values ssh will not ask for and must know
  before it dials - the address, the port, and the username - with an optional masked
  password beside them. Address and port start at what ssh WOULD use, while the
  username comes from an exact host stanza when it names one, otherwise starts empty
  for the user to enter. An address or port from OpenSSH's
  effective configuration wins; the matching stanza is the fallback when OpenSSH
  cannot report it. Missing values use the address a provider reported else the
  host's own name and port 22. Each prefilled field shows whether its value came
  from ssh configuration, discovery, a default, or the host name. A required field is marked in its
  label and an empty optional one says so in the space its value would occupy. It is not
  a modal and nothing in the nav drives it. Enter means one thing throughout: submit from
  the button, pass the focus on from anywhere else. Space picks a choice, Tab and the
  vertical arrows walk the stops, and an escape sequence xmux does not act on is consumed
  whole rather than landing in a field as text. The connection values and the two choices
  stand in two titled groups, the focused stop's name is shown in reverse video while the
  pane takes keys, and a rule parts the inputs from the login's steps and failure below
  it. The details choice is a stop only while the pane states a failure. While a login
  runs the pane keeps every value on screen and says so in place of the button it was
  submitted from, taking no key but the lone Esc that ends the attempt, since there is
  nothing left to fill in. Below the rule it lists the steps in the order they run:
  connect, authenticate, the selected recording and key registration, and find mux. Each
  step is pending, running with the spinner, done `✓`, failed `✗`, or skipped `·`, and
  changes only when the login reports it: the password handed to ssh ends the connect
  step, the ssh verdict settles both connection steps, each follow-up settles its own, and
  find mux settles on the answer to the re-probe the working login started, then on the
  first mux answer after it: a mux answering, no mux answering, or the search failing.
  A failed connect or authenticate step marks every later step skipped. A failed
  recording does not stop the key registration or find mux, and a key registration that
  declines to run is itself skipped. The steps belong to one submission on one host
  card, so a report from a replaced submission changes nothing. They stay on screen with
  the failure until the machine is probed again, and leave once a mux answers.
- **FR-B28** - Submitting creates a pending password in process memory and runs only the
  submitted login with it. Success promotes that exact credential for the machine only
  when that login actually requested the password;
  failure or replacement removes only that exact credential. Every later ssh started by
  the running app, including listing, metadata and control channels, session operations,
  display attach, and key registration, tries keys first and may request the held
  password through xmux's private local credential broker. The ssh child receives only
  an opaque per-command token and a forced askpass environment. The token stays valid
  until that child is reaped and can return the password at most once. The password is
  never written to a file, argument, environment, log, rendered frame, or status. The
  held credential allocation and current password-field allocation are overwritten in
  full when released. Transient terminal and IPC buffers remain process memory;
  operating-system crash dump policy is outside xmux's control. The submitted address,
  port, and user are included in a bounded effective ssh configuration query before the
  credential becomes available. When that query fails, a typed password is erased and the
  login reports why, while a login without a password proceeds under the user's own
  host-key policy. The helper answers an
  OpenSSH password or keyboard-interactive prompt only when the account and host exactly
  match the held account and the target alias, resolved host name, or host-key alias. A
  destination configured with `ProxyJump` or `ProxyCommand` does not enter the password
  path because the proxy would inherit askpass. The helper refuses other prompts without
  consuming the token, along with host-key questions, key
  passphrases, passcodes, and one-time codes. The
  submitted login uses `accept-new` only when OpenSSH reports the effective policy as
  `ask`; it never weakens `yes`. An unknown key under `yes` is unreachable and reports an
  `ask` command that displays the fingerprint before the user decides whether to add it. A changed host key fails without approval, and
  background probes never change host-key policy. One password answer is allowed per ssh
  child. Without a held password ssh remains non-interactive.
  Connection sharing remains enabled where the client supports it, but correctness never
  depends on a master surviving. A successful login re-probes only that machine. A
  refused password is removed and returns the host to the pane; changed values replace
  the prior in-memory credential, and cancelling a pending login removes it. The pane
  keeps the login's own bounded, control-free diagnostic, categorized as a refused
  password, unreachable host, host-key mismatch, server session failure after
  authentication, timeout, cancellation, or other ssh failure. A later probe cannot
  replace that diagnosis. A refusal that did not receive the held password remains
  visible rather than being suppressed. A password is removed only after ssh exits 255, the same command
  actually received it, and ssh emitted its own authentication refusal line. Removing a
  machine from the roster forgets its credential, and process exit forgets every
  credential. Removal immediately invalidates outstanding command tokens and releases
  the held plaintext. Probe results carry the credential generation from spawn, so an
  older result cannot undo or reclassify a newer login. The broker recreates its endpoint with backoff
  after any accept failure; while it is unavailable commands use batch mode and report
  that password login is unavailable. OpenSSH before 8.4 is isolated from the controlling terminal on Unix. On
  Windows, a client before 8.4 cannot accept a password from xmux and the pane says to
  update OpenSSH or register a key. The separate `xmux attach` command runs in a fresh
  process and uses keys or ssh's own terminal prompt. Only local and WSL hosts have no login.
- **FR-B29** - RECORDING runs after a connection that worked and says so when it could
  not. It writes an xmux-marked stanza naming the host, with the values that reached it,
  at the TOP of `~/.ssh/config`, because ssh keeps the first value it obtains for a
  keyword. The marker makes a second login replace that stanza rather than stack another,
  and nothing the user wrote is touched. The choice is offered only once a value differs
  from what ssh would have used. A password is never recorded, because ssh config has
  nowhere to put one.
- **FR-B30** - REGISTERING runs after a connection that worked and reports registered,
  skipped with a reason, or failed with ssh's reason in the login's toast, the log,
  and the host information rows. The login command
  reads the host's shell family, since a locked host's family is unknown before it. The
  registration is an ordinary ssh command using the same per-machine authentication as
  every other command, whether or not the client supports connection sharing. On a POSIX
  host it appends this machine's public key to
  `~/.ssh/authorized_keys`; on a Windows host it runs Windows PowerShell, which both
  `cmd.exe` and PowerShell start the same way, and also adds the key to
  `administrators_authorized_keys` when the host's sshd reads an Administrators member's
  keys from there and the account is one. The line it appends ends its comment with the
  `xmux-registered` mark. Either form adds the line only when no key line in the file
  holds the same key type and body, whatever its options and comment (a line starting
  with `#` holds no key), so an existing unmarked line stays unmarked, and an ed25519
  pair is generated first when the machine has no key to send.
  After adding it, registration runs one ssh login that may authenticate with a key
  only, never prompts, shares no connection master, and runs a remote command that does
  nothing. The result is registered only when that command exits 0. When the host
  authenticates the key and then cannot open a session, the result is failed with the
  server's error, and the marked line this registration added is removed from every file
  it was added to; a line that was present before registration is kept. When the login fails
  before authentication finishes (the host cannot be reached, times out, or refuses the
  key), the result is failed as not verified and the line is kept.
- **FR-B31** - Persistent UI symbols are conventional glyphs that OS-default terminal
  fonts render in one cell without emoji presentation. The vocabulary includes `❯`,
  `✓`, `✗`, braille spinner frames led by `⠋`, box drawing led by `╭`, `▲`, `?`, and
  `…`. A terminal smaller than 24 columns by 4 rows renders only a size screen naming
  the required and current dimensions, so the nav and terminal view never overlap.

- **FR-B32** - The result of work the user started is a TOAST: a login with the
  public-key registration and ssh-config recording it ran, a new session, and a re-scan.
  The newest recorded release is announced the same way at launch. A toast floats in the
  terminal view's corner nearest the hint, avoids the prefix key list and floating hint,
  is at most 40% of the window wide, and wraps a long reason inside that width. A toast
  of successes and facts leaves after five seconds and shows the time it has left as a
  bold accent line on its bottom border, with the elapsed share as a normal line.
  The history key appears inside the card when it fits. A login result leaves after five
  seconds regardless of level; its details remain in the login pane and history. Other
  toasts carrying a warning or a failure stay until a click on one or opening the history
  dismisses it. Three toasts stand at
  most, the newest in the corner. `[ui] notifications` (default true) turns toasts off;
  the history still records every result.
- **FR-B33** - `prefix m` opens the HISTORY in either focus: every toast and every
  background event, newest first, each with how long ago it happened. A background event
  is one nobody asked about, such as a host that stops answering outside a re-scan; it
  is recorded without a toast, so it never pulls attention from the terminal. The history
  is bounded at 200 records and, when full, drops its oldest success or info record
  before any warning or failure.
- **FR-B34** - A re-scan ends in ONE toast that states what changed against the inventory
  the user saw when they asked: hosts added and removed, sessions started and ended, and
  hosts that stopped or started answering, naming the first few of each and counting the
  rest. A host whose login was refused is reported as needing a login, never as its
  sessions ending. A re-scan that changed nothing says so with the host and session counts
  it found. The toast is made once every source and the roster have answered, and a host
  that stopped answering keeps it on screen until it is dismissed. `prefix R` re-scans
  the selected card's host alone: that machine's reachability probe and then every
  source it serves, with no roster resolution and no other machine asked. Its cards keep
  their sessions and numbers while it runs, and it ends in one toast titled with the
  host's name that compares that host's sources alone. It is refused while that host is
  still being scanned and while another re-scan has not reported, and a `prefix r` asked
  meanwhile takes over with its own summary.
- **FR-B35** - Every key xmux binds is listed ONCE, in one key table with the words that
  name it. Both focus paths resolve a prefix command through that table, and the help,
  the key list, and the selection hint are built from it, so a surface never names a key
  that does something else, and no path binds a key the table leaves out.
- **FR-B36** - Pressing the prefix opens the KEY LIST at once: a box titled with the
  prefix, naming every key the prefix unlocks under its section title (navigate,
  sessions, view, app), in as many columns as the room beside the indicator holds. When
  the keys do not fit, the box shortens every description first and then gives up the
  keys needed least, counting them as `+N more`; the jump, help, and quit keys are never
  given up, and no key is ever shown without its name. Its bottom border names the xmux version where it fits.
- **FR-B37** - For three seconds after the user moves the selection, the hint bar names
  the selected card's most relevant keys (one to three) and one fact about it: a
  session's windows, or a host's state word with the reason behind it. Any key ends it,
  the next move replaces it, and a selection xmux was told to make raises none. Afterwards
  the resting prefix indicator returns.
- **FR-B38** - The help lists every key in the key table section by section, then a
  legend of every glyph the screen uses (the host states, the spinner, the selection
  mark, the overflow cues, the auto-hide border, and the toast levels), as one document
  with one blank row between two sections. A row of tabs under the search field names
  each section: `←`/`→` move the active tab and scroll the body so that section's title
  is its top row (held at the end of the help), and a click on a tab does the same. The
  pointer over a tab underlines it and the body shows that tab's section while the
  pointer stays on it, without moving the active tab; off the tab row the body returns to
  where the active tab left it.
  `↑`/`↓`, `PgUp`/`PgDn`, and `Home`/`End` scroll the body, and the active tab follows
  the section whose title is at or above the top body row, so the tabs and the scroll
  never disagree. Typing searches it, ignoring case, and the tabs then name only the
  sections that kept a row; `Esc` or `prefix ?` closes it, and its bottom border says so
  whatever the search leaves.
- **FR-B41** - A popup never loses a word to its size. Every row of a popup body wraps to
  the popup's inner width: a row with a key column (the help, the command palette, the
  hosts to check, the logout facts) continues its description under the description
  column, and a key wider than its column takes rows of its own above its description. A
  text field is the one row that does not wrap: it keeps its caret in view on one row.
  A popup's height and its scrolling count the wrapped rows, so a window too small for a
  popup shows all of it by scrolling.
- **FR-B39** - LOGGING OUT of an SSH host (`prefix L`, confirmed by typing `logout`)
  takes this PC's public key off the host before it closes anything, and cancels a
  pending login on the machine when it starts. Over the machine's current connection it
  finds, in one command, the lines of the host's key files (the same files registration
  writes) whose key type and body equal one of this PC's public keys, ignoring options
  and the comment, and removes the chosen ones in a second command. Lines marked
  `xmux-registered` are chosen at once. When a matching line lacks the mark, a second
  confirmation opens where the first one was, in the same layout, and states that xmux
  did not add the key and that removing it also affects ssh outside xmux; typing
  `remove` chooses that line too, and closing the confirmation any other way keeps it
  while the marked lines go. A host that cannot be reached or a removal that fails does
  not stop the logout: it reports in its toast that the key remains and why. The removal
  reads the file as bytes, so every other line stays byte for byte whatever its
  encoding, and it builds the new file as a copy beside it and replaces the file only
  after the copy holds exactly the other lines, byte for byte, and no line it removes,
  and only while the file still holds what was read; otherwise the file stays as it was
  and the removal fails with the reason. A POSIX key file holding a NUL byte is refused
  the same way. One constraint remains: key files have no locking convention, so when
  another program (ssh-copy-id, an editor) writes the file in the instant between the
  removal's last check and its replace, that program's new line is missing from the file
  after the logout. It cannot be prevented: the check runs immediately before the
  replace, so the window is the duration of one rename, and a file that changed any
  earlier is never touched. A key registration on the machine that is under way when the
  logout starts, whichever login started last, finishes before the search, so its line
  is found; a login the logout cancelled before that point registers nothing. While the
  logout runs, a login on that machine is refused with a flash naming the running
  logout, and no login follow-up on it runs until the logout clears the machine. Then
  the held password, the metadata and display connections, and the shared SSH master go.
  SSH config is not changed. The key steps run off the event loop, and a second logout
  is refused while one is running.
- **FR-B40** - The selection follows the user's interest
  (`docs/adr/0007-context-follows-the-users-interest.md`). When the selected card leaves
  the list, by a scan, a re-scan, a poll, a logout, a session ending, mux discovery, or
  the filter, the selection moves to the nearest node up its lineage that still has a
  target: a session to its source (the `{mux}` part of its section title, or the
  source's host-state card when it has no session left), a source to its host (the
  host's card while the host is down, else the `{host}` part of a row of that host), and a
  host with nothing left to the card that now holds its place in card order (the next
  one, else the previous). A host card that resolves into sources hands the selection
  to the first of them by name. A new card takes the selection only when it is what the user asked for (the
  session `prefix n` created, the session under the selection when a full re-scan
  returns it) or, at launch, the first session to appear (FR-D5); any other new card
  leaves the selection where it is. A selection that lands on a source card shows that
  card's screen, never another session's grid.
- **FR-B42** - Hosts, sources, and sessions each have their own view screen, whether
  or not the nav has a card for them. The host screen states how the host is addressed,
  its reach state, its SSH login method, its public key, and its last successful reach,
  holds the login form while a login is needed, offers the keys that re-scan it and log
  out of it, and links each of its sources with that source's session count or state.
  The source screen names `{host}/{mux}` with the host segment linking to the host
  screen, states how its list updates and when it was last listed, offers the key that
  creates a session there, and links each of its sessions. A section title has a host
  part and a source part, and only the part the selection names is highlighted. The nav
  stays a list of numbered cards in sections: `↑`/`↓` step between cards and never stop
  on a title, `←`/`→` step between sections, and a title part takes no number. The
  hierarchy is reached through `Ctrl-↑`, which walks session, source, host, and
  `Ctrl-↓`, which returns to the child the walk came from, else the first child; through
  the title parts; and through the screen links. A
  host none of whose sources connected is one card, and a logout or an unreachable host
  gathers the selection onto it. Pointing at a nav target previews it in the terminal
  view without moving the selection; a click opens it, selects it, and focuses the
  terminal view, the same as `Enter`. In terminal focus, `↑`/`↓` select a screen link,
  and `Enter` or a click opens it.

## C. Switching (the keystone)

- **FR-C1** - A same-server pick lands on the picked session. Each mux's driver owns
  the in-place-vs-reattach decision, and it
  turns on whether that mux can name xmux's OWN client: one that can moves that
  client with `switch-client` and repaints (instant, nothing torn down); one that
  cannot reattaches by session name, which is the only address that can reach no
  terminal but xmux's own. A switch aimed at a client the mux cannot resolve is
  the failure this rules out: it moves a separate terminal of the user's. The
  attach is debounced so rapid navigation does not storm.
- **FR-C2** - A cross-host pick switches entirely in process, with no picker and no
  detach between. Each source keeps its own live PTY attachment; the target
  source's driver takes over, the previously shown session stays on screen until the
  fresh attachment has painted or reaches its bounded wait (stale-while-revalidate),
  input already targets the fresh attachment, and the canonical selection is synced
  immediately.
- **FR-C3** - Source degradation is graceful, never a silent loss: an unreachable source
  is marked `▲` and gains the word `unreachable` when selected. Its view screen leads
  with a plain verdict carrying the last successful reach when known, followed by
  the failure run and actions to check this host or every host. The details choice
  unfolds the transport reason, the mux binary asked for, how the machine is
  addressed and the wait that bounds reaching it, the socket, the session-listing command
  itself (spelled so it can be run by hand outside xmux), the PROVIDER that put that host
  on the roster (so a host the user never wrote down is traceable to the thing that
  offered it, and to the `[discovery]` key that would turn it off), the ssh stanza it was
  reached through, what the OTHER muxes on that same machine answered (which is what says
  whether the machine or the mux is down), and the log file holding the full history; a reachable-but-serverless source reads `(empty)`, and a once-connected source keeps its
  last-known cards on a transient drop. Nothing recovers on its own: a dropped display
  client is reaped and its last frame stays on screen, a re-scan reconnects a dropped
  poll host's metadata, and selecting the card reconnects a dropped control client.
- **FR-C4** - No silent loss: every dispatched switch/select command logs its exact argv
  and result; a failed attach is logged at warn level and returns to the nav rather
  than being swallowed; each driver logs its show decision and the grid-changed effect.

## D. App lifecycle

- **FR-D1** - `xmux` (no subcommand) is a persistent supervisor that owns the
  terminal and runs one mux-client child at a time per session, plus one `-CC`
  metadata client per remote source, over a single async event loop.
- **FR-D2** - The app serves its control socket concurrently while a session is
  displayed (attach spawning is off-loop), so `ping` / `dump` / `status` / `switch`
  are answered without blocking.
- **FR-D3** - The app runs inside a mux. It attaches its mux clients as PTY children
  rather than handing over the terminal, so its attachments do not nest and none of them
  is refused; a `xmux attach` handover that a mux WOULD refuse is left to that mux to
  refuse, in its own words, rather than pre-empted here. The one thing running inside a
  mux costs is the session it runs in, which is not mirrored (FR-B8).
- **FR-D4** - Socket hygiene: a stale socket is removed before bind, the socket is
  owner-only (`0600`) on unix, and it is removed on exit. A crashed instance's leftover
  `ctl-*.sock` marker is swept on the next startup (any marker whose socket no longer
  dials). Discovery enumerates the markers newest by mtime first, tie-broken by higher
  pid.
- **FR-D5** - The app launches directly into the persistent split view (nav +
  terminal view). The cursor preselects the first session to appear as the hosts
  answer, and holds it: a host answering later does not take the cursor, wherever the
  card order places it. A launch therefore attaches one session rather than one per
  answer, and what the cursor names is what the terminal view shows throughout the
  scan. The settled selection's address is persisted as the last session. There is no
  separate picker mode; `prefix q`
  quits.
- **FR-D6** - The log records what HAPPENED. An enumeration logs INFO with the session
  list when that list changed (or is the first) and WARN on failure, so a connected host
  refreshing on its cadence writes a line only when its sessions changed, and the file
  carries what changed rather than a cadence.
- **FR-D7** - No log grows without end. The daily files are kept for a bounded window and
  the oldest goes as a new day opens. A panic that a worker recovers from and hits again on
  the next frame is written by its SITE at each doubling of its count, not once per
  occurrence, so the first is kept, the scale is kept, and a repeating internal error
  cannot bury the file. A panic that ends the app is always written whole.

- **FR-D8** - One command installs xmux, on every OS with a published build and from
  every shell that OS ships: `sh` on unix-likes, and both PowerShell and CMD on Windows,
  each of which ends in the same install. The install script reads the OS and the
  architecture from the machine it runs on, downloads that build from the release, and
  refuses to install it unless its SHA-256 matches the checksum the release publishes.
  It reports where the launcher went and, when that directory is not on `PATH`, either
  adds it or states exactly what to add; it writes only the user's own `PATH`, never the
  machine's, so it needs no elevation. A named version installs instead of the newest
  one, which is what makes going back to an older build a command rather than a manual
  download.
- **FR-D9** - Installing a version never writes the binary a running xmux is
  executing. Each version goes into a directory named after it, and only the launcher
  is repointed, so an upgrade during a session leaves every running instance on the
  build it started with. On the platform where a running image cannot be overwritten,
  a launcher in use is renamed aside and the new one takes its place, and the renamed
  file is removed by a later install once nothing holds it.
- **FR-D10** - `xmux update` updates a cargo install or a binary the user placed
  themselves by replacing it in place with a checksum-verified build from the release,
  so an update never recompiles; an install the script placed re-runs that script; and
  a winget or Homebrew install runs its package manager, because those already fetch
  prebuilt binaries and overwriting one would leave the manager out of step. The
  method is read from the running executable's path; a path that cannot be read is
  reported as unknown rather than guessed, because each method writes somewhere
  different. `--check` reports what an update would do without doing it, and `--method`
  forces one, so a cargo install can still be handed back to `cargo install`.
- **FR-D11** - xmux tells the user that a newer version exists. It asks the release
  feed at most once a day and records the answer, so a launch never waits on that
  question and a launch with no network paints as fast as one with it; the answer is
  shown on startup as a toast and by `doctor`, and one config key turns the asking off. This is
  not the request rule of FR-G7: that rule governs the machines the roster names,
  which xmux reaches over ssh and which refuse every retry identically once they
  refuse one. Asking a release feed authenticates nothing and retries nothing.
- **FR-D12** - `doctor` opens with which xmux is running, where its binary is, what
  owns that install, and whether a newer version was recorded, so an update that lands
  somewhere unexpected is traceable to the install it acted on. It asks the network
  nothing.
- **FR-D13** - `xmux uninstall` removes xmux the way it was installed, read from the
  running executable's path as `xmux update` reads it: an install the script placed
  loses its version directories, its launcher and the file beside it, and the `PATH`
  change the script made, and nothing else; a cargo, winget, or Homebrew install runs
  its package manager's uninstall; and a binary the user placed is deleted. It removes
  nothing until the user answers `y` or `yes` to `Remove xmux? (y/N)`; any other
  answer, end of input, and a run with no terminal to ask are a no. A second question
  covers the settings and data directories, and its default keeps them. `--yes`
  answers only the first question and `--purge` the second. It refuses while an xmux
  instance is running, checking again right before each removal, and it reaches no
  remote host, so a key registered on a host stays until the app's logout removes it.

## E. Session management

xmux aggregates and switches; it does not edit what a mux already edits. Starting a
session is the one mutation it keeps, because a reachable source with no sessions has
nothing to switch to until one exists.

- **FR-E1** - Create a session on the host and mux the selected card belongs to
  (`prefix n`), then it appears in the nav. Under an unreachable host the action is
  refused with a flash.
- **FR-E2** - There is no rename, kill, or window/pane command: not on a key, not
  in a modal, not on the wire, and not in the mux command set.
- **FR-E3** - Create runs off the key path so a slow ssh round-trip never freezes
  rendering or the control channel. The committing key becomes a deferred operation the
  run loop spawns off-loop.

## F. Control channel

- **FR-F1** - A per-instance local socket (`ctl-<name>.sock`) drives the running app
  headlessly. Its navigation/display verbs (`ping`, `dump`, `status`,
  `switch <source> <session>`, `focus <terminal|nav>`, `rescan`, `quit`,
  `width <delta>` (a signed column delta, not an absolute width), `toggle-auto-hide`)
  and its one session-lifecycle verb, `new-session` (a session named by its source
  and session separately), resolve to a domain action. There are no kill/rename/window
  verbs: xmux aggregates and switches, so editing a session stays with the mux. Raw
  key/text injection stays behind the unstable `raw:` namespace (`raw:key` /
  `raw:keys` / `raw:text`). A command-level failure replies `err: …` and `xmux send`
  exits non-zero. A `switch` to a source/session pair the inventory does not
  list is such a failure: it replies `err:` naming which half is missing (the source,
  or a session under a present source), so the reply reflects the resolution.
- **FR-F2** - There is one unified socket, not a separate app socket: `switch
  <source> <session>` is a first-class ctl verb resolving to the same switch action a
  key press does.
- **FR-F3** - Every instance takes a NAME at startup: an auto-generated
  `<adjective>-<noun>` whose walk skips names live instances hold (a crashed
  instance's undialable marker is reused), or an explicit `--name` validated to 1-32
  characters of `[a-z0-9-]` so it is always a legal path segment and Windows pipe
  name. Socket discovery enumerates the `ctl-*.sock` markers, newest by mtime first
  then by name. `xmux send <id>` resolves `id` against LIVE instances only (exact
  name, then unique name prefix, with `-` for the sole one) and refuses ambiguity by
  naming the candidates. `xmux instances` shows each (name, pid, cwd, tty, displayed
  session, focus).
- **FR-F4** - Length-framed messages (decimal count + newline + bytes) with a bounded
  read; endpoint naming works for `ctl-*.sock` on every platform.

## G. Transport & safety

- **FR-G1** - ssh uses a connect timeout. The user-submitted login uses accept-new
  host-key policy only when the effective policy is `ask`; every other command leaves the user's policy intact. Without a held
  password every app-owned ssh uses batch mode and never waits on a prompt. With one,
  every direct app-owned ssh forces the private askpass path and permits one password answer,
  including a tty attach when the client supports forced askpass. An older Unix client
  runs non-interactive children in a new session so it cannot read the user's terminal;
  an older Windows client does not enter the password path. A command holding a password
  offers a key first; when the host drops the connection before a session starts, without
  refusing authentication and before the password is handed over, the command runs once
  more with key authentication off within the same time budget. Once that retry succeeds,
  later commands holding that password skip the key. Attach requests a tty.
  ControlMaster multiplexing is added only off Windows and remains an optimization.
  Effective ssh configuration is resolved with bounded concurrency, and a timed-out
  resolver process is terminated.
  A destination configured with `ProxyJump` or `ProxyCommand` requires key authentication
  because the proxy would inherit target askpass state.
- **FR-G2** - A session name from a remote list is injection-safe when it re-enters
  a remote shell command (POSIX single-quote escaping).
- **FR-G3** - Mux session env (`TMUX`/`TMUX_PANE`/`PSMUX*`) is stripped for listing so a
  command run from inside a mux is not refused as nesting; lookalikes survive.
- **FR-G4** - A remote attach runs the mux-supplied attach argv in one `ssh -t`
  connection using the machine's current authentication environment. Its password can
  never appear in the terminal view. Local psmux routes to its per-session server.
- **FR-G5** - A command bound for a WSL distribution is exec'd there rather than handed
  to the launcher as a command LINE, so Windows quoting is never re-read as shell syntax
  and the POSIX quoting of FR-G2 stays the only boundary a session name crosses. The
  command then runs in a LOGIN shell, because a mux installed under the user's own home is
  not on the bare environment's `PATH`. A distribution's attach runs one command
  exactly as FR-G4's remote attach does.
- **FR-G6** - Which shell family answers a remote is READ from its reachability probe, in
  the round trip that probe already makes, and held for every source that machine serves.
  A remote outside the POSIX family is then not sent POSIX-only syntax: it gets the attach
  by itself where a POSIX remote gets it behind a POSIX prefix. A LOCKED host's family is
  unknown, because the probe that reads it never got past the refusal that locked the card,
  so the login command reads the family without assuming POSIX syntax (FR-B28).
- **FR-G7** - xmux reaches a machine only when something asked it to. Every request
  traces to the launch scan, to a user action (a re-scan, a login, selecting a card, an
  operation on a session), or to a push stream that is already open. No failure raises its
  own retry, because a request that answers a failed request cannot stop: a machine that
  refuses one connection refuses the next identically, so a client reconnecting on every
  refusal reconnects without end, which is what a machine's own defences are built to read
  as an attack. A POLL source that answered is re-enumerated on a cadence only over a path
  the machine already holds open (the local box, a WSL distribution, or an ssh master
  this side shares across runs), because a repeat there opens no connection; where every
  repeat would be a fresh login, it is enumerated only when something asked for it - the
  launch scan or an explicit re-scan. The first enumeration that fails ends the cadence.
  What the user sees follows from this and is deliberate: a
  metadata channel that dropped stays dropped, a display whose client died keeps the last
  frame it drew, and an unreachable or locked card stays as it is - each until the user
  asks. A push stream is one open connection the far side speaks over, so it carries
  changes without asking for them.
- **FR-G8** - A machine is asked one thing at a time. Work fans out ACROSS machines and
  never within one, because a machine counts the connections that have not authenticated
  yet and drops the ones past its limit: a burst is both what makes a legitimate probe
  fail and what its logs record as an attack. So the muxes a machine is asked about are
  asked in sequence, and the probes of separate machines still run together.

---

## Use cases (end-to-end scenarios)

- **UC-1, jump from my laptop to a remote dev session.** From the split view, move
  the cursor to a remote session and land in it in one action. *(FR-B1, FR-C2,
  FR-D1/D2)*
- **UC-2, hop between two same-server sessions.** Select a session on the current
  server for an instant switch-client. *(FR-C1)*
- **UC-3, survey then stay put.** Look around the nav, then quit; the current
  session is untouched. *(FR-B5)*
- **UC-4, find one session among many then go.** Filter to narrow, Enter on the
  visible match. *(FR-B4, FR-B6)*
- **UC-5, the remote is down and I am not left in the dark.** An unreachable source shows
  `▲`, adds `unreachable` when selected, and keeps the reason on its host screen; a
  failed attach is logged and the nav stays usable.
  *(FR-A2, FR-B7, FR-C4)*
- **UC-6, deep in a remote, get back home.** Native detach (`prefix d`) inside the
  remote returns control to the local app's split view; pick local or another host.
  *(FR-C2, FR-D1)*
- **UC-7, spin up a throwaway on a remote and switch to it.** Create on the
  host's card, then switch to it. *(FR-E1, FR-C2)*
- **UC-8, survey what's running everywhere before deciding.** The nav shows every
  session on every host; the terminal view previews the selection.
  *(FR-B1, FR-B3, FR-B8)*
- **UC-9, drive xmux from a script.** Control channel: dump, inject keys, signal a
  switch. *(FR-F1, FR-F2)*
- **UC-10, switch in either direction, local to remote to local.** The app re-attaches
  whatever the next target is, local or remote, in any order, with no picker between.
  *(FR-C2, FR-D1)*
- **UC-11, go straight to the session I can already see.** Read the number off the
  card, press `prefix <digit>`, and the selection is there; keep typing for a number
  past 9. *(FR-B10, FR-C1)*

## Accepted limitations

The seamless cross-host switch is bought with three costs, accepted by design:

- One live app per terminal owns the display; a second one cannot share it.
- Handing the display from one mux client to another can flash a repaint.
- On Windows, ssh has no ControlMaster multiplexing, so each remote round trip
  pays a fresh connection.
- A push-channel mux inside a WSL distribution needs a terminal allocated on the
  distribution's side, because a control stream reads its terminal attributes and exits
  without one. A distribution that cannot allocate one reports that mux unreachable; a
  polled mux there is unaffected.

## Design principles

- **Asked-for requests** - xmux is a guest on every machine it reaches. It asks when
  something asked it to, one thing at a time, and it answers a refusal by reporting it
  rather than by asking again. Recovering is the user's to ask for, which costs a
  keystroke and is the only version of it that ever stops. *(FR-G7, FR-G8)*
- **Honesty** - The nav shows only what it can back with an answer, and says
  so when it cannot. A value is never guessed, assumed, or shown as a fact
  before it is one: a mux appears on a card only when the enumeration
  answered through it or the machine was resolved to serve it, an unresolved
  host card turns a spinner instead of a value, and a failure keeps its own
  state colour while the reason is stated on the screen. *(FR-B7)*
