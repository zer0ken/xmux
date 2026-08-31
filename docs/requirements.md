# xmux: functional requirements & use cases

xmux is a stateless cross-environment session switcher: one terminal that sees and
moves between every reachable tmux/psmux/zellij/screen session, local and over ssh,
regardless of OS or mux kind. Its reason to exist is to deliver tmux's `prefix + s`
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
  sessions run by name ascending. A routine poll reproduces the same order, so one
  source's cards are contiguous and the nav never names a source twice.
- **FR-A5** - The roster (which HOSTS are offered) comes from providers the
  `[discovery]` table selects: `~/.ssh/config` aliases and this machine's tailnet peers,
  both on by default. A tailnet peer is offered under its DNS label; this machine and
  offline peers are skipped. A provider that cannot answer contributes nothing instead
  of failing the run, so a machine without the tailscale CLI installed reaches an empty
  list rather than an error, and ssh-config names keep their position when a provider
  repeats them. The roster is resolved again on every re-scan, so a machine that has
  come online, and an edit to the `[discovery]` table, both take effect without a
  restart. A machine the roster stops naming is dropped along with every source it
  served and everything on screen for it; a machine it still names keeps the sources it
  is serving, including any that were found by asking the machine rather than by
  configuration.
- **FR-A6** - A host's mux is identified by what its binary answers as, not by the
  name it was invoked under, so tmux, psmux, zellij, abduco, and screen mix freely
  across hosts with no configuration. Each mux is one implementation behind the mux axis: the
  command plans default to tmux-compatible argv (so a tmux-compatible mux is identity
  plus a few overrides), and a mux that shares no argv with tmux overrides every plan
  together with the shape of what each plan prints. zellij is that case: it is
  enumerated from its
  session listing, its windows come from its tab listing, and its sessions are polled
  because it offers no push channel. abduco is the simplest case: one server per
  session, no windows, no control stream, and no per-session query, so its sessions are
  polled from the bare listing and each resolves as the session alone.
- **FR-A7** - A SOURCE is one mux on one host, so a host running several
  muxes at once contributes one source per mux and every one of them is listed. A `mux`
  value is a name or a LIST of names, in `[local]` and in `[[hosts]]` alike. A host
  given several muxes has its sources named `<host>:<mux>`; a host given one keeps
  the bare host alias, so an existing setup's ids, addresses, and typed targets do
  not move. `exclude` names hosts, so it drops every mux on one. A listed mux that is
  not installed there surfaces as unreachable rather than being dropped, because a name
  the user wrote is a name they meant.
- **FR-A8** - A polled source cannot wedge on one unanswered command. Every command in
  a poll sweep runs under a fixed per-command budget, because the poll ticker only
  advances once the sweep returns: a timed-out listing surfaces as that source's error
  (the nav shows it unreachable).
- **FR-A9** - No mux list needs configuring, on any host. A host that named no
  mux is asked which of the ones xmux SUPPORTS it has, and each one that answers becomes a
  source. The candidate set is what xmux can drive, and each candidate is asked with the
  same identity probe a configured mux gets, so a binary carrying a mux's name while being
  another mux is not counted as that mux: where psmux answers, a `tmux` that also answers
  is psmux's own alias of itself (which names itself by the name it was invoked under, so
  no probe can tell it apart) and is dropped. A WRITTEN value is never probed, keeping
  FR-A7's rule that a name the user wrote stays visible even when it is missing; a host
  where nothing answers keeps the mux it was assumed to run, so the nav names what is
  unreachable rather than showing nothing.
- **FR-A10** - A REMOTE host is discovered AFTER launch, asynchronously, and its
  answer only ADDS. The app paints the sources the config names first (a remote probe is
  an ssh round trip per mux, and nothing may wait for that), then each host's answer
  arrives and every mux it reports that the host does not already serve becomes a
  scanning card on the spot. An added source's id is always qualified (`prod:zellij`)
  while the mux already served keeps the id it was painted with: that id is what the
  deterministic order, the persisted selection, and anything the user typed are keyed
  to, so
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
  its own windows, so a card has nothing to add below the session name.
- **FR-B2** - Render-first: the source skeleton paints instantly; each source's
  sessions stream in independently.
- **FR-B3** - The terminal view shows the confirmed session's live grid and follows
  the cursor. A switch keeps the prior grid on screen until the new one is ready
  (stale-while-revalidate); only the first launch, before any grid exists, shows a
  blank view. An attachment a host warms on a session of its own choosing is kept
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
  neither is defined by where a card sits on screen.
