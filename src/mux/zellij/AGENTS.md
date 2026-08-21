# Working Notes: /src/mux/zellij

## Purpose

`mux/zellij` is the zellij family: everything mux-specific to zellij lives here so no
zellij code sits at the `src` root. It owns BOTH sides of the mux:

- the metadata mux `Zellij` (`Mux` impl) - binary name, `ServerModel::PerSession`,
  `list-sessions` enumeration, attach argv, poll cadence, death signal, and the command
  plans, none of which are tmux-compatible;
- the display driver `ZellijDriver` (`MuxDriver` impl, in `display.rs`) - the per-host
  display orchestration for a per-session mux with no in-place switch;
- the output shapes (`parse.rs`) - the session listing (a human line) and the tab
  listing (JSON), as pure functions.

`Zellij::driver()` constructs `ZellijDriver`, so zellij selection lives in this family
and never in a central `match server_model()`. zellij has no `-CC` control stream; it is
polled.

## Mental Model

zellij is a PER-SESSION mux, and it is the one mux family that shares NO argv with
tmux: every command plan is overridden rather than inherited, and both of its outputs
are parsed here rather than by `mux::vocab`.

Its CLI is one process per query, and every query is addressed with `--session <name>`
because zellij's actions otherwise target the session the caller is inside, which xmux
never is. Vocabulary maps as: a zellij TAB is what xmux calls a window, and its
`position` is the window index.

The display driver holds ONE per-host PTY and REATTACHES it on every session change.
There is no in-place switch to make: `switch-session` moves whichever client runs it,
and a client cannot be named from outside its own session, so xmux has no way to aim a
switch at its own display client the way it does for tmux and psmux.

## Module Seams

- `mod.rs` - `Zellij` (`Mux`), the poll cadence constant (`ZELLIJ_POLL_MS`), the
  `--session <name> action <verb>` argv builder, and `now_secs` (the clock the reported
  session age is subtracted from).
- `parse.rs` - `parse_sessions` (the `list-sessions -n` line grammar), `parse_age_secs`
  (the `humantime` duration zellij prints an age in), and `parse_tabs` (the
  `list-tabs -a -j` records). Pure and total: anything that does not fit is skipped.
- `display.rs` - `ZellijDriver` (`MuxDriver`). Re-exported from `mod.rs` as
  `crate::mux::zellij::ZellijDriver`.
- The driver pulls the mux-agnostic seam (`MuxDriver`, `DriverCtx`, `lower_select_window`)
  from `crate::driver`, and the supervisor capabilities (`request_attach`,
  `host_selection_key`, `terminal_view_size`, `display_key`) from `crate::app::runtime`.
  `crate::driver` does NOT import `ZellijDriver`; the dependency is one-way (no cycle).

## Invariants

- Every action argv carries `--session <name>`. An action without it targets the
  caller's own session, and xmux is outside every session.
- `go-to-tab` counts tabs from ONE while xmux and zellij's own `position` count from
  zero. The shift happens in `select_window_plan` and nowhere else.
- The attach is plain `attach <name>`, never `attach -c`: showing a session must not
  create or resurrect one. A session that died between the scan and the attach fails the
  attach, which is the EOF the death signal is waiting for.
- A session listed as `EXITED` is a resurrectable record, not a session, and is dropped
  during enumeration.
- `last_attached` carries the session's CREATION instant. zellij reports no attach time,
  and the nav's recency sort needs a value on the same epoch scale tmux reports.
- On a reattach the stale attachment is HELD (not removed) so its grid stays on screen
  until DisplayReady swaps in the fresh one (stale-while-revalidate).
- `sync` never pre-warms (attaches are selected on demand by `show`); it only reaps the
  host PTY when the host has no sessions left.

## Common Pitfalls

- Do not name `ZellijDriver` outside `crate::mux::**`; the supervisor selects it via
  `Mux::driver()` (through `driver_for`), never a `match server_model()`.
- Do not inherit a tmux-compatible command plan by leaving a verb unoverridden. zellij
  refuses tmux's flags outright (`list-sessions -F` is an unexpected-argument error), so
  a missed override surfaces as an unreachable host, not a degraded one.
- Do not parse the session listing by splitting on whitespace. zellij forbids only `/`
  in a session name, so a name may hold spaces; the split is on the ` [Created ` marker.
- Do not read the window list from `list-panes`. It marks a focused pane per tab AND per
  layer, so it cannot name the one active tab; `list-tabs` is the query that reports tab
  activeness.
- Do not treat a non-zero `list-sessions` exit as an unreachable host. An idle zellij
  writes `No active zellij sessions found.` to stderr and exits 1.

## Before Editing

- Decide whether the behavior is zellij mux vocabulary (`Zellij`), an output shape
  (`parse.rs`), or display orchestration (`ZellijDriver`).
- Verify a new argv against a live zellij before shipping it. zellij's flags move between
  versions and several of its subcommands accept a flag that changes only the TABLE
  columns while `-j` always dumps every field.
- Check tmux and psmux for parity when changing the shared `MuxDriver`/`Mux` trait shape.

## Verification

- Run the family's tests (`cargo test --lib mux::zellij`) for plan, parser, and driver
  changes.
- Run app/host tests when the event source, death signal, or display decision changes.
- Set `XMUX_LOG=xmux::mux::zellij=debug` to trace the driver's `display_show` /
  `display_inventory` decisions.
- A live check needs a real zellij host: create a detached session with
  `zellij attach -b <name>` and let xmux attach to it. Do NOT create tabs from outside a
  clientless session first - zellij 0.45.0 then panics its server on the next
  `AddClient` (`failed to attach client N to tab with index M`), which looks like an
  xmux attach failure and is not one.
