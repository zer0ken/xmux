# End-to-End Suite

The suite drives the real xmux against real hosts: every host is a Docker container
reached over ssh, every mux runs real sessions, and xmux runs in a pseudo terminal that
the driver types into and reads back. One command runs every scenario for every mux and
host system and prints a pass/fail table in two groups:

- **xmux behavior**: xmux's own features work on every mux and host system.
- **native workflow**: each mux's own keys, panes, detach, and clients keep working
  inside xmux's terminal view.

## Usage

```sh
scripts/e2e/run.sh                                  # the whole Linux client matrix
scripts/e2e/run.sh --os alpine --mux tmux,zellij    # a slice
scripts/e2e/run.sh --scenario first-launch,switch
```

The script needs Docker. It builds the host and client images and a static Linux xmux
from this checkout, runs the suite inside the client container, and removes every
container it started. `XMUX_E2E_BIN` names a prebuilt static Linux xmux to test
instead. The table and a screen dump of each failed cell land in `scripts/e2e/out`.
The command exits non-zero when any cell fails.

CI runs the same command on every pull request and on pushes to `main`.

## Hosts

Each host system has three hosts, started from one image per system:

| Host | Login |
| --- | --- |
| `deb-1`, `alp-1` | key |
| `deb-2`, `alp-2` | key |
| `deb-pw`, `alp-pw` | password only |

Every host serves two sessions in each mux, `<mux>1` and `<mux>2`. A host starts them
each time it boots, before its sshd accepts a connection, so a host that returns after
a stop serves them again and xmux never reaches a session that is still starting:

| Mux | Debian 12 | Alpine 3.22 |
| --- | --- | --- |
| tmux | 3.3a | 3.5a |
| screen | 4.09.00 | 5.0.1 |
| zellij | 0.45.1 | 0.45.1 |
| abduco | master branch, listing with a pid column | 0.6, listing without a pid column |
| tuios | 0.8.5 | 0.8.5 |
| herdr | 0.9.3 | 0.9.3 |

tuios and herdr are installed in the remote user's `~/.local/bin`, which is on `PATH`
only through the login profile, as an install without root leaves them. Each mux runs
on both systems. A host user has seen herdr's first-run screen, and abduco starts the
login shell, since no host installs dvtm. Every host has `ss` from iproute2, as a
server install does, because xmux reads it to follow a zellij client's session switch.

## Scenarios

The xmux behavior group:

| Scenario | Passes when |
| --- | --- |
| `first-launch` | the landing screen appears, choosing a session attaches it, and typed input runs in that session |
| `switch` | after both sessions of a mux were shown, moving to a session of another mux on another host and back to the second session shows each one and runs what is typed there |
| `new-session` | `prefix n` creates a session that becomes selected, attached, and live on the host |
| `password-login` | logging in to the password-only host lists its sessions, and logging out, confirming the removal of the ssh config entry xmux did not write, removes that entry and leaves one host card |
| `unreachable` | a stopped host shows as one unreachable host card after a re-scan, and returns as sections once it starts again |

The native workflow group, typed through xmux's terminal view:

| Scenario | Passes when |
| --- | --- |
| `native-keys` | the mux's own keys open and switch its windows, panes, or tabs, and its copy mode or a mouse click works (below) |
| `detach-inside` | the mux's own detach key leaves the session alive, and selecting it again attaches it |
| `shared-client` | a client attached directly on the host stays attached while xmux attaches the same session, and shows what was typed through xmux |
| `in-client-switch` | moving the client to another session with the mux's own input moves the nav selection with it |

`native-keys` runs on a session of its own, and checks each mux this way:

| Mux | Input | Check |
| --- | --- | --- |
| tmux | `C-b %`, then `C-b [` and `PgUp` | the window has two panes; copy mode shows its position, and `q` ends it |
| screen | `C-a c`, then `C-a n` | `$WINDOW` reads 1 in the new window and 0 after moving on |
| zellij | `C-p n`, a click on the first pane, then `C-t n` | the new pane, then the clicked pane, answers with its id; a second tab opens |
| tuios | `C-b c` | tuios lists two windows |
| herdr | `C-b v` | herdr lists two panes |
| abduco | none | abduco has no keys besides detach, which `detach-inside` covers |

A session proves it is the one expected by answering `whereami`, a host command that
prints the host, mux, and session its shell runs in.

## Results

A cell is `PASS`, `FAIL`, `KNOWN #<issue>` for a known defect or limitation, or `n/a`
where the mux has no such feature. A known cell still runs and reads `PASS` once the
issue is fixed.

| Cell | Result | Reason |
| --- | --- | --- |
| `in-client-switch`, zellij | `KNOWN #670` | zellij can drop the client right after `switch-session` moves it |
| `in-client-switch`, tuios | `KNOWN #333` | tuios gives no signal that its client moved |
| `in-client-switch`, screen, abduco, and herdr | `n/a` | a client belongs to one session's server and cannot move |
| `native-keys`, abduco | `n/a` | abduco has no keys besides detach |
| `first-launch`, tmux, Alpine, Windows client | `KNOWN #673` | the tail of a terminal reply reaches tmux 3.5a as typed text |
| `switch`, Alpine, Windows client | `KNOWN #673` | each of these cells shows Alpine's tmux, for the same reason |

## Isolation

Each scenario starts xmux as a new user in the client container, with its own home, ssh
key, ssh config naming only the hosts it needs, and xmux config with neighbour and WSL
discovery and the update check off. Nothing is read from or written to the home of the
person running the suite. The containers, their network, and their images carry the
`xmux-e2e` prefix.

## Windows Client

`suite.py --client windows` runs the Windows xmux on the machine itself against the same
hosts through published ssh ports. Each scenario gets a temporary home, named by both
`HOME` and `USERPROFILE`, and an `ssh.exe` wrapper that hands Windows OpenSSH the
temporary ssh config. It runs `first-launch` and `switch` unless `--scenario` names
others, and needs the host images `run.sh` builds, `pyte` and `pywinpty`, and `rustc`
for the wrapper:

```sh
uv run --with pyte --with pywinpty scripts/e2e/suite.py --client windows --xmux target/debug/xmux.exe
```

The run proves its isolation from both ends:

- Before it starts the hosts, it runs `xmux doctor` under a temporary home whose config
  holds a key no other config has. Unless the doctor reports that key as unknown and
  writes its log into that home, the run stops.
- After the table, it compares every entry under `~/.xmux`, `~/.config/xmux`, and
  `~/.ssh` of the person running it with its size and modification time from before the
  run, and fails when one differs. An xmux of that person's running at the same time
  changes `~/.xmux` too, so the run expects none.

psmux, the Windows-only mux, has no cell. Its sessions run on the Windows machine
itself and register under the user's `~/.psmux`, so a psmux scenario would start
sessions beside the user's own; it needs the Windows client run and a psmux that can be
pointed at another home.

## Files

| File | Purpose |
| --- | --- |
| `run.sh` | builds the images and xmux, runs the suite in the client container, and cleans up |
| `suite.py` | the hosts, the clients, the scenarios, and the result table |
| `driver.py` | the pseudo terminal, the screen model, and the nav reader |
| `hosts/` | the host images, the session setup, and `whereami` |
| `client/` | the Linux client image |
| `windows/` | the `ssh.exe` wrapper for the Windows client run |
