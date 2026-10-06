# xmux

English · [한국어](README.ko.md)

*A cross-machine, cross-mux session switcher.*

xmux is a persistent, terminal-owning supervisor written in Rust. It owns the
terminal it is launched in, keeps its live mux attachments running, and renders
a split view: the **nav** on the left holds a card for every reachable session,
and the **terminal view** on the right shows the selected session. Moving the
selection switches the terminal view to that session in place.

xmux is built for people who:

- **Work across many remote machines, mostly servers**
  - xmux reaches each of them and switches between their sessions without a
    manual reconnect.
- **Would rather not install anything on those machines**
  - xmux does everything over ssh and the mux each machine already runs, so it
    is installed only on the machine it is used from.
- **Trust tmux**
  - xmux is not an alternative to tmux. It only makes reaching tmux sessions
    simpler.

![Two terminals recorded side by side at the same typing speed. On the left,
ssh gpu-01, tmux ls and tmux attach reach a remote tmux session in 7.1
seconds; on the right, xmux selects the same session from its nav in 2.7
seconds.](docs/assets/xmux-demo.gif)

- **Every session in one nav.** The sessions on this machine, on its WSL
  distributions, and on every ssh host it reaches appear side by side.
- **Real attachments.** The terminal view is a live mux client, not a
  reconstruction, so it shows what the mux draws.
- **Nothing to configure.** Hosts come from `~/.ssh/config` and from the
  machines this one already reaches; each host's mux is detected from what it
  runs.
- **Scriptable.** Every running instance takes commands over a local control
  socket.

**Switch sessions**

![Moving down one card, then jumping to sessions 5 and 3 by number; the terminal
view follows each selection.](docs/assets/xmux-nav-switch.gif)

**Resize the nav**

![Holding prefix Ctrl-→ widens the nav one column per press, and Ctrl-← narrows
it back.](docs/assets/xmux-nav-resize.gif)

**Move the nav**

![Each prefix p moves the nav to the next side of the terminal view: top, right,
bottom, then back to the left.](docs/assets/xmux-nav-move.gif)

**Auto-hide the nav**

![With auto-hide on, focusing the terminal view hides the nav and gives the
terminal the full width; prefix Tab brings the nav back.](docs/assets/xmux-nav-autohide.gif)

## Quick start

### 1. Installation

**Native install (recommended)**

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

