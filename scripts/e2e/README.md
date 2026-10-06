# End-to-End Suite

The suite drives the real xmux against real hosts: every host is a Docker container
reached over ssh, every mux runs real sessions, and xmux runs in a pseudo terminal that
the driver types into and reads back. One command runs every scenario for every mux and
host system and prints a pass/fail table.

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

Every host serves two sessions in each mux, `<mux>1` and `<mux>2`:

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
login shell, since no host installs dvtm.

## Scenarios

| Scenario | Passes when |
| --- | --- |
| `first-launch` | the landing screen appears, choosing a session attaches it, and typed input runs in that session |
| `switch` | moving to a session of another mux on another host, and back to a second session, shows each one |
| `new-session` | `prefix n` creates a session that becomes selected, attached, and live on the host |
| `password-login` | logging in to the password-only host lists its sessions, and logging out leaves one host card |
| `unreachable` | a stopped host shows as one unreachable host card after a re-scan, and returns as sections once it starts again |
| `detach-inside` | the mux's own detach key leaves the session alive, and selecting it again attaches it |
| `shared-client` | a client attached directly on the host stays attached while xmux attaches the same session, and shows what was typed through xmux |
| `in-client-switch` | moving the client to another session with the mux's own input moves the nav selection with it |

A session proves it is the one expected by answering `whereami`, a host command that
prints the host, mux, and session its shell runs in.

## Results

A cell is `PASS`, `FAIL`, `KNOWN #<issue>` for a known defect or limitation, or `n/a`
where the mux has no such feature. A known cell still runs and reads `PASS` once the
issue is fixed.

| Cell | Result | Reason |
| --- | --- | --- |
| every scenario, abduco on Alpine | `KNOWN #578` | the 0.6 listing yields no sessions |
| every scenario, screen on Alpine | `KNOWN #588` | the screen 5 listing yields no sessions |
| `new-session`, zellij and abduco | `KNOWN #584` | the create over ssh times out although the session starts |
| `switch`, abduco | `KNOWN #585` | keys typed right after returning to an abduco source are lost |
| `in-client-switch`, tmux | `KNOWN #586` | a directly attached remote client's move is not followed |
| `in-client-switch`, zellij | `KNOWN #587` | a remote client's move is not followed |
| `in-client-switch`, tuios | `KNOWN #333` | tuios gives no signal that its client moved |
| `in-client-switch`, screen, abduco, and herdr | `n/a` | a client belongs to one session's server and cannot move |

## Isolation

Each scenario starts xmux as a new user in the client container, with its own home, ssh
key, ssh config naming only the hosts it needs, and xmux config with neighbour and WSL
discovery and the update check off. Nothing is read from or written to the home of the
person running the suite. The containers, their network, and their images carry the
`xmux-e2e` prefix.

## Windows Client

`suite.py --client windows` runs the Windows xmux on the machine itself against the same
hosts through published ssh ports, with a temporary home and an `ssh.exe` wrapper that
hands Windows OpenSSH the temporary ssh config. The run stops before it starts xmux
until #581 is resolved: on Windows, xmux keeps its config and state in the profile folder
whatever `HOME` and `USERPROFILE` say, so a run would use the real `~/.xmux`.

## Files

| File | Purpose |
| --- | --- |
| `run.sh` | builds the images and xmux, runs the suite in the client container, and cleans up |
| `suite.py` | the hosts, the clients, the scenarios, and the result table |
| `driver.py` | the pseudo terminal, the screen model, and the nav reader |
| `hosts/` | the host images, the session setup, and `whereami` |
| `client/` | the Linux client image |
| `windows/` | the `ssh.exe` wrapper for the Windows client run |
