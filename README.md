# xmux

English · [한국어](README.ko.md)

*A cross-host terminal-multiplexer switcher.*

xmux is a persistent, terminal-owning supervisor written in Rust. It owns the
terminal you launch it in, keeps its live mux attachments running, and renders
a split view: a **nav list** of every reachable session on the left, the
selected session's **live screen** on the right. Move the cursor and the
right pane switches to that session in place, whether it's a local psmux
session, a tmux session over ssh, or a zellij session on a third machine.

![The xmux split view: a nav list of psmux sessions on this machine and tmux
sessions inside a WSL distribution, with the selected session's live screen
filling the right pane.](docs/assets/xmux.png)

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
xmux for remote hosts, and a supported mux on each host you target: `tmux` on
unix, `psmux` on Windows, or `zellij`. A host's mux is detected from the binary
it answers as, so a mix of the three across your hosts needs no configuration.

## Usage

Run xmux with no arguments to open the app:

```sh
xmux                          # open the app
xmux ls                       # list every reachable session (scriptable)
xmux attach <source>/<name>   # attach one session directly, e.g. xmux attach prod/api
xmux doctor                   # check config and per-source reachability
xmux instances                # list running instances
xmux send <name> <command…>  # drive one of them over its control socket
xmux version
```

The left pane is the nav list; the right pane shows the selected session's live
screen. Keyboard focus is on one region at a time.

## Keys

In the nav list:

| Key | Action |
|---|---|
| `↑` / `↓` (or `k` / `j`) | move one card (wraps at both ends) |
| `Home` / `End` | jump to the first / last card |
| `PageUp` / `PageDown` | jump ten cards |
| `Enter` | move focus into the selected session's live screen |
| `prefix 0`-`prefix 9` | jump to a session by the number in its left column (keep typing for 10+) |
| `prefix n` | start a new session on the selected host |
| `/` | fuzzy-filter the list |
| `prefix r` | re-scan every source |

xmux has its own prefix, like tmux's `set -g prefix`. The default is `Ctrl-g`,
configurable via `[ui] prefix`. Press the prefix, then a chord: `prefix q`
quits, `prefix ?` toggles the keybinding help, `prefix Tab` moves focus between
the nav and the screen. The mouse works too: click a row to select it, click
the right pane to focus it. See [`docs/keybind.md`](docs/keybind.md) for the
rest.

## Hosts and sources

A **host** is a machine that hosts muxes and that xmux can reach. Hosts are
auto-discovered from `~/.ssh/config`, the tailnet, this box's WSL
distributions, and `local`. One host can serve several muxes at once, so a host
running both psmux and zellij exposes both. Each host and mux pairing is a
**source**, named `local:psmux` when a host serves several and `prod` when it
serves one. That name is what the nav shows and what commands address as
`<source>/<session>`. Remote hosts are probed after the app is up, so a
discovered source appears as its host answers.

## Configuration

Configuration is entirely optional. xmux reads `~/.config/xmux/config.toml`:

```toml
exclude = ["bastion", "wsl.docker-desktop"]   # hide these machines

[local]
mux = "auto"          # "auto" (default): every mux installed here,
                      # or a list: ["psmux", "zellij"]

[ui]
prefix = "C-g"                        # xmux's prefix (e.g. C-g, C-Space, C-b)
auto-hide-nav = false                 # initial auto-hide-nav state
view-active-border-style = "green"    # focused view-border colour
hint-bar-style = "bg=blue,fg=white"   # hint bar colour (tmux status-style)

[[hosts]]
ssh = "prod"          # an ssh-config alias
mux = "tmux"          # defaults to "tmux" when omitted
```

Hosts come from `~/.ssh/config` first; the config file augments that discovery,
never replaces it. Persistent state (last selected session, the live
auto-hide-nav toggle, logs, and control sockets) lives under `~/.xmux/`.

## Control socket

Every running instance has a name and listens on `~/.xmux/ctl-<name>.sock`.
Sessions are addressed `<source>/<session>`. The socket speaks navigation verbs
(`ping`, `status`, `dump`, `rescan`, `switch`, `focus`, `width`,
`toggle-auto-hide`, `quit`) and one session-lifecycle verb (`new-session`).
There are no kill, rename, or window verbs; the mux owns editing a session.

```sh
xmux instances                       # NAME · PID · CWD · TTY · displayed · focus
xmux send amber-otter switch prod/api
xmux send am focus terminal          # any unambiguous name prefix
xmux send - dump                     # `-` when exactly one is running
```

An unknown name, an ambiguous prefix, or `-` with several instances running is
an error naming the candidates, never a guess.

## License

MIT - see [`LICENSE`](LICENSE).

## More

- [`docs/keybind.md`](docs/keybind.md) - the keybinding and prefix detail
- [`docs/requirements.md`](docs/requirements.md) - the behavior requirements
- [`docs/adr/`](docs/adr/) - the architecture decision records
- [`CONTEXT.md`](CONTEXT.md) - the vocabulary and the design overview
- [`AGENTS.md`](AGENTS.md) - the per-directory working notes
