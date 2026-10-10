# xmux: functional requirements & use cases

xmux is a cross-machine, cross-mux session switcher that brings tmux's `prefix + s`
experience across machines: one terminal that sees and switches in place between every
reachable supported mux session across local machines, WSL, and SSH.
Each requirement has a stable ID and states one behavior the implementation is checked
against, naming no source file, function, or test.

---

## A. Discovery & inventory

- **FR-A1** - `xmux ls` lists every reachable session across all hosts, one line per
  session written as its `<machine>/<mux>/<session>` path.
- **FR-A2** - A reachable mux with no sessions is reported as empty, a host on a dead
  machine as unreachable, and the case where every host is unreachable is distinguished.
- **FR-A3** - `xmux doctor` reports config health, ssh availability, and per-host
  reachability with session counts.
- **FR-A4** - Sessions are ordered deterministically (local, then WSL, then remote
  machines, each tier by host name, sessions by name), and a re-enumeration reproduces
  the same order.
- **FR-A5** - The machine roster comes from the providers the `[discovery]` table enables
  (`~/.ssh/config` aliases and one-hop neighbours that answer ssh), is resolved again on
  every re-scan, drops a machine a record stops naming, and keeps a machine only a probe
  offered as an unreachable card.
- **FR-A6** - A machine's mux is identified by what its binary answers as, so tmux, psmux,
  zellij, abduco, screen, tuios, and herdr mix freely across machines with no
  configuration.
- **FR-A7** - A host is one mux on one machine, so a machine given several muxes (a `mux`
  list in `[local]` or `[[hosts]]`) contributes one `<machine>:<mux>` host per mux, and a
  listed mux that is not installed surfaces as unreachable.
- **FR-A8** - Every command in an enumeration runs under a fixed per-command budget, so
  a timed-out listing shows that host as unreachable instead of holding it open.
- **FR-A9** - A machine that names no mux is probed for each mux xmux supports and serves
  exactly the ones that answer, with no card when none does.
- **FR-A10** - A remote machine's muxes are discovered asynchronously after launch, and its
  answer only adds hosts, without renaming or removing any card already shown.
- **FR-A11** - A mux inside a WSL distribution is a host on its own machine
  `wsl.<distribution>`, offered by the `[discovery] wsl` provider or a `[[wsl]]` entry,
  and behaves as FR-A7 to FR-A10 describe.
- **FR-A12** - A re-enumeration reads a session as renamed only when the mux lists the
  same session identity under a new name, and then the card, its number, the selection,
  and the display follow the new name; a session killed and another created between two
  listings are a lost session and a new one.

## B. The switcher: "see the list, decide whether & where to move"

- **FR-B1** - The nav renders one single-row card per session, flat with no window or
  pane rows, grouped under a non-selectable `{machine}/{mux}` section title in the
  deterministic order.
- **FR-B2** - The host skeleton paints instantly and each host's sessions stream in
  independently.
- **FR-B3** - The terminal view shows the confirmed session's live grid, follows the
  cursor, and keeps the prior grid on screen during a switch until the fresh attachment
  paints or its bounded wait ends.
- **FR-B4** - Up/down step one card, left/right step one category, a fuzzy filter
  narrows the list over `<host>/<name>`, and `prefix R` re-scans every machine.
- **FR-B5** - Quitting (`prefix q` or the ctl `quit` verb) leaves every mux session
  untouched.
- **FR-B6** - Under a filter, `Enter` attaches the visible filtered session, never a
  filtered-out one.
- **FR-B7** - Every host-state card shows one fixed-width state glyph (a spinner while
  waiting, `?` login needed, `▲` unreachable, `✗` listing failure, blank for empty) and
  names a mux only once that mux is confirmed.
- **FR-B8** - The session xmux itself runs in is never mirrored into the terminal view
  and shows a screen saying why instead, while its card stays selectable.
- **FR-B9** - The nav carries a prefix hint that names the prefix at rest and opens
  the key list over the terminal view when the prefix is armed, without moving any card.
- **FR-B10** - Every card carries a number, and `prefix <digit>` jumps to the card with
  that number.