The error `The token '&&' is not a valid statement separator` means the CMD
command ran in PowerShell, and `'irm' is not recognized as an internal or
external command` means the PowerShell command ran in CMD. A PowerShell prompt
starts with `PS C:\`; a CMD prompt is just `C:\`.

The install script:

- downloads the build for the machine it runs on
- refuses the build unless it matches the checksum the release publishes
- puts the `xmux` command on `PATH` without asking for elevation

A terminal opened after the install picks up the new `PATH`.

> `xmux update` upgrades a native install. xmux reports a newer release on
> startup but never installs one on its own.

**Homebrew** (macOS)

```sh
brew install zer0ken/xmux/xmux
```

> A Homebrew install does not update itself. `xmux update` or
> `brew upgrade zer0ken/xmux/xmux` installs a new release.

**WinGet** (Windows)

```powershell
winget install --id zer0ken.xmux
```

> A WinGet install does not update itself. `xmux update` or
> `winget upgrade --id zer0ken.xmux` installs a new release. The winget catalog
> is updated through a review in the community repository, so it can trail the
> newest release; the native install always gets the newest one.

**Cargo** (any OS with Rust)

```sh
cargo install xmux
```

[`INSTALL.md`](INSTALL.md) covers the rest of installation:

- pinning a version
- a custom install directory
- the prebuilt binaries
- building from source
- removing xmux with `xmux uninstall`

### 2. Install check

```sh
xmux version
xmux doctor
```

`xmux doctor` reports which xmux is running and where it was installed, then
checks the config and whether each source is reachable.

Remote hosts need `ssh` on the machine that runs xmux, and at least one
[supported mux](#supported-muxes) on each host.

### 3. First launch

```sh
xmux
```

The nav fills with this machine's sessions at once, and remote hosts join as
they answer. In the nav:

- `↑` / `↓` move the selection.
- `Enter` sends the keyboard to the selected session.
- `Ctrl-g` then `Tab` returns focus to the nav.
- `Ctrl-g ?` lists every key, and `Ctrl-g q` quits.

## Supported muxes

| Platform   | Muxes                                                      |
| ---------- | ---------------------------------------------------------- |
| unix-likes | `tmux`, GNU `screen`, `zellij`, `abduco`, `tuios`, `herdr` |
| Windows    | `psmux`, `herdr`                                            |

xmux detects a host's mux from the binary the host answers as, so hosts that
run different muxes need no configuration.

## Usage

```sh
xmux                          # open the app
xmux ls                       # list every reachable session (scriptable)
xmux attach <source> <name>   # attach one session directly, e.g. xmux attach prod api
xmux doctor                   # check config and per-source reachability
xmux instances                # list running instances
xmux send <name> <command…>   # drive one of them over its control socket
xmux update                   # update the installed binary
xmux uninstall                # remove the installed xmux, asking first
xmux version
```

The nav sits on the left and the terminal view on the right shows the selected
session's live grid. Keyboard focus is on one view at a time.

## Keys

The nav takes these keys while it holds focus:

| Key                      | Action                                                                   |
| ------------------------ | ------------------------------------------------------------------------ |
| `↑` / `↓` (or `k` / `j`) | move one card (wraps at both ends)                                       |
| `←` / `→` (or `h` / `l`) | previous / next `host/mux` section, the host cards counting as one       |
| `Home` / `End`           | jump to the first / last card                                            |
| `PageUp` / `PageDown`    | jump ten cards                                                           |
| `Enter`                  | move focus into the selected session's terminal view                     |
| `prefix 1`-`prefix 9`    | jump to a session by the number in its left column (keep typing for 10+) |
| `prefix n`               | start a new session on the selected host                                 |
| `prefix /`               | fuzzy-filter the cards                                                   |
| `prefix r`               | re-scan: refresh which machines exist, and every source's sessions       |
| `prefix L`               | log out of the selected SSH host                                         |

xmux has its own prefix, like tmux's `set -g prefix`. The default is `Ctrl-g`,
and `[ui] prefix` replaces it. A chord is the prefix followed by one key:

| Chord        | Action                                           |
| ------------ | ------------------------------------------------ |
| `prefix q`   | quit                                             |
| `prefix ?`   | toggle the help (type to search keys and glyphs) |
| `prefix m`   | toggle the history of results and events         |
| `prefix Tab` | move focus between the nav and the terminal view |
| `prefix p`   | move the nav to the next side of the view        |

Pressing the prefix opens a box beside the prefix indicator that lists every key it
unlocks. A click on a card selects it, and a click on the terminal view focuses it.
The first key pressed after installation briefly points out the configured prefix and
help key. xmux records that the introduction has been shown.
[`docs/keybind.md`](docs/keybind.md) lists the remaining keys.

## Hosts and sources

A **host** is a machine that hosts muxes and that xmux can reach. A **source**
is one mux on one host, so a host running both psmux and zellij is two sources.
A source is named `local:psmux` when its host serves several muxes and `prod`
when it serves one; that name is what the nav shows. Commands name a session by
its source and its session separately (e.g. `switch prod api`).

xmux probes remote hosts after the app is up, so each source appears as its
host answers.

### Host login

A remote host that ssh cannot reach with the values it works out on its own
shows `login required` (a `?` mark). Its panel in the terminal view takes the
login:

1. The panel holds the values ssh does not ask for, each starting at what ssh
   would have used:
   - the address
   - the port
   - the username
   - an optional masked password
2. On submit, xmux hands those values to ssh and answers the host-key question
   and the password itself, so the login needs no further input. Esc ends the
   attempt.
3. A login that works re-probes that host, and the panel gives way to the
   sessions it found. The submitted values become the machine's, so everything
   xmux runs there afterwards connects the way the login did.

A login the values cannot finish reports what the server asked for. One
radio choice decides what a working login leaves behind:

- nothing
- the values, recorded as an `~/.ssh/config` stanza
- the user's public key, registered on the host so it stops asking for a
  password

The line xmux appends ends its comment with `xmux-registered`, which sshd ignores and
which tells xmux's lines from the user's own. A host that already holds the same key,
under any comment or options, gets no second line, and its line stays unmarked.

After registering the key, xmux runs one separate login that may use only that key,
and reports the key registered only when that login runs a command. When the host
accepts the key but cannot open a session, xmux reports the server's error and removes
the line this registration added, so the host stays reachable by password. When that
login cannot be tried at all, xmux keeps the key and reports it as not verified.

The information screen's `SSH login` row shows the SSH authentication method reported
by the selected session's display connection. On a host card it shows the machine's last observed
method. If SSH reuses a connection without reporting its method, the screen says
`not observed`. A held password disappearing closes that machine's
metadata and display connections. A new login or explicit re-scan is needed to
connect again.

On an SSH host, `prefix L` opens a confirmation that states the selected session, its
observed SSH login, what happens to a held password and to this PC's key, and the
machine whose connections close. Type `logout` to take this PC's public key off the
host, then clear the password xmux holds in memory and close that machine's
connections, including its SSH master where present. Before closing anything, xmux
looks for lines in the host's key files that hold one of this PC's public keys,
comparing the key type and body and ignoring options and the comment. The lines marked
`xmux-registered` are removed. A matching line without the mark was not added by xmux,
and removing it also stops ssh outside xmux from using the key, so a second
confirmation opens in the same place and asks first: type `remove` to remove it too,
or press Esc to keep it. When the host cannot be reached or the removal fails, the
logout still clears the password and the connections, and its toast says the key
remains and why. After the key, xmux removes the stanza a login saved for the host in
`~/.ssh/config`, the one under its `# xmux: <host>` line, and leaves every other line of
the file as it was; the toast says whether it was removed. A re-scan reconnects only with
a key the host still accepts; otherwise, log in again.

