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

By default the nav takes a **left column** while that leaves the terminal view wider than
it is tall (wider than tall as it looks on screen, where a row is about two columns tall),
and a **top band** once the column would leave the terminal view square or taller. Four
`[ui]` settings shape this:

```toml
[ui]
auto-nav-position = true        # follow the wide/narrow judgment (default)
wide-nav-position = "left"      # the placement when the terminal stays wider (default "left")
narrow-nav-position = "top"     # the placement when it does not (default "top")
# force-nav-position = "right"  # with auto-nav-position = false: pin this side
```

Each placement names one of `left`, `top`, `right`, `bottom` (an unknown word falls back
to that setting's default). The effective placement is resolved each frame in one order:
a placement pinned at runtime wins outright; otherwise, with `auto-nav-position` on, the
fixed wide/narrow judgment picks between `wide-nav-position` and `narrow-nav-position`;
with it off, `force-nav-position` applies (falling back to the wide placement when unset).
The judgment itself never changes: it always asks whether a side column would leave the
terminal view wider than tall, whatever the nav is actually doing, so the answer cannot
feed back into the question and nothing oscillates at the boundary.

The inner layout of the nav region is identical at all four placements: a right column is
the same vertical card list as a left one, and a bottom band is the same down-then-right
flow as a top one. Only what sits on which side of the view border flips. The status line
stays the nav region's bottom row in all four (see below).

`prefix p` moves the nav one side clockwise from where it is now - left → top → right →
bottom - and the fifth step returns it to the automatic behavior above. The choice is
remembered in `~/.xmux/nav_position` and wins over these settings until the key cycles
back to automatic.

| Key | Action |
|---|---|
| `↑` / `↓` (or `k` / `j`) | move one card (wraps at both ends) |
| `←` / `→` | move to the previous / next category, landing on its first card (wraps) |
| `PageUp` / `PageDown` | jump ten cards (wraps, like the card step) |
| `Home` / `End` | jump to the first / last card |

A category is a source that has sessions to show, entered at its first session, or the
whole band of host cards at once, entered at its first card. The band holds one card per
machine with nothing running on it, and crossing those one at a time would be a long walk
past nothing, so the category step treats the band as a single stop; the card step still
reaches every one of them. A category is left from any card of it, so a selection deep
inside the band steps straight out.

`Enter` hands focus to the terminal view, as does `prefix →`.

## Nav actions

xmux aggregates and switches; it does not edit what a mux already edits. There is
no rename, no kill, and no window or pane command - do those in the mux itself.
Two actions remain. `/` filter needs nav focus; `prefix n` / `prefix r` also work
while the terminal view is focused:

| Key | Action |
|---|---|
| `/` | fuzzy-filter the list by `<source>/<name>` (no prefix; applies as you type) |
| `prefix 1`-`prefix 9` | jump to a session by its number |
| `prefix n` | start a new session on the selected host |
| `prefix r` | re-scan: refresh which machines exist, and every source's sessions |

`prefix n` starts the new session on the host/mux the selected card belongs to -
a host row or a session row both name one. Creating under an unreachable host is
refused. The session's name is asked for; left empty, one is auto-assigned - by the
mux where it names its own sessions, otherwise by xmux, which picks an
`<adjective>-<noun>` name (the instance-name vocabulary) that no session on that
host already holds.

### Jumping by number

Every card carries a dim number in its left column, on the same row as the session it
names, counted from 1 in the same order the list reads: the first card is 1 and the
last is the card count. The selected card shows the selection mark there instead: its number
is the address of where you already are. `prefix <digit>` jumps straight there
and opens the jump input in the hint bar holding the number, so anything past 9 is
reached by typing the rest of it (`prefix 1` then `2` lands on 12, then `7` on 127).

Every digit is taken as typed: the selection follows the number while it names a real
entry and stays put while it does not. No card carries 0, so `prefix 0` opens the
jump input holding a number no card carries and leaves the selection where it is;
0 matters only inside a longer number (10, 20, 100), and a leading zero is just a
spelling (01 is 1). `Enter` closes the input when the number names
an entry and flashes the valid range (1 to the last card) while leaving it open
otherwise; `Esc` cancels it and returns to where you started. Digits are
prefix-gated, so a bare digit never jumps by accident.

## Prefix commands

Press the prefix, then the command key. These behave identically whether the
nav or the terminal view holds focus.

| Chord | Action |
|---|---|
| `prefix q` | quit xmux (the only quit binding) |
| `prefix ?` | toggle the keybinding help |
| `prefix t` | toggle auto-hide-nav (focusing the screen then gives it the full width) |
| `prefix p` | move the nav one side clockwise (left → top → right → bottom → automatic) |
| `prefix h` / `prefix l` | narrow / widen the nav (down to just past the resting `C-g` status line) |
| `prefix Ctrl-←` / `prefix Ctrl-→` | narrow / widen the nav (then a bare `Ctrl-←`/`Ctrl-→` keeps resizing for a moment) |
| `prefix Ctrl-↑` / `prefix Ctrl-↓` | shrink / grow the nav band's height in a band layout (then a bare `Ctrl-↑`/`Ctrl-↓` keeps resizing for a moment) |
| `prefix prefix` | send one literal prefix byte to the focused session's pane |

## The status line

The nav's bottom row is its status line, in all four placements. With the nav as a left or
right column, or a top band, that is the lowest row the nav owns on screen; with the nav as
a bottom band it is the bottom row of the screen itself. At rest it shows one thing, the
prefix, and stops at the view border so the terminal view keeps every row it has. In a band
it stops at its own text instead, because it shares that row with the
offscreen-card counts. Press the prefix and the same row widens to the whole window,
floating over the border and the terminal view to list the keys that prefix unlocks; it
shrinks back when the function it started ends, or when the prefix is canceled (a
focus switch or any mouse action: a click, a wheel, a drag - a prefix waits for the
next input, whatever that turns out to be).

Most keys end their function as they run, so the bar shrinks with the keystroke. Two
kinds run longer and keep the bar up for as long as they last: a key that opens an
input row holds it until Enter or Esc closes the row, and a resize holds it until the
repeat window lapses, so a whole Ctrl+arrow burst reads as one interaction.

A second prefix is `prefix prefix` (above): one literal prefix byte reaches the pane.
Holding the prefix down takes the same path, because a terminal sends no key-up and
an autorepeat is byte-identical to repeated taps: the pane collects literals and the
bar blinks until the key comes up.

Only the paint moves, never the layout, so arming the prefix never shifts a card.

With the nav auto-hidden the mux owns every row, status line included, until a prefix
interaction starts: then the nav comes back for the moment it is needed, so a jump can
read the card numbers, and it hides again when the interaction ends. The bar also floats
over the bottom of the window for the two things that must be seen the moment they
happen: a live prefix, and a refusal. Scan progress and the active filter
persist, so they stay in the nav and never take a row back from a hidden one. Four
states outrank the prefix while they apply, in order: a refusal message (in yellow), the
scan progress, the active filter, and then the resting prefix. A
refusal too long for the nav width wraps onto more rows rather than clipping.

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

When the terminal view has focus, every key that is not a prefix chord is
forwarded raw to the session's active pane, so programs running inside the mux
(vim, a pager, a shell) see exact input.

## Modals

- **Help** (`prefix ?`): a scrollless key reference. `q` or `Esc` closes it;
  any other key is swallowed while it is open.
- **Input** (filter, new session, jump): the hint bar becomes the input line,
  `[feature] guide: <buffer>` with the caret at the edit position. Type into the
  buffer, `Backspace` deletes, `Enter` submits, `Esc` cancels.
- **Filter** (`/`): the list re-filters as you type, so which cards survive is
  visible before you press anything else; the selection holds its card while that
  survives and lands on the first remaining card otherwise. `Enter` closes it and
  keeps the filter; `Esc` restores the filter you opened with.
- **Jump** (`prefix <digit>`): digits only. It acts while open (each edit moves the
  selection while the number names a card), so `Enter` closes when the number names a
  card and flashes the range otherwise, and `Esc` restores where you started.

## Mouse

| Gesture | Action |
|---|---|
| left-click a card | select that card (nav focused) |
| left-click a view | focus that view |
| wheel over the nav | move the selection (nav focused) |
| drag the view border | resize the nav (at any of the four borders: the drag mirrors the placement, measuring from the near edge) |
| drag a modal's border | move the modal |

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