- **FR-B5** - Surveying without committing is first-class: xmux is a switcher, not a
  session owner. Quitting (`prefix q`, or the ctl `quit` verb) leaves the current
  mux session untouched: it is never killed or altered by exiting.
- **FR-B6** - Under a filter, `Enter` attaches the **visible (filtered)** session,
  never a filtered-out one, even when a host row is selected.
- **FR-B7** - A card that is WAITING turns ONE spinner trailing its line, in the
  same place whatever the host has or has not resolved, so every scanning card reads as
  the same thing loading and none leaves a blank second row. The nav's scan progress
  turns the same spinner on the same frame. A session is a plain session card from the
  moment its host resolves - no card spins for a resolved session. A card that has
  SETTLED reads a
  word only when it has one to carry: an unreachable host carries its `⚠` mark after
  the host name, while a reachable empty host is a single host row with
  no status word, and its view screen states `no sessions`. A host-state card claims a
  mux only when the mux is CONFIRMED: a settled reachable host's enumeration answered
  through its mux, and a source id that names its own mux was resolved from what the
  machine actually serves. A bare-id host that is unreachable or still scanning claims
  none - the card reads the host alone.
  The word is all a card
  carries: WHY a host failed is stated on
  its view screen, which has the room to keep a tool's diagnostic whole, while a card
  is only as wide as the nav and could carry no more than a cut-down copy of it.
- **FR-B8** - The session xmux is ITSELF running in is never mirrored into the terminal
  view: showing it attaches a second client to the session that HOLDS xmux, which moves
  the user's own client and paints xmux inside itself. The refusal is on the terminal-view
  TARGET, the one value the display reconcile, the attach, and the mux-side switch all
  read, so none of them reaches that session by another path; the card stays selectable
  and killable, and a screen stands in place of the grid to say why. A session running a
  DIFFERENT xmux is not refused - it mirrors like any other session, showing that xmux's
  screen. A session xmux cannot name (the mux does not say, and cannot be asked) is not
  refused either, because a refusal keyed to a guess would hide a session at random.
- **FR-B9** - The nav's bottom row is a status line, not a screen-wide footer. At
  rest it names only the prefix; the states that outrank it (a refusal, scan progress,
  an active filter) take the row while they apply. Arming the prefix widens the PAINT
  to the whole window so the cheatsheet floats over the view border and the live grid,
  leaving the layout alone so no card shifts. When the nav is auto-hidden, a live
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
  may not read on the bar's own background. What the
  sixteen slots cannot say is said with an attribute: the selected card is REVERSE VIDEO,
  the terminal swapping its own pair, which is what a theme itself means by "selected".
  A background xmux picked instead would be wrong on every theme it was not picked for,
  and it cannot be computed from the terminal's own background either, since a terminal
  is free to answer no colour query at all. `[ui] selection-style` names a background
  anyway, in the same colour slots as the view border, and `xmux doctor` reports
  which of the two is in effect because it is invisible on a screenshot. The view
  border's two halves hold the same two slots on every source: what the border states is
  which VIEW holds focus, a fact about xmux, so no host and no mux may recolour it and a
  selection moving between hosts leaves it exactly as it was.
- **FR-B12** - On a portrait screen the nav is a wide, short band, and its rows flow
  into COLUMNS: down a column, then right. A column takes whole SECTIONS (a
  `{host}/{mux}` title over its session cards), so a source's rows stay together under
  the one title naming them and the section that does not fit opens the next column
  rather than splitting across the break; only a section taller than the whole column
  splits, having nowhere else to go, and the continuation picks it up at the TOP of the
  next column, naming nothing: the title stands once, over the column the section starts
  in, and the reading order is what says the continuation is the same section. Card order
  does not change, so the numbers still count in
  reading order. The paint records each card's rect and the hit-test reads it back, so a
  click cannot land on a card the renderer put elsewhere. A title in the band is the
  `{host}/{mux}` label alone: the rule the side column trails after that label underlines
  a group across one full-width run, and a column standing beside another has no such
  width to underline - the rule would reach the gutter and read as a bar parting the
  columns instead. A column is as wide as the widest thing standing in it and the title is
  one of those things, so sessions named in one character do not shrink the column under
  the `{host}/{mux}` above them: a label with more name than room is answered in the
  column's WIDTH, never by carrying the name onto a second row.
  What draws the group in the band is a CONNECTOR down the left of each
  session card, since columns standing side by side leave a card's place in the reading
  order saying nothing about which title owns it. It marks the title that owns the group,
  so a section that splits keeps it in that title's own column and the continuation
  columns carry none. The connector is the title's furniture and NOT part of the card: it
  stands in a strip left of the card's rect, so the selection - which paints a card by
  inverting that rect - leaves it alone rather than notching the line at the one row the
  eye is on, and a click on the strip is a click on no card. Every session card is pushed
  right by the strip whether or not a glyph is painted in it, so a card reads at one
  offset inside its column wherever the flow put it. The side list draws no connector:
  one full-width run under one title needs nothing to say where the group ends.
