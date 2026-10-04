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
  resize reaches both attachments. Only the first launch, before any grid exists, shows
  a blank view. An attachment a host warms on a session of its own choosing is kept
  live, because that is what makes its host instant to reach, but it is never
  confirmed and so cannot take the view. Whenever the confirmed session is not the
  one the cursor names, the view is carried back to the cursor for as long as the two
  differ. The waiting and unreachable state hints live in the nav, not here.
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
  line states the match count and how many matching hosts are normally hidden, and the
  matching characters on cards are bold. Enter keeps the filter; Esc restores the
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
  an active filter) take its place while they apply. Arming the prefix opens the
  cheatsheet from the indicator toward the terminal view while the indicator keeps the
  prefix: across the terminal view's columns on a side column's indicator row, on the row
  below a top band's seam, and on the row above a bottom band's seam. Only the PAINT
  moves, floating over the live grid and leaving the layout alone so no card shifts. When the nav is auto-hidden, a live
  prefix interaction brings the nav back for the moment it needs it (a jump reads the
  card numbers), and it hides again when the interaction ends.
- **FR-B10** - Every unselected card carries a 1-based number in its address column, on
  the row of the session it addresses, and `prefix <digit>` jumps to it. The selected
  card holds the selection mark in that same column instead. Selecting a card changes
  nothing else on the card (the address column keeps its width), so a
  name holds its column as the selection passes over it. The input stays
  open in the hint bar so the number can grow, and every digit is taken as typed: the
  number only has to name a real card at Enter. Each edit moves the selection while
  the number names a card and leaves it alone otherwise; `Enter` closes when the
  number names a card and flashes the valid range while leaving the input open
  otherwise; `Esc` returns to where the jump started.
- **FR-B11** - Every colour xmux paints is an ANSI-16 slot, so the TERMINAL THEME
  resolves the hue and the whole UI recolours with the user's own scheme. A THEME is a
  named role→ANSI-slot assignment curated in a registry: the built-ins are
  `auto-dark` (the default) and `auto-light`, one for a dark and one for a light
  terminal background, and `[ui] theme` selects one (an unknown name falls back to
  `auto-dark`, reported by `xmux doctor`). The session level reads BOLD so the level a
  user actually picks stands off the text parts of the same line; the
  hint bar keys read its own `bar_accent` slot, because a slot that reads on the cards
  may not read on the bar's own background. Every interaction screen renders key tokens
  in the same bold shape. A section title reads in the dim `decoration` slot, the same
  quiet role as the card numbers, so the group label stays below the sessions it names.
  What the sixteen slots cannot say is said with an attribute: the selected card is
  REVERSE VIDEO while the nav holds focus, the terminal swapping its own pair, which is
  what a theme itself means by "selected"; while the terminal holds focus the selected
  card keeps only its mark, so the selection and the view border say the same thing
  about the focus.
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
  cells of name, and never narrower than the collapsed nav, so a wider configured prefix
  can raise that floor; a band is never less than one row. The values therefore travel as
  ONE value carrying
  the width the user set, the width on screen, the band height, the attachment side, and
  the collapsed state,
  so the renderer, the PTY sizing and mouse hit-testing cannot read different answers,
  and the effective width keeps its single owner. Hiding the nav does not move the
  layout: the side travels with the hidden nav, so the nav returns the shape it left.
  `prefix z` collapses and expands it from either view,
  and dragging the view border past the nav's minimum width or height collapses it;
  dragging back out within the same drag expands it at the size the pointer reached. A
  collapsed side nav is a column exactly as wide as the resting prefix with a cell either
  side, the prefix on its bottom row; a collapsed band is the seam row alone, the prefix at
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
  lapses, so a whole burst of arrows is one interaction. The cheatsheet and the
  auto-hidden nav show for exactly that span, so neither drops out from under an
  interaction still running.
