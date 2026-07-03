# xmux

*A cross-host terminal-multiplexer switcher — tmux's `prefix + s` / `switch-client`, but reaching every machine.*

xmux is a persistent, terminal-owning supervisor written in Rust. It owns the
terminal you launch it in, keeps live mux display attachments alive, and renders
a split view: a **tree** of every reachable session on the left, and the
selected session's **live screen** on the right. Move through the tree and the
right pane switches to that session in place — a local psmux session, a tmux
session over ssh, whatever — with no detach dance and no picker round-trip.

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
  `Transport` axis (**local** and **ssh**) compose freely — any mux over any
  transport — without either knowing about the other.
- **Metadata without polling where it counts.** tmux hosts are tracked over
  control mode (`-CC`); psmux hosts are polled. Either way the tree reflects the
  servers, which remain the source of truth.
- **Mouse and keyboard.** Navigate, filter, create, rename, and kill sessions
  from the keyboard; click, scroll, and right-click work too.
- **A control socket.** A local socket exposes semantic verbs for scripting and
  headless driving (see [Control socket](#control-socket)).

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
xmux for remote hosts, and a supported mux on each machine you target —
`tmux` on unix, `psmux` on Windows (both speak the same command language, and
xmux drives either).

## Usage

Run xmux with no arguments to open the interactive split view:

```sh
xmux                          # the interactive tree + live-screen app
xmux ls                       # list every reachable session (scriptable)
xmux attach <source>/<name>   # attach one session directly, e.g. xmux attach prod/api
xmux doctor                   # check config and per-host reachability
xmux ctl <command…>           # drive a running instance over its control socket
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
| `n` | create (session / window / split, depending on the selected level) |
| `R` | rename the selected session or window |
| `x` | kill the selected session (with a confirm prompt) |
| `/` | fuzzy-filter the tree |
| `r` | re-scan every host |

The mouse works too: click a row to select it, click the right pane to focus it,
scroll the wheel over the tree, and right-click a row for a context menu.

**Prefix keys.** xmux has its own prefix, like tmux's `set -g prefix` — the
default is `Ctrl-g`, configurable via `[ui] prefix` (see below). Press the
prefix, then:

| Chord | Action |
|---|---|
| `prefix q` | quit xmux |
| `prefix ?` | toggle the keybinding help |
| `prefix t` | toggle auto-hide-tree (focusing the screen gives it full width) |
| `prefix h` / `prefix l` (or `prefix Ctrl-←/→`) | narrow / widen the tree |
| `prefix Tab` / arrow / `Esc` | move focus between the tree and the screen |
| `prefix prefix` | send one literal prefix byte to the focused session |

See [`docs/keybind.md`](docs/keybind.md) for more on the prefix.

## Configuration

Configuration is entirely optional — zero-config is the default. xmux reads
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
auto-hide-tree = false                # initial auto-hide-tree state
view-active-border-style = "green"    # focused view-border colour (tmux colour vocabulary)
view-border-style = "default"         # unfocused view-border colour
view-border-hover-style = "yellow"    # drag-to-resize hover cue
```

Hosts come from `~/.ssh/config` first — connection details (user, port, key,
jump host) are taken from there. The config file augments that discovery; it
never replaces it. Run `xmux doctor` to see the resolved local mux, ssh
availability, and per-host reachability. Persistent state (last selected
session, the live auto-hide-tree toggle, logs, and control sockets) lives under
`~/.xmux/`.

## Control socket

A running xmux instance listens on a local socket (`~/.xmux/ctl-<pid>.sock`) that
speaks semantic verbs — `ping`, `status`, `dump`, `rescan`, `switch <addr>`,
`focus <target>`, `width <n>`, `toggle-auto-hide`, `quit` — with an unstable
`raw:` namespace reserved for low-level key/byte injection. Drive it with:

```sh
xmux ctl status
xmux ctl switch prod/api
```

With one instance running, `xmux ctl` targets it automatically. When several are
running it refuses to guess: list them and target one by pid.

```sh
xmux ctl list                 # PID · CWD · TTY · displayed session · focus
xmux ctl --pid 51907 switch local/logs
```

## Architecture

xmux is built around two orthogonal axes — `Mux` (per-mux behavior) and
`Transport` (per-machine execution) — so that mux families and machine families
compose without conflating. The metadata path and the display path are kept
separate, and the supervisor branches on nothing mux-specific.

The canonical guidance lives in the per-directory Working Notes
([`AGENTS.md`](AGENTS.md) files) and in [`CONTEXT.md`](CONTEXT.md), which holds
the vocabulary and the orthogonal-design overview. Architecture decisions are
recorded under [`docs/adr/`](docs/adr/), and behavior requirements in
[`docs/requirements.md`](docs/requirements.md).

## License

MIT — see [`LICENSE`](LICENSE).