- **FR-B13** - The nav says what is off screen without spending a row on furniture. The
  side list's scrollbar takes a COLUMN of its own from the nav region, never painted over
  the cards, because the selected card is painted by inverting its whole rect and a thumb
  inside that rect inverts with it into a hole in the bar. The portrait flow scrolls
  sideways instead and says so in words on its status row: `<< 5 more` at the left end and
  `7 more >>` at the right, counting CARDS behind the columns the window does not reach.
  That row is the band's own last row, never a card's. Nothing is drawn while everything
  fits.
- **FR-B14** - An arrow points AT the view it focuses, on either axis: the terminal is
  right of the nav in the side layout and below it in the portrait one, so `prefix right`
  and `prefix down` both focus the terminal while `prefix left` and `prefix up` both focus
  the nav. An arrow naming the view that already has focus does
  nothing. Bare arrows belong to the cards instead: up and down step one card, left and
  right step one category (FR-B4). Neither is by column, because the portrait band
  puts the next card below in one place and one column over in another, and a key that
  moved by column would mean two different things in the two layouts.
- **FR-B15** - Which layout is in force is decided by ONE test, always measured as if the
  nav kept its side column: the terminal that column would leave is the window width less
  the nav and its border, over the window's full height, and while that is WIDER than tall
  the nav is the side column. The moment it is not (square included) the nav becomes the
  top band and the side column is gone. Wider than tall is judged in the proportions the
  user SEES: a terminal row is about two columns tall, so the rows count double. Judging
  it by cell counts alone held the side column until the terminal was half as wide as it
  looked, and measuring the LIVE terminal would flip the test's own input, since going to
  the band hands those columns back and takes rows instead, so the layout would oscillate
  at the boundary.
- **FR-B16** - The nav's width and the portrait band's height are both live: the saved
  pref seeds them, the resize keys step them, a border drag sets them, and auto-hide takes
  the width away while no prefix interaction is live (a live one brings the nav back). The
  width has a floor at the resting prefix label plus a one-cell gap each side, so the
  border can collapse to just past the `C-g` status line and a wider configured prefix
  raises the floor. The values therefore travel as ONE value carrying
  the width the user set, the width on screen, and the band height, so the renderer, the
  PTY sizing and mouse hit-testing cannot read three different answers, and the effective
  width keeps its single owner. Hiding the nav does not move the layout: the turnover
  reads the width the user SET, so the nav returns the shape it left.
- **FR-B17** - The status row is a bar where it owns its row and a label where it does not:
  the side column's bar fills its row, and so does any ready or flashing bar, which has to
  be readable over what it covers; the portrait band's resting bar paints its text plus a
  cell of padding, leaving the rest of the row to the offscreen counts.
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
  the pending hosts draining toward the sessions they become.
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

- **FR-B24** - The nav hides the hosts no scan has reached: an unreachable host takes no
  card by default, and `[ui] hide-unreachable` (default true) turns the hiding off. The
  filter naming a hidden host brings its card back, and that named card is the one entry
  to its unreachable screen. An empty filter hides every unreachable host, and a filter
  matching nothing does not bring them back through the no-match fallback that shows the
  other hosts. A reachable host with no sessions keeps its card, and a host still scanning
  never hides, whatever stale failure it carries. A host that goes unreachable mid-run
  hides from that result on and returns when a scan answers.

## C. Switching (the keystone)

- **FR-C1** - A same-server pick lands on the picked session, pre-selecting the
  chosen window. Each mux's driver owns the in-place-vs-reattach decision, and it
  turns on whether that mux can name xmux's OWN client: one that can moves that
  client with `switch-client` and repaints (instant, nothing torn down); one that
  cannot reattaches by session name, which is the only address that can reach no
  terminal but xmux's own. A switch aimed at a client the mux cannot resolve is
  the failure this rules out: it moves a separate terminal of the user's. The
  attach is debounced so rapid navigation does not storm.
- **FR-C2** - A cross-host pick switches entirely in process, with no picker and no
  detach between. Each source keeps its own live PTY attachment; the target
  source's driver takes over, the previously shown session stays on screen until the fresh grid is ready
  (stale-while-revalidate), and the canonical selection is synced immediately.
