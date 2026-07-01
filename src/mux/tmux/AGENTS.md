# Working Notes: /src/mux/tmux

## Purpose

`mux/tmux` is the tmux family: everything mux-specific to tmux lives here so no
tmux code sits at the `src` root. It owns BOTH sides of the mux:

- the metadata backend `Tmux` (`Backend` impl) — binary name, `ServerModel::Shared`,
  aggregate `list-sessions` enumeration, attach argv, the `-CC` control argv, event
  source, death signal, and window/session operation plans;
- the display driver `TmuxDriver` (`MuxDriver` impl, in `display.rs`) — the per-host
  display orchestration for a shared-server mux.

`Tmux::driver()` constructs `TmuxDriver`, so tmux selection lives in this family and
never in a central `match server_model()`. The `-CC` wire protocol lives in
`control_proto.rs` behind `ControlProtocol`.

## Mental Model

tmux is a SHARED-server mux: one aggregate server holds every session. The display
driver keeps ONE PTY per host, warmed on the first session and MOVED to another session
with `switch-client` (an in-place move, no teardown). A remote shared attach records its
OWN controlling tty to a per-host file before exec so a later `switch-client -c <tty>`
targets xmux's own display client, never the user's own attached client. A LOCAL shared
host has no remote shell to record/read the tty, so it reattaches instead.

The backend supplies mux vocabulary (argv, model, enumeration, control payload); the
driver consumes it and owns the concrete attach/switch decision. `Transport` lowers the
machine execution (local `-S` / `ssh -tt`); the tmux family never hardcodes ssh.

## Module Seams

- `mod.rs` — `Tmux` (`Backend`), the per-host display-tty file helpers
  (`display_tty_path`, the record/switch commands via `display_tty_record_prefix` /
  `switch_via_recorded_tty_cmd`), the control argv, and `TmuxControl`.
- `display.rs` — `TmuxDriver` (`MuxDriver`) plus the tmux-only attach helper
  `with_display_tty_record`. Re-exported from `mod.rs` as `crate::mux::tmux::TmuxDriver`.
- `control_proto.rs` — the pure, headlessly-testable `-CC` line classification, the
  notification→event table, and the command-line builders behind `ControlProtocol`.
- The driver pulls the mux-agnostic seam (`MuxDriver`, `DriverCtx`, `lower_select_window`)
  from `crate::driver`, and the supervisor capabilities (`request_attach`, `run_lowered`,
  `host_selection_key`, `terminal_view_size`, `display_key`) from `crate::cockpit`.
  `crate::driver` does NOT import `TmuxDriver`; the dependency is one-way (no cycle).

## Invariants

- A shared host keeps ONE PTY, keyed by host id; a session change MOVES it
  (`switch-client`), it is not torn down.
- A remote in-place switch reads the tty the attach recorded to its per-host file, so it
  moves xmux's own display client and never the user's — never `switch-client -c ""`.
- `sync` warms the host PTY on the first session and reaps it when the host has no
  sessions; a fresh value of `TmuxDriver` per call is fine (it is zero-sized, state lives
  on `host.display`/`AttachRegistry`).
- A reachable empty tmux enumerates as `Ok(vec![])`; unreachable is `Err`.

## Common Pitfalls

- Do not name `TmuxDriver` outside `crate::mux::**`; the supervisor selects it via
  `Backend::driver()` (through `driver_for`), never a `match server_model()`.
- Do not fold the display-tty record prefix into a LOCAL attach (there is no shell to run
  it — it would corrupt the argv's session-name argument).
- Do not thread a `remote` bool through the backend; the driver reads
  `host.transport.is_remote()` and the backend stays transport-blind.

## Before Editing

- Decide whether the behavior is tmux mux vocabulary (`Tmux`), display orchestration
  (`TmuxDriver`), or `-CC` wire protocol (`TmuxControl`).
- Keep the driver's behavior byte-identical unless the change is explicitly a behavior
  change; the display decision is the highest-risk surface.
- Check psmux for parity when changing the shared `MuxDriver`/`Backend` trait shape.

## Verification

- Run backend and driver tests (`cargo test --lib mux::tmux`) for plan, control, and
  driver changes.
- Run cockpit/host tests when the event source, death signal, or display decision changes.
- Set `XMUX_LOG=xmux::mux::tmux=debug` to trace the driver's `display_show` /
  `display_inventory` / `attach_created` decisions.
