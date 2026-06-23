# xmux

Cross-environment mux session switcher — one terminal that sees and moves
between every reachable tmux/psmux session, local and over ssh, regardless of
OS or mux kind. Pick a session and attach; detach returns you to the tree.

xmux keeps no state of its own: sessions, recency, and reachability are scanned
from the mux servers each time. The servers are the source of truth.

## Install

xmux is written in Rust. From source:

    cargo build --release        # binary at target/release/xmux

Or install into `~/.cargo/bin`:

    cargo install --path .

(The previous Go implementation is preserved under `legacy-go/` for reference.)

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

The left pane is one tree over every environment — Host → Session → Window →
Pane — with the live preview of the focused node's pane on the right.

| Key | Action |
|---|---|
| `↑` / `↓` | move (panes are shown but skipped; the preview follows) |
| `Home` / `End` | jump to the first / last node |
| `Enter` | attach — on a host: its most-recent session; on a session: that session; on a window: that window |
| `n` | new session on the focused host |
| `R` | rename the focused session |
| `x` | kill the focused session (inline `y`/`n` confirm) |
| `/` | fuzzy filter `<source>/<name>` |
| `r` | re-scan every host |
| `C-g ?` | toggle the keybinding help overlay |
| `q` / `Esc` | quit |

The mouse works too: click selects, double-click attaches, the wheel scrolls.

The right pane is a **live preview**: it polls and shows the screen of the pane
that attaching here would land on — a host previews its most-recent session's
active window, a session its active window, a window its active pane. A host
that cannot be reached shows `⚠ unreachable`; a reachable host with no sessions
is still a valid create target. Panes are shown for context but are not
selectable.

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
