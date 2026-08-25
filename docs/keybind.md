# Keybindings

Under the app (entered by running `xmux` with no arguments) the screen is split
into two views: a **nav list** of every reachable session on the left and the
selected session's **live screen** on the right. Keyboard focus is on one view
at a time. You move down the list with the keys below; moving the selection
switches the right view to that session in place. A tmux-style **prefix** gates
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
cards together, the source you used most recently first. While a side column leaves the live screen
wider than it is tall, the nav IS that column and the cards run down it (wider than tall
as it looks on screen, where a row is about two columns tall). Once the window
is narrow or short enough that the column would leave the live screen square or taller,
the nav moves to a band across the top instead and the same cards flow down a column and
then continue to the right, a whole source at a time. Either
way the order is the same, so every key below moves along one axis: the next card is the
one below, or the top of the next column.

| Key | Action |
|---|---|
| `↑` / `↓` / `←` / `→` (or `k` / `j`) | move one card (wraps at both ends) |
| `PageUp` / `PageDown` | jump ten cards (wraps, like the arrows) |
| `Home` / `End` | jump to the first / last card |

`Enter` hands focus to the live screen, as does `prefix →`.

## Nav actions

xmux aggregates and switches; it does not edit what a mux already edits. There is
no rename, no kill, and no window or pane command - do those in the mux itself.
Two actions remain. `/` filter needs nav focus; `prefix n` / `prefix r` also work
while the live screen is focused:

| Key | Action |
|---|---|
| `/` | fuzzy-filter the list by `<source>/<name>` (no prefix) |
| `prefix 0`-`prefix 9` | jump to a session by its number |
| `prefix n` | start a new session on the selected host |
| `prefix r` | re-scan: refresh which machines exist, and every source's sessions |

`prefix n` starts the new session on the host/mux the selected card belongs to -
a host row or a session row both name one. Creating under an unreachable host is
refused.

### Jumping by number

Every card carries a dim 0-based number in its left column, on the same row as the
session it names. The selected card shows the selection mark there instead: its number
is the address of where you already are. `prefix <digit>` jumps straight there
and leaves a small popup open holding the number, so anything past 9 is reached by
typing the rest of it (`prefix 1` then `2` lands on 12, then `7` on 127).

The popup only accepts a digit that keeps the number addressing a real entry, so
one, two, and three digit numbers behave identically: whatever the buffer shows is
somewhere you can land. With ten sessions, `prefix 9` is refused outright with a
brief message, and after `prefix 1` a second `9` is simply not taken. `Enter` closes
the popup and keeps the selection; `Esc` closes it and returns to where you started.
Digits are prefix-gated, so a bare digit never jumps by accident.

## Prefix commands

Press the prefix, then the command key. These behave identically whether the
nav or the live screen holds focus.

| Chord | Action |
|---|---|
| `prefix q` | quit xmux (the only quit binding) |
| `prefix ?` | toggle the keybinding help |
| `prefix t` | toggle auto-hide-nav (focusing the screen then gives it the full width) |
| `prefix h` / `prefix l` | narrow / widen the nav |
| `prefix Ctrl-←` / `prefix Ctrl-→` | narrow / widen the nav (then a bare `Ctrl-←`/`Ctrl-→` keeps resizing for a moment) |
| `prefix prefix` | send one literal prefix byte to the focused session's pane |

## The status line

The nav's bottom row is its status line. At rest it shows one thing, the prefix, and
stops at the view border so the live screen keeps every row it has. In the portrait
layout it stops at its own text instead, because it shares that row with the
offscreen-card counts. Press the prefix and the same row widens to the whole window,
floating over the border and the live screen to list the keys that prefix unlocks; it
shrinks back once the command key lands, or once any mouse action does (a click, a wheel,
a drag: a prefix waits for the next input, whatever that turns out to be). Only the paint
moves, never the layout, so arming the prefix never shifts a card.

With the nav auto-hidden the mux owns every row, status line included. The bar still
floats over the bottom of the window for the two things that must be seen the moment
they happen: the armed prefix, and a refusal. Scan progress and the active filter
persist, so they stay in the nav and never take a row back from a hidden one. Four states outrank the prefix while they apply, in order: a refusal message
(in red), the scan progress, the active filter, and then the resting prefix. A
refusal too long for the nav width wraps onto more rows rather than clipping.

## Focus

| Key | Action |
|---|---|
| `Enter` | move focus from the nav into the live screen |
| `prefix Tab` | toggle focus between the nav and the live screen |
| `prefix →` / `prefix ↓` | focus the live screen |
| `prefix ←` / `prefix ↑` / `prefix Esc` | focus the nav |

An arrow points at the view it focuses. The live screen is right of the nav on a
landscape screen and below it on a portrait one, so `→` and `↓` both name it; `←` and
`↑` both name the nav. An arrow naming the view that already has focus does nothing.

When the live screen has focus, every key that is not a prefix chord is
forwarded raw to the session's active pane, so programs running inside the mux
(vim, a pager, a shell) see exact input.

## Modals

- **Help** (`prefix ?`): a scrollless key reference. `q` or `Esc` closes it;
  any other key is swallowed while it is open.
- **Input dialogs** (filter, new session): type into the buffer, `Backspace`
  deletes, `Enter` submits, `Esc` cancels.
- **Jump** (`prefix <digit>`): digits only, and only digits that keep the number in
  range. It acts while open (each edit moves the selection), so `Enter` merely closes
  it and `Esc` restores where you started.

## Mouse

| Gesture | Action |
|---|---|
| left-click a card | select that card (nav focused) |
| left-click a view | focus that view |
| wheel over the nav | move the selection (nav focused) |
| drag the view border | resize the nav |
| drag a modal's border | move the modal |

There is no context menu: every action a right-click could offer is either a
plain click (focus, select) or a prefix chord. While the live screen is focused,
mouse events over it are forwarded to the pane (the mux needs its own mouse mode
enabled to use them).

## Automation

A running xmux instance listens on a local control socket. Sessions are addressed
`<source>/<session>`. It speaks navigation/display verbs - `ping`, `status`,
`dump`, `rescan`, `switch <source>/<session>`, `focus <nav|terminal>`,
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
xmux send amber-otter switch prod/api
xmux send am focus terminal          # any unambiguous name prefix works
xmux send - dump                     # `-` when exactly one is running
printf 'switch prod/api\nfocus terminal\n' | xmux send amber-otter
```

An unknown name, an ambiguous prefix, or `-` with several instances running is an
error naming the candidates, never a guess: sending a command to the wrong instance
switches the wrong terminal. With no command, `send` reads them from stdin, one per
line. A refused command exits non-zero so a script can detect it.

A low-level `raw:` namespace (`raw:key`, `raw:keys`, `raw:text`) injects keystrokes
or bytes; it is unstable and not part of the supported surface.