- **FR-C3** - Source degradation is graceful, never a silent loss: an unreachable source
  is marked `⚠ unreachable`, and its view screen states everything known about the
  failure rather than leaving the user with a message alone - the reason its transport
  gave, how many failures in a row it is, the mux binary asked for, how the machine is
  addressed and the wait that bounds reaching it, the socket, the session-listing command
  itself (spelled so it can be run by hand outside xmux), the PROVIDER that put that host
  on the roster (so a host the user never wrote down is traceable to the thing that
  offered it, and to the `[discovery]` key that would turn it off), the ssh stanza it was
  reached through, what the OTHER muxes on that same machine answered (which is what says
  whether the machine or the mux is down), and the log file holding the full history; a reachable-but-serverless source reads `(empty)`, a once-connected source keeps its last-known cards on a transient drop, and
  the reconnect sweep self-heals; a dropped display client is reaped and re-attached.
- **FR-C4** - A switch lands on the picked window. A fresh first attach folds the
  window into the attach argv (ssh folds the pre-selection into one `ssh -t`);
  a live client is moved server-side by a dispatched `select-window`.
- **FR-C5** - No silent loss: every dispatched switch/select command logs its exact argv
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
- **FR-D6** - The log records what HAPPENED, never the rate xmux asks. A sweep that says
  what the sweep before it said is not written: an unchanged session list is not, and
  neither is a failure already standing, which is counted instead. A failure is written
  when it arrives and when its message changes, and the source answering again is written
  too, with how many sweeps failed, so a run of failures reads as one event with a
  beginning and an end. Without this a source that cannot answer writes one line every
  poll for as long as xmux runs, and it alone fills the file.
- **FR-D7** - No log grows without end. The daily files are kept for a bounded window and
  the oldest goes as a new day opens. A panic that a worker recovers from and hits again on
  the next frame is written by its SITE at each doubling of its count, not once per
  occurrence, so the first is kept, the scale is kept, and a repeating internal error
  cannot bury the file. A panic that ends the app is always written whole.

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
  `switch <source>/<session>`, `focus <terminal|nav>`, `rescan`, `quit`,
  `width <delta>` (a signed column delta, not an absolute width), `toggle-auto-hide`)
  and its one session-lifecycle verb, `new-session` (sessions addressed
  `<source>/<session>`), resolve to a domain action. There are no kill/rename/window
  verbs: xmux aggregates and switches, so editing a session stays with the mux. Raw
  key/text injection stays behind the unstable `raw:` namespace (`raw:key` /
  `raw:keys` / `raw:text`). A command-level failure replies `err: …` and `xmux send`
  exits non-zero. A `switch` to a `<source>/<session>` address the inventory does not
  list is such a failure: it replies `err:` naming which half is missing (the source,
  or a session under a present source), so the reply reflects the address resolution.
- **FR-F2** - There is one unified socket, not a separate app socket: `switch <address>`
  is a first-class ctl verb resolving to the same switch action a key press does.
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

- **FR-G1** - ssh uses a connect-timeout; listing uses `BatchMode` (never hangs on a
  prompt); attach requests a tty; ControlMaster multiplexing is added only off Windows.
- **FR-G2** - A session name from a remote list is injection-safe when it re-enters
  a remote shell command (POSIX single-quote escaping).
- **FR-G3** - Mux session env (`TMUX`/`TMUX_PANE`/`PSMUX*`) is stripped for listing so a
  command run from inside a mux is not refused as nesting; lookalikes survive.
- **FR-G4** - A remote attach folds the window pre-selection into the single
  `ssh -t` connection (no second connection to hang or lose), and the mux axis supplies
  the attach argv (local psmux routes to its per-session server).
- **FR-G5** - A command bound for a WSL distribution is exec'd there rather than handed
  to the launcher as a command LINE, so Windows quoting is never re-read as shell syntax
  and the POSIX quoting of FR-G2 stays the only boundary a session name crosses. The
  command then runs in a LOGIN shell, because a mux installed under the user's own home is
  not on the bare environment's `PATH`. A distribution's attach folds its pre-selection
  into one command exactly as FR-G4's remote attach does.

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
  `⚠ unreachable`; a failed attach is logged and the nav stays usable.
  *(FR-A2, FR-B7, FR-C5)*
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

- **Honesty** - The nav shows only what it can back with an answer, and says
  so when it cannot. A value is never guessed, assumed, or shown as a fact
  before it is one: a mux appears on a card only when the enumeration
  answered through it or the machine was resolved to serve it, an unresolved
  host card turns a spinner instead of a value, and a failure keeps its own
  state colour while the reason is stated on the screen. *(FR-B7)*
