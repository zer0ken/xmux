---
title: "Cockpit model — seamless cross-host mux switching via a terminal-owning supervisor"
category: architecture-patterns
module: xmux
problem_type: architecture_pattern
component: tooling
severity: high
date: 2026-06-17
applies_when:
  - "you need tmux/psmux prefix+s switch-client UX but ACROSS mux servers (remote hosts over ssh, or different local sockets)"
  - "an in-mux popup (display-popup -E) must trigger a switch but cannot itself hand over the controlling terminal"
  - "a switch must be a single in-place action, not a detach-to-a-home-screen bounce"
tags:
  - mux
  - tmux
  - psmux
  - ssh
  - terminal-handover
  - cockpit
  - control-socket
  - cross-host
related_components:
  - cockpit
  - main
  - control
  - source
  - attach
---

# Cockpit model — seamless cross-host mux switching via a terminal-owning supervisor

## Context

tmux `switch-client` is instant because the mux *server* repaints the *existing*
client — no process is replaced, no terminal is handed over. This works only
within one server. A session on another server (a remote host, or a different
local socket) has no existing client: reaching it requires standing up a new
client (`tmux attach`, or `ssh -t host tmux attach`) and tearing down the current
one, handing the controlling terminal across.

Only the process that *owns* the controlling terminal can hand it over. A
`display-popup -E "xmux popup"` overlay does **not** own the terminal — the mux
*client* process (what the user launched) does. So cross-server switching cannot
be done from inside the popup; it needs a persistent process outside the mux to
perform the re-attach.

The earlier "detach-to-home" model (popup writes a `pending-jump` file, detaches,
and a home loop re-attaches) failed the goal: it detaches to a home screen
(flash), depends on the user having entered via that home loop (a plain
`tmux attach` breaks it and the pick is silently lost), and feels nothing like
`switch-client`.

## Guidance

Make `xmux` (no subcommand) a **cockpit**: a persistent supervisor that owns the
terminal and runs exactly one mux-client child at a time, while serving a control
socket the in-mux popup can signal.

```
cockpit:
  serve cockpit control socket (~/.xmux/cockpit-<pid>.sock), publish a pointer (~/.xmux/cockpit)
  target = picker.pick()   # first launch shows the full-screen switcher; quit → exit
  loop:
    attach(target)         # local: psmux attach; remote: ssh -t host tmux attach — inherits stdio, owns the terminal
    target = fresh pending switch (no picker)  else  picker.pick()  else exit
```

The attach runs as an **async** child (`tokio::process::Command::status().await`)
so the cockpit can serve its socket *concurrently while blocked on the child* —
this is the load-bearing detail (verified live with `ping → pong` issued while a
remote attach was running).

The three switch paths:
- **Same-server (local or remote):** native `switch-client` / the remote mux's own
  `prefix + s`. Instant, no cockpit involvement.
- **Cross-server from the local cockpit (the keystone):** the local popup reads the
  cockpit pointer, sends `switch <window|-> <source>/<name>`, then detaches its own
  client. The cockpit's child exits, the cockpit drains the pending target and
  re-attaches it **with no picker between** — a single, in-place action.
- **Cross-server from inside a remote:** the remote popup can't reach the local
  cockpit socket (different machine), so it degrades to native detach
  (`prefix d`) → the ssh child exits → cockpit shows the picker → pick. Reliable
  and topology-independent, at one extra keystroke.

Hardening that the happy path does not surface (each found by adversarial review):
- **Refuse to run the cockpit inside a mux** (exit non-zero), don't warn-and-continue:
  nested, every attach is nest-guard-refused, leaving a doomed picker-flap loop.
- **Capture the attach child's exit status.** A non-zero exit (e.g. ssh 255 for an
  unreachable remote) is a *failed switch*, not silence — matching only the spawn
  `Err` arm of `.status().await` drops it. The cockpit can't render UI between
  attaches (the picker owns the screen next), so log failures to `~/.xmux/cockpit.log`
  to survive the picker's screen clears.
- **Bound a queued switch with a freshness window** (~15s). Without it, an abandoned
  switch (popup killed, or its detach failed) sits in the pending cell and silently
  teleports the user on a much-later unrelated child exit.
- **Pointer hygiene:** remove the pointer on exit only if it still names *this*
  cockpit (don't orphan a sibling); clear a stale pointer when a popup's dial fails
  so the next popup takes the honest no-cockpit path at once.
- **Carry the picked window across the wire** (`switch <window|-> <addr>`, window
  first so an address with spaces is taken verbatim), or a cross-host switch silently
  drops the window the user selected while same-server switching keeps it.

## Why This Matters

Cross-host switching is the entire reason xmux exists; if it is not as seamless as
`prefix + s`, the project has no reason to exist. The cockpit is the only model
that delivers an in-place, single-action, topology-independent cross-host switch,
because the terminal-ownership constraint is physical: nothing inside the popup can
hand over the terminal. Everything else (detach-to-home) is a workaround that
leaks the constraint into the UX.

The irreducible costs to be honest about: the inter-client alternate-screen flash
(two separate clients each manage the alt screen — only a PTY-multiplexer cockpit
would remove it), and on Windows the ssh-connect latency per remote attach (Windows
OpenSSH has no ControlMaster). The *interaction* is seamless; the remote *latency*
includes ssh connect.

## When to Apply

When building a cross-environment switcher that must feel like same-server
switching. The cockpit is the floor; two documented escalations exist if the local
vantage is not enough: **reverse-tunnel** the cockpit socket into remotes (single
popup action from a remote too), or a **PTY-multiplexer** cockpit that intercepts a
global hotkey and removes the inter-client flash (a much larger ConPTY/openpty
subsystem).

## Examples

Live-verified headless (real psmux + ssh, throwaway sessions on jupiter06/jupiter00):

```
# cockpit attached to jupiter06/probeR (a remote attach, no nesting)
$ xmux ctl --sock ~/.xmux/cockpit-<pid>.sock ping        # while blocked on the child
pong                                                      # → concurrent serving works
$ printf 'switch 1 jupiter00/xmux-probe\n' | xmux ctl --sock ~/.xmux/cockpit-<pid>.sock
ok
$ ssh jupiter06 'tmux detach-client -s probeR'            # force the child to exit
# drive pane status bar flips to:  jupiter00  xmux-probe  1:1:bash   (no picker between)
```

Failure is no longer silent:

```
$ printf 'switch - jupiter06/does-not-exist\n' | xmux ctl --sock <cockpit-sock>
ok
# after the child exits 1:
$ cat ~/.xmux/cockpit.log
attach jupiter06/does-not-exist failed: child exited exit code: 1
```

### Live-verification gotchas (Windows + psmux)

- A fully-detached psmux pane could not host the cockpit's full-screen picker;
  launch via a `pwsh -NoProfile -File launcher.ps1` that strips `TMUX`/`PSMUX_SESSION`
  (so `nest_guard` treats the pane as a real terminal entry). A bare `bash` in a
  psmux pane resolves to WSL, not git-bash.
- Drive the picker over the control channel via **stdin** (`printf 'key /\n' | xmux ctl …`),
  not args — git-bash MSYS path-conversion mangles a `/` argument into a Windows path.
- The picker quits on Esc/q; to reach an attach, navigate (filter via `/` then Enter
  to apply, Enter again to attach) — never Esc.
