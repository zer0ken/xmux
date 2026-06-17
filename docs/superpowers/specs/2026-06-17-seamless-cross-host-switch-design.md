# Seamless cross-host switch — design

## Goal

Deliver tmux's `prefix + s` experience across hosts: from inside any mux, pop up a
list of every reachable session (local + every ssh remote), pick one, and the
terminal becomes that session — in a single action, in place, without bouncing
through a home screen. Cross-host switching must feel as close to tmux's
same-server `switch-client` as the medium allows.

## The constraint that shapes everything

`switch-client` is instant because the mux *server* repaints the *existing*
client: no process is replaced, no terminal is handed over. This works only
within one server.

A target on another server (a remote host, or a different local socket) has no
existing client. Reaching it requires standing up a new client
(`tmux attach` locally, or `ssh -t host tmux attach` for a remote) and tearing
down the current one. The terminal must pass from the old client to the new one.

Only the process that owns the controlling terminal can hand it over. A
`display-popup -E` overlay does not own the terminal — the mux *client* process
(what the user launched) does. Therefore cross-server switching requires a
persistent process that owns the terminal, runs the current mux client as a
child, and swaps that child on demand. This process is the **cockpit**.

## Model

`xmux` (no subcommand) is the cockpit: a persistent supervisor that owns the
terminal and runs exactly one mux-client child at a time.

```
cockpit:
  publish cockpit socket (so the in-mux popup can reach it)
  target = first pick (run the picker) or none → quit
  loop:
    spawn child = attach(target)          # local: psmux attach; remote: ssh -t host tmux attach
    wait for child to exit                # the child owns the terminal while it runs
    if a switch request arrived → target = it; continue   # NO picker between: seamless
    else → target = run the picker        # genuine detach / session end → pick again, or quit
```

A control socket served for the cockpit's whole lifetime accepts `switch <addr>`
from the in-mux popup and records the requested target. Because the child owns
the terminal while attached, the cockpit cannot draw anything itself during an
attach — the switch is performed by ending the child (the popup detaches it) and
re-attaching with no intermediate UI.

### The three switch paths

1. **Same-server (local or remote)** — native. Inside any mux, picking a session
   on the *same* server uses `switch-client` (instant, in-place). On a remote,
   the remote mux's own `prefix + s` already does this; no xmux involvement
   needed. The local popup uses `switch-client` for a local→local pick.

2. **Cross-server from the local cockpit** — the seamless single action. The
   popup runs locally, finds the cockpit socket, sends `switch <source>/<name>`,
   then detaches its own client. The cockpit's child exits, the cockpit reads the
   recorded target and re-attaches it immediately — no picker, no home tree. The
   only visible artifact is the alternate-screen teardown of the old client and
   setup of the new one (identical to a same-server detach+reattach).

3. **Cross-server from inside a remote** — graceful native fallback. The popup
   inside a remote mux cannot reach the local cockpit socket (different machine).
   It reports that crossing hosts from here uses a native detach. The user
   presses the mux's own detach (`prefix d`); the `ssh -t … attach` child exits;
   the cockpit, finding no recorded switch, shows the picker; the user picks any
   target. Reliable and topology-independent (detach always returns to the
   cockpit), at the cost of one extra keystroke versus path 2.

## Components and interfaces

### `cockpit` (new, in `main.rs` + a small module)

- `run_cockpit(env) -> i32`: the supervisor loop above. Replaces `run_home`'s
  role as the `xmux`-no-subcommand entry. Runs the attach child via an async
  spawn so the cockpit can serve its socket concurrently with the child's run.
- Initial target: when launched with none, runs the picker (`run_switcher`) to
  get the first target; quitting the picker exits the cockpit.
- The attach child is built by the existing `Source::attach_command` (local
  `attach`, or remote `ssh -t … ; exec attach` with window pre-selection folded
  in). Terminal handover is by inherited stdio, as today.

### cockpit control socket (reuses `control.rs` wire protocol)

- Path: `~/.xmux/cockpit-<cockpit_pid>.sock`, recorded in a single well-known
  pointer file `~/.xmux/cockpit` containing the live socket path. The cockpit
  removes both on exit.
- A new dispatch (distinct from the switcher's key/dump dispatch) handling:
  - `switch <window|-> <source>/<name>` → parse the leading window token (a window
    index, or `-` for none), validate the source is known → store the pending
    target (with window) → reply `ok`; an unknown/invalid target replies `err: …`.
    A bare `switch <source>/<name>` (no leading window token) is also accepted, so
    the address — which may contain spaces — is taken verbatim when the first
    token is not a window spec.
  - `ping` → `pong` (liveness, used by the popup to confirm a live cockpit).
- The pending target is shared with the loop via an `Arc<Mutex<Option<PendingSwitch>>>`
  (target + the `Instant` it was queued) the accept task writes and the loop
  drains after each child exit. The popup receives `ok` before it detaches, so the
  target is stored before the child can exit. A queued switch older than a 15s
  freshness window is discarded on drain, so an abandoned/failed switch cannot
  trigger a stale cross-host teleport on a much-later unrelated child exit.

### `run_popup` (reworked cross-server branch)

- Same-server pick (`chosen.source` is the local source): `switch-client`
  (with `select-window` pre-select) — unchanged, instant.
- Cross-server pick: resolve the cockpit via the `~/.xmux/cockpit` pointer; if a
  live cockpit answers, send `switch <window|-> <addr>` (carrying the picked
  window); on `ok`, `detach-client`. If no live cockpit (no pointer, or the dial
  fails on a stale pointer), print a clear message (cross-host switch needs the
  xmux cockpit; start the terminal with `xmux`) and exit non-zero — never a silent
  loss. A failed dial also removes the stale pointer so the next popup takes the
  honest no-cockpit path at once.

### Cockpit discovery (new, small, in `control.rs` or a `cockpit` module)

- `write_cockpit_pointer(dir, socket_path)` / `read_cockpit_pointer(dir)`:
  manage `~/.xmux/cockpit`. Liveness is proven by connecting to the socket, not
  by a freshness window.

### Retired

- `jump.rs` (the `pending-jump` file + freshness window) and the `take_pending`
  branch in the home loop are removed: the cockpit socket replaces the file
  handoff with a live, addressed, race-free channel.
- `attach::plan_switch`'s cross-server branch (which produced a bare
  `detach-client` for the home loop to interpret) is replaced by the popup's
  explicit cockpit signal; the same-server teleport branch is kept.

