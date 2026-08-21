# xmux

*A cross-host terminal-multiplexer switcher — tmux's `prefix + s` / `switch-client`, but reaching every machine.*

xmux is a persistent, terminal-owning supervisor written in Rust. It owns the
terminal you launch it in, keeps its live mux attachments running, and renders
a split view: a **tree** of every reachable session on the left, the selected
session's **live screen** on the right. Move through the tree and the right pane
switches to that session in place, whether it's a local psmux session or a tmux
session over ssh. No detaching and reattaching, no picker to click through.

The goal is the `switch-client` experience you already know from tmux, extended
across hosts: instant, in-place switching between any configured machine's mux
sessions from one terminal.

## Features

- **One tree over every host.** Hosts → sessions → windows → panes, local and
  over ssh, in a single view. Hosts are auto-discovered from your
  `~/.ssh/config`.
- **In-place cross-host switching.** Selecting a session on another machine
  re-attaches to it within the same terminal window; selecting another session
  on the current server switches the client in place. No manual detach, and
  nothing to install on the remote.
- **Live screens, not previews.** The right pane is a real per-session PTY
  attachment, so what you see is the session's actual screen, kept alive as you
  navigate.
- **Two orthogonal axes.** A `Mux` axis (**tmux** and **psmux**) and a
  `Transport` axis (**local** and **ssh**) compose freely: any mux over any
  transport, without either knowing about the other.
- **Metadata without polling where it counts.** tmux hosts are tracked over
  control mode (`-CC`); psmux hosts are polled. Either way the tree reflects the
  servers, which remain the source of truth.
- **Switching, not editing.** Navigate, filter, jump by number, and start a
  session on an empty host. Renaming, killing, and window/pane work stay in the mux that already does
  them well.
