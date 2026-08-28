# Working Notes: src/mux/screen/

screen: one daemon per session under a per-user socket directory, polled, display
reattaches per session.

- `Mux` shape: `PerSession` model (no in-place `switch-client`, so every session change
  reattaches), `EventSource::Poll` (no control-mode channel), `DeathSignal::Eof`.
- No session switch from inside a session, so there is no nav follow: `C-a d` detaches the
  client and detaching is the only move screen offers it, which puts a client on another
  session only by ending it and starting a new one from outside. tmux's
  `%client-session-changed` follow therefore has no screen counterpart to wire, and a nav
  selection that stays where the user put it is the right answer here rather than a missed
  event. The vocabulary this rests on is screen 4.09.00 and 4.9.1, where `select` and
  `other` under `C-a` are WINDOW commands and `sessionname` only renames; 5.x is
  unverified.
- `takes_server_socket` is FALSE: screen's `-S` names a session, not a server socket.
- `enumerate` runs `screen -ls`; exit code 1 (stdout `No Sockets found`) is an
  empty-but-reachable mux, never a dead host.
- `attach_plan` = `screen -x <name>` (multi-display) so xmux adds its display client
  whether the session is detached or attached elsewhere.
- Detection: `screen -v` prints `Screen version … (GNU)`; `-V` errors, so screen is
  never caught by the tmux `-V` fallback.
