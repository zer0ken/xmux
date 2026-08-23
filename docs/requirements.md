# xmux: functional requirements & use cases

xmux is a stateless cross-environment session switcher: one terminal that sees and
moves between every reachable tmux/psmux/zellij session, local and over ssh,
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
  a dead host is reported unreachable; "every source unreachable" is distinguished.
- **FR-A3** - `xmux doctor` reports config health, ssh availability, and per-source
  reachability with session counts.
- **FR-A4** - Sessions are ordered by recency (most-recently-attached first) within
  their source, and the sources themselves by their most recent session, so one source's
  cards are contiguous and the nav never names a source twice.
- **FR-A5** - The roster (which ssh targets are offered) comes from providers the
  `[discovery]` table selects: `~/.ssh/config` aliases (on by default) and this
  machine's tailnet peers (off by default, since it runs an external CLI). A tailnet
  peer is offered under its DNS label; this machine and offline peers are skipped. A
  provider that cannot answer contributes nothing instead of failing the run, and
  ssh-config names keep their position when a provider repeats them.
- **FR-A6** - A host's mux is identified by what its binary answers as, not by the
  name it was invoked under, so tmux, psmux, and zellij mix freely across hosts with no
  configuration. Each mux is one family behind the mux axis: the command plans
  default to tmux-compatible argv (so a tmux-compatible mux is identity plus a few
  overrides), and a mux that shares no argv with tmux overrides every plan together with
  the shape of what each plan prints. zellij is that case: it is enumerated from its
  session listing, its windows come from its tab listing, and its sessions are polled
  because it offers no push channel.
- **FR-A7** - A SOURCE is one mux on one machine, so a machine running several
  muxes at once contributes one source per mux and every one of them is listed. A `mux`
  value is a name or a LIST of names, in `[local]` and in `[[hosts]]` alike. A machine
  given several muxes has its sources named `<machine>:<mux>`; a machine given one keeps
  the bare machine alias, so an existing setup's ids, addresses, and typed targets do
  not move. `exclude` names machines, so it drops every mux on one. A listed mux that is
  not installed there surfaces as unreachable rather than being dropped, because a name
  the user wrote is a name they meant.
- **FR-A8** - A polled host cannot wedge on one unanswered command. Every command in
  a poll sweep runs under a fixed per-command budget, because the poll ticker only
  advances once the sweep returns: a timed-out listing surfaces as that host's error
  (the nav shows it unreachable), and a timed-out pane query still emits an EMPTY panes
  answer, since a card whose panes never arrive would keep its spinner forever.
- **FR-A9** - No mux list needs configuring, on any machine. A machine that named no
  mux is asked which of the ones xmux SUPPORTS it has, and each one that answers becomes a
  source. The candidate set is what xmux can drive, and each candidate is asked with the
  same identity probe a configured mux gets, so a binary carrying a mux's name while being
  another mux is not counted as that mux: where psmux answers, a `tmux` that also answers
  is psmux's own alias of itself (which names itself by the name it was invoked under, so
  no probe can tell it apart) and is dropped. A WRITTEN value is never probed, keeping
  FR-A7's rule that a name the user wrote stays visible even when it is missing; a machine
  where nothing answers keeps the mux it was assumed to run, so the nav names what is
  unreachable rather than showing nothing.
- **FR-A10** - A REMOTE machine is discovered AFTER launch, asynchronously, and its
  answer only ADDS. The app paints the sources the config names first (a remote probe is
  an ssh round trip per mux, and nothing may wait for that), then each machine's answer
  arrives and every mux it reports that the machine does not already serve becomes a
  scanning card on the spot. An added source's id is always qualified (`prod:zellij`)
  while the mux already served keeps the id it was painted with: that id is what the
  frozen order, the persisted selection, and anything the user typed are keyed to, so
  nothing is renamed and nothing is removed. New cards APPEND, so a card the user is
  looking at does not move because another machine answered. An added source is
  OPERABLE, not merely visible: creating a session on it, reading its panes, and reading
  its border styles work exactly as on a configured source.

## B. The switcher: "see the list, decide whether & where to move"

- **FR-B1** - The nav renders ONE CARD PER SESSION across every reachable source,
  most recently used first: a context line `{host}/{mux}` over a detail line
  `{session}/{window}` naming the session's focused window. The list is flat, with
  no window or pane rows: xmux aggregates and switches, and the mux itself already shows
  its own windows. The window is written in its own mux's convention, `{index}:{name}`
  for tmux and psmux and the bare tab name for zellij, so a card reads the way that
  mux's own status line reads.
