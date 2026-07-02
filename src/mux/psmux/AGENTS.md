# Working Notes: /src/mux/psmux

## Purpose

`mux/psmux` is the psmux family: everything mux-specific to psmux lives here so no
psmux code sits at the `src` root. It owns BOTH sides of the mux:

- the metadata mux `Psmux` (`Mux` impl) — binary name,
  `ServerModel::PerSession`, registry-merge enumeration for a LOCAL host (list-sessions
  over ssh for a REMOTE one), attach argv, poll cadence, death signal, and window/session
  operation plans;
- the display driver `PsmuxDriver` (`MuxDriver` impl, in `display.rs`) — the per-host
  display orchestration for a per-session mux.

`Psmux::driver()` constructs `PsmuxDriver`, so psmux selection lives in this family and
never in a central `match server_model()`. psmux has no `-CC` control stream; it is polled.

## Mental Model

psmux is a PER-SESSION mux: one server per session on its own port
(`~/.psmux/<name>.port`), coordinated over the default socket. The display driver holds
ONE per-host PTY and, on a session change, either:

- SWITCHES it in place (`switch-client -c <tty> -t <session>`) when a live client AND its
  captured tty are known — no teardown, so the terminal view never goes blank; followed by a
  `refresh-client` to force a full repaint; or
- REATTACHES (`new-session -A -s <name>`, which routes to that session's OWN server — the
  4a5f053 correctness fix; a bare `attach -t` lands on a warm clone) when there is no live
  client or no captured tty.

The tty is captured off-loop by a read-only `list-clients` probe, correlating the client
by the session it shows (one server per session ⇒ the client showing session S is on S's
own server). A remote psmux host is enumerated/displayed the generic way; the local probe
is skipped there.

The mux supplies mux vocabulary (argv, model, enumeration); the driver consumes it
and owns the concrete switch/reattach decision. `Transport` lowers the machine execution.

## Module Seams

- `mod.rs` — `Psmux` (`Mux`), the poll cadence constant (`PSMUX_POLL_MS`), and
  `switch_in_place` (the exec `SwitchPlan` — `switch-client` + `refresh-client` — the
  driver's in-place switch runs).
- `display.rs` — `PsmuxDriver` (`MuxDriver`) plus the psmux-only helpers
  `parse_psmux_client_tty` and `spawn_local_psmux_tty_capture`.
  Re-exported from `mod.rs` as `crate::mux::psmux::PsmuxDriver`.
- `registry.rs` — the `~/.psmux` per-machine session registry that backs local
  `enumerate` (existence set) and the merge with one list-sessions detail row.
- The driver pulls the mux-agnostic seam (`MuxDriver`, `DriverCtx`, `lower_select_window`)
  from `crate::driver`, and the supervisor capabilities (`request_attach`, `run_lowered`,
  `host_selection_key`, `terminal_view_size`, `display_key`) from `crate::app::app`.
  `crate::driver` does NOT import `PsmuxDriver`; the dependency is one-way (no cycle).

## Invariants

- A per-session attach uses `new-session -A -s <name>` (routes to that session's own
  server), never a bare `attach -t` on the default socket (a warm clone / wrong content).
- An in-place switch runs ONLY with a live client AND a non-empty captured tty; otherwise
  it reattaches, so a box where the tty is never captured behaves exactly as before (the
  4a5f053 guard — no regression).
- On a reattach the stale attachment is HELD (not removed) so its grid stays on screen
  until DisplayReady swaps in the fresh one (stale-while-revalidate).
- `sync` never pre-warms (attaches are selected on demand by `show`); it only reaps the
  host PTY when the host has no sessions left.
- A LOCAL psmux host reads `~/.psmux`; a REMOTE one enumerates via list-sessions over ssh
  and never touches the local registry.

## Common Pitfalls

- Do not name `PsmuxDriver` outside `crate::mux::**`; the supervisor selects it via
  `Mux::driver()` (through `driver_for`), never a `match server_model()`.
- Do not run `switch-client -c ""` — the tty capture is guarded; an empty/absent tty must
  fall back to reattach.
- Do not fold the local registry into a REMOTE host (it would inject local session names
  as phantoms and swallow an ssh failure into a fake empty list).

## Before Editing

- Decide whether the behavior is psmux mux vocabulary (`Psmux`), display orchestration
  (`PsmuxDriver`), or registry enumeration (`registry.rs`).
- Keep the driver's behavior byte-identical unless the change is explicitly a behavior
  change; the switch/reattach decision is the highest-risk surface.
- Check tmux for parity when changing the shared `MuxDriver`/`Mux` trait shape.

## Verification

- Run mux and driver tests (`cargo test --lib mux::psmux`) for plan, registry,
  and driver changes.
- Run app/host tests when the event source, death signal, or display decision changes.
- Set `XMUX_LOG=xmux::mux::psmux=debug` to trace the driver's `display_show` /
  `tty_probe` / `display_inventory` decisions.