- **FR-B11** - Every colour xmux paints is an ANSI-16 slot chosen by the `[ui] theme`
  (`auto-dark` or `auto-light`), so the terminal's own scheme resolves every hue.
- **FR-B12** - A group is drawn the same way at every nav position, as a `{machine}/{mux}`
  title over its indented cards, flowing into whole-section columns in a horizontal nav.
- **FR-B13** - The nav marks off-screen cards on its nav border only, with a thickened
  `┃` stretch beside a side list and `‹ N` / `N ›` counts on a horizontal nav.
- **FR-B14** - With the prefix, the arrow pair facing the terminal focuses the terminal
  and the other pair focuses the nav, while bare arrows move between cards.
- **FR-B15** - The nav's side is a placement pinned at runtime by `prefix p` when one
  exists, else the `[ui] nav-position` default, and the nav never moves on its own.
- **FR-B16** - The nav's width, horizontal nav height, side, and collapsed state are live and
  persisted, set by resize keys, nav border drag, `prefix z`, and auto-hide.
- **FR-B17** - The prefix hint is a chip naming the prefix, and the active filter paints
  its match cell by cell; a result of an action is a toast, never advice.
- **FR-B18** - A prefix interaction lasts until the function it starts ends, and the key
  list and an auto-hidden nav show for exactly that span. While the terminal view holds
  the focus, a nav that would leave it smaller than 24 columns by 4 rows hides as an
  auto-hidden nav does.
- **FR-B19** - A mouse click, release, wheel, or drag cancels a pending prefix chord,
  while bare hover and dragging the key list do not.
- **FR-B20** - A held prefix key counts as repeated taps, each sending the
  doubled-prefix literal to the pane.
- **FR-B21** - The nav shows three groups in order (session cards, reachable hosts with
  no sessions, unresolved machines) separated by one blank row in a side nav or one
  blank character column in a top or bottom nav, including while scrolling.
- **FR-B22** - A machine and its mux are always shown as one `{machine}/{mux}` label, except
  for a mux not yet known, where the card reads the machine alone.
- **FR-B23** - When the mux moves xmux's own display client to another session, the nav
  selection follows in terminal focus and the client is carried back in nav focus.
- **FR-B24** - `prefix h` opens the table of machine problems, grouped by login needed,
  unreachable, and inventory failure, where Enter or a click selects the machine and
  opens its login pane.
- **FR-B25** - The nav attaches on the left, top, right, or bottom of the terminal view,
  or floats as a box over the terminal's empty space, with `prefix p` cycling the side
  clockwise and unpinning at `floating`, the last position, and the layout inside the
  nav identical at every side. The floating box is content-fit (the widest card line
  plus its border wide, the card list plus its border tall) with the prefix hint on its
  top border's left, starts at the top-right corner, and moves in real time to the
  widest text-free area, its right border never more than five cells from the window's
  right wall. Dragging it from anywhere on the box moves it anywhere in the window; a
  drop holds the position for ten seconds, then the position is forgotten and the scan
  resumes. While the nav view holds the focus the box docks and behaves exactly as a
  right nav does; the focus's return to the terminal view undocks it.
- **FR-B26** - A machine ssh refuses for a reason a login can fix is blocked: it shows `?`
  and the login pane, with the failing input field marked `✗`.
- **FR-B27** - The login pane takes the address, port, username, and an optional masked
  password prefilled from what ssh would use, and lists each login step's progress after
  submit. A login that works closes the pane at once, and the machine screen states the
  scan of its hosts with a spinner. What ssh would use for a machine is read again after xmux records or removes the
  machine's ssh config entries and on `prefix r`, so the login pane and the machine screen
  follow the file as it is.
- **FR-B28** - A submitted password is held only in process memory, released to ssh only
  through xmux's private askpass broker for the exact account and machine, and forgotten on
  refusal, removal from the roster, or exit.
- **FR-B29** - The login pane offers recording only while an entered address, port, or
  username differs from what ssh resolves for the machine; after a working login, recording
  writes one xmux-marked stanza with the values that worked at the top of
  `~/.ssh/config`, replacing an earlier xmux stanza and never storing the password.
