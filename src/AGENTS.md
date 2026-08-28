# Working Notes: /src

## Purpose

`src/` contains the runtime application: the CLI command surface, provisioning
(config, roster, discovery, and the resolved environment), the app event loop,
per-source connection management, display attachment spawning, the control socket,
and the app / ui / model / mux / transport / state submodules, all built on the
foundational session data types.

## Mental Model

The app is the coordinator. It receives stdin, control socket commands, source
metadata events, display worker events, PTY output events, resize events, and
ticks. Domain actions fold into the runtime state, which answers with commands
the app dispatches; the state stays in sync with the switcher selection, drives
the debounced attach, and renders the live split view.

## Module Seams

- `cli/` is the command surface: parsing and dispatch for the commands and the
  default interactive app, plus the self-update subcommand. `cli::run` is the sole
  entry the binary shim calls.
- `provision/` resolves what exists on this box: the optional TOML config merged
  with ssh-config discovery, the roster of ssh targets, the concurrent source
  probe, and the resolved runtime view over them.
- The control socket (in `link/`) owns ctl wire parsing, framing, endpoint
  naming, and the ctl client. Semantic ctl verbs resolve to domain actions; the
  `raw:` namespace is low-level injection.
- `display/` is the shared PTY / grid / input display-mechanics layer, both
  mux-agnostic and app-agnostic: the terminal handover and attach spawn/lifecycle,
  an off-runtime worker that spawns attachments and returns display events without
  owning the registry, the registry itself, the grid, and the terminal input
  mechanics.
- `app/` holds the application orchestration layer: the persistent supervisor
  and main event loop (which also owns the selection) and the focus/modal
  routing state machine. Focus is UI state, not display mechanics.
- `driver.rs` holds ONLY the mux-agnostic display seam: the `MuxDriver` trait,
  the supervisor capabilities a driver borrows, the display target, the shared
  window-selection helper, the composition that reads a host's live display client
  for the session it is on (the mux names the variable, the transport says whether
  a process on that host can be read at all), and the thin wrapper resolving a
  source's own driver.
  The concrete drivers live in their mux family, and each mux constructs its
  OWN, so this module names no concrete mux type and no central `match` on
  server model exists. Showing a session carries the per-source display
  orchestration (which PTY to use, whether to switch in place or reattach) and
  is the sole site for that per-mux decision. The runtime resolves the driver
  and calls it; it branches on nothing mux-specific. The dependency is one-way:
  a mux family imports the seam, and the seam never imports a concrete driver.
- `link/` owns the live host-facing channels: per-source connection management
  (control-mode reader/writer, poll task management, inventory, and source
  events), the mux operations xmux issues against a live host, and the
  control-socket protocol for headless driving. Spawning the control-mode child
  composes its argv across the two orthogonal axes: the mux supplies the control
  payload and the transport wraps it for local or ssh execution. Nothing here
  hardcodes a mux verb or hand-rolls ssh.
- `transport/` is the TRANSPORT axis: the `Transport` trait, the local/ssh/wsl
  families, and the shared shell vocabulary. A source builds one at construction.
- `mux/` is the MUX axis: the `Mux` trait, the per-mux families owning metadata,
  command plans, and a display driver, and the shared mux vocabulary.
- The runtime coordinates these modules and owns the main event loop. Inbound
  source events route through the event-driven mutation site on the state; the
  handler is then a thin executor running the returned effects against the source
  clients, the registry, and the display worker.
- A source definition (`model/source`) is a thin config adapter: an alias, a mux
  binary, a host kind, an injectable runner, and the assembly of a runtime source
  for the off-loop and CLI paths that cannot borrow the event loop's live one.
  Enumeration, manage lifecycle operations, and interactive-attach argv are NOT
  here: they live on the runtime source, the mux, and the transport, which the
  adapter reaches by building one and injecting its runner. The host axis is
  solely the transport, whose shared shell vocabulary lives beside it; the adapter
  carries no transport-wrapping implementation of its own.
- The resolved environment (`provision/env`) is config assembly plumbing for
  source definitions and command construction, and the ONE place this host's mux
  list is resolved. That
  answer is threaded into the construction of both the source list and the runtime
  registry rather than re-derived, so the two cannot disagree on which sources
  exist, and resolving it
  is why environment construction is async. The environment owns the source LIST
  behind a lock, because async mux discovery adds to it while the app runs; the
  guard is never held across an await. A source added there is paired with an
  insert into the runtime source registry in the same handler: the environment is
  what off-loop operations resolve, the registry is what the loop drives, and a
  source in one but not the other is a card that scans and refuses every
  operation.
