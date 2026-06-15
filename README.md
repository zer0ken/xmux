# xmux

Cross-environment mux session switcher — one terminal that sees and moves
between every reachable tmux/psmux session, local and over ssh, regardless of
OS or mux kind. Pick a session and attach; detach returns you to the tree.

xmux keeps no state of its own: sessions, recency, and reachability are scanned
from the mux servers each time. The servers are the source of truth.

## Install

From source:

    go build -o xmux ./cmd/xmux

Or:

    go install github.com/zer0ken/xmux/cmd/xmux@latest

## Requirements

- `ssh` on the machine running xmux (for remote sources).
- `tmux` or `psmux` on each machine you target (`psmux` on Windows, `tmux`
  elsewhere). Both speak the same command language; xmux drives either.

## Use

    xmux                         # full-screen cross-environment tree
    xmux popup                   # in-mux switcher (bind via display-popup, see below)
    xmux ls                      # list every reachable session (scriptable)
    xmux attach <source>/<name>  # attach one session directly, e.g. xmux attach prod/api
    xmux doctor                  # check config and per-source reachability
    xmux ctl <command…>          # drive a running switcher over its control socket
    xmux version

### In the tree

| Key | Action |
|---|---|
| `↑` / `↓` | move (the detail pane follows the cursor) |
| `Enter` | switch to a session · expand/collapse a host |
| `n` | new session on the focused host |
| `R` | rename the focused session |
| `x` | kill the focused session (inline `y`/`n` confirm) |
| `/` | fuzzy filter `<source>/<name>` |
| `r` | re-scan every host |
| `q` / `Esc` | quit |

The left pane is the merged tree of all hosts; the right pane shows the focused
session's windows and panes. A host that cannot be reached shows
`⚠ unreachable`; a reachable host with no sessions is still a valid create
target.

## Keybind

Bind a key in your mux to pop xmux up over your current pane — see
[docs/keybind.md](docs/keybind.md):

    bind g display-popup -E "xmux popup"

`prefix g` overlays the cross-environment tree. Pick a same-server session to
teleport instantly; pick another server to detach back to the home tree; press
Esc to return to your pane untouched.

## Configure

`~/.config/xmux/config.toml`, all optional (zero-config is the default):

    [local]
    mux = "auto"        # auto: psmux on Windows, tmux elsewhere

    [[hosts]]
    ssh = "prod"        # an ssh-config alias; mux defaults to tmux
    mux = "tmux"

    exclude = ["bastion"]

Hosts are auto-derived from `~/.ssh/config`; connection details (user, port,
key, jump host) come from there. Config augments that discovery — it never
replaces it.
