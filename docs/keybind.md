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
the same at all four: a dim `{host}/{mux}` title with its cards indented under it, and a
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
| `prefix n` | start a new session on the selected host |
| `prefix r` | re-scan: refresh which machines exist, and every source's sessions |
| `prefix R` | re-scan the selected card's host alone |
| `prefix h` | open the table of the hosts to check |
| `prefix s` | step the nav scope: sessions, all hosts, needs attention |

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

### Jumping by number

Every card carries a dim number in its left column, on the same row as the session it
names. A card takes its number the first time it appears and keeps it for the whole run.
A card that ends leaves its number vacant, so no other card's number shifts, and a new
card takes the next number past the highest one given; a session that comes back under
its own name takes its number back. While a full scan runs (the launch scan and every
`prefix r`) the numbers are dealt again from 1 in the order the list reads, so after
`prefix r` they read 1, 2, 3 down the list with no gap. `prefix R`, a filter, and the
nav scope leave every number where it is. The cards stay in list order whatever their
numbers say.

The selected card shows the selection mark there instead: its number is the address of
where you already are. `prefix <digit>` jumps straight there and opens the jump input in
the hint bar holding the number, so anything past 9 is reached by typing the rest of it
(`prefix 1` then `2` lands on 12, then `7` on 127).

Every digit is taken as typed: the selection follows the number while it names a card on
the list and stays put while it does not. No card carries 0, so `prefix 0` opens the
jump input holding a number no card carries and leaves the selection where it is;
0 matters only inside a longer number (10, 20, 100), and a leading zero is just a
spelling (01 is 1). `Enter` closes the input when the number names a card and, for a
vacant number or one past the highest, flashes the range (1 to the highest number on
the list) while leaving it open; `Esc` cancels it and returns to where you started.
Digits are prefix-gated, so a bare digit never jumps by accident.

### Nav scope

`prefix s` steps the nav through three scopes, from either focus:

| Scope | What the nav lists |
|---|---|
| `sessions` | every session, and a card for each host with none to show, except the hidden hosts (the default) |
| `all hosts` | the same list with nothing hidden: every unreachable host takes a card |
| `needs attention` | only the hosts in a problem state (`?`, `▲`, `✗`), and no session |

The scope shows only while you interact with the nav: on the key list's bottom border,
and in a toast when `prefix s` steps it. The resting nav says nothing about it. The
filter, the order, and the card numbers work the same in every scope. The scope is
remembered in `~/.xmux/nav_scope`.

### Hidden hosts

`[ui] hide-unreachable` (default true) keeps an unreachable host off the nav. How many
hosts it hides shows on the key list's bottom border (`nav: sessions · 2 hidden`) and in
the open filter's line, which counts the hidden hosts the filter matches. A nav left with
no card at all writes one line in its body, how many hosts are hidden and the key that
lists them (`2 hosts hidden · C-g h`), or, in the needs-attention scope, that nothing
needs attention.

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
left or right nav keeps a column just wide enough for the prefix; a collapsed top or bottom
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
switch or any mouse action: a click, a wheel, a drag - a prefix waits for the next input,
whatever that turns out to be). Its bottom border names the nav scope and, while the
hiding leaves any host without a card, how many (`nav: sessions · 2 hidden`), with the
xmux version at its right end where both fit.

Most keys end their function as they run, so the box closes with the keystroke. Two kinds
run longer and keep it up for as long as they last: a key that opens an input row holds
it until Enter or Esc closes the row (the input line takes the hint bar meanwhile), and a
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

With the nav auto-hidden the mux owns every row, prefix indicator included, until a prefix
interaction starts: then the nav comes back for the moment it is needed, so a jump can
read the card numbers, and it hides again when the interaction ends. With no indicator on
screen, the key list opens over the window's bottom left, and the bar floats over the
bottom of the window for what must be seen the moment it happens: an input line, a
refusal, and the hint after a selection move. A refusal is the reason a key did nothing (a
jump number no card carries, a new session on an unreachable host); it opens where the
hint after a selection move does, in the error colour, wraps instead of clipping, and goes
away on the next key or after ten seconds. Scan progress and the active filter persist,
so they stay in the nav and never take a row back from a hidden one. The bar shows one
thing at a time, in order: a refusal, an input line, the prefix alone while the key list
is open, the hint after a selection move, the scan progress, the active filter, and then
the resting prefix.

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

