# Working Notes: /src/mux/zellij

## Purpose

`mux/zellij` is the zellij implementation, the one mux that shares NO argv or output
shape with tmux: every command plan is overridden and its listing is parsed here. Its
CLI is one process per query, and every query is addressed with `--session <name>`,
because zellij's actions otherwise target the session the caller is inside, which xmux
never is. A zellij tab's position is the window index. zellij moves a client between
sessions inside the client process with `switch-session`, so the display reattaches on
every session change, and `ZELLIJ_SESSION_NAME` in xmux's own client is the only record
of a user's move, readable only for a client on THIS machine.

## Module Seams

- The implementation root holds the mux itself and the
  `--session <name> action <verb>` argv builder.
- The parsing module holds the session-line grammar, pure and total: anything that
  does not fit is skipped.
- The driver sits beside them.

## Invariants

- Every action argv carries `--session <name>`.
- The attach is plain `attach <name>`, never `attach -c`: showing a session must not
  create or resurrect one.
- A session listed as exited is a resurrectable record, not a session, and is dropped
  during enumeration.
- An attachment already recorded as showing the selected session is left alone.

## Common Pitfalls

- Do not parse a window or tab list: a session's cards name the session alone, and the
  display mirrors whatever tab the attached client lands on.
- Do not assume an action always answers. On WINDOWS an action addressed at a stale
  session never returns; the per-command poll budget bounds it. Verify zellij behavior
  on Linux, where the same queries answer immediately.

## Before Editing

- Verify a new argv against a live zellij before shipping it. zellij's flags move
  between versions, and several subcommands accept a flag that changes only the TABLE
  columns while the JSON flag always dumps every field.

## Verification

- A live check of the client-switch follow needs a real LOCAL zellij on Windows: attach
  xmux's terminal view to one session, run `zellij action switch-session <other>`
  inside it, and confirm the nav selection lands on the other session's card while the
  same client stays on screen.
- A live check needs a real zellij host: create a detached session with
  `zellij attach -b <name>` and let xmux attach to it. Do NOT create tabs from outside a
  clientless session first: zellij 0.45.0 then panics its server on the next client
  attach, which looks like an xmux attach failure and is not one.