- **FR-B2** - Render-first: the host skeleton paints instantly; each source's
  sessions and each session's panes stream in independently.
- **FR-B3** - The terminal view shows the confirmed session's live grid and follows
  the cursor. A switch keeps the prior grid on screen until the new one is ready
  (stale-while-revalidate); only the first launch, before any grid exists, shows a
  blank view. The `scanning…` / `loading…` state hints live in the nav, not here.
- **FR-B4** - Navigation: up/down/home/end/pgup/pgdn; fuzzy filter over
  `<source>/<name>`; manual `prefix r` rescan.
- **FR-B5** - Surveying without committing is first-class: xmux is a switcher, not a
  session owner. Quitting (`prefix q`, or the ctl `quit` verb) leaves the current
  mux session untouched: it is never killed or altered by exiting.
- **FR-B6** - Under a filter, `Enter` attaches the **visible (filtered)** session,
  never a filtered-out one, even when a host row is selected.
- **FR-B7** - Per-element state hints: `scanning…`, `loading…`, `(empty)`,
  `⚠ unreachable: <reason>`. A card carries the clause that NAMES the failure, since
  a tool wraps it in its own context and a card is only as wide as the nav; the selected
  host's panel carries the whole message, so no part of why it failed is cut off.
- **FR-B8** - A session running xmux is never mirrored into the terminal view.
  This is prevented structurally, not by a runtime check: the nest guard (FR-D3)
  refuses to run xmux inside a mux, so no attachable session can be running xmux.
- **FR-B9** - The nav's bottom row is a status line, not a screen-wide footer. At
  rest it names only the prefix; the states that outrank it (a refusal, scan progress,
  an active filter) take the row while they apply. Arming the prefix widens the PAINT
  to the whole window so the cheatsheet floats over the view border and the live grid,
  leaving the layout alone so no card shifts.
- **FR-B10** - Every unselected card carries a 0-based number in its address column, on
  the row of the session it addresses, and `prefix <digit>` jumps to it. The selected
  card holds the selection mark in that same column instead. Selecting a card moves
  nothing on its rows (the column keeps its width and the connector stays drawn), so a
  name holds its column as the selection passes over it. The popup stays
  open so the number can grow, and accepts a digit only while the result still addresses
  a real session, so one-, two-, and three-digit numbers behave identically. Each edit
  moves the selection; `Enter` keeps it, `Esc` returns to where the jump started.
- **FR-B11** - Every colour xmux paints is an ANSI-16 slot, so the TERMINAL THEME
  resolves the hue and the whole UI recolours with the user's own scheme. What the
  sixteen slots cannot say is said with an attribute: the selected card is REVERSE VIDEO,
  the terminal swapping its own pair, which is what a theme itself means by "selected".
  A background xmux picked instead would be wrong on every theme it was not picked for,
  and it cannot be computed from the terminal's own background either, since a terminal
  is free to answer no colour query at all. `[ui] selection-style` names a background
  anyway, in the same colour vocabulary as the view border, and `xmux doctor` reports
  which of the two is in effect because it is invisible on a screenshot.
- **FR-B12** - On a portrait screen the nav is a wide, short band, and its cards flow
  into COLUMNS: down a column, then right. A column takes whole host/mux runs, so a
  source's cards stay together under the one context line naming them and the run that
  does not fit opens the next column rather than splitting across the break; only a run
  taller than the whole column splits, having nowhere else to go, and its continuation
  states its context again. Card order does not change, so the numbers still count in
  reading order. The paint records each card's rect and the hit-test reads it back, so a
  click cannot land on a card the renderer put elsewhere.
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
  the nav (as `prefix Esc` does). An arrow naming the view that already has focus does
  nothing. Bare arrows belong to the cards instead: each steps ONE card along the list,
  back for left and up, on for right and down. Not by column, because the portrait band
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
  the width away entirely. They therefore travel as ONE value carrying the width the user
  set, the width on screen, and the band height, so the renderer, the PTY sizing and mouse
  hit-testing cannot read three different answers, and the effective width keeps its single
  owner. Hiding the nav does not move the layout: the turnover reads the width the user
  SET, so the nav returns the shape it left.
