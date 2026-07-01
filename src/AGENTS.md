# Working Notes: /src

## Purpose

`src/` contains the runtime application: CLI/config assembly, mux discovery,
the cockpit event loop, host metadata management, display attachment spawning,
the control socket, and the UI/proxy/model/backend/state submodules.

## Mental Model

The cockpit is the coordinator. It receives stdin, control socket commands,
host metadata events, display worker events, PTY output events, resize events,
and ticks. It folds domain `Action`s in through `State::apply` and dispatches the
returned `Command`s, keeps `state::State` in sync with the switcher cursor (via
`Action::Select`), drives the debounced attach (via `Action::Tick`), and renders
the live split view.

## Module Seams

- `control.rs` owns ctl wire parsing, framing, endpoint naming, and the ctl
  client. Semantic ctl verbs parse to a domain `model::Action`; `raw:` verbs are
  low-level injection.
- `display.rs` owns the off-runtime worker that spawns PTY attachments and
  returns `DisplayEvent`s. It never owns the registry.
- `driver.rs` holds ONLY the mux-agnostic display seam: the `MuxDriver` trait,
  `DriverCtx`, `Target`, the shared `lower_select_window` helper, and the thin
  `driver_for(host)` wrapper (`host.mux.driver()`). The concrete drivers live in
  their mux family (`backend/tmux/driver.rs` = `TmuxDriver`,
  `backend/psmux/driver.rs` = `PsmuxDriver`); each backend constructs its OWN via
  `Backend::driver()`, so driver.rs names no concrete mux type and there is no
  `match server_model()`. `MuxDriver::show` carries per-host display orchestration —
  which PTY to use, whether to `switch-client` or reattach — and is the sole site for
  that per-mux decision. `cockpit.rs` calls `driver_for` and then `show`; it branches
  on nothing mux-specific. `TmuxDriver` keeps one PTY per host and moves it with
  `switch-client`; `PsmuxDriver` switches in place via `switch-client -c <tty>`
  when a live client with a captured tty is known, else reattaches. `DriverCtx`
  injects supervisor-owned capabilities (registry, hosts, worker, mgr, env,
  pty_tx, attach_seq, view size) so the driver owns the decision without owning
  the infrastructure. The dependency is one-way: `backend/<mux>/driver.rs` imports
  the seam from `crate::driver`; driver.rs never imports a concrete backend driver.
- `host.rs` owns control-mode reader/writer machinery, poll task management,
  host inventory, and `HostEvent`s. It is a metadata path only. `HostManager::ensure`
  spawns the `-CC` control child with an argv composed across the two orthogonal axes
  — `Transport::control_argv(&Backend::control_argv())` (the mux supplies the control
  payload, the transport wraps it for local `-S`/`ssh -tt`); it never hardcodes a mux
  verb or hand-rolls ssh here.
- `cockpit.rs` coordinates these modules and owns the main event loop. Inbound
  `HostEvent`s route through `State::apply_event` (the event-driven mutation site);
  `handle_host_event` is then a thin executor that runs the returned `EventEffect`s
  (`run_event_effect`) against the host clients, registry, and display worker.
- `source.rs` is thin source config/data: the `Source` fields plus delegating
  accessors (`transport()`, `run_with()`, `list_sessions`, `interactive_attach_command`)
  and the shared pure shell vocab (`remote_command`/`quote`/`is_shell_safe`) that
  `model::Transport` calls. It carries no transport-wrapping implementation of its
  own — the machine axis is solely `model::Transport`. `env.rs` is config assembly
  plumbing for source definitions and command construction.
- `logging.rs` sets up the `tracing` subscriber for the process. `logging::init(xmux_dir)`
  attaches a daily rolling file appender (`tracing_appender`) writing to
  `<xmux_dir>/xmux.log`, wrapped in a non-blocking worker. ANSI codes are
  disabled (`with_ansi(false)`); target strings are emitted (`with_target(true)`);
  span lifetimes are recorded (`FmtSpan::NEW | FmtSpan::CLOSE`). The filter reads
  the `XMUX_LOG` env var and falls back to `xmux=info` when absent or invalid.
  `init` returns a `WorkerGuard`; `cli.rs::run()` binds the guard for the process
  lifetime so the background writer stays alive.


