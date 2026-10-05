# Keybindings

Under the app (entered by running `xmux` with no arguments) the screen is split
into two views: a **nav list** of every reachable session and the selected session's
**terminal view**, parted by a view border (the nav on the left by default; it can ride on
any of the four sides, see below). Keyboard focus is on one view
at a time. You move down the list with the keys below; moving the selection
switches the terminal view to that session in place. A tmux-style **prefix** gates
the handful of commands that apply regardless of which view holds focus.

## The prefix

xmux has its own prefix, like tmux's `set -g prefix`. It is read **only** from
the config file - there is no environment-variable override. Set it under
`[ui]` in `~/.config/xmux/config.toml`:

```toml
[ui]
prefix = "C-g"      # the default
```

Accepted specs: `C-<letter>` (e.g. `C-g`, `C-b`, `C-a`) and `C-Space`. Anything
unrecognised falls back to `C-g`. The prefix is a single control byte, so it
never collides with typed text, and a prefix pasted as data (bracketed paste) is
passed through untouched rather than intercepted.

## Nav navigation

These act on the nav while it holds focus. It holds one card per session, each source's
cards together under its `{host}/{mux}` section title, in a deterministic order
(local sources first, then WSL distros, then remote hosts, each by source name, sessions
by name). The nav rides on one of four sides of the terminal view: a left or right
**column**, or a top or bottom **band**. In a column the cards run down it; in a band the
same cards flow down a column and then continue to the right, a whole source at a time.
Either way the order is the same, and the keys below read that order rather than the shape on
screen: one steps a card, the other steps a category.

### Where the nav attaches

By default the nav takes a **left column** on one of the four sides: left, top, right, or
bottom. A single `[ui]` setting names the default placement:

```toml
[ui]
nav-position = "left"       # the nav's default side (default "left")
```

Each placement names one of `left`, `top`, `right`, `bottom` (an unknown word falls back
to the default). The nav never moves on its own: the effective placement each frame is a
side pinned at runtime, else this default.

The inner layout of the nav region is identical at all four placements: a right column is
the same vertical card list as a left one, and a bottom band is the same down-then-right
flow as a top one. Only what sits on which side of the view border flips. The groups read
the same at all four: a bold `{host}/{mux}` title with its cards indented under it, and a
band column that continues a section repeats that title on its top row followed by `…`.
A band one row tall writes titles and cards along that one row and scrolls sideways. The
view border is the only line the nav draws; what is off screen is shown on it (see the
prefix indicator below).

`prefix p` moves the nav one side clockwise from where it is now - left → top → right →
bottom - and the fifth step returns it to the default above. The choice is
remembered in `~/.xmux/nav_position` and wins over this setting until the key cycles
back to the default.

| Key | Action |
|---|---|
| `↑` / `↓` (or `k` / `j`) | move one card (wraps at both ends) |
| `←` / `→` (or `h` / `l`) | move to the previous / next category, landing on its first card (wraps) |
| `PageUp` / `PageDown` | jump ten cards (wraps, like the card step) |
| `Home` / `End` | jump to the first / last card |

A category is a source that has sessions to show, entered at its first session, or the
whole band of host cards at once, entered at its first card. The band holds one card per
machine with nothing running on it, and crossing those one at a time would be a long walk
past nothing, so the category step treats the band as a single stop; the card step still
reaches every one of them. A category is left from any card of it, so a selection deep
inside the band steps straight out.

Every host card reserves one state-glyph cell: `?` means login needed, `▲` means
unreachable, `✗` means the session listing could not be parsed, a braille glyph means
scanning, and a blank cell means reachable with no sessions. Only the selected card
adds the state word. Long names retain their beginning and end with a middle ellipsis.

`Enter` hands focus to the terminal view, as does `prefix →`.

## Nav actions

xmux aggregates and switches; it does not edit what a mux already edits. There is
no rename, no kill, and no window or pane command - do those in the mux itself.
The remaining actions all take the prefix and work from either focus:

| Key | Action |
|---|---|
| `prefix /` | fuzzy-filter the list by `<source>/<name>` (applies as you type, shows match counts, and bolds matching characters) |
| `prefix 1`-`prefix 9` | jump to a session by its number |
| `prefix i` | select the current source title and show its session count and update method |
| `prefix n` | start a new session on the selected host |
| `prefix r` | re-scan: refresh which machines exist, and every source's sessions |
| `prefix R` | re-scan the selected card's host alone |
| `prefix L` | log out of the selected SSH machine (asks for the word `logout`): remove this PC's key from it, clear the held password, and close its connections |
| `prefix h` | open the table of the hosts to check |
| `prefix :` | search commands by name; type to filter, use arrows to select, Enter or a click to run, Esc to close |

While the nav holds focus, bare `i` selects the current source title too. Click the
title for the same screen. Titles do not take card numbers or interrupt card stepping.
The host screen states the session count, how the list updates, and when the source
last answered. An unreachable host leads with its verdict and re-scan actions; `d`
unfolds the full diagnostic.

`prefix n` starts the new session on the host/mux the selected card belongs to -
a host row or a session row both name one. Creating under an unreachable host is
refused. The session's name is asked for; left empty, one is auto-assigned - by the
mux where it names its own sessions, otherwise by xmux, which picks an
`<adjective>-<noun>` name (the instance-name vocabulary) that no session on that
host already holds.

`prefix R` asks the selected card's machine again and nothing else: its reachability
probe, then every source it serves. Its cards keep their sessions and numbers while it
runs, and it reports in one toast titled `re-scan <host>` that compares that host alone.
It is refused while the host is still being scanned and while another re-scan has not
reported; a `prefix r` pressed meanwhile takes over.

`prefix L` asks for `logout` typed in full before it acts. It then looks for the lines
of the host's key files that hold one of this PC's public keys, by key type and body
alone, and removes the ones marked `xmux-registered`. A matching line without the mark
opens a second confirmation, which says that xmux did not add the key and that removing
it also affects ssh outside xmux: `remove` typed in full removes that line too, and Esc
or anything else that closes the confirmation keeps it. The held password and the
connections are cleared only after that, and a host that cannot be reached or a removal
that fails still logs out, with a toast saying the key remains and why.

### Jumping by number

Every card carries a dim number in its left column, on the same row as the session it
names. With `[ui] renumbering = true` (the default), cards are numbered from 1 in
the current sorted nav list. Adding or removing a card, filtering,
and scanning can change a card's number. With `renumbering = false`, a card keeps
its number until a full scan; an ended card leaves a vacant number and a new card
takes the next one. A full scan deals numbers again in list order. The cards stay in
list order under either setting.

The selected card shows the selection mark there instead: its number is the address of
where you already are. `prefix <digit>` jumps straight there and opens the jump popup
holding the number, so anything past 9 is reached by typing the rest of it
(`prefix 1` then `2` lands on 12, then `7` on 127).

Every digit is taken as typed: the selection follows the number while it names a card on
the list and stays put while it does not. No card carries 0, so `prefix 0` opens the
jump input holding a number no card carries and leaves the selection where it is;
0 matters only inside a longer number (10, 20, 100), and a leading zero is just a
spelling (01 is 1). `Enter` closes the input when the number names a card and, for a
vacant number or one past the highest, states in the popup that no card carries it
while leaving it open (the top border names the range, 1 to the highest number on the
list); `Esc` cancels it and returns to where you started.
Digits are prefix-gated, so a bare digit never jumps by accident.

### Card groups and focus

The nav lists actual session cards, reachable hosts with no sessions, and hosts
whose connection or inventory is unresolved, in that order. One blank row in a
side column or one blank column in a top or bottom band separates adjacent groups.
The first visible boundary can carry a horizontal rule while a side list scrolls.

Leaving nav focus from a session card paints only the session group. Leaving from
either host group keeps every group visible. Returning focus to the nav shows
every group. Prefix and modal interactions preserve the focus decision. While
the selection is on a host card, every group is painted, so the selected card is
always visible. Card numbers and selection identity stay the same.

`prefix h` lists hosts with a problem, grouped by cause and carrying their reasons.
Enter selects the host and opens its login pane when needed. The command palette
also lists `log in to <host>` for these hosts.

