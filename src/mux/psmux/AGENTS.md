# Working Notes: /src/mux/psmux

## Purpose

`mux/psmux` is the psmux implementation: one server per session on its own port,
recorded in a per-host registry under the user's home directory. psmux can name no
client from outside its own session, so the display driver REATTACHES its one
per-source PTY with `new-session -A -s <name>` on every session change; a reattach is
addressed by session NAME, so it can only land on xmux's own PTY. psmux also moves its
own client inside the client process, and `PSMUX_SESSION_NAME` in that process's
environment is the only record of where it went, readable only for a client on THIS
machine. A remote psmux source is enumerated and displayed the generic way.

## Module Seams

- The implementation root holds the mux itself.
- The driver sits beside it and owns the per-source display orchestration.
- The session registry backs local enumeration: an existence set merged with one
  detail row from a session listing.
- Identity detection is this implementation's own: one `help` question, read with the
  tmux name dropped because psmux presents itself as a tmux alternative; `-V` mimics
  tmux's version line, so it is never asked.

## Invariants

- A per-session attach uses `new-session -A -s <name>`, never a bare `attach -t` on the
  default socket, which lands on a warm clone with the wrong content.
- A session change ALWAYS reattaches, at any client tty on record and whatever the
  display bookkeeping says. The ONE thing that suspends it is the live client's own
  report that it is already on the selected session.
- A LOCAL psmux source reads the per-host registry; a REMOTE one enumerates over ssh
  and never touches the local registry.

## Common Pitfalls

- Do not reach for a client-addressed command (a switch, a refresh, a detach of one
  client). psmux accepts the client selector, ignores it, and acts on the client its
  own default route reached, with a success exit either way.
- Do not let the reattach guard rest on display bookkeeping; a guard that holds when
  the client is not there leaves the terminal view blank.
- Do not fold the local registry into a REMOTE source: it would inject local session
  names and swallow an ssh failure into a fake empty list.