The view border's colour shows which view holds focus, and the selected card agrees: it is
reverse video while the nav holds focus and keeps only its `❯` mark while the terminal view
does.

When the terminal view has focus, every key that is not a prefix chord is
forwarded raw to the session's active pane, so programs running inside the mux
(vim, a pager, a shell) see exact input.

## Modals

- **Help** (`prefix ?`): every key, section by section, from the same table the key list
  and the hint after a selection move are built from, then a glyph legend: the host
  states `?`, `▲`, and `✗`, the spinner, the selection mark `❯`, the overflow cues `‹ ›`
  and `┃`, the auto-hide border `║`, and the toast levels. Typing searches it: each
  printable key narrows the rows to those whose keys or description contain the query
  (ignoring case), keeping each match under its section title, and a section title that
  matches keeps its whole section. `Backspace` shortens the query and `Ctrl-U` clears it.
  `↑`/`↓` scroll one row, `PgUp`/`PgDn` ten, and `Home`/`End` jump to either end; the
  title names the rows on screen whenever they are not all of them. `Esc` or `prefix ?`
  closes it, as the search line says whatever the search leaves, and any other key is
  swallowed while it is open.
- **History** (`prefix m`): every result and background event, newest first, each with
  how long ago it happened. `↑`/`↓` (or `k`/`j`) scroll one record and `PgUp`/`PgDn`
  ten; `q`, `Esc`, or `prefix m` closes it, and any other key is swallowed while it is
  open. Opening it takes every toast down.
- **Hosts to check** (`prefix h`): every host in a problem state, grouped under its cause
  (`?` login needed, `▲` unreachable, `✗` list failed), each with the reason its last
  answer gave and `hidden` on the ones the nav leaves without a card. The title counts
  the hidden ones. `↑`/`↓` (or `k`/`j`) move the row; `Enter` closes the table and
  selects that host's card, setting the filter to the host's name when it has no card on
  the list, and for a host that needs a login it also focuses the terminal view, whose
  login pane then takes the keys. `q`, `Esc`, or `prefix h` closes it.
- **Input** (filter, new session, jump): the hint bar becomes the input line,
  `[feature] guide: <buffer>` with the caret at the edit position. Type into the
  buffer, `Backspace` deletes, `Enter` submits, `Esc` cancels.
- **Filter** (`prefix /`): the list re-filters as you type, so which cards survive is
  visible before you press anything else; the selection holds its card while that
  survives and lands on the first remaining card otherwise. `Enter` closes it and
  keeps the filter; `Esc` restores the filter you opened with. With the filter
  applied and the input closed, `Esc` in the nav clears it. The input line states the
  total matches and how many matching hosts are normally hidden.
- **Jump** (`prefix <digit>`): digits only. It acts while open (each edit moves the
  selection while the number names a card), so `Enter` closes when the number names a
  card and flashes the range otherwise, and `Esc` restores where you started.

A terminal smaller than 24 columns by 4 rows shows the required and current size in
place of the split interface.

## Toasts

The result of work you started floats as a toast in the terminal view's top corner
farthest from the nav (the bottom right corner when the nav rides on top): a login and
the public-key registration it ran, a new session, and a re-scan, which reports in one
toast what changed (hosts added or removed, sessions started or ended, hosts that stopped
or started answering) or that nothing did; `prefix R` reports the same way for its one
host. Stepping the nav scope names the new scope in a toast. The newest release, when one is recorded, is
announced the same way at launch. A toast is at most 40% of the window wide and names its
subject on its top border and `prefix m history` on its bottom one.

A toast that reports only successes and facts leaves after five seconds, and an underline
under its first line shrinks with the time it has left. A toast carrying a warning (`▲`)
or a failure (`✗`) stays until it is dismissed: a click on it takes it down, and opening
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
| drag a modal's border | move the modal |
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