## Prefix commands

Press the prefix, then the command key. These behave identically whether the
nav or the terminal view holds focus.

| Chord | Action |
|---|---|
| `prefix q` | quit xmux (the only quit binding) |
| `prefix ?` | toggle the help: every key and glyph, searchable |
| `prefix m` | toggle the history of results and background events |
| `prefix t` | toggle auto-hide-nav (focusing the screen then gives it the full width) |
| `prefix z` | collapse or expand the nav |
| `prefix p` | move the nav one side clockwise (left → top → right → bottom → default) |
| `prefix Ctrl-←` / `prefix Ctrl-→` | move the view border left / right (the nav width follows the placement; the floor fits a card with eight cells of name), then a bare `Ctrl-←`/`Ctrl-→` keeps resizing for a moment |
| `prefix Ctrl-↑` / `prefix Ctrl-↓` | move the band's view border up / down in a band layout (then a bare `Ctrl-↑`/`Ctrl-↓` keeps resizing for a moment) |
| `prefix prefix` | send one literal prefix byte to the focused session's pane |

The resize keys move the view border the way the key points, so whether the nav grows or
shrinks follows the placement: on a left or top nav the border is the nav's far edge, so
moving it outward grows the nav; on a right or bottom nav the border is the nav's near
edge, so the same movement shrinks it. The width floor fits a card's indent, a two-digit
number, and eight cells of name; a band is at least one row.

`prefix z` collapses or expands the nav, and dragging the view border past the nav's
minimum collapses it (dragging back out in the same drag expands it again). A collapsed
left or right nav keeps a column exactly as wide as the prefix, with the prefix on its
bottom row and the view border running down the column's edge beside the terminal view
on every row above the prefix; a collapsed top or bottom
nav keeps only its view border row, with the prefix at its right end. Cards are not shown
while collapsed. A click anywhere on the collapsed nav expands it, and so does focusing
the nav by keyboard. Auto-hide still takes the whole nav away and restores the same
collapsed or expanded state when it returns.

## The prefix indicator

The nav shows the prefix in one place at rest: the bottom row of a left or right column,
and the right end of the view border row of a top or bottom band, so every row of a band
holds cards. It is a label sized to its text, and the rest of its row stays the nav's (a
column) or the view border's (a band, where the offscreen-card counts share the row).
Cards off screen are shown on the view border too: a column thickens the stretch beside
the cards on screen to `┃`, and a band writes `‹ 5` at its left end and `7 ›` before the
prefix, counting the cards scrolled off each side.
Press the prefix and the key list opens at once: a rounded box titled with the prefix,
naming every key the prefix unlocks under four section titles (navigate, sessions, view,
app), in as many columns as the room beside the indicator holds. It opens from the
indicator toward the terminal view while the indicator keeps the prefix: beside a left
column it rises from the indicator's row at the terminal view's left edge, beside a right
column at the terminal view's right edge, below a top band's seam it hangs under the
seam's right end, and above a bottom band's seam it stands over the seam's right end. A
terminal view too narrow beside a side column for a box lends the box the window's whole
width, against the same corner. When the keys do not fit, the box first shortens every description, then gives up the keys
needed least and counts them as `+N more`; a key is never shown without its name, and the
jump, help, and quit keys are never given up. The box floats over the terminal view and
closes when the function the prefix started ends, or when the prefix is canceled (a focus
switch or any mouse action outside the box: a click, a wheel, a drag - a prefix waits for
the next input, whatever that turns out to be). Pressing anywhere on the box and dragging
moves it and keeps the prefix, and the popup a key then opens takes the place it was moved
to; the next prefix opens it in its usual place again. Its bottom border names the xmux
version where it fits.

Most keys end their function as they run, so the box closes with the keystroke. Two kinds
run longer and keep it up for as long as they last: a key that opens an input row holds
it until Enter or Esc closes the row (the input's popup takes the key list's place
meanwhile), and a
resize holds it until the repeat window lapses, so a whole Ctrl+arrow burst reads as one
interaction.

