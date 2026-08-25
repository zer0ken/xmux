# ADR 0005: Source Layout Separates Transport, Link, and Host

## Status

Accepted

## Context

The source root accumulated loose files, and two directory names sat against the
vocabulary. `src/machine/` held the execution axis and `src/host/` held per-source
connection management, while the domain type `Host` (a machine that hosts muxes)
lived in `src/model/`. A contributor reading the tree could not tell the execution
axis from connection management from the domain concept, because "machine" and
"host" overlap in ordinary English. ADR 0004 recorded the vocabulary but deferred
the directory rename: "Renaming a directory is a code change and is not decided
here."

The root also held files that belonged to the existing seams: the source
definition, the attach handover, the UI preferences, the control socket, the mux
operations xmux issues, and the provisioning trio (config, roster, discovery,
env) all sat beside the foundational session data and the entry layers.

## Decision

Reorganize `src/` so the three "host-ish" concerns are distinct in the layout and
each file lives in the seam that owns its behavior.

- `machine/` is renamed to `transport/`: the TRANSPORT axis, matching the
  `Transport` trait and the axis name the vocabulary already used. "machine" as a
  module name is gone.
- `host/` is renamed to `link/`: per-source connection management, the mux
  operations xmux issues, and the control-socket protocol.
- The domain type `Host` stays in `src/model/host.rs`.
- `cli.rs` becomes `cli/`, gaining the `update` subcommand.
- `config`/`roster`/`discovery`/`env` move to a new `provision/` directory: the
  resolution of what exists on this box.
- `source` moves to `model/`; `attach` moves to `display/`; `prefs` moves to
  `ui/`; `control` and `manage` move to `link/`.
- `session.rs` (the foundational data the axes and model build on), `driver.rs`
  (the documented display seam), and `logging.rs` (infrastructure) stay at the
  root.

## Consequences

- The three concepts are now distinct in the layout: the execution axis is
  `src/transport/`, connection management is `src/link/`, and the domain type is
  `src/model/host.rs`.
- The root holds only foundational and entry files: `main`, `lib`, `session`,
  `driver`, and `logging`.
- Adding a concern now has a named home: provisioning goes in `provision/`, the
  command surface in `cli/`, and a host-facing operation in `link/`.
- Directory-level working notes and `CONTEXT.md` were updated to match.