## Invariants

- `State::apply(Action) -> Vec<Command>` is the single intent-driven mutation site;
  `dispatch_action` runs its synchronous commands for key- and ctl-derived
  actions, and the loop-top `Tick` runs the settled-attach commands. The two
  surfaces (keys, ctl) can never diverge because both flow through `apply`.
  `State::apply_event(HostEvent) -> Vec<EventEffect>` is the matching event-driven
  mutation site for inbound backend events.
- Every batch of `Command`s a switcher key produces (`handle_key`) routes through
  the single `dispatch_commands` dispatcher — never a `RunOp`-only filter — so no
  future non-`RunOp` command from a key is silently dropped.
- `Selection` is the canonical selected source/session/window value consumed by
  display selection and rendering.
- The per-mux display decision lives in the `MuxDriver` implementation.
  `cockpit.rs` does not branch on mux kind for display; it calls
  `driver_for(host).show(sel, ctx)` and reads back the grid via `driver.grid`.
- `DisplayWorker` spawns attachments and hands them back; `AttachRegistry`
  stores and tears them down.
- Host metadata events update inventory and selection aids, not display grids.

## Common Pitfalls

- Do not make ctl public verbs depend on internal key names.
- Do not block the cockpit loop on process spawn, PTY close, pipe reads, writes,
  or resize operations.
- Do not treat `Source` as the preferred place for new execution semantics;
  prefer `model::Transport` for machine execution, `backend::Backend` for
  mux vocabulary and classification (attach argv, server model, enumeration),
  and the per-mux `MuxDriver` impls (`backend/tmux/driver.rs`,
  `backend/psmux/driver.rs`) for per-host display orchestration and the concrete
  switch/reattach decision.

- All structured log output goes to `<xmux_dir>/xmux.log` (the non-blocking file
  sink). Logging macros (`tracing::info!`, `tracing::debug!`, etc.) must never
  write to stdout or stderr: ratatui owns the terminal in alt-screen mode, and
  a stray byte to stdout or stderr corrupts the display.
- The panic hook in `cockpit.rs` restores the terminal before printing the panic
  message. This is what makes a runtime panic appear on the real screen rather
  than garbling the alt-screen.
- `display_show`, `attach_created`, `tty_probe`, `display_inventory` (emitted by
  the per-mux drivers in `backend/{tmux,psmux}/driver.rs`) and `display_grid_changed`
  (emitted by `cockpit.rs`) are the
  diagnostic surface for whether a session switch actually landed. The first
  grid change after the displayed session changes is INFO; steady-state repaints
  of the same session (htop, build logs, clocks) are TRACE. A `display_show
  decision=switch` not followed by an INFO `display_grid_changed` means the
  switch did not change the screen.

## Before Editing

- For ctl changes, add a `model::Action` variant (and its `Command`/`apply` arm)
  only when the behavior is a real domain action.
- For cockpit changes, locate the event source and the state it owns before
  adding fields or channels.
- For host/display changes, decide whether the behavior belongs to metadata,
  display PTY, or transport lowering.

## Verification

- Run module tests for `control`, `host`, `display`, `cockpit`, and any touched
  submodule.
- Exercise ctl parser tests when adding or renaming control verbs.
- Check redraw and blocking behavior when moving work into the cockpit loop.
- Set `XMUX_LOG=xmux::backend=debug` to emit `display_show`, `tty_probe`, and
  `display_inventory` events at debug verbosity; useful for tracing whether a
  session-switch request reaches the driver and which decision branch it takes.
  The log file is at `<xmux_dir>/xmux.log` (daily-rolling suffix).
