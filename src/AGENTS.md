# Working Notes: /src

## Purpose

`src/` is the xmux crate. Its root modules hold what the directories below build on:
the binary shim and the crate entry, the cross-environment session types, the
mux-agnostic display seam, and the process-wide log. The app coordinates the rest: it
receives terminal input, ctl commands, source events, display events, PTY output,
resizes, and ticks, folds each into the application model, and from that drives the
debounced attach and the live split view.

## Module Seams

- `main` - the binary shim: one multi-thread runtime for the scan, then the CLI entry.
- `lib` - the crate root. The CLI's single entry is the only public surface; every
  layer below it is crate-internal.
- `session` - a session, its window and pane detail, and the `<source>/<name>` address
  that both axes and the model build on.
- `driver` - the mux-agnostic display seam: the driver trait each mux builds for itself,
  the supervisor capabilities a driver borrows, the display target, and the read of a
  host's live display client for the session it is on (the mux names the variable, the
  transport says whether a process on that host can be read at all). It names no
  concrete mux, and no central match on server model exists.
- `logging` - the process-wide structured log: daily rolling files under the xmux dir,
  an error-only triage mirror, and a filter read from `XMUX_LOG` that falls back to
  `xmux=info`.

## Before Editing

- Place a new source file by the concern it belongs to:
  - a host implementation, or per-host execution: `transport/`
  - a mux implementation, or per-mux behavior: `mux/<kind>/`
  - PTY, grid, or terminal-protocol mechanics: `display/`
  - orchestration of the runtime loop: `app/`
  - per-source connection management: `link/`
  - domain types: `model/`
  - config, roster, discovery, and the resolved environment: `provision/`
  - the CLI command surface: `cli/`
  - switcher, nav rows, and status UI: `ui/`
  - runtime state, focus, modal data, and chrome data: `state/`