- **FR-B19** - A prefix waits for the next INPUT, and a mouse action is input: a click, a
  release, a wheel or a drag cancels the prefix chord (the ready wait) in either focus,
  because mouse bytes are scanned
  out of the stream before either focus path's key handling sees them and a chord left
  half-open keeps its cheatsheet on screen and then eats the next key. Bare hover is not
  an action: the pointer drifting must not break a chord being typed.
- **FR-B20** - Input is read as key presses only, because a terminal's byte stream
  carries no key-up. A held prefix is therefore indistinguishable from repeated taps and
  is treated as such: each repeat sends the doubled-prefix literal to the pane and blinks
  the cheatsheet for as long as the key is down. Recovering the key-up would mean
  requiring the kitty keyboard protocol from the terminal and from every mux enclosing
  xmux, which would make behaviour depend on what that chain passes through; a uniform
  input path everywhere is worth more than this one case.
- **FR-B21** - The nav is two BANDS, and the cards of a host with no session to show are
  the lower one: a host card sits below every session card, whatever order the hosts were
  scanned in. In the side column, while the cards can spare a row for it, the bands are
  pushed APART - the session cards against the top edge, the host cards against the bottom
  - and the blank rows between them are the parting, since a gap says a different kind of
  thing follows without spending a glyph on saying it. Once they cannot the column is one
  scrolling list, because a gap only parts what is on screen together, and a rule across
  the cards takes the boundary's row instead. The parting always holds a row of its own:
  the column is measured with the rule's row counted in, so the bands go from a gap of one
  straight to a rule and never meet, and the list starts scrolling a row before the cards
  alone would fill it. Neither the gap nor the rule is a card: a click on either moves
  nothing. In the portrait band the parting is the same statement on the other axis: the
  session columns hold the left edge, the host band is pushed to the right while a blank
  column parts them, and a vertical rule takes the boundary's column once they cannot. A
  list with NOTHING but host cards is the host band alone, and it still takes its side of
  the split: anchored to the bottom (side) / right edge (portrait), the blank rows or
  columns opposite being where the sessions that will be found land, so a scan reads as
  the pending hosts draining toward the sessions they become. The host band is HIDDEN
  while the terminal view holds the focus, decided once on the move from the nav into
  it: a session card selected then hides the band, because what the user went to look
  at is a session and hosts with nothing to show are noise beside it; a host card
  selected keeps it, because the screen beside the nav is that host's own. A modal
  over the terminal view is not a move back, the move back into the nav shows the band
  again, and a selection that reaches a host card while the band is hidden shows it,
  since a selected card is never one nobody can see. While a prefix is live the band is
  painted, because the hint bar offers a jump to any card by number, and it is hidden
  again when the prefix ends. Hiding takes the cards off the screen, not off the list:
  their numbers and the keys that walk the list stay the same.
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

- **FR-B24** - The nav hides the hosts no scan has reached: an unreachable host takes no
  card by default, and `[ui] hide-unreachable` (default true) controls that hiding. The
  filter naming a hidden host brings its card back, and that named card is the one entry
  to its unreachable screen. An empty filter hides every unreachable host, and a filter
  matching nothing does not bring them back through the no-match fallback that shows the
  other hosts. A reachable host with no sessions keeps its card, and a host still scanning
  never hides, whatever stale failure it carries. A host that goes unreachable mid-run
  hides from that result on and returns when a scan answers. A host the user LOGGED IN to
  keeps its card while xmux still holds that machine's credential. A blocked host is kept
  because it is actionable, and a credential-backed host is kept while its requested
  result is pending. The exemption ends when an authentication refusal, roster removal,
  broker outage, or process exit makes the credential unavailable and is per machine, since a login authenticates
  the machine and not the one mux whose card carried the pane. A listing parse failure
  proves that the host answered, so its `✗` card remains visible and its host screen
  states the parser reason.
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
  opposite side. The focus arrow pairs follow the placement (FR-B14), and the cheatsheet
  and help modal name the pair the current placement makes active.
