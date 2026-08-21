# Keybindings

Under the app (entered by running `xmux` with no arguments) the screen is split
into two views: a **tree** of every reachable session on the left and the
selected session's **live screen** on the right. Keyboard focus is on one view
at a time. You navigate the tree with the keys below; moving the selection
switches the right view to that session in place. A tmux-style **prefix** gates
the handful of commands that apply regardless of which view holds focus.

## The prefix

xmux has its own prefix, like tmux's `set -g prefix`. It is read **only** from
the config file — there is no environment-variable override. Set it under
`[ui]` in `~/.config/xmux/config.toml`:

```toml
[ui]
prefix = "C-g"      # the default
```

Accepted specs: `C-<letter>` (e.g. `C-g`, `C-b`, `C-a`) and `C-Space`. Anything
unrecognised falls back to `C-g`. The prefix is a single control byte, so it
never collides with typed text, and a prefix pasted as data (bracketed paste) is
passed through untouched rather than intercepted.

## Tree navigation

These act on the tree while it holds focus.

| Key | Action |
|---|---|
| `↑` / `↓` (or `k` / `j`) | move between siblings at the current level |
| `→` / `l` | descend into the selected node's first child |
| `←` / `h` | ascend to the parent node |
| `PageUp` / `PageDown` | jump ten rows |
| `Home` / `End` | jump to the first / last node |

## Tree actions

xmux aggregates and switches; it does not edit what a mux already edits. There is
no rename, no kill, and no window or pane command — do those in the mux itself.
Two actions remain. `/` filter needs tree focus; `prefix n` / `prefix r` also work
while the live screen is focused:

| Key | Action |
|---|---|
| `/` | fuzzy-filter the tree by `<source>/<name>` (no prefix) |
| `prefix 0`-`prefix 9` | jump to a session by its number |
| `prefix n` | start a new session on the selected host |
| `prefix r` | re-scan every host |

`prefix n` needs a host row: a session row has nothing to create, and it says so
with a brief message. Creating under an unreachable host is likewise refused.

### Jumping by number

Every card carries a dim 0-based number in its left gutter, on the same row as the
session it names, the selected card included. `prefix <digit>` jumps straight there
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
tree or the live screen holds focus.

| Chord | Action |
|---|---|
| `prefix q` | quit xmux (the only quit binding) |
| `prefix ?` | toggle the keybinding help |
| `prefix t` | toggle auto-hide-nav (focusing the screen then gives it the full width) |
| `prefix h` / `prefix l` | narrow / widen the tree |
| `prefix Ctrl-←` / `prefix Ctrl-→` | narrow / widen the tree (then a bare `Ctrl-←`/`Ctrl-→` keeps resizing for a moment) |
| `prefix prefix` | send one literal prefix byte to the focused session's pane |

## The status line

The nav's bottom row is its status line. At rest it shows one thing, the prefix, and
stops at the view border so the live screen keeps every row it has. Press the prefix
and the same row widens to the whole window, floating over the border and the live
screen to list the keys that prefix unlocks; it shrinks back once the command key
lands. Only the paint moves, never the layout, so arming the prefix never shifts a
card. Four states outrank the prefix while they apply, in order: a refusal message
(in red), the host-scan progress, the active filter, and then the resting prefix. A
refusal too long for the nav width wraps onto more rows rather than clipping.

## Focus

| Key | Action |
|---|---|
| `Enter` | move focus from the tree into the live screen |
| `prefix Tab` | toggle focus between the tree and the live screen |
| `prefix →` | focus the live screen |
| `prefix ←` / `prefix Esc` | focus the tree |

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
| left-click a tree row | select that row (tree focused) |
| left-click a view | focus that view |
| wheel over the tree | move the selection (tree focused) |
| `Ctrl`+wheel over the tree | change the tree level — descend / ascend (tree focused) |
| drag the view border | resize the tree |
| drag a modal's border | move the modal |

There is no context menu: every action a right-click could offer is either a
plain click (focus, select) or a prefix chord. While the live screen is focused,
mouse events over it are forwarded to the pane (the mux needs its own mouse mode
enabled to use them).

## Automation

A running xmux instance listens on a local control socket. Sessions are addressed
`<source>/<session>`. It speaks navigation/display verbs — `ping`, `status`,
`dump`, `rescan`, `switch <source>/<session>`, `focus <nav|terminal>`,
`width <delta>` (a signed column delta, not an absolute width),
`toggle-auto-hide`, `quit` — and one session-lifecycle verb:

- `new-session <source> [name]`

The wire carries no kill/rename/window verbs, for the same reason the keys do
not: the mux owns editing a session.

Drive it with `xmux ctl <verb>`, e.g. `xmux ctl switch prod/api`. A low-level `raw:`
namespace (`raw:key`, `raw:keys`, `raw:text`) injects keystrokes or bytes for
tests; it is unstable and not part of the supported surface.

With one instance running `xmux ctl` targets it automatically; with several it
refuses to guess. `xmux ctl list` prints each instance (pid, working directory,
tty, displayed session, focus) so you can drive a specific one with
`xmux ctl --pid <pid> <verb>`.
