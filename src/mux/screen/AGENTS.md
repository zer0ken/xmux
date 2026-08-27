# Working Notes: src/mux/screen/

screen: one per-user daemon, polled, display reattaches per session.

- `Mux` shape: `PerSession` model (no in-place `switch-client`, so every session change
  reattaches), `EventSource::Poll` (no control-mode channel), `DeathSignal::Eof`.
- `takes_server_socket` is FALSE: screen's `-S` names a session, not a server socket.
- `enumerate` runs `screen -ls`; exit code 1 (stdout `No Sockets found`) is an
  empty-but-reachable mux, never a dead host.
- `attach_plan` = `screen -x <name>` (multi-display) so xmux adds its display client
  whether the session is detached or attached elsewhere.
- Detection: `screen -v` prints `Screen version … (GNU)`; `-V` errors, so screen is
  never caught by the tmux `-V` fallback.
