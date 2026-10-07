# Working Notes: /src/mux/screen

## Purpose

`mux/screen` is the GNU screen implementation: one daemon per session under a per-user
socket directory, no control-mode channel, every query a separate `screen` process, and
a display that reattaches on every session change because screen has no in-place
client switch and no session switch from inside a session.

## Module Seams

- The implementation root holds the mux itself.
- The argv module holds the screen-native builders and the `-ls` parser; screen shares
  no argv with tmux.
- The driver sits beside them.

## Invariants

- `screen -ls` exiting 1 with `No Sockets found` is an empty-but-reachable mux, never a
  dead machine.
- The attach is `screen -x <name>`, so xmux adds its display client whether the session
  is detached or attached elsewhere. screen has no way to keep one display from sizing a
  window, so a window the view showed first keeps the view's size on the user's display.
- screen's `-S` names a session, so a server socket never reaches it.
- Identity is one `-v` question; `-V` errors.
