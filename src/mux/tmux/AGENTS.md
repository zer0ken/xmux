# Working Notes: /src/mux/tmux

## Purpose

`mux/tmux` is the tmux implementation, the one shared-server mux: one aggregate server
holds every session and serves a `-CC` control stream. The display driver keeps one
client per session, like every other mux. A remote attach records its OWN controlling
tty to a file unique to that process run and attachment before exec, so a control
notice about a client is matched to xmux's own attachment and never to the user's own
attached client. A LOCAL attach has no shell to run the record; its tty is the PTY xmux
opened for it.

## Module Seams

- The implementation root holds the mux itself, the per-attach display-tty file helpers
  (the path and the implementation-private record prefix), the control argv, and the
  control-protocol implementation.
- The driver sits beside it with the tmux-only attach helper that wraps the tty record.
- The control-mode wire module holds the pure, headlessly testable line
  classification, the notification-to-event table, and the command-line builders.

## Invariants

- Each session's client is keyed by its session. When tmux moves one of xmux's clients
  to another session (`%client-session-changed`), the client is re-keyed to that
  session rather than moved back.
- Inventory sync ends only the clients of sessions it no longer lists; selection alone
  opens a display PTY.
- The `-CC` metadata client sets `ignore-size` and never sends a client size, so it
  cannot shrink the session it lands in, which is often the one xmux itself runs in.
- tmux counts the metadata client on the session it is attached to, so the session
  `%session-changed` names is reported for the app to leave that client out.
- Display sizing changes only the local PTY dimensions. It never changes mux
  options or display-client sizing flags.

## Common Pitfalls

- Do not fold the display-tty record prefix into a LOCAL attach; there is no shell to
  run it.
- Do not read a file back over the control connection with `run-shell`; its output
  lands after the reply block closes. Stage the file in a named buffer and read it with
  `show-buffer`.
- Do not decide inside the mux whether an attach runs through the machine's shell; the driver
  reads that from the transport.