- **FR-B30** - After a working login, registering adds this PC's public key to the
  machine's authorized keys file with the `xmux-registered` mark and reports registered
  only when a key-only login then succeeds. This choice also saves the working address,
  port, and username in an xmux-marked `~/.ssh/config` stanza, never the password.
- **FR-B31** - Persistent UI symbols use only one-cell glyphs that OS default terminal
  fonts render, and a terminal smaller than 24 columns by 4 rows shows only a size
  screen.
- **FR-B32** - The result of a user action (a login, a new session, a re-scan, or the
  reason an action was refused) appear as a toast in the terminal view's corner, which `[ui]
  notifications` can turn off. A value typed into a popup that the popup cannot accept is
  stated inside that popup beside its field, and the popup stays open, with no toast.
- **FR-B33** - `prefix m` opens the history of every toast and background event, newest
  first, bounded at 200 records.
- **FR-B34** - A re-scan ends in one toast stating what changed since the re-scan
  request, and `prefix r` re-scans only the selected card's machine.
- **FR-B35** - Every key xmux binds is defined once in one key table that both focus
  paths and every key surface read.
- **FR-B36** - Pressing the prefix opens the key list at once, naming every key the
  prefix unlocks by section and never dropping the jump, help, or quit keys.
- **FR-B37** - While the nav holds the focus, the selected standalone card may end in
  `⏎` when it fits inside the card without covering text or padding, and a selected
  part of a shared item carries none; a move of the selection leaves the
  prefix hint as it is.
- **FR-B38** - The help lists every key by section and a legend of every glyph, with
  section tabs, scrolling, and case-insensitive search, closed by `Esc` or `prefix ?`.
- **FR-B41** - Every popup body row wraps to the popup's width, except a text field, and
  a popup too tall for the window scrolls.
- **FR-B39** - Logging out of an SSH machine (`prefix L`, confirmed by typing `logout`)
  removes this PC's matching public keys from the machine's key files and the ssh config
  stanza a login saved for the machine, then drops the held password and the machine's
  connections. A key line or a `Host` entry naming the machine exactly that xmux did not
  add changes only after one second confirmation covering both: an entry naming only
  this machine goes with its options, an entry naming other machines too loses only this
  name, and every other line of ssh config stays as it was.
- **FR-B40** - When the selected card leaves the list, the selection moves to the
  nearest remaining node up its lineage, and a new card takes the selection only when
  the user asked for it (Selection by Interest in docs/principles.md). When nothing of
  the selected machine is left, the selection names nothing and the terminal view shows
  the landing list and attaches nothing until the user picks a card, or until a filter
  edit lists the node the user was on again.
- **FR-B42** - Machines, hosts, and sessions each have a view screen linked to one
  another, reached through `Ctrl-↑` / `Ctrl-↓`, the section title parts, and the screen
  links.
- **FR-B43** - From launch until the first execution the terminal view attaches to no
  session and shows the landing screen: how many machines the scan has reached, with the
  scan spinner, and every nav card in nav order and numbering as its `/`-separated path.
  The landing list and the nav share one selection, which only highlights; the
  first execution closes the landing screen for the rest of the run, opens the chosen
  card, and focuses the terminal view.
- **FR-B44** - Every surface names the levels machine, host, and session, with `host`
  always one mux on a machine, a machine screen and a host screen open with their level
  and path (`machine db-01`, `host db-01/tmux`), and each states only the facts of its
  own level. A surface that names a session apart from its host's section, title, or
  screen writes its whole path (`db-01/tmux/pg-primary`).
- **FR-B45** - A bell, an OSC 9 notification, or an OSC 777 notification that any kept
  session's client sends reaches the terminal xmux runs in. A session whose grid is not
  on screen also marks its card with `!` until it is shown, and the history records the
  bell or the notification's words.
- **FR-B46** - The terminal's window title is the OSC 0 or OSC 2 title the client of the
  session on screen set. Once a title xmux wrote is no longer backed by the session on
  screen, the title is `xmux`, and on exit xmux restores the title the terminal had at
  launch where the terminal keeps a title stack.
- **FR-B47** - A psmux card attaches only to its named existing session; a missing
  session is not created, and a failed display attach reports a toast and history entry.
- **FR-B48** - A herdr card attaches only to an existing running or saved stopped
  session; a missing session fails with a notification, and explicit new-session
  actions complete server creation before selecting the new session.

## C. Switching (the keystone)

- **FR-C1** - A same-server pick lands on the picked session, by `switch-client` when
  the mux can name xmux's own client and otherwise by reattaching by session name.
- **FR-C2** - A cross-host pick switches entirely in process, with no picker and no
  detach, by handing the display to the target host's live attachment.
- **FR-C3** - An unreachable host is marked `▲` with a view screen stating the verdict
  and diagnostics, and nothing reconnects until the user re-scans or selects the card.
- **FR-C4** - Every dispatched switch or select command logs its exact argv and result,
  and a failed attach is logged at warn level, returns to the nav, and is not attached
  again until the user selects the card, executes it, or re-scans.
- **FR-C5** - Keys typed after a pick reach the picked session in the order typed: they
  wait while its attachment starts and, for a mux whose client drops the keys it reads
  before its first frame, until that attachment draws. Keys still waiting after 5 s, or
  when the pick moves on, are dropped.
- **FR-C6** - A paste reaches the focused session as one paste, wrapped in the bracketed
  paste markers when the session's client enabled bracketed paste and as plain text
  otherwise, and nothing in it acts as the prefix or an xmux key. A text field takes a
  paste without its line breaks and other control characters, and a paste over the nav
  or a screen without a field is dropped.
- **FR-C7** - The session in the terminal view holds the focus while xmux's window does
  and the terminal view holds xmux's focus with no popup open, and its client, when it
  enabled focus reports, is told each time it gains or loses that focus.
- **FR-C8** - When xmux's terminal has the kitty keyboard protocol, keys reach the
  session in the terminal view encoded with the protocol flags its client pushed, and
  the client's query for its flags is answered; without the protocol the client is told
  nothing and reads legacy keys. The prefix and every xmux key work in either encoding.
- **FR-C9** - The session in the terminal view gets only the mouse events its client's
  mouse mode asks for, encoded in the form the client enabled, and a drag that starts in
  the terminal view reaches it until its release, with motion past the view at the
  view's nearest edge.
- **FR-C10** - xmux's display client leaves the size of a session the user also has open
  to the user's own clients. On tmux it stops sizing a session while another client that
  sizes windows is attached to it (`refresh-client -f ignore-size`) and sizes the session
  again once it is the session's only such client. On abduco it attaches with the lowest
  priority (`abduco -l -a`), so it sizes the session only while no other client is
  attached. A tmux window larger than the terminal view shows the part around the
  cursor, and a smaller one shows its edge with the rest of the view filled with dots.

## D. App lifecycle

- **FR-D1** - `xmux` with no subcommand is a persistent supervisor that owns the
  terminal and runs its mux clients as children over a single async event loop.
- **FR-D2** - The app answers its control socket (`ping`, `dump`, `status`, `switch`)
  without blocking while a session is displayed.
- **FR-D3** - The app runs inside a mux by attaching its mux clients as PTY children, so
  its attachments never nest.
- **FR-D4** - The control socket is removed if stale before bind, owner-only (`0600`) on
  unix, and removed on exit, and a crashed instance's `ctl-*.sock` marker is swept on
  the next startup.
- **FR-D5** - The app launches directly into the split view with the landing screen
  (FR-B43), preselects the first session to appear, and keeps that selection as later
  hosts answer; the preselect attaches nothing until the user executes a card.
- **FR-D6** - An enumeration logs at INFO only when its session list changed and at WARN
  on failure.
- **FR-D7** - Daily log files are kept for a bounded window, and a repeating recovered
  panic is logged only at each doubling of its count.
- **FR-D8** - One command from `sh`, PowerShell, or CMD installs the build for the
  machine's OS and architecture after verifying its SHA-256, without elevation, and
  accepts a named version.
- **FR-D9** - Installing a version never overwrites the binary a running xmux executes,
  because each version has its own directory and only the launcher is repointed.
- **FR-D10** - `xmux update` updates by the install method read from the executable's
  path (in-place verified replace, the install script, winget, or Homebrew), with
  `--check` and `--method`.
- **FR-D11** - Before interactive launch, xmux checks for a newer release and offers
  update and enable automatic updates on this device (the default), update once, or skip;
  saved consent enables automatic checking and updating on each launch, successful
  updates run the new build, failures continue with the current build, subcommands
  never prompt, and `[update] check = false` disables startup updates.
- **FR-D12** - `doctor` opens with the running version, its binary path, the install
  method, and any recorded newer version, without network access.
- **FR-D13** - `xmux uninstall` removes xmux by its install method after a `y` or `yes`
  confirmation, keeps settings and data by default, and refuses while an instance is
  running.

## E. Session management

xmux aggregates and switches, so creating a session and resuming a stopped one are the
only session changes it makes.

- **FR-E1** - `prefix n` creates a session on the selected card's host and mux, which
  then appears in the nav, and is refused with a toast under an unreachable host.
- **FR-E2** - There is no rename, kill, or window or pane command anywhere in xmux.
- **FR-E3** - Creating a session runs off the key path, so a slow ssh round trip never
  freezes rendering or the control channel.
- **FR-E4** - A session the mux keeps while nothing of it runs (a tuios session saved
  while its daemon is down, a stopped herdr session, an exited zellij session) is listed
  with a card marked `stopped` and `xmux ls` marks its line. Selecting it shows its
  screen and attaches nothing, and executing it attaches it, which resumes it through
  the mux's own attach.

## F. Control channel

- **FR-F1** - A per-instance socket `ctl-<name>.sock` drives the running app with the
  verbs `ping`, `dump`, `status`, `switch`, `focus`, `rescan`, `quit`, `width`,
  `toggle-auto-hide`, `new-session`, and the `raw:` namespace, replying `err: …` on
  failure.
- **FR-F2** - There is one unified socket, and its `switch <host> <session>` verb runs
  the same switch action as a key press.
- **FR-F3** - Every instance has a name, generated as `<adjective>-<noun>` or given by
  `--name`, which `xmux send` and `xmux instances` resolve against live instances.
- **FR-F4** - Control messages are length-framed (decimal count, newline, bytes) with a
  bounded read.

## G. Transport & safety

- **FR-G1** - Every app-owned ssh uses a connect timeout and batch mode, except that a
  held password is answered only through the private askpass path, and the user's
  host-key policy is weakened to `accept-new` only for a submitted login under `ask`.
- **FR-G2** - A session name from a remote list is POSIX single-quote escaped when it
  re-enters a remote shell command.
- **FR-G3** - Mux session variables (`TMUX`, `TMUX_PANE`, `PSMUX*`) are stripped for
  listing, so a command run inside a mux is not refused as nesting.
- **FR-G4** - A remote attach runs the mux-supplied attach argv in one `ssh -t`
  connection, and its password never appears in the terminal view.
- **FR-G5** - A command bound for a WSL distribution is exec'd there in a login shell
  rather than passed as a command line.
- **FR-G6** - A remote machine's shell family is read during its reachability probe, and a
  non-POSIX remote is never sent POSIX-only syntax.
- **FR-G7** - xmux reaches a machine only for the launch scan, a user action, or an
  already open push stream, and no failure triggers its own retry except the one
  reattach per selection of a display client that zellij drops right after attaching.
- **FR-G8** - A machine is asked one thing at a time, while separate machines are asked
  in parallel.

---

## Use cases (end-to-end scenarios)

- **UC-1, jump from my laptop to a remote dev session.** Move the cursor to a remote
  session and land in it in one action. *(FR-B1, FR-C2, FR-D1/D2)*
- **UC-2, hop between two same-server sessions.** Select a session on the current server
  for an instant switch-client. *(FR-C1)*
- **UC-3, survey then stay put.** Look around the nav, then quit, leaving the current
  session untouched. *(FR-B5)*
- **UC-4, find one session among many then go.** Filter, then press Enter on the visible
  match. *(FR-B4, FR-B6)*
- **UC-5, the remote is down and I am not left in the dark.** An unreachable host
  shows `▲` and its reason, and the nav stays usable. *(FR-A2, FR-B7, FR-C4)*
- **UC-6, deep in a remote, get back home.** Native detach (`prefix d`) returns to the
  split view to pick another session. *(FR-C2, FR-D1)*
- **UC-7, spin up a throwaway on a remote and switch to it.** Create a session on the
  host's card, then switch to it. *(FR-E1, FR-C2)*
- **UC-8, survey what's running everywhere before deciding.** The nav shows every
  session and the terminal view previews the selection. *(FR-B1, FR-B3, FR-B8)*
- **UC-9, drive xmux from a script.** Dump, inject keys, and switch over the control
  channel. *(FR-F1, FR-F2)*
- **UC-10, switch in either direction, local to remote to local.** Each pick re-attaches
  the next target, local or remote, with no picker. *(FR-C2, FR-D1)*
- **UC-11, go straight to the session I can already see.** Press `prefix <digit>` with
  the number on the card. *(FR-B10, FR-C1)*

## Accepted limitations

The seamless cross-host switch is bought with these costs, accepted by design:

- One live app per terminal owns the display, and a second one cannot share it.
- Handing the display from one mux client to another can flash a repaint.
- On Windows, ssh has no ControlMaster multiplexing, so each remote round trip opens a
  fresh connection.
- A push-channel mux inside a WSL distribution that cannot allocate a terminal is
  reported unreachable.
- A zellij client moved to another session from inside itself is followed only on
  Windows locally and on a Linux machine with `ss` reached locally, through WSL, or over a
  shared ssh connection; elsewhere the nav stays on the card it was on.
- A herdr client moved to a saved machine from inside itself is followed only while it is
  its user's one herdr client on its machine, and only on a machine with `/proc`. A saved
  machine on the client's own machine moves the selection to that session; any other
  saved machine is named on the card of the session xmux opened, and the selection stays
  there. A herdr client that starts on the saved machine its user chose last is named on
  its card the same way while the nav holds the focus.
- A session listed without an identity, which is every zellij session and every local
  psmux session its own server does not answer for, is not followed through a rename made
  inside its mux: the rename reads as a lost session and a new one, and the selection
  moves up to the host.
- A tmux session change made inside the terminal view, into a session the user also has
  open in their own client, resizes that client to the terminal view until xmux hears of
  the change on the control connection and gives the size back. Nothing prevents it.
- tmux older than 3.2 has no `ignore-size` flag, so xmux's client sizes a session the user
  also has open like any client: under tmux's default `window-size latest`, typing in the
  terminal view shrinks the user's client of that session until the user types there.
  Upgrading tmux prevents it.
- screen does not resize a window that two displays show, so a window the terminal view
  showed first keeps the view's size when the user's own display shows it too, and their
  display shows it in its top-left corner. The user's `C-a F` (`fit`) gives it their size.
- zellij sizes a session by its smallest client, so while the terminal view shows a
  zellij session the user also has open, the user's client shows it at the view's size.
  Nothing prevents it.
- psmux sizes a window by the client that last had input (`window-size latest`), so typing
  in the terminal view resizes a window the user's own client shows until the user types
  there. `window-size largest` keeps the narrower view from shrinking it.
- tuios sizes a session by its smallest client by default (`daemon.window_size
  smallest`), so while the terminal view shows a tuios session the user also has open, the
  user's client shows it at the view's size. `daemon.window_size largest` keeps the
  narrower view from shrinking it.
- herdr sizes a session by the client that last had input, so typing in the terminal view
  resizes the panes the user's own client shows until the user types there. Nothing
  prevents it.
- A Windows client attached to tmux older than 3.6 can show the end of a terminal reply
  as typed text in the session: Windows OpenSSH passes the reply on in pieces, and tmux
  before 3.6 ends it at the gap. tmux 3.6 fixes this upstream (tmux/tmux#4411,
  microsoft/terminal#7185).
