# Working Notes: /src/mux/zellij

## Purpose

`mux/zellij` is the zellij implementation, the one mux that shares NO argv or output
shape with tmux: every command plan is overridden and its listing is parsed here. Its
CLI is one process per query. Enumeration runs `list-sessions -n`, then
`--session <name> action list-tabs --json` for each live session to count its windows
without a display attachment. JSON counts tabs independently of their names. zellij
moves a client between sessions inside the client process with `switch-session`, so
the display reattaches on every session change and no zellij command reports where a
client went. On Windows xmux reads `ZELLIJ_SESSION_NAME` out of its own client. On
Linux it asks the kernel, once a second over the machine's open path, which session's
server socket is the peer of its client's sockets (`ss -xn`). The client is named by
process id, which an attach run through the machine's shell records before `exec`.

## Module Seams

- The implementation root holds the mux itself and its command plans.
- The parsing module holds the session-line grammar, pure and total: anything that
  does not fit is skipped.
- The driver sits beside them.

## Invariants

- The attach is plain `attach <name>`, never `attach -c`: showing a session must not
  create one.
- A session listed as exited is a stopped session: it is asked for no tab count, and
  only an execution of its card attaches, which resurrects it.
- An attachment already recorded as showing the selected session is left alone.
- A query answer counts only for the attachment it asked about, and never mid-reattach.

## Common Pitfalls

- Enumeration costs one listing plus one sequential query per live session, unlike
  muxes whose session listing includes counts. Zellij 0.45.0 offers no aggregate tab
  listing, so xmux accepts that poll cost for accurate counts. The shared connection
  is reused, and all queries share the seven-second sweep budget and six-second
  command limit. A query that answers no tab list leaves that session listed with no
  count for the sweep. zellij 0.45 loses a CLI client's reply when its server reuses a
  client id a probe has just released (zellij-org/zellij#5270), so a lost reply is
  routine under load, and the listing has already proved the session live.
- Tab counts do not select a tab; the display mirrors the attached client's tab.
- `/proc/<pid>/environ` holds the environment a process started with, never the
  client's rewritten session variable, and `list-clients` ties no client to a process.
- Do not assume an action always answers. On WINDOWS an action addressed at a stale
  session never returns; the per-command poll budget bounds it. Verify zellij behavior
  on Linux, where the same queries answer immediately.

## Before Editing

- Verify a new argv against a live zellij before shipping it. zellij's flags move
  between versions, and several subcommands accept a flag that changes only the TABLE
  columns while the JSON flag always dumps every field.

## Verification

- Check the client-switch follow live: `zellij action switch-session <other>` inside
  the terminal view moves the nav and keeps the same client on screen.
- A live check needs a real zellij host: create a detached session with
  `zellij attach -b <name>` and let xmux attach to it. Do NOT create tabs from outside a
  clientless session first: zellij 0.45.0 then panics its server on the next client
  attach, which looks like an xmux attach failure and is not one.