- **FR-B17** - The status row is a bar where it owns its row and a label where it does not:
  the side column's bar fills its row, and so does any armed or flashing bar, which has to
  be readable over what it covers; the portrait band's resting bar paints its text plus a
  cell of padding, leaving the rest of the row to the offscreen counts.
- **FR-B18** - A prefix waits for the next INPUT, and a mouse action is input: a click, a
  release, a wheel or a drag disarms it in either focus, because mouse bytes are scanned
  out of the stream before either focus path's key handling sees them and a chord left
  half-open keeps its cheatsheet on screen and then eats the next key. Bare hover is not
  an action: the pointer drifting must not break a chord being typed.

## C. Switching (the keystone)

- **FR-C1** - A same-server pick switches the live client in place via
  `switch-client` (instant), pre-selecting the chosen window. Each mux's driver owns
  the in-place-vs-reattach decision: with a known display tty it moves xmux's own
  client and repaints; without one it reattaches. The attach is debounced so rapid
  navigation does not storm.
- **FR-C2** - A cross-host pick switches entirely in process, with no picker and no
  detach between. Each host keeps its own live PTY attachment; the target host's driver
  takes over, the previously shown session stays on screen until the fresh grid is ready
  (stale-while-revalidate), and the canonical selection is synced immediately.
- **FR-C3** - Host degradation is graceful, never a silent loss: an unreachable host
  is marked `⚠ unreachable: <reason>`, a reachable-but-serverless host reads
  `(empty)`, a once-connected host keeps its last-known cards on a transient drop, and
  the reconnect sweep self-heals; a dropped display client is reaped and re-attached.
- **FR-C4** - A switch lands on the picked window. A fresh first attach folds the
  window into the attach argv (ssh folds the pre-selection into one `ssh -t`);
  a live client is moved server-side by a lowered `select-window`.
- **FR-C5** - No silent loss: every lowered switch/select command logs its exact argv
  and result; a failed attach is logged at warn level and returns to the nav rather
  than being swallowed; each driver logs its show decision and the grid-changed effect.

## D. App lifecycle

- **FR-D1** - `xmux` (no subcommand) is a persistent supervisor that owns the
  terminal and runs one mux-client child at a time per session, plus one `-CC`
  metadata client per remote host, over a single async event loop.
- **FR-D2** - The app serves its control socket concurrently while a session is
  displayed (attach spawning is off-loop), so `ping` / `dump` / `status` / `switch`
  are answered without blocking.
- **FR-D3** - Running the app inside a mux is refused (exit 2 with guidance), not
  warned: nested, every attach is refused, leaving a doomed loop.
- **FR-D4** - Socket hygiene: a stale socket is removed before bind, the socket is
  owner-only (`0600`) on unix, and it is removed on exit. A crashed instance's leftover
  `ctl-*.sock` marker is swept on the next startup (any marker whose socket no longer
  dials). Discovery enumerates the markers newest by mtime first, tie-broken by higher
  pid.
- **FR-D5** - The app launches directly into the persistent split view (nav +
  terminal view) with the cursor preselected: the persisted last session if set,
  else a local-first recency preselect. There is no separate picker mode; `prefix q`
  quits.

## E. Session management

xmux aggregates and switches; it does not edit what a mux already edits. Starting a
session is the one mutation it keeps, because a reachable host with no sessions has
nothing to switch to until one exists.

- **FR-E1** - Create a session on a HOST card (`prefix n`), then it appears in the
  nav. On a session card the action is refused with a flash naming where to press it.
- **FR-E2** - There is no rename, kill, or window/pane command: not on a key, not
  in a modal, not on the wire, and not in the mux command vocabulary.
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
  exits non-zero.
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
- **UC-5, the remote is down and I am not left in the dark.** An unreachable host shows
  `⚠ unreachable`; a failed attach is logged and the nav stays usable.
  *(FR-A2, FR-B7, FR-C5)*
- **UC-6, deep in a remote, get back home.** Native detach (`prefix d`) inside the
  remote returns control to the local app's split view; pick local or another host.
  *(FR-C2, FR-D1)*
- **UC-7, spin up a throwaway on a remote and switch to it.** Create on the
  host's card, then switch to it. *(FR-E1, FR-C2)*
- **UC-8, survey what's running everywhere before deciding.** The nav shows every
  session on every host with its focused window; the terminal view previews the selection.
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