- **FR-B26** - BLOCKED means ssh refused the host for a reason the submitted login
  answers. Two of ssh's own refusals enter this state: its final account-and-host
  authentication line, and a host-key verification failure for a host with no recorded
  key when the effective ssh policy is `ask`, which the submitted login resolves. An
  unknown key under a strict policy stays unreachable and gives a command that displays
  its fingerprint. Remote command
  permissions, name resolution, connectivity failures, and a changed host key stay
  unreachable. A
  blocked host keeps its card whatever hide-unreachable says, renders the `?` mark, and
  shows the pane above the same failure facts the unreachable screen states. What it was
  blocked on is not in its state word: the reason row carries a plain-language summary
  followed by ssh's sanitized detail.
- **FR-B27** - The LOGIN PANE holds the three values ssh will not ask for and must know
  before it dials - the address, the port, and the username - with an optional masked
  password beside them. Every value starts at what ssh WOULD use. An address, port, or
  user from OpenSSH's effective configuration wins; the matching stanza is the fallback
  when OpenSSH cannot report it. Missing values use the address a provider
  reported else the host's own name, port 22, and this machine's account name. Each
  field shows whether its value came from ssh configuration, discovery, a default, the
  host name, the local account, or an edit. A required field is marked in its
  label and an empty optional one says so in the space its value would occupy. It is not
  a modal and nothing in the nav drives it. Enter means one thing throughout: submit from
  the button, pass the focus on from anywhere else. Space picks a choice, Tab and the
  vertical arrows walk the stops, and an escape sequence xmux does not act on is consumed
  whole rather than landing in a field as text. While a login runs the pane keeps every
  value on screen and says so in place of the button it was submitted from, taking no key
  but the lone Esc that ends the attempt, since there is nothing left to fill in.
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
  keys from there and the account is one. Either form adds the line only when it is
  absent, and an ed25519 pair is generated first when the machine has no key to send.
- **FR-B31** - Persistent UI symbols are conventional glyphs that OS-default terminal
  fonts render in one cell without emoji presentation. The vocabulary includes `❯`,
  `✓`, `✗`, braille spinner frames led by `⠋`, box drawing led by `╭`, `▲`, `?`, and
  `…`. A terminal smaller than 24 columns by 4 rows renders only a size screen naming
  the required and current dimensions, so the nav and terminal view never overlap.

- **FR-B32** - The result of work the user started is a TOAST: a login with the
  public-key registration and ssh-config recording it ran, a new session, and a re-scan.
  The newest recorded release is announced the same way at launch. A toast floats in the
  terminal view's top corner farthest from the nav, or its bottom right corner when the
  nav rides on top, is at most 40% of the window wide, and wraps a long reason inside that
  width. A toast of successes and facts leaves after five seconds and shows the time it has
  left as an underline under its first line that shrinks with it, which needs no extra
  row and no colour the view border already uses. A toast carrying a warning or a failure
  stays until a click on it or opening the history dismisses it. Three toasts stand at
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
  rest. A re-scan that changed nothing says so with the host and session counts it found.
  The toast is made once every source has answered, and a host that stopped answering
  keeps it on screen until it is dismissed.

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
  is marked `▲` and gains the word `unreachable` when selected, and its view screen states
  everything known about the failure rather than leaving the user with a message alone -
  the reason its transport
  gave, how many failures in a row it is, the mux binary asked for, how the machine is
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

## E. Session management

xmux aggregates and switches; it does not edit what a mux already edits. Starting a
session is the one mutation it keeps, because a reachable source with no sessions has
nothing to switch to until one exists.

- **FR-E1** - Create a session on a HOST card (`prefix n`), then it appears in the
  nav. On a session card the action is refused with a flash naming where to press it.
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
  an older Windows client does not enter the password path. Attach requests a tty.
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
