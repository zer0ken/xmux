# User Guide

The README shows xmux at work and how to install and launch it. This guide covers
the rest of daily use: the command line, how hosts are found and logged in to, the
configuration file, and the control socket. [`keybind.md`](keybind.md) lists every key.

## Command Line

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

## Hosts and Sources

A **host** is a machine that hosts muxes and that xmux can reach. A **source**
is one mux on one host, so a host running both psmux and zellij is two sources.
A source is named `local:psmux` when its host serves several muxes and `prod`
when it serves one; that name is what the nav shows. Commands name a session by
its source and its session separately (e.g. `switch prod api`).

xmux probes remote hosts after the app is up, so each source appears as its
host answers.

### Host Login

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
remains and why. After the key, xmux removes the host from every `Host` entry in
`~/.ssh/config` that names it exactly, ignoring case. An entry that names only this host
goes with its options, including the one a login saved under its `# xmux: <host>` line;
an entry that names other hosts too loses only this name. Wildcard and negated patterns
and `Match` blocks stay, as does every other line of the file; the toast names each
entry that changed. A re-scan reconnects only with
a key the host still accepts; otherwise, log in again.

For the requirements of a Windows host and the limits of Entra-only accounts, see
[`INSTALL.md`](../INSTALL.md#windows-hosts).

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

### Neighbour Discovery

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

  Host and roster edits take effect on a `prefix R` rescan.
- **Nav position.** The nav rides on one of the four sides of the terminal view
  (a left or right column, a top or bottom band). `[ui] nav-position` picks the
  default, and the nav never moves on its own. `prefix p` places it one side
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

## Control Socket

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
