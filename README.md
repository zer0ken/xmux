# xmux

English · [한국어](README.ko.md)

*A cross-host terminal-multiplexer switcher.*

xmux is a persistent, terminal-owning supervisor written in Rust. It owns the
terminal you launch it in, keeps its live mux attachments running, and renders
a split view: a **nav list** of every reachable session on the left, the
selected session's **terminal view** on the right. Move the cursor and the
terminal view switches to that session in place.

xmux is built for people who:

- **Work across many remote machines, mostly servers**
  - xmux reaches each of them and switches between their sessions without a
    manual reconnect.
- **Would rather not install anything on those machines**
  - xmux does everything over ssh and the mux each machine already runs, so it
    is installed on one machine only: the one you use it from.
- **Trust tmux**
  - xmux is not an alternative to tmux. It only makes getting to your tmux
    sessions simpler.

![The xmux split view: a nav list of psmux sessions on this machine and tmux
sessions inside a WSL distribution, with the selected session's terminal view
filling the right side.](docs/assets/xmux.png)

- **Every session in one list.** Sessions on this machine, on its WSL
  distributions, and on every ssh host it can reach, side by side.
- **Real attachments.** The terminal view is a live mux client, not a
  reconstruction, so what you see is what the mux draws.
- **Nothing to configure.** Hosts come from `~/.ssh/config` and the machines
  this box already reaches; each host's mux is detected from what it runs.
- **Scriptable.** Every running instance takes commands over a local control
  socket.

## Quick start

### 1. Install xmux

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

If you see `The token '&&' is not a valid statement separator`, you are in
PowerShell, not CMD. If you see `'irm' is not recognized as an internal or
external command`, you are in CMD, not PowerShell. A PowerShell prompt starts
with `PS C:\`; a CMD prompt is just `C:\`.

The script downloads the build for your machine, refuses it unless it matches
the checksum the release publishes, and puts the `xmux` command on your `PATH`
without asking for elevation. Open a new terminal afterwards so it picks up the
new `PATH`.

> A native install is upgraded with `xmux update`. xmux tells you on startup
> when a newer version has been released, but it never installs one on its own.

**Homebrew** (macOS)

```sh
brew install zer0ken/xmux/xmux
```

> A Homebrew install does not update itself. Run `xmux update`, or
> `brew upgrade zer0ken/xmux/xmux`, to pick up a new release.

**WinGet** (Windows)

```powershell
winget install --id zer0ken.xmux
```

> A WinGet install does not update itself. Run `xmux update`, or
> `winget upgrade --id zer0ken.xmux`. The winget catalog is updated through a
> review in the community repository, so it can trail the newest release; the
> native install always gets the newest one.

**Cargo** (any OS with Rust)

```sh
cargo install xmux
```

A pinned version, a custom install directory, the prebuilt binaries, and
building from source are covered in [`INSTALL.md`](INSTALL.md).

### 2. Check the install

```sh
xmux version
xmux doctor
```

`xmux doctor` reports which xmux is running and where it was installed, then
checks the config and whether each source is reachable.

xmux needs `ssh` on the machine that runs it, for remote hosts, and at least
one [supported mux](#supported-muxes) on each host you target.

### 3. Open the app

```sh
xmux
```

The nav list fills with this machine's sessions at once; remote hosts join as
they answer. Move with `↑` / `↓`, press `Enter` to type into the selected
session, and press `Ctrl-g` then `Tab` to get back to the nav. `Ctrl-g ?` shows
every key, and `Ctrl-g q` quits.

## Supported muxes

| Platform   | Muxes                                                      |
| ---------- | ---------------------------------------------------------- |
| unix-likes | `tmux`, GNU `screen`, `zellij`, `abduco`, `tuios`, `herdr` |
| Windows    | `psmux`, `herdr`                                            |

A host's mux is detected from the binary it answers as, so a mix of these across
your hosts needs no configuration.

## Usage

```sh
xmux                          # open the app
xmux ls                       # list every reachable session (scriptable)
xmux attach <source> <name>   # attach one session directly, e.g. xmux attach prod api
xmux doctor                   # check config and per-source reachability
xmux instances                # list running instances
xmux send <name> <command…>   # drive one of them over its control socket
xmux update                   # update the installed binary
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
| `Enter`                  | move focus into the selected session's terminal view                     |
| `prefix 1`-`prefix 9`    | jump to a session by the number in its left column (keep typing for 10+) |
| `prefix n`               | start a new session on the selected host                                 |
| `/`                      | fuzzy-filter the list                                                    |
| `prefix r`               | re-scan: refresh which machines exist, and every source's sessions       |

xmux has its own prefix, like tmux's `set -g prefix`. The default is `Ctrl-g`,
configurable via `[ui] prefix`. Press the prefix, then a chord:

| Chord        | Action                                          |
| ------------ | ----------------------------------------------- |
| `prefix q`   | quit                                            |
| `prefix ?`   | toggle the keybinding help                      |
| `prefix Tab` | move focus between the nav and the terminal view |
| `prefix p`   | move the nav to the next side of the view       |

The mouse works too: click a row to select it, click the terminal view to focus
it. See [`docs/keybind.md`](docs/keybind.md) for the rest.

## Hosts and sources

A **host** is a machine that hosts muxes and that xmux can reach. A **source**
is one mux on one host, so a host running both psmux and zellij is two sources.
A source is named `local:psmux` when its host serves several muxes and `prod`
when it serves one; that name is what the nav shows. Commands name a session by
its source and its session separately (e.g. `switch prod api`).

