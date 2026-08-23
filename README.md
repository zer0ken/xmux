# xmux

*A cross-host terminal-multiplexer switcher — tmux's `prefix + s` / `switch-client`, but reaching every machine.*

xmux is a persistent, terminal-owning supervisor written in Rust. It owns the
terminal you launch it in, keeps its live mux attachments running, and renders
a split view: a **nav list** of every reachable session on the left, the selected
session's **live screen** on the right. Move down the list and the right pane
switches to that session in place, whether it's a local psmux session, a tmux
session over ssh, or a zellij session on a third machine. No detaching and
reattaching, no picker to click through.

The goal is the `switch-client` experience you already know from tmux, extended
across hosts: instant, in-place switching between any configured machine's mux
sessions from one terminal.

## Features

- **One list over every host.** One card per session, local and over ssh, in a
  single view, ordered by how recently you were in it. Each card names the host, the
  mux serving it, and the session's focused window. Hosts are auto-discovered from your
  `~/.ssh/config`.
- **In-place cross-host switching.** Selecting a session on another machine
  re-attaches to it within the same terminal window; selecting another session
  on the current server switches the client in place. No manual detach, and
  nothing to install on the remote.
- **Live screens, not previews.** The right pane is a real per-session PTY
  attachment, so what you see is the session's actual screen, kept alive as you
  navigate.
- **Two orthogonal axes.** A `Mux` axis (**tmux**, **psmux**, and **zellij**) and a
  `Transport` axis (**local** and **ssh**) compose freely: any mux over any
  transport, without either knowing about the other.
- **Metadata without polling where it counts.** tmux hosts are tracked over
  control mode (`-CC`); psmux and zellij hosts are polled, because neither offers a
  push channel. Either way the nav list reflects the servers, which remain the source of
  truth.
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
xmux for remote hosts, and a supported mux on each machine you target: `tmux` on
unix, `psmux` on Windows (both speak the same command language), or `zellij`,
which speaks its own and is driven through its own CLI. A machine's mux is detected
from the binary it answers as, so a mix of the three across your hosts needs no
configuration.

## Usage

Run xmux with no arguments to open the interactive split view:

```sh
xmux                          # the interactive nav + live-screen app
xmux ls                       # list every reachable session (scriptable)
xmux attach <source>/<name>   # attach one session directly, e.g. xmux attach prod/api
xmux doctor                   # check config and per-host reachability
xmux instances                # list running instances
xmux send <name> <command…>  # drive one of them over its control socket
xmux version
```

### In the app

The left pane is the nav list; the right pane shows the selected session's live
screen. Keyboard focus is on one region at a time.

**Nav keys:**

| Key | Action |
|---|---|
| `↑` / `↓` (or `k` / `j`) | move one card (wraps at both ends) |
| `Home` / `End` | jump to the first / last card |
| `PageUp` / `PageDown` | jump ten cards |
| `Enter` | move focus into the selected session's live screen |
| `prefix 0`-`prefix 9` | jump to a session by the number in its left column (keep typing for 10+) |
| `prefix n` | start a new session on the selected host |
| `/` | fuzzy-filter the list |
| `prefix r` | re-scan every host |

The mouse works too: click a row to select it, click the right pane to focus it,
and scroll the wheel over the list.

**Prefix keys.** xmux has its own prefix, like tmux's `set -g prefix`. The
default is `Ctrl-g`, configurable via `[ui] prefix` (see below). Press the
prefix, then:

| Chord | Action |
|---|---|
| `prefix q` | quit xmux |
| `prefix ?` | toggle the keybinding help |
| `prefix t` | toggle auto-hide-nav (focusing the screen gives it full width) |
| `prefix h` / `prefix l` (or `prefix Ctrl-←/→`) | narrow / widen the nav |
| `prefix Tab` / arrow / `Esc` | move focus between the nav and the screen |
| `prefix prefix` | send one literal prefix byte to the focused session |

The nav's bottom row is its status line. At rest it shows just the prefix; press the
prefix and it widens to the whole window, floating over the live screen to list the
keys that prefix unlocks.

