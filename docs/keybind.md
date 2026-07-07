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

While the tree holds focus:

| Key | Action |
|---|---|
| `/` | fuzzy-filter the tree by `<source>/<name>` (no prefix) |
| `prefix n` | create — a session on a host, a window on a session, or a split (pane) on a window |
| `prefix R` | rename the selected session or window |
| `prefix x` | kill the selected session or window (with a `y`/`n` confirm) |
| `prefix r` | re-scan every host |

`prefix n`, `prefix R`, and `prefix x` are level-aware — they act on the host,
session, or window the selection is on. Renaming or killing a host row is refused
with a brief message; creating under an unreachable host is likewise refused. The
prefix guards these so a stray keystroke cannot destroy or disrupt a session.

## Prefix commands

Press the prefix, then the command key. These behave identically whether the
tree or the live screen holds focus.

| Chord | Action |
|---|---|
| `prefix q` | quit xmux (the only quit binding) |
| `prefix ?` | toggle the keybinding help |
| `prefix t` | toggle auto-hide-tree (focusing the screen then gives it the full width) |
| `prefix h` / `prefix l` | narrow / widen the tree |
| `prefix Ctrl-←` / `prefix Ctrl-→` | narrow / widen the tree (then a bare `Ctrl-←`/`Ctrl-→` keeps resizing for a moment) |
| `prefix prefix` | send one literal prefix byte to the focused session's pane |

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
- **Input dialogs** (filter, new, rename, split): type into the buffer,
  `Backspace` deletes, `Enter` submits, `Esc` cancels.
- **Kill confirm**: `y` (or `Y`) confirms; `n`, `Esc`, or any other key cancels.

## Mouse

| Gesture | Action |
|---|---|
| left-click a tree row | select that row (tree focused) |
| left-click a view | focus that view |
| wheel over the tree | move the selection (tree focused) |
| `Ctrl`+wheel over the tree | change the tree level — descend / ascend (tree focused) |
| right-click a tree row | press-hold to open its context menu, release on an item to run it; release off the menu to cancel |
| drag the view border | resize the tree |
| drag a modal's border | move the modal |

The context menu offers the same level-aware actions as the keyboard — focus,
new session / new window, rename, kill — as applicable to the clicked row. While
the live screen is focused, mouse events over it are forwarded to the pane (the
mux needs its own mouse mode enabled to use them).

## Automation

A running xmux instance listens on a local control socket. Sessions are addressed
`<source>/<session>` and windows `<source>/<session>:<window>`. It speaks
navigation/display verbs — `ping`, `status`, `dump`, `rescan`,
`switch <source>/<session>`, `focus <tree|terminal>`, `width <delta>` (a signed
column delta, not an absolute width), `toggle-auto-hide`, `quit` — and
session-lifecycle verbs:

- `new-session <source> [name]`
- `kill-session <source>/<session>`
- `rename-session <source>/<session> <name>`
- `new-window <source>/<session> [name]`
- `split-window <source>/<session>:<window> [v|h]` — vertical by default
- `kill-window <source>/<session>:<window>`
- `rename-window <source>/<session>:<window> <name>`

Drive it with `xmux ctl <verb>`, e.g. `xmux ctl switch prod/api`. A low-level `raw:`
namespace (`raw:key`, `raw:keys`, `raw:text`) injects keystrokes or bytes for
tests; it is unstable and not part of the supported surface.

With one instance running `xmux ctl` targets it automatically; with several it
refuses to guess. `xmux ctl list` prints each instance (pid, working directory,
tty, displayed session, focus) so you can drive a specific one with
`xmux ctl --pid <pid> <verb>`.
