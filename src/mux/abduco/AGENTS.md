# Working Notes: /src/mux/abduco

## Purpose

`mux/abduco` is the abduco implementation, the simplest mux xmux drives. Each session
is its own server process owning its own unix socket under `~/.abduco`, with no windows
(a session is one PTY running one command), no control-mode channel, no server-socket
flag, and no per-session query. One listing is the whole answer, and the display
reattaches with `abduco -a <name>` on every session change.

## Module Seams

- The implementation root holds the mux itself and the listing parser; the bare binary
  is the listing.
- The driver sits beside it and owns the per-host display orchestration.
- Identity detection is this implementation's own: one `-v` question, answered by the
  name in its output.

## Invariants

- A per-session attach uses `abduco -a <name>`, which reaches that session's own
  server. The display attach adds `-l`, so it sizes the session only while no other
  client is attached.
- A session resolves as the session alone, one card per session, never with a
  per-session command that cannot exist.
- abduco cannot move an attached client, so there is no session change to follow.

## Common Pitfalls

- Do not invent a per-session query; a bogus command would run on every enumeration
  and fail.
- Do not use `-V` anywhere; abduco rejects it and its version flag is `-v`.
- Do not rely on dvtm: abduco's default session command is the user's tool inside the
  session, and xmux only creates the session.