## Data flow — cross-server from the cockpit (the keystone)

```
[cockpit] attach local/work ─ child owns terminal ─────────────────┐
   user triggers popup (display-popup -E "xmux popup")              │
[popup] picks remote jupiter06/api                                  │
[popup] read ~/.xmux/cockpit → dial socket → "switch - jupiter06/api" │
[cockpit socket] store pending=jupiter06/api → reply "ok" ──────────┤
[popup] detach-client ──────────────────────────────────────────── child exits
[cockpit] child.wait() returns → take pending → attach jupiter06/api
[cockpit] spawn ssh -t jupiter06 tmux attach ─ terminal becomes the remote session
```

## Error handling and degradation

- Running the cockpit inside a mux is refused (exit 2 with guidance): nested,
  every attach is nest-guard-refused, so the only alternative — warn-and-continue —
  is a doomed picker-flap loop. The cockpit must be the terminal's entry point.
- No cockpit socket / dead socket on a cross-server popup pick → explicit message,
  non-zero exit, and the stale pointer is removed. Predictable, never a silent
  discard.
- Unknown source in `switch … <addr>` → `err`, popup surfaces it, no detach.
- The cockpit cannot render UI between attaches (the picker owns the screen next),
  so attach failures and the socket-bind failure are recorded in
  `~/.xmux/cockpit.log` to survive the picker's screen clears. A non-zero exit of
  the attach child (e.g. ssh 255 for an unreachable remote) is treated as a failed
  switch and logged — not silently swallowed — and the cockpit falls back to the
  picker so the user can re-pick.
- A queued switch is bounded by a 15s freshness window: an abandoned switch
  (popup killed / detach failed) cannot teleport the user on a much-later child
  exit. The cockpit clears pending whenever it falls through to the picker.
- The cockpit pointer is removed on exit only if it still names this cockpit, so a
  sibling cockpit's pointer is not orphaned.

### Accepted limitations (single-cockpit-per-machine model)

- The pointer is a single well-known file: with two cockpits on one machine, a
  popup signals whichever cockpit wrote the pointer last (possible misroute). The
  model assumes one cockpit owns the machine's switching; per-terminal addressing
  is out of scope (the popup's environment does not propagate through
  `display-popup`).
- `ok` means "queued," not "committed": if the cockpit is killed in the sub-second
  window between `ok` and the child exit, the switch is dropped — but the cockpit
  owning the terminal is gone anyway, so there is nothing to switch.
- Concurrent/rapid switches are last-write-wins; the freshness window plus a single
  drain per child exit keep this bounded to the user's most recent pick.

## Testing strategy

- Pure/unit: cockpit pointer round-trip and liveness; `switch <addr>` dispatch
  (valid/invalid/unknown-source); popup decision (same-server → switch-client;
  cross-server + live cockpit → signal+detach; cross-server + no cockpit →
  message). All testable without a terminal.
- Loop: the cockpit supervisor loop driven with a fake child-runner and a fake
  picker — assert that a recorded switch re-attaches with no picker, and that a
  bare exit shows the picker.
- Live (headless via the control channel, throwaway sessions on `jupiter06`):
  run the cockpit in a psmux pane, drive its picker via the existing `xmux ctl`
  channel to attach a local session, then exercise a cross-server switch and
  confirm the cockpit re-attaches the remote target. The final visual
  confirmation of the on-screen handover is the one step that needs a human eye.

## Out of scope (documented future paths)

- **A+ (reverse tunnel):** `ssh -R` forward the cockpit socket into remotes +
  remote xmux, so cross-server *from a remote* is also a single popup action.
- **C (PTY multiplexer):** the cockpit owns the PTY, passes bytes through, and
  intercepts a global hotkey to draw the picker itself — uniform single-action
  switching from anywhere, including deep in a remote, and eliminates the
  inter-client alternate-screen flash. A new ConPTY/openpty subsystem.
- **Windows ssh-connect latency:** Windows OpenSSH lacks ControlMaster, so each
  cross to a remote pays a fresh ssh handshake. The *interaction* is seamless
  (one action, in place, no home tree); the remote *latency* includes ssh
  connect. Reducing it (a warm-connection helper) is a separate optimization.
```