A second prefix is `prefix prefix` (above): one literal prefix byte reaches the pane.
Holding the prefix down takes the same path, because a terminal sends no key-up and
an autorepeat is byte-identical to repeated taps: the pane collects literals and the
box blinks until the key comes up.

Only the paint moves, never the layout, so arming the prefix never shifts a card.

For three seconds after you move the selection, the hint bar opens from the indicator the
same way and names the selected card's most relevant keys and one fact about it: a session
offers `Enter` and `prefix n` and states its windows, a host that failed offers `Enter`
(its screen) and `prefix R` and states its state word with the reason behind it, an empty
host offers `prefix n` and `prefix R`, and a scanning host offers `prefix /`. When the
terminal view holds the focus after the move (a jump typed from it, a click on a band's
count), a bare key would reach the pane, so only the prefix keys are offered. Any key ends
it at once, and the next move replaces it. A narrow bar shortens the descriptions first,
then drops the reason, then the later keys. A selection xmux was told to make (a ctl
`switch`, the nav following the mux) raises no hint.

In a top or bottom band the selection hint occupies the view border row beside the
prefix. Offscreen card counts return when the hint closes. A refusal still opens into
the terminal view next to the border so its text has room to wrap.

With the nav auto-hidden the mux owns every row, prefix indicator included, until a prefix
interaction starts: then the nav comes back for the moment it is needed, so a jump can
read the card numbers, and it hides again when the interaction ends. With no indicator on
screen, the key list opens over the window's bottom left, and the bar floats over the
bottom of the window for what must be seen the moment it happens: a refusal and the
hint after a selection move. A refusal is the reason a key did nothing (a new
session on an unreachable host, a logout confirm without the word); it opens where the
hint after a selection move does, in the error colour, wraps instead of clipping, and goes
away on the next key or after ten seconds. Scan progress and the active filter persist,
so they stay in the nav and never take a row back from a hidden one. The bar shows one
thing at a time, in order: a refusal, the prefix alone while the key list or an input
is open, the hint after a selection move, the scan progress, the active filter, and then
the resting prefix.

## Braille animation

The scanning view and the space below a settled host screen show a centered Braille
X by default. The animation can be hidden in `~/.config/xmux/config.toml`:

```toml
[ui]
braille-animation = false
```

The setting applies to a running xmux when the file changes. Nav activity spinners
remain visible.

## Focus

| Key | Action |
|---|---|
| `Enter` | move focus from the nav into the terminal view |
| `prefix Tab` | toggle focus between the nav and the terminal view |
| the arrow pair facing the terminal's side | focus the terminal view |
| the other pair | focus the nav |

The arrow PAIR facing the terminal's side names the terminal, and the other pair names the
nav. With the nav on the left or above, that is `prefix →` / `prefix ↓` for the terminal
and `prefix ←` / `prefix ↑` for the nav; with the nav on the right or below the whole pair
flips (`prefix ←` / `prefix ↑` name the terminal, `prefix →` / `prefix ↓` the nav). An
arrow naming the view that already has focus does nothing.

The view border's colour shows which view holds focus. The selected card keeps its `❯` mark
and reverse video in both focus states.

When the terminal view has focus, every key that is not a prefix chord is
forwarded raw to the session's active pane, so programs running inside the mux
(vim, a pager, a shell) see exact input.

## Modals

Every modal opens as a popup where the key list opens, growing the way it does: beside a
side column against the prefix indicator, over a bottom band's seam or under a top
band's seam at its right end, and over the window's bottom left with the nav hidden. A
prefix key therefore replaces the key list with its popup in the same place. A popup is
the key list's rounded box: its title and a count or machine in the top border, its keys
on the bottom border. Its width follows its content or the window, never the nav's
width. A popup
whose left edge would leave one or two cells of the row beside it starts at the window's
left edge instead. Every row of a popup wraps to the popup's width rather than being cut:
a description continues under its own column, and a key wider than its column takes a row
of its own above it. A text field stays on one row and keeps its caret in view. Pressing
anywhere on a popup and dragging moves it. A popup with items to pick (the help's tabs,
the hosts to check, the command palette) keeps selecting and executing apart: the arrow
keys move its selection, the pointer over an item underlines it without moving that
selection, and a click on an item (a press released where it was pressed) executes it
exactly as `Enter` on it would. Any key ends the underline until the pointer moves
again. While a popup's text field takes keys, the
terminal's own cursor sits on the field's caret, so an input method composes in the
field.