See [`docs/keybind.md`](docs/keybind.md) for more on the prefix.

## Several muxes on one machine

A machine can run more than one mux at a time, and xmux offers each as its own
source. On THIS machine you do not have to say so: with `mux` left at its default,
xmux asks this box which of the muxes it supports are installed, and offers each one
it finds. Install zellij next to your psmux and it is simply there on the next run.

Name them explicitly when you want a specific set, on this machine or a remote:

```toml
[local]
mux = ["psmux", "zellij"]

[[hosts]]
ssh = "prod"
mux = ["tmux", "zellij"]
```

Both then appear in the list, `local/psmux` over its sessions and `local/zellij` over
its own, and moving between them is the same keystroke as moving between hosts. A
machine given several muxes has its sources named `local:psmux` and `local:zellij`, and
that is the name `xmux ls` prints and `xmux send switch` takes; a machine given one
keeps its bare name (`local`, `prod`) exactly as before. `exclude` names machines, so
excluding one drops every mux on it.

Listing a mux that is not installed on that machine is not silently ignored: the source
appears as unreachable with the mux's own message, because a name you wrote is a name
you meant. A DISCOVERED list works the other way round, since nothing was written: only
the muxes that answered are offered. `xmux doctor` says which of the two you have.

Remote machines are asked too, but AFTER the app is up. A remote probe is an ssh round
trip per mux, so nothing waits for it: the sources you configured paint immediately, and a
mux nobody wrote down appears as its machine answers, a second or several later. Discovery
only ever ADDS. The mux a machine was already showing keeps its name, so nothing you typed
or selected changes under you.

## Where the host list comes from

By default xmux offers the `Host` aliases in `~/.ssh/config`, the online peers of this
machine's tailnet, and `local`. The machines you can reach are the machines xmux offers,
with nothing to keep in sync by hand. Each provider can be turned off:

```toml
[discovery]
ssh-config = true   # default; the `Host` aliases in ~/.ssh/config
tailscale = true    # default; the online peers of this machine's tailnet
```

A tailnet peer is offered under its DNS label (`jupiter00`), the name that resolves
and the name an ssh config would already use. Offline peers and this machine itself
are skipped: this machine is `local`, and an offline peer has nothing to scan. A
provider that cannot answer (no CLI, daemon down) contributes nothing rather than
failing the run. Names from `~/.ssh/config` come first and a provider repeating one
adds nothing, so a host you configured by hand keeps the position you gave it.

## Configuration

Configuration is entirely optional. Zero-config is the default. xmux reads
`~/.config/xmux/config.toml`:

```toml
# The mux used on the local machine. A LIST runs several at once: each is its own
# source, and both appear side by side in the list.
[local]
mux = "auto"          # "auto" (default): every mux xmux supports that is actually
                      # installed here, the conventional one first (psmux on Windows,
                      # tmux elsewhere). Also accepts "tmux", "psmux", "zellij",
                      # or a list: ["psmux", "zellij"]

# Override the mux for a discovered ssh host, or add a host ssh-config
# discovery did not surface.
[[hosts]]
ssh = "prod"          # an ssh-config alias
mux = "tmux"          # defaults to "tmux" when omitted

# Hide these ssh aliases from the nav.
exclude = ["bastion"]

[ui]
prefix = "C-g"                        # xmux's prefix (e.g. C-g, C-Space, C-b)
auto-hide-nav = false                # initial auto-hide-nav state
view-active-border-style = "green"    # focused view-border colour (tmux colour vocabulary)
view-border-style = "default"         # unfocused view-border colour
view-border-hover-style = "yellow"    # drag-to-resize hover cue
hint-bar-style = "bg=blue,fg=white"   # hint bar colour (tmux status-style; empty = the built-in bar)
selection-style = "#2d4f6b"           # the selected card's background. Empty (default): reverse video,
                                      # your terminal theme's own selected look.
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
nav width by a signed column count, a delta rather than an absolute width),
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
