# Working Notes: /scripts/e2e

## Purpose

The end-to-end suite: real xmux builds driven through a pseudo terminal against Docker
hosts, one table cell per scenario, mux, and host system. It answers two questions, one
table group each: whether xmux's own behavior works, and whether each mux's native
workflow survives inside xmux. The README lists the scenarios, the hosts, and the known
cells.

## Module Seams

- `run.sh` owns the images, the xmux build, and the client container; `suite.py` owns
  the host containers, the scenarios, and the table; `driver.py` owns the terminal and
  reads the screen.
- A scenario reaches the app only through keys and the screen. It may ask a host
  container directly what runs there, never xmux's ctl socket.

## Invariants

- The suite never reads or writes the home of the person running it: each scenario
  runs xmux as a new user, or under a temporary home on Windows, and every Docker
  object it creates carries the `xmux-e2e` prefix.
- Every wait has a timeout and every command a time limit, so a hung app fails its
  cell instead of the run.
- A cell that fails because of an xmux defect, including a native workflow xmux breaks,
  is marked known only with the issue that tracks it, and still runs.
