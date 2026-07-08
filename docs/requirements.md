# xmux — functional requirements & use cases

xmux is a stateless cross-environment session switcher: one terminal that sees and
moves between every reachable tmux/psmux session — local and over ssh — regardless
of OS or mux kind. Its reason to exist is to deliver tmux's `prefix + s`
(choose-tree / switch-client) experience **across hosts**: instant, in-place
switching to any host's session.

Each requirement has a stable ID and a **Tests** line naming the covering tests
(module path omitted; all live in that area's `#[cfg(test)]`).

---

## A. Discovery & inventory

- **FR-A1** — `xmux ls` lists every reachable session across all sources as
  `<source>/<name>` lines. **Tests:** `ls_lines_reachable_and_unreachable`.
- **FR-A2** — A reachable mux with zero sessions is reported as empty, not failed;
  a dead host is reported unreachable; "every source unreachable" is distinguished.
  **Tests:** `ls_lines_all_unreachable`, `ls_lines_reachable_empty_is_not_all_unreachable`,
  `is_no_sessions_classification`, `enumerate_with_benign_no_server_is_empty_not_error`,
  `enumerate_with_unreachable_is_error`.
- **FR-A3** — `xmux doctor` reports config health, ssh availability, and per-source
  reachability with session counts. **Tests:** per-source probe via
  `list_sessions_*`; the doctor print wiring is in `cli.rs` (`run_doctor`, not unit-tested).
- **FR-A4** — Sessions are ordered by recency (most-recently-attached first).
  **Tests:** `to_groups_sorts_sessions_by_recency`, `sort_by_recency_orders` (tree).

## B. The switcher — "see the list, decide whether & where to move"

- **FR-B1** — The tree renders a `Hosts · Sessions · Windows · Panes` tree of all
  reachable sessions, expandable to per-window panes with the running command.
  **Tests:** `parse_panes_*` (data), switcher render tests (`dump_*`, `tree_*`).
- **FR-B2** — Render-first: the host skeleton paints instantly; each source's
  sessions and each session's panes stream in independently.
  **Tests:** `connect_all_sources_connects_remote_hosts`,
  `apply_source_result_turns_scanning_into_sessions`, `apply_panes_*`.
- **FR-B3** — The terminal view shows the confirmed session's live grid and follows
  the cursor. A switch keeps the prior grid on screen until the new one is ready
  (stale-while-revalidate); only the first launch, before any grid exists, shows a
  blank view. The `scanning…` / `loading…` state hints live in the tree, not here.
  **Tests:** `render_terminal_view_draws_live_grid`,
  `render_terminal_view_none_grid_is_blank_not_attaching`,
  `terminal_view_target_follows_cursor`, `dump_screen_renders_the_live_grid`.
- **FR-B4** — Navigation: up/down/home/end/pgup/pgdn; fuzzy filter over
  `<source>/<name>`; manual `prefix r` rescan. **Tests:** `filter_narrows`,
  `up_down_move_within_level_and_hjkl_match_arrows`, `navigation_wraps_around`,
  `request_rescan_*`.
- **FR-B5** — Surveying without committing is first-class: xmux is a switcher, not a
  session owner. Quitting (`prefix q`, or the ctl `quit` verb) leaves the current
  mux session untouched — it is never killed or altered by exiting.
  **Tests:** `control_end_to_end` (quit), `input_esc_cancels_without_acting` (a
  modal dismiss acts on nothing).
- **FR-B6** — Under a filter, `Enter` attaches the **visible (filtered)** session —
  never a filtered-out one — even when a host row is selected.
  **Tests:** `filter_host_enter_targets_visible_session`,
  `filter_leaves_cursor_on_visible_session`.
- **FR-B7** — Per-element state hints: `scanning…`, `loading…`, `(empty)`,
  `⚠ unreachable: <reason>`. **Tests:** `apply_source_result_empty_shows_empty_status`,
  `apply_source_result_unreachable_marks_tree_and_reason_in_info_pane`.
- **FR-B8** — A session running xmux is never mirrored into the terminal view.
  This is prevented structurally, not by a runtime check: the nest guard (FR-D3)
  refuses to run xmux inside a mux, so no attachable session can be running xmux.
  **Tests:** `nest_guard_inside`, `nest_guard_outside`, `in_mux_value_cases`.

## C. Switching (the keystone)

- **FR-C1** — A same-server pick switches the live client in place via
  `switch-client` (instant), pre-selecting the chosen window. Each mux's driver owns
  the in-place-vs-reattach decision: with a known display tty it moves xmux's own
  client and repaints; without one it reattaches. The attach is debounced so rapid
  navigation does not storm. **Tests:** `psmux_driver_show_switches_in_place_when_tty_known`,
  `psmux_driver_show_reattaches_when_tty_unknown`, `select_window_argv`,
  `should_attach_fires_on_change_and_recovery_never_storms_in_flight`,
  `apply_tick_arms_then_fires_one_attach_after_debounce`.
- **FR-C2** — A cross-host pick switches entirely in process, with no picker and no
  detach between. Each host keeps its own live PTY attachment; `select_attach` picks
  the target host's driver, the previously shown session stays on screen until the
  fresh grid is ready (stale-while-revalidate), and the canonical selection is synced
  immediately. **Tests:** `shared_host_reuses_one_attachment_and_in_flight_guards_current`,
  `display_key_is_per_host_for_shared_and_reattach_psmux`,
  `ctl_switch_syncs_canonical_selection_immediately`,
  `tmux_driver_show_warms_the_shared_host_pty_on_first_attach`. **Live-verified**
  (real psmux + ssh).
- **FR-C3** — Host degradation is graceful, never a silent loss: an unreachable host
  is marked `⚠ unreachable: <reason>`, a reachable-but-serverless host reads
  `(empty)`, a once-connected host keeps its last-known tree on a transient drop, and
  the reconnect sweep self-heals; a dropped display client is reaped and re-attached.
  **Tests:** `host_exited_before_connect_marks_unreachable`,
  `host_exited_with_no_sessions_marks_empty_not_unreachable`,
  `host_exited_after_connect_keeps_tree`,
  `refresh_after_a_dropped_host_resolves_instead_of_loading_forever`,
  `client_detached_matching_our_tty_reaps_display_and_rearms`.
- **FR-C4** — A switch lands on the picked window. A fresh first attach folds the
  window into the attach argv (ssh folds the pre-selection into one `ssh -t`);
  a live client is moved server-side by a lowered `select-window`. **Tests:**
  `interactive_attach_remote_folds_pre_select_into_one_connection`,
  `interactive_attach_remote_without_pre_select_execs_over_ssh_tty`,
  `selection_from_window_row_target`, `active_window_probe_moves_tree_selection`.
- **FR-C5** — No silent loss: every lowered switch/select command logs its exact argv
  and result through `tracing`; a failed attach logs `attach_failed` (warn) and returns
  to the tree rather than being swallowed; each driver logs its show decision and the
  grid-changed effect. **Tests:** the decision paths that must emit are exercised by
  `psmux_driver_show_*` and `tmux_driver_show_*`.

## D. App lifecycle

- **FR-D1** — `xmux` (no subcommand) is a persistent supervisor (`run_app`) that owns
  the terminal and runs one mux-client child at a time per session, plus one `-CC`
  metadata client per remote host, over a single `tokio::select!` loop. **Tests:**
  `connect_all_sources_connects_remote_hosts`,
  `should_attach_fires_on_change_and_recovery_never_storms_in_flight`, and the
  nest-guard entry `nest_guard_inside`.
- **FR-D2** — The app serves its control socket concurrently while a session is
  displayed (attach spawning is off-loop), so `ping` / `dump` / `status` / `switch`
  are answered without blocking. **Tests:** `control_end_to_end`,
  `dispatch_dump_and_key_still_work`, `dispatch_resolves_semantic_verbs_to_op_cmds`.
  **Live-verified** (ping→pong while attached).
- **FR-D3** — Running the app inside a mux is refused (exit 2 with guidance), not
  warned — nested, every attach is refused, leaving a doomed loop. **Tests:**
  `nest_guard_inside`, `nest_guard_outside`, `in_mux_value_cases`; `run_app` wiring is
  in `runtime.rs`. **Live-verified** (exit 2).
- **FR-D4** — Socket hygiene: a stale socket is removed before bind, the socket is
  owner-only (`0600`) on unix, and it is removed on exit. A crashed instance's leftover
  `ctl-*.sock` marker is swept on the next startup (any marker whose socket no longer
  dials). Discovery enumerates the markers newest by mtime first, tie-broken by higher
  pid. **Tests:** `control_handle_drop_removes_socket`, `control_socket_is_owner_only`
  (unix), `prune_stale_removes_dead_markers_and_keeps_own`,
  `discover_all_newest_then_higher_pid`, `discover_all_tie_break_higher_pid`.
- **FR-D5** — The app launches directly into the persistent split view (tree +
  terminal view) with the cursor preselected — the persisted last session if set,
  else a local-first recency preselect. There is no separate picker mode; `prefix q`
  quits. **Tests:** `preselects_local_first_session`,
  `preferred_session_wins_preselect_when_it_streams_in`,
  `streaming_keeps_local_preselect_when_untouched`.

## E. Session management

- **FR-E1** — Create a session on any source (`prefix n`), then it appears in the tree.
  **Tests:** `create_*`, `new_session_*` (mux), `create_on_unreachable_host_refused`.
- **FR-E2** — Kill a session (`prefix x`) behind an inline confirmation. **Tests:**
  `menu_release_kill_arms_confirm`, `kill_confirm_esc_cancels`,
  `kill_removes_session_and_cache`.
- **FR-E3** — Rename a session (`prefix R`); a leading-dash name is refused.
  **Tests:** `rename_*`, `rename_rejects_leading_dash`.
- **FR-E4** — Create/kill/rename run off the key path so a slow ssh round-trip never
  freezes rendering or the control channel. A committing key folds through
  `State::apply` into a `Command::RunOp(MuxOp)` the run loop spawns off-loop.
  **Tests:** `slow_op_is_deferred_off_the_key_path`, `*deferred*`, `apply_*` (the
  RunOp folds).

## F. Control channel

- **FR-F1** — A single per-pid local socket (`ctl-<pid>.sock`) drives the running app
  headlessly. Its navigation/display verbs — `ping`, `dump`, `status`,
  `switch <source>/<session>`, `focus <terminal|tree>`, `rescan`, `quit`,
  `width <delta>` (a signed column delta, not an absolute width), `toggle-auto-hide` —
  and its session-lifecycle verbs — `new-session`, `kill-session`, `rename-session`,
  `new-window`, `split-window`, `kill-window`, `rename-window` (sessions addressed
  `<source>/<session>`, windows `<source>/<session>:<window>`) — parse to a domain
  `Action`; raw key/text injection stays behind the unstable `raw:` namespace
  (`raw:key` / `raw:keys` / `raw:text`), reserved for tests. A command-level failure
  replies `err: …` and `xmux ctl` exits non-zero. **Tests:**
  `parse_ctl_op_semantic_verbs`, `parse_ctl_op_session_lifecycle_verbs`,
  `parse_ctl_op_raw_namespace_is_test_only_surface`, `parse_ctl_op_rejects_malformed`,
  `parse_request_cases`, `parse_key_*`, `control_end_to_end`,
  `dispatch_resolves_semantic_verbs_to_op_cmds`.
- **FR-F2** — There is one unified socket, not a separate app socket: `switch <address>`
  is a first-class ctl verb resolving to `Action::Switch`. **Tests:**
  `control_end_to_end`, `dispatch_resolves_semantic_verbs_to_op_cmds`,
  `parse_ctl_op_semantic_verbs`.
- **FR-F3** — Socket discovery enumerates the `ctl-*.sock` markers, newest by mtime
  first then higher pid. `xmux ctl` with no `--pid`/`--sock` drives the sole LIVE
  instance — a dialable socket, so a crashed instance's stale marker is filtered out —
  and refuses to guess when several are live; `xmux ctl list` shows each (pid, cwd, tty,
  displayed session, focus) so a specific one can be targeted with `--pid`. **Tests:**
  `discover_all_newest_then_higher_pid`, `discover_all_tie_break_higher_pid`,
  `live_instances_filters_out_dead_markers`, `choose_sole_instance_needs_exactly_one_live`,
  `socket_path_format`.
- **FR-F4** — Length-framed messages (decimal count + `\n` + bytes) with a bounded
  read; endpoint naming works for `ctl-*.sock` on every platform. **Tests:**
  `read_frame_oversized`, `frame_round_trip`, `socket_path_format`,
  `parse_request_cases`.

## G. Transport & safety

- **FR-G1** — ssh uses a connect-timeout; listing uses `BatchMode` (never hangs on a
  prompt); attach requests a tty; ControlMaster multiplexing is added only off Windows.
  **Tests:** `ssh_opts_non_interactive_batches_and_multiplexes`,
  `ssh_opts_interactive_requests_tty_no_batch`, `ssh_opts_windows_omits_control_master`.
- **FR-G2** — A session name from a remote list is injection-safe when it re-enters
  a remote shell command (POSIX single-quote escaping). **Tests:**
  `quote_neutralizes_shell_metachars`, `remote_command_joins_quoted`.
- **FR-G3** — Mux session env (`TMUX`/`TMUX_PANE`/`PSMUX*`) is stripped for listing so a
  command run from inside a mux is not refused as nesting; lookalikes survive. **Tests:**
  `is_mux_var_matches_exactly_tmux_and_psmux_markers`, `mux_env_keys_to_clear_selects_only_mux_vars`.
- **FR-G4** — A remote attach folds the window pre-selection into the single
  `ssh -t` connection (no second connection to hang or lose), and the mux axis supplies
  the attach argv (local psmux routes to its per-session server). **Tests:**
  `interactive_attach_remote_folds_pre_select_into_one_connection`,
  `interactive_attach_remote_without_pre_select_execs_over_ssh_tty`,
  `interactive_attach_local_psmux_routes_to_the_per_session_server`.

---

## Use cases (end-to-end scenarios)

- **UC-1 — Jump from my laptop to a remote dev session.** From the split view, move
  the cursor to a remote session and land in it in one action. *(FR-B1, FR-C2,
  FR-D1/D2)* — Tests: FR-C2 set; **live-verified**.
- **UC-2 — Hop between two same-server sessions.** Select a session on the current
  server → instant switch-client. *(FR-C1)*
- **UC-3 — Survey, then stay put.** Look around the tree, then quit; the current
  session is untouched. *(FR-B5)* — Test: `control_end_to_end` (quit).
- **UC-4 — Find one session among many, then go.** Filter to narrow, Enter on the
  visible match. *(FR-B4, FR-B6)* — Tests: the FR-B6 set.
- **UC-5 — The remote is down — don't leave me in the dark.** An unreachable host shows
  `⚠ unreachable`; a failed attach is logged and the tree stays usable.
  *(FR-A2, FR-B7, FR-C5)* — **live-verified** (tracing log entry).
- **UC-6 — Deep in a remote, get back home.** Native detach (`prefix d`) inside the
  remote returns control to the local app's split view; pick local or another host.
  *(FR-C2, FR-D1)*
- **UC-7 — Spin up a throwaway on a remote and switch to it.** Create on a source,
  then switch to it. *(FR-E1, FR-C2)*
- **UC-8 — Survey what's running everywhere before deciding.** The tree shows hosts,
  sessions, windows, per-pane commands; the terminal view previews the selection.
  *(FR-B1, FR-B3, FR-B8)*
- **UC-9 — Rename / kill a session from the switcher.** *(FR-E2, FR-E3)*
- **UC-10 — Drive xmux from a script.** Control channel: dump, inject keys, signal a
  switch. *(FR-F1, FR-F2)* — Tests: `control_end_to_end`, the semantic-verb set.
- **UC-11 — Switch in either direction, local↔remote↔local.** The app re-attaches
  whatever the next target is, local or remote, in any order, with no picker between.
  *(FR-C2, FR-D1)*

## Out of scope (documented elsewhere)

- The seamless-cross-host-switch design and its accepted limitations (single live app,
  inter-client repaint flash, Windows ssh latency): `docs/superpowers/` planning
  material.