- **Named instances.** Every running instance takes a name, so
  `xmux send <name> <command>` drives a specific one (see
  [Control socket](#control-socket)).

## Install

xmux is a Cargo project. Build the release binary:

```sh
cargo build --release        # binary at target/release/xmux
```

Or install it onto your `PATH`:

```sh
cargo install --path .
```

It runs on Windows and on unix-likes. You need `ssh` on the machine running
xmux for remote hosts, and a supported mux on each machine you target:
`tmux` on unix, `psmux` on Windows (both speak the same command language, and
xmux drives either).

## Usage

Run xmux with no arguments to open the interactive split view:

```sh
xmux                          # the interactive tree + live-screen app
xmux ls                       # list every reachable session (scriptable)
xmux attach <source>/<name>   # attach one session directly, e.g. xmux attach prod/api
xmux doctor                   # check config and per-host reachability
xmux instances                # list running instances
xmux send <name> <command…>  # drive one of them over its control socket
xmux version
```

### In the app

The left pane is the tree; the right pane shows the selected session's live
screen. Keyboard focus is on one region at a time.

**Tree navigation:**

| Key | Action |
|---|---|
| `↑` / `↓` (or `k` / `j`) | move between siblings at the current level |
| `→` / `←` (or `l` / `h`) | descend into children / ascend to the parent |
| `Home` / `End` | jump to the first / last row |
| `PageUp` / `PageDown` | jump ten rows |
| `Enter` | move focus into the selected session's live screen |
| `prefix 0`-`prefix 9` | jump to a session by the number in its gutter (keep typing for 10+) |
| `prefix n` | start a new session on the selected host |
| `/` | fuzzy-filter the tree |
| `prefix r` | re-scan every host |

The mouse works too: click a row to select it, click the right pane to focus it,
and scroll the wheel over the tree.

**Prefix keys.** xmux has its own prefix, like tmux's `set -g prefix`. The
default is `Ctrl-g`, configurable via `[ui] prefix` (see below). Press the
prefix, then:

| Chord | Action |
|---|---|
| `prefix q` | quit xmux |
| `prefix ?` | toggle the keybinding help |
| `prefix t` | toggle auto-hide-nav (focusing the screen gives it full width) |
| `prefix h` / `prefix l` (or `prefix Ctrl-←/→`) | narrow / widen the tree |
| `prefix Tab` / arrow / `Esc` | move focus between the tree and the screen |
| `prefix prefix` | send one literal prefix byte to the focused session |

The nav's bottom row is its status line. At rest it shows just the prefix; press the
prefix and it widens to the whole window, floating over the live screen to list the
keys that prefix unlocks.

See [`docs/keybind.md`](docs/keybind.md) for more on the prefix.

## Configuration

Configuration is entirely optional. Zero-config is the default. xmux reads
`~/.config/xmux/config.toml`:

```toml
# The mux used on the local machine.
[local]
mux = "auto"          # "auto" (default): psmux on Windows, tmux elsewhere

# Override the mux for a discovered ssh host, or add a host ssh-config
# discovery did not surface.
[[hosts]]
ssh = "prod"          # an ssh-config alias
mux = "tmux"          # defaults to "tmux" when omitted

# Hide these ssh aliases from the tree.
exclude = ["bastion"]

[ui]
prefix = "C-g"                        # xmux's prefix (e.g. C-g, C-Space, C-b)
auto-hide-nav = false                # initial auto-hide-nav state
view-active-border-style = "green"    # focused view-border colour (tmux colour vocabulary)
view-border-style = "default"         # unfocused view-border colour
view-border-hover-style = "yellow"    # drag-to-resize hover cue
hint-bar-style = "bg=blue,fg=white"   # hint bar colour (tmux status-style; empty = the built-in dark bar)
```

Hosts come from `~/.ssh/config` first. Connection details (user, port, key,
jump host) are taken from there. The config file augments that discovery; it
never replaces it. Run `xmux doctor` to see the resolved local mux, ssh
availability, and per-host reachability. Persistent state (last selected
session, the live auto-hide-nav toggle, logs, and control sockets) lives under
`~/.xmux/`.

## Control socket

Every running instance has a name — an auto-generated `<adjective>-<noun>`, or
whatever `xmux --name <name>` says — and listens on `~/.xmux/ctl-<name>.sock`.
Sessions are addressed `<source>/<session>`.

It speaks navigation/display verbs — `ping`, `status`, `dump`, `rescan`,
`switch <source>/<session>`, `focus <nav|terminal>`, `width <delta>` (adjusts the
tree width by a signed column count, a delta rather than an absolute width),
`toggle-auto-hide`, `quit` — and one session-lifecycle verb,
`new-session <source> [name]`. There are no kill/rename/window verbs, for the same
reason the keys are gone: the mux owns editing a session. An unstable `raw:`
namespace is reserved for low-level key/byte injection.

```sh
xmux instances                       # NAME · PID · CWD · TTY · displayed · focus
xmux send amber-otter switch prod/api
xmux send am focus terminal          # any unambiguous name prefix
xmux send - dump                     # `-` when exactly one is running
```

An unknown name, an ambiguous prefix, or `-` with several instances running is an
error naming the candidates, never a guess. With no command, `send` reads them from
stdin, one per line; a refused command exits non-zero.

## Architecture

xmux is built around two orthogonal axes, `Mux` (per-mux behavior) and
`Transport` (per-machine execution), so that mux families and machine families
compose without conflating. The metadata path and the display path are kept
separate, and the supervisor branches on nothing mux-specific.

The canonical guidance lives in the per-directory Working Notes
([`AGENTS.md`](AGENTS.md) files) and in [`CONTEXT.md`](CONTEXT.md), which holds
the vocabulary and the orthogonal-design overview. Architecture decisions are
recorded under [`docs/adr/`](docs/adr/), and behavior requirements in
[`docs/requirements.md`](docs/requirements.md).

## License

MIT — see [`LICENSE`](LICENSE).
