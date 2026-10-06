# Demo GIFs

`make-gifs.sh` regenerates the GIFs the READMEs show, from a published xmux
release or from a build of the checkout:

| GIF | Shows |
| --- | --- |
| `docs/assets/xmux-demo.gif` | the same remote tmux session reached by hand (`ssh`, `tmux ls`, `tmux attach`) and from the xmux landing screen with the arrow keys and `Enter`, side by side |
| `docs/assets/xmux-landing.gif` | the landing screen filling as the hosts answer, and a session opened from it |
| `docs/assets/xmux-nav-switch.gif` | moving between sessions, by arrow and by number |
| `docs/assets/xmux-hierarchy.gif` | walking up from a session to its source and host screens with `Ctrl-↑`, and back down with `Ctrl-↓` |
| `docs/assets/xmux-login.gif` | logging in to a password-only host, registering the key, and opening one of its sessions |
| `docs/assets/xmux-nav-resize.gif` | widening and narrowing the nav |
| `docs/assets/xmux-nav-place.gif` | placing the nav on each side of the terminal view |
| `docs/assets/xmux-nav-autohide.gif` | auto-hiding the nav |
| `docs/assets/xmux.png` | the xmux window alone after a session switch, as a still for places that take only an image |

## Usage

```sh
scripts/demo/make-gifs.sh            # the version in Cargo.toml
scripts/demo/make-gifs.sh 0.12.3     # any published release
scripts/demo/make-gifs.sh --local    # a build of the checkout
```

The script needs Docker and Node.js on `PATH`. With `--local` it builds the
checkout for Linux in a Rust container, keeping the build cache and the crate
registry in Docker volumes of their own, and installs that build in place of a
download. It overwrites the GIFs in `docs/assets`; review them before committing.

## How the recording works

Every machine in the recordings is a container on a private Docker network, built
from one image that installs the requested xmux release or the local build:

| Machine | Role | Sessions |
| --- | --- | --- |
| `laptop` | runs xmux and the manual ssh | `dotfiles`, `notes` |
| `gpu-01` | ssh server | `my-important-session`, `train-llm` |
| `web-01` | ssh server | `api-server`, `deploy` |
| `db-01` | ssh server that takes only the demo user's password | `postgres`, `backup` |

Nothing from the machine that runs the script appears in a GIF: the user is `dev`,
the hosts and sessions are the ones above, and xmux offers only the hosts in the
demo user's ssh config. `db-01` joins that config only in the login scenario,
so the other GIFs never show it.

Each scenario runs a shell on a pseudo terminal inside `laptop` and types keys at
a fixed pace. A step that waits on the app waits until the screen shows the
expected text, so the recording keeps the real latency of ssh, tmux, and xmux.
Both sides of the comparison type at the same pace. Every xmux scenario starts
with no remembered selection or placement, the first-key introduction already
seen, and a nav wide enough to show each demo card whole. The comparison and the
feature tour reach their first session from the landing screen with the arrow keys
and `Enter`. The pacing constants and the nav size sit at the top of `record.py`.

The recordings are replayed in a terminal emulator in a headless browser, one
frame at a time, so the GIF timing does not depend on the speed of the machine.
Each pressed key is shown at the bottom right of its terminal, a typed password as
`•`. In the comparison, each side's thick border pulses from the moment that side
reaches the session. The area around the terminals is transparent, so the GIFs sit
on any page background.

Each GIF is decoded again after it is written and compared frame by frame with
what was rendered; a mismatch, a frame of a different size, or a terminal that
moves between frames stops the script.

## Files

| File | Purpose |
| --- | --- |
| `make-gifs.sh` | builds xmux for `--local`, builds the image, starts the machines, records, renders, encodes, and removes the machines |
| `Dockerfile`, `ssh_config`, `xmux.toml` | the demo machines and the demo user's configuration |
| `sessions.sh` | the tmux sessions on each machine, and the password-only login on `db-01` |
| `record.py` | the scenarios and the recorder |
| `stage.html`, `render.mjs`, `package.json` | the replay page and the renderer |
| `encode.py` | the GIF encoder, run inside the demo image |
