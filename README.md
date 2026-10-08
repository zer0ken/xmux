# xmux

English · [한국어](README.ko.md)

*A cross-machine, cross-mux session switcher.*

![Two terminals recorded side by side at the same typing speed. On the left,
ssh gpu-01, tmux ls and tmux attach reach a remote tmux session in 7.3
seconds; on the right, xmux opens the same session from its landing screen with
two arrow keys and Enter in 2.6 seconds.](docs/assets/xmux-demo.gif)

**Open a session from the landing screen**

![xmux starts on the landing screen, which lists every session it finds under its
number while the machines answer; two arrow keys and Enter open a session on
gpu-01.](docs/assets/xmux-landing.gif)

**Switch sessions**

![Moving down one card, then jumping to sessions 5 and 3 by number; the terminal
view follows each selection.](docs/assets/xmux-nav-switch.gif)

**Walk up to the host and the machine**

![Ctrl-↑ selects the session's host and shows its screen, a second Ctrl-↑ selects the
machine and shows its screen, and Ctrl-↓ walks back down to the
session.](docs/assets/xmux-hierarchy.gif)

**Log in to a password machine**

![A machine that takes only a password shows login needed. Its login pane takes the
password, registers this PC's public key on the machine, and the machine's sessions
join the nav. The machine screen stays, and its host link leads to the host screen,
where a session opens.](docs/assets/xmux-login.gif)

**Resize the nav**

![Holding prefix Ctrl-→ widens the nav one column per press, and Ctrl-← narrows
it back.](docs/assets/xmux-nav-resize.gif)

**Place the nav**

![Each prefix p places the nav on the next side of the terminal view: top, right,
bottom, then back to the left.](docs/assets/xmux-nav-place.gif)

**Auto-hide the nav**

![With auto-hide on, focusing the terminal view hides the nav and gives the
terminal the full width; prefix Tab brings the nav back.](docs/assets/xmux-nav-autohide.gif)

## What xmux is

xmux owns the terminal it is launched in and splits it in two. The **nav** holds a
card for every session on this machine, on its WSL distributions, and on every ssh
host it reaches, and the **terminal view** shows the selected session through a
live mux client. Hosts come from `~/.ssh/config` and from the machines this one
already reaches, and each host's mux is detected from what it runs.

xmux works over ssh and the mux each host already runs, so it is installed only on
the machine it is used from. It is not an alternative to tmux: it makes reaching
tmux sessions simpler.

## Install

macOS, Linux, WSL, Android Termux:

```sh
curl -fsSL https://github.com/zer0ken/xmux/releases/latest/download/install.sh | sh
```

Windows PowerShell:

```powershell
irm https://github.com/zer0ken/xmux/releases/latest/download/install.ps1 | iex
```

Windows CMD:

```batch
curl -fsSL https://github.com/zer0ken/xmux/releases/latest/download/install.cmd -o install.cmd && install.cmd && del install.cmd
```

[`INSTALL.md`](INSTALL.md) covers Homebrew, WinGet, and Cargo, pinning a version,
updating, and removing xmux.

## First launch

```sh
xmux
```

The landing screen lists every session as the machines answer. In the nav:

- `↑` / `↓` move the selection.
- `Enter` sends the keyboard to the selected session.
- `Ctrl-g` then `Tab` returns focus to the nav.
- `Ctrl-g ?` lists every key, and `Ctrl-g q` quits.

The accent background marks the **selection**, where the next key acts. **Hover**
previews the item under the pointer without moving the selection; over a selected
item, it adds an underline. A standalone card has one blank cell inside each side
of its selection or hover background. A part of a shared label, such as the machine
or mux in `machine/mux`, highlights only its text. A selected standalone card can
show `⏎` while the nav has focus; shared label parts carry no Enter mark.

## Supported muxes

| Platform   | Muxes                                                      |
| ---------- | ---------------------------------------------------------- |
| unix-likes | `tmux`, GNU `screen`, `zellij`, `abduco`, `tuios`, `herdr` |
| Windows    | `psmux`, `herdr`                                            |

Remote hosts need `ssh` on the machine that runs xmux and one of these muxes.

## More

- [`INSTALL.md`](INSTALL.md) - every install path, the install check, upgrading, and pinning a version
- [`docs/guide.md`](docs/guide.md) - the command line, hosts and login, configuration, and the control socket
- [`docs/keybind.md`](docs/keybind.md) - every key, the prefix, popups, and the mouse
- [`docs/principles.md`](docs/principles.md) - the design principles
- [`docs/requirements.md`](docs/requirements.md) - the behavior requirements
- [`CONTEXT.md`](CONTEXT.md) - the vocabulary and the design overview
- [`AGENTS.md`](AGENTS.md) - the per-directory working notes

## License

MIT. The full text is in [`LICENSE`](LICENSE).