- The roster (`provision/roster`) answers only "which HOSTS does xmux offer":
  one or more providers, each yielding plain ssh target names, so nothing
  downstream can tell which provider a name came from. A provider that cannot run
  yields an empty list rather than an error. Distinct from the transport axis
  (how a command REACHES a host) and from discovery (scanning a source for
  sessions).
- Preferences (`ui/prefs`) persist the lightweight UI hints across runs
  (last-selected session address, nav width and height, auto-hide-nav), one small
  file each under the xmux dir. Every value is best-effort: a stale, missing, or
  unparsable file falls back to the built-in default, so xmux stays stateless
  about sessions themselves.
- `session.rs` is the foundational cross-environment data types (a `Session` and
  the `<source>/<name>` address) that the axes and
  the model build on.
- Logging sets up the process-wide structured log: a daily rolling file appender
  writing to `<xmux_dir>/xmux.log` behind a non-blocking worker, with ANSI
  disabled, targets emitted, and span lifetimes recorded. The filter reads the
  `XMUX_LOG` environment variable and falls back to `xmux=info` when absent or
  invalid. The CLI binds the writer's guard for the process lifetime so the
  background writer stays alive.

## Invariants

- The nav's live size travels as one value (the width the user set, the width on screen,
  the portrait band's height), never as two loose numbers: the effective width has a single
  owner, and every geometry - the draw, the PTY sizing, mouse hit-testing - is cut from the
  same value, so a resize while xmux runs cannot reach one consumer and miss another.
- Applying a domain action to the runtime state is the single intent-driven
  mutation site, and applying a source event is the matching event-driven one.
  Keys and ctl can never diverge, because both flow through the same apply.
- Every batch of commands a switcher key produces routes through the single
  command dispatcher, never a filter that keeps only one command kind, so no
  future command from a key is silently dropped.
- The selection is the canonical selected source / session value
  consumed by display selection and rendering.
- The per-mux display decision lives in the driver implementation. The runtime
  does not branch on mux kind for display; it resolves the source's driver, asks
  it to show the selection, and reads the grid back from it.
- The display worker spawns attachments and hands them back; the registry stores
  and tears them down.
- Source metadata events update inventory and selection aids, not display grids.

## Common Pitfalls

- Do not make ctl public verbs depend on internal key names.
- Do not block the app loop on process spawn, PTY close, pipe reads, writes, or
  resize operations.
- Do not treat the source adapter as the preferred place for new execution
  semantics. Host execution belongs to the transport, mux vocabulary and
  classification (attach argv, server model, enumeration) to the mux, and
  per-source display orchestration with its concrete switch-or-reattach decision
  to the per-mux driver.
- All structured log output goes to `<xmux_dir>/xmux.log`. Logging must never
  write to stdout or stderr: the renderer owns the terminal in alt-screen mode,
  and a stray byte corrupts the display.
- The runtime's panic hook restores the terminal before printing the panic
  message. That is what makes a runtime panic appear on the real screen rather
  than garbling the alt-screen.
- The display log events are the diagnostic surface for whether a session switch
  actually landed. The first grid change after the displayed session changes is
  INFO; steady-state repaints of the same session are TRACE. A show decision of
  "switch" not followed by an INFO grid change means the switch did not change
  the screen.

## Before Editing

- For ctl changes, add a domain action only when the behavior is a real domain
  action rather than a key alias.
- For app changes, locate the event source and the state it owns before adding
  fields or channels.
- For connection or display changes, decide whether the behavior belongs to metadata,
  display PTY, or transport lowering.

## Verification

- Exercise the behavior each touched seam is responsible for, both from a key
  and from the ctl verb, since both must agree.
- Check redraw and blocking behavior when moving work into the app loop.
- Set `XMUX_LOG=xmux::mux=debug` to raise the display events to debug
  verbosity; useful for tracing whether a session-switch request reaches the
  driver and which decision branch it takes. The log file is at
  `<xmux_dir>/xmux.log`, with a daily-rolling suffix.