- **Help** (`prefix ?`): every key, section by section, from the same table the key list
  and the hint after a selection move are built from, then a glyph legend: the host
  states `?`, `▲`, and `✗`, the spinner, the selection mark `❯`, the overflow cues `‹ ›`
  and `┃`, the auto-hide border `║`, and the toast levels. Typing searches it: each
  printable key narrows the rows to those whose keys or description contain the query
  (ignoring case), keeping each match under its section title, and a section title that
  matches keeps its whole section. `Backspace` shortens the query and `Ctrl-U` clears it.
  One blank row parts two sections. Under the search field a row of tabs names the
  sections, the active one in the title's accent: `←`/`→` (or a click on a tab) move the
  active tab and scroll that section's title to the top of the body, held at the end of
  the help. The pointer over a tab underlines it and shows its section while it stays
  there; off the tab row, the body shows the active tab's position again. `↑`/`↓` scroll one row, `PgUp`/`PgDn` ten, and `Home`/`End` jump to either end,
  and the active tab then follows the section at the top of the body. A search leaves the
  tabs of the sections it matched. When the tabs do not fit the popup's width, the row
  shows the ones around the active tab, with `‹` or `›` where more are hidden. The top
  border names the rows on screen whenever they are not all of them. `Esc` or `prefix ?`
  closes it, as its bottom border says whatever the search leaves, and any other key is
  swallowed while it is open.
- **History** (`prefix m`): every result and background event, newest first, each with
  how long ago it happened. `↑`/`↓` (or `k`/`j`) scroll one record and `PgUp`/`PgDn`
  ten; `q`, `Esc`, or `prefix m` closes it, and any other key is swallowed while it is
  open. Opening it takes every toast down.
- **Hosts to check** (`prefix h`): every host in a problem state, grouped under its cause
  (`?` login needed, `▲` unreachable, `✗` list failed), each with the reason its last
  answer gave. The top border
  counts the hosts. `↑`/`↓` (or `k`/`j`) move the row; `Enter` or a click on a host closes the table and
  selects that host's card, setting the filter to the host's name when it has no card on
  the list, and for a host that needs a login it also focuses the terminal view, whose
  login pane then takes the keys. `q`, `Esc`, or `prefix h` closes it.
- **Input** (filter, jump, new session, logout, remove key): a popup with one text field and the
  caret at the edit position. Type into the field, `Backspace` deletes, `Enter`
  submits, `Esc` cancels.
- **New session** (`prefix n`): the popup names the host and mux the session lands on
  and takes its name; an empty name is assigned automatically, as above.
- **Logout** (`prefix L`): the popup states the session, the observed SSH login, what
  happens to a held password and to this PC's key, and the machine whose connections
  close. Typing `logout` and `Enter` logs out.
- **Remove key** (opens during a logout): when the host holds this PC's key in a line
  xmux did not add, a second popup opens in the same place with the same layout. It
  states how many such lines there are and in which file, that ssh outside xmux loses
  the key too, what keeping them leaves, and that the logout goes on either way. Typing
  `remove` and `Enter` removes those lines too; `Esc` keeps them.
- **Filter** (`prefix /`): the list re-filters as you type, so which cards survive is
  visible before you press anything else; the selection holds its card while that
  survives and otherwise moves to the nearest visible card related to it: a hidden
  session goes to its section title, and a hidden source to the card that takes its
  place. `Enter` closes it and
  keeps the filter; `Esc` restores the filter you opened with. With the filter
  applied and the input closed, `Esc` in the nav clears it. The top border counts
  the cards kept of the cards listed.
- **Jump** (`prefix <digit>`): digits only. It acts while open (each edit moves the
  selection while the number names a card), and the popup names that card beside the
  number. `Enter` closes when the number names a card and otherwise states in the
  popup that no card carries it, with the range in the top border. `Esc` restores
  where you started.

