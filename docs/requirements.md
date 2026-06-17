# xmux — functional requirements & use cases

xmux is a stateless cross-environment session switcher: one terminal that sees and
moves between every reachable tmux/psmux session — local and over ssh — regardless
of OS or mux kind. Its reason to exist is to deliver tmux's `prefix + s`
(choose-tree / switch-client) experience **across hosts**: instant, in-place
switching to any host's session.

Each requirement has a stable ID and a **Tests** line naming the covering tests
(module path omitted; all live in that area's `#[cfg(test)]`). `GAP→added` marks a
requirement whose coverage this round adds.

---

## A. Discovery & inventory

- **FR-A1** — `xmux ls` lists every reachable session across all sources as
  `<source>/<name>` lines. **Tests:** `ls_lines_reachable_and_unreachable`.
- **FR-A2** — A reachable mux with zero sessions is reported as empty, not failed;
  a dead host is reported unreachable; "every source unreachable" is distinguished.
  **Tests:** `ls_lines_all_unreachable`, `ls_lines_reachable_empty_is_not_all_unreachable`,
  `is_no_sessions_classification`, `list_sessions_benign_no_server_is_empty_not_error`,
  `list_sessions_unreachable_is_error`.
- **FR-A3** — `xmux doctor` reports config health, ssh availability, and per-source
  reachability with session counts. **Tests:** per-source probe via
  `list_sessions_*`; the doctor print wiring is in `main.rs` (not unit-tested).
- **FR-A4** — Sessions are ordered by recency (most-recently-attached first).
  **Tests:** `to_groups_sorts_sessions_by_recency`, `sort_by_recency` (tree).

## B. The picker — "see the list, decide whether & where to move"

- **FR-B1** — The picker renders a `Hosts · Sessions · Windows · Panes` tree of all
  reachable sessions, expandable to per-window panes with the running command.
  **Tests:** `parse_panes_*` (data), switcher render tests (`dump_*`, `tree_*`).
- **FR-B2** — Render-first: the host skeleton paints instantly; each source's
  sessions and each session's panes stream in independently.
  **Tests:** `event_loop_kicks_probes_on_start`, `event_loop_streams_source_result`,
  `apply_source_result_turns_scanning_into_sessions`, `apply_panes_*`.
- **FR-B3** — A live preview of the focused session (its active pane, captured) is
  shown and follows the cursor; revisits keep cached content; first visits show a
  loading state. **Tests:** `preview_shows_loading_until_fetched`,
  `preview_reconnecting_on_revisit`, `apply_capture_*`.
- **FR-B4** — Navigation: up/down/home/end/pgup/pgdn; fuzzy filter over
  `<source>/<name>`; manual `r` rescan. **Tests:** `filter_narrows`,
  `move_selection_*`, `request_rescan_*`.
- **FR-B5** — Deciding **not** to move is first-class: `Esc`/`q` cancels with no
  attach and no mutation. **Tests:** `quit_leaves_no_choice`, `control_end_to_end`
  (q leaves no choice). `GAP→added`: `event_loop_cancel_leaves_no_choice`.
- **FR-B6** — Under a filter, `Enter` attaches the **visible (filtered)** session —
  never a filtered-out one — even when a host row is selected and even when a
  rescan or pane fetch streams in between the filter and the Enter.
  **Tests:** `host_enter_under_filter_picks_visible_session`,
  `filter_then_enter_picks_visible_not_attached_recent`,
  `filter_then_enter_picks_visible_with_panes_streaming`.
- **FR-B7** — Per-element state hints: `scanning…`, `loading…`, `(empty)`,
  `⚠ unreachable: <reason>`. **Tests:** `host_hint_*`, `apply_source_result_empty_shows_empty_hint`,
  `apply_source_result_unreachable_shows_reason`.
- **FR-B8** — A preview target whose active pane runs xmux is suppressed (no
  recursive self-mirror). **Tests:** `preview_self_*`, `focused_runs_xmux_*`.

## C. Switching (the keystone)

- **FR-C1** — A same-server pick switches in place via `switch-client` (instant),
  pre-selecting the chosen window. **Tests:** `popup_decision_table` (SwitchClient),
  `switch_client_argv`, `select_window_argv`.
- **FR-C2** — A cross-server pick from the local cockpit signals the cockpit over
  its socket (`switch <window|-> <addr>`) then detaches; the cockpit re-attaches the
  target with **no picker between**. **Tests:** `popup_decision_table` (SignalCockpit),
  `signal_cockpit_switch_acks_and_sets_pending`, `cockpit_socket_switch_sets_pending`,
  `loop_reattaches_on_pending_then_picks_on_bare_exit`,
  `dispatch_switch_ping_and_errors`. **Live-verified** (real psmux + ssh).
- **FR-C3** — A cross-server pick from inside a remote (no reachable cockpit
  socket) degrades to a clear message + native detach→picker — never a silent loss.
  **Tests:** `popup_decision_table` (NoCockpit); the message/exit is in `main.rs`.
- **FR-C4** — A switch lands on the picked window for both same-server and
  cross-host paths. **Tests:** `dispatch_switch_carries_window`,
  `attach_command_remote_folds_window_into_one_connection`.
- **FR-C5** — No silent loss: no live cockpit → clear message + non-zero exit + stale
  pointer cleared; a failed attach (e.g. ssh 255) is logged to `~/.xmux/cockpit.log`,
  not swallowed. **Tests:** `popup_decision_table`, `pointer_removed_only_if_ours`;
  attach-failure logging is in `RealAttacher` (live-verified). The switch is
  direction-agnostic — `GAP→added`: `loop_switches_local_remote_both_directions`.

## D. Cockpit lifecycle

- **FR-D1** — `xmux` (no subcommand) is a persistent supervisor that owns the
  terminal and runs one mux-client child at a time. **Tests:** `cockpit_loop` tests.
- **FR-D2** — The cockpit serves its control socket concurrently while blocked on
  the attach child (async child). **Tests:** `cockpit_socket_switch_sets_pending`
  (socket served independent of the loop). **Live-verified** (ping→pong while attached).
- **FR-D3** — Running the cockpit inside a mux is refused (exit 2 with guidance),
  not warned — nested, every attach is refused, leaving a doomed picker loop.
  **Tests:** `nest_guard_inside`, `nest_guard_outside` (the decision primitive);
  `run_cockpit` wiring is in `main.rs`. **Live-verified** (exit 2).
- **FR-D4** — Pointer hygiene: the pointer is written on start, removed on exit only
  if it still names this cockpit; a queued switch older than a freshness window
  (15s) is discarded so an abandoned switch cannot teleport later.
  **Tests:** `pointer_round_trip_and_absent`, `pointer_removed_only_if_ours`,
  `switch_freshness_window`, `loop_discards_stale_pending`.
- **FR-D5** — First launch with no target shows the picker; quitting the picker
  exits the cockpit. **Tests:** `loop_runs_picker_first_when_no_initial_target`.

## E. Session management

- **FR-E1** — Create a session on any source (`n`), then it appears in the tree.
  **Tests:** `create_*`, `new_session_*` (mux), `create_on_unreachable_host_refused`.
- **FR-E2** — Kill a session (`x`) behind an inline confirmation. **Tests:**
  `kill_*`, `arm_kill_*`, `resolve_kill_*`.
- **FR-E3** — Rename a session (`R`); a leading-dash name is refused.
  **Tests:** `rename_*`, `rename_rejects_leading_dash`.
- **FR-E4** — Create/kill/rename run off the key path so a slow ssh round-trip never
  freezes rendering or the control channel. **Tests:** `take_pending_op_*`,
  `*deferred*`, `event_loop` op handling.

## F. Control channel

- **FR-F1** — A per-pid local socket drives the running switcher headlessly:
  `ping`/`dump`/`key <name>`/`text <chars>`. **Tests:** `control_end_to_end`,
  `parse_key_*`, `parse_request_cases`, `frame_round_trip`.
- **FR-F2** — The cockpit socket speaks `switch <window|-> <addr>` and `ping`.
  **Tests:** `dispatch_switch_ping_and_errors`, `dispatch_switch_carries_window`.
- **FR-F3** — Socket discovery: newest `ctl-*.sock` by mtime then pid; the cockpit
  pointer names the live cockpit socket. **Tests:** `discover_newest_then_higher_pid`,
  `discover_tie_break_higher_pid`, `read_cockpit_pointer`.
- **FR-F4** — Length-framed messages with a bounded read; endpoint naming works for
  both `ctl-*` and `cockpit-*` on every platform. **Tests:** `read_frame_oversized`,
  `frame_round_trip`, `endpoint_name_accepts_cockpit_socket`, `socket_path_format`.

## G. Transport & safety

- **FR-G1** — ssh uses a connect-timeout; listing uses `BatchMode` (never hangs on a
  prompt); attach requests a tty. **Tests:** `ssh_args_non_interactive`,
  `ssh_args_interactive_requests_tty`, `ssh_args_windows_omits_control_master`.
- **FR-G2** — A session name from a remote list is injection-safe when it re-enters
  a remote shell command (POSIX single-quote escaping). **Tests:**
  `quote_neutralizes_shell_metachars`, `remote_command_joins_quoted`.
- **FR-G3** — Mux session env (`TMUX`/`PSMUX*`) is stripped for listing so a command
  run from inside a mux is not refused as nesting; lookalikes survive. **Tests:**
  `is_mux_var_is_precise`, `mux_clean_env_*`.
- **FR-G4** — A remote attach folds the window pre-selection into the single
  `ssh -t` connection (no second connection to hang or lose). **Tests:**
  `attach_command_remote_folds_window_into_one_connection`,
  `attach_command_remote_without_window`, `attach_command_local_ignores_window`.

---

## Use cases (end-to-end scenarios)

- **UC-1 — Jump from my laptop to a remote dev session.** Inside the local mux, pop
  up the list, pick a remote session, land in it in one action. *(FR-B1, FR-C2,
  FR-D1/D2)* — Tests: FR-C2 set; **live-verified**.
- **UC-2 — Hop between two same-server sessions.** Pick a session on the current
  server → instant switch-client. *(FR-C1)*
- **UC-3 — Popped up the list, decided to stay.** Survey, then cancel; current
  session untouched. *(FR-B5)* — Test: `event_loop_cancel_leaves_no_choice` (added).
- **UC-4 — Find one session among many, then go.** Filter to narrow, Enter on the
  match. *(FR-B4, FR-B6)* — Tests: the FR-B6 set.
- **UC-5 — The remote is down — don't leave me in the dark.** Unreachable host shows
  `⚠ unreachable`; a failed attach is logged, the cockpit returns to the picker.
  *(FR-A2, FR-B7, FR-C5)* — **live-verified** (cockpit.log entry).
- **UC-6 — Deep in a remote, get back home.** Native detach (`prefix d`) → the
  cockpit's picker → pick local or another host. *(FR-C3, FR-D5)*
- **UC-7 — Spin up a throwaway on a remote and switch to it.** Create on a source,
  then switch to it. *(FR-E1, FR-C2)*
- **UC-8 — Survey what's running everywhere before deciding.** Tree shows hosts,
  sessions, windows, per-pane commands, plus a live preview. *(FR-B1, FR-B3, FR-B8)*
- **UC-9 — Rename / kill a session from the switcher.** *(FR-E2, FR-E3)*
- **UC-10 — Drive xmux from a script.** Control channel: dump, inject keys, signal a
  switch. *(FR-F1, FR-F2)* — Tests: `control_end_to_end`, the cockpit socket set.
- **UC-11 — Switch in either direction, local↔remote↔local.** The cockpit re-attaches
  whatever the next target is, local or remote, with no picker between, in any order.
  *(FR-C5, FR-D1)* — Test: `loop_switches_local_remote_both_directions` (added).

## Coverage added this round

- `loop_switches_local_remote_both_directions` (cockpit) — UC-11 / FR-C5: a
  fake-attacher loop attaches `local → remote → local → remote` via queued
  switches, asserting both directions re-attach with no picker between.
- `event_loop_cancel_leaves_no_choice` (ui/run) — UC-3 / FR-B5: quitting the live
  event loop leaves no chosen session.
- `event_loop_filter_then_enter_attaches_visible` (ui/run) — UC-4 / FR-B6: driving
  the live event loop with `/`, a filter, and Enter attaches the filtered session,
  end-to-end through the loop (not just the Switcher in isolation).

## Out of scope (documented elsewhere)

- The seamless-cross-host-switch design, accepted limitations (single-cockpit,
  inter-client flash, Windows ssh latency), and future escalations (reverse-tunnel,
  PTY-multiplexer): `docs/superpowers/specs/2026-06-17-seamless-cross-host-switch-design.md`
  and `docs/solutions/architecture-patterns/cockpit-cross-host-switch.md`.
