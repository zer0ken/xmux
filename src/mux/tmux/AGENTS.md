# Working Notes: /src/mux/tmux

## Purpose

`mux/tmux` is the tmux implementation, the one shared-server mux: one aggregate server
holds every session and serves a `-CC` control stream. The display driver keeps ONE PTY
per host, warmed on the first session and MOVED to another session with
`switch-client`, an in-place move with no teardown. A remote shared attach records its
OWN controlling tty to a file unique to that process run and attachment before exec, so
a later switch targets xmux's own display client and never the user's own attached
client. A LOCAL shared host has no remote shell to record or read the tty, so it
reattaches instead.

## Module Seams

- The implementation root holds the mux itself, the per-attach display-tty file helpers
  (the path, the implementation-private record prefix, and the in-place switch plan
  that reads the recorded tty), the control argv, and the control-protocol
  implementation.
- The driver sits beside it with the tmux-only attach helper that wraps the tty record.
- The control-mode wire module holds the pure, headlessly testable line
  classification, the notification-to-event table, and the command-line builders.

## Invariants

- A shared host keeps ONE PTY, keyed by host id; a session change MOVES it rather
  than tearing it down.
- A remote in-place switch reads the tty the live attach recorded to its own file and
  never runs with an empty client tty.
- Sync warms the host PTY on the first session and reaps it when the host has no
  sessions.
- The `-CC` metadata client sets `ignore-size` and never sends a client size, so it
  cannot shrink the session it lands in, which is often the one xmux itself runs in.

## Common Pitfalls

- Do not fold the display-tty record prefix into a LOCAL attach; there is no shell to
  run it.
- Do not read a file back over the control connection with `run-shell`; its output
  lands after the reply block closes. Stage the file in a named buffer and read it with
  `show-buffer`.
- Do not decide inside the mux whether an attach runs through the machine's shell; the driver
  reads that from the transport.