A terminal smaller than 24 columns by 4 rows shows the required and current size in
place of the split interface.

## Toasts

The result of work you started floats as a toast in the terminal view's corner
nearest the hint: a login and the public-key registration it ran, a new session,
and a re-scan, which reports in one
toast what changed (hosts added or removed, sessions started or ended, hosts that stopped
or started answering) or that nothing did; `prefix R` reports the same way for its one
host. The newest release, when one is recorded, is
announced the same way at launch. A toast is at most 40% of the window wide and names its
subject on its top border and the history key inside the card when it fits.

A toast that reports only successes and facts leaves after five seconds. Its bottom
border shows the time left as a bold accent line that gives way to a normal line.
A toast carrying a warning (`▲`) or a failure (`✗`) stays until it is dismissed:
a click on it takes it down, and opening
the history takes every toast down. Up to three toasts stand at once, the newest in the
corner.

Something nobody asked about, such as a host that stops answering while you work, raises
no toast. It goes to the history only, so it never pulls attention from the terminal.
The history keeps the last 200 records; when it is full, the oldest success or info
record goes first, so failures outlive routine reports.

```toml
[ui]
notifications = true   # false keeps results out of toasts; the history still has them
```

## Mouse

| Gesture | Action |
|---|---|
| left-click a card | select that card (nav focused) |
| left-click a view | focus that view |
| left-click a collapsed nav | expand the nav |
| left-click `‹ 5` or `7 ›` on a band's view border | select the nearest card scrolled off that side |
| wheel over the nav | move the selection (nav focused) |
| drag the view border | resize the expanded nav (at any of the four borders: the drag mirrors the placement, measuring from the near edge); past the minimum it collapses the nav |
| drag the key list or a popup | move it (press anywhere on it, an item included) |
| hover a help tab | underline it and show its section until the pointer leaves the tab row |
| hover a popup item | underline it without moving the keyboard selection |
| left-click a help tab | make it the active tab and scroll the help to that section |
| left-click a popup item | execute it, as `Enter` on it would |
| left-click a toast | dismiss it |

There is no context menu: every action a right-click could offer is either a
plain click (focus, select) or a prefix chord. While the terminal view is focused,
mouse events over it are forwarded to the pane (the mux needs its own mouse mode
enabled to use them).

## Automation

A running xmux instance listens on a local control socket. A command names a
session by its source and its session separately. It speaks navigation/display
verbs - `ping`, `status`,
`dump`, `rescan`, `switch <source> <session>`, `focus <nav|terminal>`,
`width <delta>` (a signed column delta, not an absolute width),
`toggle-auto-hide`, `quit` - and one session-lifecycle verb:

- `new-session <source> [name]`

The wire carries no kill/rename/window verbs, for the same reason the keys do
not: the mux owns editing a session.

Every running instance has a NAME. It takes one at startup - an auto-generated
`<adjective>-<noun>`, or whatever `xmux --name <name>` says (lowercase letters,
digits, and `-`, up to 32 characters) - and owns `ctl-<name>.sock` while it lives.

`xmux instances` lists the live ones with their name, pid, working directory, tty,
displayed session, and focus. `xmux send <name> <command>` drives one:

```
xmux instances
xmux send amber-otter switch prod api
xmux send am focus terminal          # any unambiguous name prefix works
xmux send - dump                     # `-` when exactly one is running
printf 'switch prod api\nfocus terminal\n' | xmux send amber-otter
```

An unknown name, an ambiguous prefix, or `-` with several instances running is an
error naming the candidates, never a guess: sending a command to the wrong instance
switches the wrong terminal. With no command, `send` reads them from stdin, one per
line. A refused command exits non-zero so a script can detect it. A `switch` to a
`<source>/<session>` address the instance's current inventory does not list is a
refused command too: it replies `err:` naming which half is missing (the source, or
a session under a present source), so a script learns the switch did not resolve
instead of a blind `ok`.

A low-level `raw:` namespace (`raw:key`, `raw:keys`, `raw:text`) injects keystrokes
or bytes; it is unstable and not part of the supported surface.
