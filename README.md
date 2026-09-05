# xmux

English · [한국어](README.ko.md)

*A cross-host terminal-multiplexer switcher.*

xmux is a persistent, terminal-owning supervisor written in Rust. It owns the
terminal you launch it in, keeps its live mux attachments running, and renders
a split view: a **nav list** of every reachable session on the left, the
selected session's **terminal view** on the right. Move the cursor and the
terminal view switches to that session in place.

![The xmux split view: a nav list of psmux sessions on this machine and tmux
sessions inside a WSL distribution, with the selected session's terminal view
filling the right side.](docs/assets/xmux.png)

## Install

xmux is one self-contained binary, provided as a prebuilt package for Windows,
macOS, and Linux on the [releases](https://github.com/zer0ken/xmux/releases)
page. Step-by-step instructions for each OS - prebuilt binary, package manager,
or build from source - are in [`INSTALL.md`](INSTALL.md).

Quick package-manager installs:

```sh
brew install zer0ken/xmux/xmux        # macOS
cargo install xmux                    # any OS with Rust
```

There is no winget install yet: the manifest in
[`packaging/winget`](packaging/winget) is not registered in the community
winget-pkgs repository. On Windows, use the prebuilt binary or
`cargo install xmux`.

Install the `xmux` command onto your `PATH` from source:

```sh
cargo install --path .
```

Or build the release binary without installing it:

```sh
cargo build --release        # binary at target/release/xmux
```

It runs on Windows and on unix-likes. You need `ssh` on the machine running
xmux for remote hosts, and at least one supported mux on each host you target.

## Supported muxes

xmux supports the following muxes:

- unix-likes
  - `tmux`
  - GNU `screen`
  - `zellij`
  - `abduco`
- Windows
  - `psmux`

A host's mux is detected from the binary it answers as, so a mix of these across
your hosts needs no configuration.

## Usage

Run xmux with no arguments to open the app:

```sh
xmux                          # open the app
xmux ls                       # list every reachable session (scriptable)
xmux attach <source> <name>   # attach one session directly, e.g. xmux attach prod api
xmux doctor                   # check config and per-source reachability
xmux instances                # list running instances
xmux send <name> <command…>  # drive one of them over its control socket
xmux update                  # update the installed binary
xmux version
```

The nav list fills the left side; the terminal view on the right shows the
selected session's live grid. Keyboard focus is on one region at a time.

## Keys

In the nav list:

| Key                      | Action                                                                   |
| ------------------------ | ------------------------------------------------------------------------ |
| `↑` / `↓` (or `k` / `j`) | move one card (wraps at both ends)                                       |
| `←` / `→` (or `h` / `l`) | previous / next `host/mux` section, the host cards counting as one       |
| `Home` / `End`           | jump to the first / last card                                            |
| `PageUp` / `PageDown`    | jump ten cards                                                           |
| `Enter`                  | move focus into the selected session's terminal view                    |
| `prefix 1`-`prefix 9`    | jump to a session by the number in its left column (keep typing for 10+) |
| `prefix n`               | start a new session on the selected host                                 |
| `/`                      | fuzzy-filter the list                                                    |
| `prefix r`               | re-scan: refresh which machines exist, and every source's sessions       |

xmux has its own prefix, like tmux's `set -g prefix`. The default is `Ctrl-g`,
configurable via `[ui] prefix`. Press the prefix, then a chord: `prefix q`
quits, `prefix ?` toggles the keybinding help, `prefix Tab` moves focus between
the nav and the terminal view. The mouse works too: click a row to select it, click
the terminal view to focus it. See [`docs/keybind.md`](docs/keybind.md) for the
rest.

## Roster

The roster assembles the machine candidates xmux offers as hosts. It gathers
ssh target names from three providers:

- `~/.ssh/config` aliases
- online peers on this machine's tailnet
- this machine's WSL distributions

Every provider yields ssh target names. Whichever provider suggested a name, the
downstream behavior is the same; the suggesting provider is kept alongside the
name and shown when the host becomes unreachable, so you can tell which provider
to inspect or disable. When a provider's CLI is missing, its daemon is down, or
its output cannot be parsed, the roster treats it as an empty list rather than
an error, so one dead provider never hides the hosts the others suggest.

The roster decides which machines become hosts. A machine no provider names is
a machine xmux has nothing to do with. `local`, this machine reached without
ssh, is not part of the roster. The roster is rebuilt at startup and on every
rescan. The `[discovery]` table is how you disable providers individually; all
are on by default.

## Hosts and sources

A **host** is a machine that hosts muxes and that xmux can reach. One host can
serve several muxes at once, so a host running both psmux and zellij is exposed
as two sources. Each host and mux pairing is a **source**, named `local:psmux`
when a host serves several and `prod` when it serves one. That name is what the
nav shows; commands name a session by its source and its session separately (e.g.
`switch prod api`). Remote hosts are
probed after the app is up, so a discovered source appears as its host answers.
A remote host that answers the network but refuses your credentials shows
`locked` (a `?` mark). Focus its panel in the terminal view and type the
username and the masked password into it; xmux establishes one authenticated
connection the rest of the session reuses, and re-probes just that host.

## Configuration

Configuration is entirely optional. xmux reads `~/.config/xmux/config.toml`:

```toml
exclude = ["bastion", "wsl.docker-desktop"]   # hide these machines

[local]
mux = "auto"          # "auto" (default): every mux installed here,
                      # or a list: ["psmux", "zellij", "abduco"]

[ui]
theme = "auto-dark"                  # built-in ANSI theme: "auto-dark" (default)
                                      # or "auto-light" (for a light terminal)
prefix = "C-g"                        # xmux's prefix (e.g. C-g, C-Space, C-b)
auto-hide-nav = false                 # initial auto-hide-nav state
hide-unreachable = true               # hide hosts no scan has reached (the filter names one to show its card)
nav-position = "left"                 # the nav's default side (left|top|right|bottom)
view-active-border-style = "green"    # focused view-border colour
hint-bar-style = "bg=blue,fg=white"   # hint bar colour (tmux status-style)
primary = "brightwhite"               # per-role colour overrides: primary, secondary,
accent = "lightgreen"                # accent, decoration, warning, error, disabled,
bar-bg = "colour235"                  # and the hint bar's bar-bg / bar-fg / bar-accent

[[hosts]]
ssh = "prod"          # an ssh-config alias
mux = "tmux"          # defaults to "tmux" when omitted
```

The `[ui]` presentation settings (theme, the per-role colour overrides,
selection-style, hint-bar-style, view-border styles) are watched and re-applied live
when `config.toml` changes - no restart needed.
Host/roster edits still need a `prefix r` rescan.

The nav rides on one of the four sides of the terminal view (a left or right column, a top
or bottom band); the `[ui] nav-position` key above picks the default and the nav never
moves on its own. `prefix p` moves the nav one side clockwise (left → top → right →
bottom → default) and remembers the choice in `~/.xmux/nav_position`, which wins over the
setting until the key cycles back to the default.

Hosts come from `~/.ssh/config` first; the config file augments that discovery,
never replaces it. Persistent state (last selected session, the live
auto-hide-nav toggle, the pinned nav position, logs, and control sockets) lives under
`~/.xmux/`.

## Control socket

Every running instance has a name and listens on `~/.xmux/ctl-<name>.sock`.
Commands name a session by its source and its session separately (`switch
<source> <session>`), which the nav shows joined as `<source>/<session>`. The
socket speaks navigation verbs
(`ping`, `status`, `dump`, `rescan`, `switch`, `focus`, `width`,
`toggle-auto-hide`, `quit`) and one session-lifecycle verb (`new-session`).
There are no kill, rename, or window verbs; the mux owns editing a session.

```sh
xmux instances                       # NAME · PID · CWD · TTY · displayed · focus
xmux send amber-otter switch prod api
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