Remote hosts are probed after the app is up, so a source appears as its host
answers.

### Logging in to a host

A remote host xmux could not reach with the values ssh works out on its own
shows `login required` (a `?` mark). Focus its panel in the terminal view:

1. The panel holds the address, the port and the username ssh will not ask you
   for, each starting at what ssh would have used, plus an optional masked
   password.
2. Submitting hands those values to ssh. xmux answers the host-key question and
   the password itself, so there is nothing to watch and nothing to type. Esc
   ends the attempt.
3. A login that works re-probes that host, and the panel gives way to the
   sessions it found. The values you submitted become the machine's, so
   everything xmux runs there afterwards reaches it the way the login did.

A login the values cannot finish says what the server asked for. Two checkboxes
decide what a working login leaves behind: recording the values as an
`~/.ssh/config` stanza, and registering your public key on the host so it stops
asking for a password.

## Roster

The roster assembles the machine candidates xmux offers as hosts. It gathers
ssh target names from three providers:

| Provider               | What it names                                              |
| ---------------------- | ---------------------------------------------------------- |
| ssh config             | the aliases in `~/.ssh/config`                             |
| one-hop network        | the machines this box already reaches in one hop and that answer ssh |
| WSL                    | this machine's WSL distributions                           |

The roster is rebuilt at startup and on every rescan. `local`, this machine
reached without ssh, is not part of the roster, and a machine no provider names
is a machine xmux has nothing to do with. The `[discovery]` table disables
providers individually; all are on by default.

Every provider yields ssh target names, and the downstream behavior is the same
whichever one suggested a name. The suggesting provider is kept alongside the
name and shown when the host becomes unreachable, so you can tell which provider
to inspect or disable. A provider whose command is missing, whose OS will not
answer, or whose output cannot be parsed counts as an empty list rather than an
error, so one dead provider never hides the hosts the others suggest.

### How the one-hop network is read

The one-hop provider reads the operating system's own network state, so it
needs no VPN client installed and no account anywhere. It reads it from the OS
directly - over netlink on Linux and Android, through IP Helper on Windows - so
it also runs where the usual command-line tools are missing or, as on Android,
refused.

- **Who is reachable.** Two records say so: the routing table, where a mesh VPN
  writes one route per peer (or one for a handful of them, which is read as
  those addresses), and the neighbour table (the ARP cache), which holds the
  machines on this link this box has actually exchanged frames with. Where the
  OS refuses the neighbour table, as Android does, the link this machine is on
  is asked address by address instead.
- **Which of them are machines.** An entry that resolved to nothing, and one
  hardware address answering for many addresses (a router speaking for a
  subnet), name no machine. What is left is asked whether it answers ssh,
  because a printer on the same switch is a neighbour and not a host.
- **What to call them.** The system resolver is asked first, which is where a
  mesh VPN's own naming already lives, so a peer arrives under the name its
  network gave it. A machine no resolver knows is asked for its own name, which
  it answers over mDNS whether or not anyone registered it anywhere. A name is
  used only when this machine can resolve it back, because the name is also
  what ssh is given; a machine whose name leads nowhere keeps its address as its
  name.

## Configuration

Configuration is entirely optional. xmux reads `~/.config/xmux/config.toml`:

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
hide-unreachable = true               # hide hosts no scan has reached (the filter names one to show its card)
nav-position = "left"                 # the nav's default side (left|top|right|bottom)
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

- **Live reload.** The `[ui]` presentation settings (theme, the per-role colour
  overrides, selection-style, hint-bar-style, view-border styles) are re-applied
  as soon as `config.toml` changes, with no restart. Host and roster edits need
  a `prefix r` rescan.
- **Nav position.** The nav rides on one of the four sides of the terminal view
  (a left or right column, a top or bottom band); `[ui] nav-position` picks the
  default and the nav never moves on its own. `prefix p` moves it one side
  clockwise (left → top → right → bottom → default) and remembers the choice in
  `~/.xmux/nav_position`, which wins over the setting until the key cycles back
  to the default.
- **Hosts.** Hosts come from `~/.ssh/config` first; the config file augments
  that discovery, never replaces it.
- **State.** Persistent state (last selected session, the live auto-hide-nav
  toggle, the pinned nav position, logs, and control sockets) lives under
  `~/.xmux/`.

## Control socket

Every running instance has a name and listens on `~/.xmux/ctl-<name>.sock`.
Commands name a session by its source and its session separately (`switch
<source> <session>`), which the nav shows joined as `<source>/<session>`. The
socket speaks navigation verbs (`ping`, `status`, `dump`, `rescan`, `switch`,
`focus`, `width`, `toggle-auto-hide`, `quit`) and one session-lifecycle verb
(`new-session`). There are no kill, rename, or window verbs; the mux owns
editing a session.

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

- [`INSTALL.md`](INSTALL.md) - every install path, upgrading, and pinning a version
- [`docs/keybind.md`](docs/keybind.md) - the keybinding and prefix detail
- [`docs/requirements.md`](docs/requirements.md) - the behavior requirements
- [`docs/adr/`](docs/adr/) - the architecture decision records
- [`CONTEXT.md`](CONTEXT.md) - the vocabulary and the design overview
- [`AGENTS.md`](AGENTS.md) - the per-directory working notes