For the requirements of a Windows host and the limits of Entra-only accounts, see
[`INSTALL.md`](INSTALL.md#windows-hosts).

## Roster

The roster assembles the machines xmux offers as hosts. It gathers ssh target
names from three providers:

| Provider               | What it names                                              |
| ---------------------- | ---------------------------------------------------------- |
| ssh config             | the aliases in `~/.ssh/config`                             |
| neighbours             | the machines this one already reaches in one hop and that answer ssh |
| WSL                    | this machine's WSL distributions                           |

The roster is rebuilt at startup and on every rescan. `local`, this machine
reached without ssh, is not part of the roster, and a machine no provider names
is a machine xmux has nothing to do with. The `[discovery]` table turns
providers off one by one; all are on by default.
Each source's first contact and session listing share a ten-second scan limit.
An unanswered card stops scanning after ten seconds and shows a timeout.

Every provider yields ssh target names, and xmux behaves the same whichever
provider suggested a name. The suggesting provider is kept beside the name and
shown when the host becomes unreachable, which tells which provider to inspect
or turn off. A provider whose command is missing, whose OS does not answer, or
whose output cannot be parsed counts as an empty list rather than an error, so
one failing provider never hides the hosts the others suggest.

### Neighbour discovery

The neighbours provider reads the operating system's own network state, so it
needs no VPN client and no account anywhere. It asks the OS directly (over
netlink on Linux and Android, through IP Helper on Windows), so it also runs
where the usual command-line tools are missing or, as on Android, refused.

- **Who is reachable.** Two records say so. The routing table holds one route
  per peer that a mesh VPN writes, or one route for a handful of peers, which
  is read as those addresses. The neighbour table (the ARP cache) holds the
  machines on this link this machine has exchanged frames with. Where the OS
  refuses the neighbour table, as Android does, the provider asks the link this
  machine is on address by address instead.
- **Which of them are machines.** An entry that resolved to nothing, and one
  hardware address answering for many addresses (a router speaking for a
  subnet), name no machine. Each remaining entry is asked whether it answers
  ssh, because a printer on the same switch is a neighbour and not a host.
- **What to call them.** The provider asks the system resolver first, which is
  where a mesh VPN's own naming already lives, so a peer arrives under the name
  its network gave it. A machine no resolver knows is asked for its own name,
  which it answers over mDNS whether or not anyone registered it. A name is used
  only when this machine can resolve it back, because the name is also what ssh
  is given; a machine whose name leads nowhere keeps its address as its name.

## Configuration

Configuration is optional. xmux reads `~/.config/xmux/config.toml`:

```toml
exclude = ["bastion", "wsl.docker-desktop"]   # hide these machines

[local]
mux = "auto"          # "auto" (default): every mux installed here,
                      # or a list: ["psmux", "zellij", "abduco", "tuios", "herdr"]

[ui]
theme = "auto-dark"                  # built-in ANSI theme: "auto-dark" (default)
                                      # or "auto-light" (for a light terminal)
prefix = "C-g"                        # xmux's prefix (e.g. C-g, C-Space, C-b)
auto-hide-nav = false                 # initial auto-hide-nav state
renumbering = true                     # keep card numbers in sorted nav order
notifications = true                  # show results as toasts (the prefix m history keeps them either way)
braille-animation = true             # show the central Braille X on scanning and host screens
nav-position = "left"                 # the nav's default side (left|top|right|bottom)
max-fps = 30                          # maximum xmux draws per second (10 to 120)
view-active-border-style = "green"    # focused view-border colour
hint-bar-style = "bg=blue,fg=white"   # hint bar colour (tmux status-style)
primary = "brightwhite"               # per-role colour overrides: primary, secondary,
accent = "lightgreen"                 # accent, decoration, warning, error, disabled,
bar-bg = "colour235"                  # and the hint bar's bar-bg / bar-fg / bar-accent

[update]
check = true                          # ask once a day whether a newer release exists

[[hosts]]
ssh = "prod"          # an ssh-config alias
mux = "tmux"          # omitted or "auto": every mux the host answers it has
```

- **Live reload.** When `config.toml` changes, xmux re-applies the `[ui]`
  presentation settings without a restart:
  - theme
  - the per-role colour overrides
  - selection-style
  - hint-bar-style
  - view-border styles
  - max-fps
  - notifications
  - renumbering
  - braille-animation
  - nav-position

  Host and roster edits take effect on a `prefix r` rescan.
- **Nav position.** The nav rides on one of the four sides of the terminal view
  (a left or right column, a top or bottom band). `[ui] nav-position` picks the
  default, and the nav never moves on its own. `prefix p` moves it one side
  clockwise (left → top → right → bottom → default) and remembers the choice in
  `~/.xmux/nav_position`, which wins over the setting until the key cycles back
  to the default.
- **Hosts.** Hosts come from `~/.ssh/config` first; the config file adds to
  that discovery and never replaces it.
- **State.** The state kept between runs lives under `~/.xmux/`:
  - the last selected session
  - the live auto-hide-nav toggle
  - the pinned nav position
  - logs
  - control sockets

## Control socket

Every running instance has a name and listens on `~/.xmux/ctl-<name>.sock`.
Commands name a session by its source and its session separately (`switch
<source> <session>`), which the nav shows joined as `<source>/<session>`. The
socket takes navigation verbs (`ping`, `status`, `dump`, `rescan`, `switch`,
`focus`, `width`, `toggle-auto-hide`, `quit`) and one session-lifecycle verb
(`new-session`). It has no kill, rename, or window verbs, because the mux owns
editing a session.

```sh
xmux instances                       # NAME · PID · CWD · TTY · displayed · focus
xmux send amber-otter switch prod api
xmux send am focus terminal          # any unambiguous name prefix
xmux send - dump                     # `-` when exactly one is running
```

xmux answers each of these with an error that names the candidates, and never
picks one by guessing:

- an unknown name
- a prefix matching several instances
- `-` while several instances run

## License

MIT. The full text is in [`LICENSE`](LICENSE).

## More

- [`INSTALL.md`](INSTALL.md) - every install path, upgrading, and pinning a version
- [`docs/keybind.md`](docs/keybind.md) - the keybinding and prefix detail
- [`docs/requirements.md`](docs/requirements.md) - the behavior requirements
- [`docs/principles.md`](docs/principles.md) - the design principles
- [`CONTEXT.md`](CONTEXT.md) - the vocabulary and the design overview
- [`AGENTS.md`](AGENTS.md) - the per-directory working notes
