# xmux — functional requirements & use cases

xmux is a stateless cross-environment session switcher: one terminal that sees and
moves between every reachable tmux/psmux/zellij session — local and over ssh —
regardless of OS or mux kind. Its reason to exist is to deliver tmux's `prefix + s`
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
- **FR-A5** — The roster (which ssh targets are offered) comes from providers the
  `[discovery]` table selects: `~/.ssh/config` aliases (on by default) and this
  machine's tailnet peers (off by default, since it runs an external CLI). A tailnet
  peer is offered under its DNS label; this machine and offline peers are skipped. A
  provider that cannot answer contributes nothing instead of failing the run, and
  ssh-config names keep their position when a provider repeats them. **Tests:**
  `takes_online_peers_by_their_dns_label`, `skips_self_and_offline_peers`,
  `the_dns_label_wins_over_hostname`, `a_provider_that_cannot_answer_yields_nothing`,
  `a_label_that_is_not_a_dns_label_is_refused`,
  `merge_keeps_first_seen_order_and_drops_duplicates`.

- **FR-A6** — A host's mux is identified by what its binary answers as, not by the
  name it was invoked under, so tmux, psmux, and zellij mix freely across hosts with no
  configuration. Each mux is one family behind the `Mux` trait: the command plans
  default to tmux-compatible argv (so a tmux-compatible mux is identity plus a few
  methods), and a mux that shares no argv with tmux overrides every plan together with
  the shape of what each plan prints. zellij is that case: it is enumerated from
  `list-sessions`, its windows come from its tab listing, and its sessions are polled
  because it offers no push channel. **Tests:**
  `detect_backend_classifies_zellij_by_help_marker`,
  `zellij_resolves_by_binary_name_and_by_kind`,
  `zellij_is_per_session_polled_and_dies_by_eof`,
  `the_window_query_is_the_tab_listing_and_it_reads_json`,
  `an_idle_zellij_is_empty_and_an_unreachable_host_is_an_error`,
  `a_session_carries_its_name_kind_and_creation_recency`,
  `a_resurrectable_session_is_not_offered`, `tabs_are_windows_in_tab_bar_order`.
  **Live-verified** (real zellij over ssh: attach, cross-session switch, input, and the
  window row following the session's focused tab).

- **FR-A7** — A SOURCE is one mux on one machine, so a machine running several
  muxes at once contributes one source per mux and every one of them is listed. A `mux`
  value is a name or a LIST of names, in `[local]` and in `[[hosts]]` alike. A machine
  given several muxes has its sources named `<machine>:<mux>`; a machine given one keeps
  the bare machine alias, so an existing setup's ids, addresses, and typed targets do
  not move. `exclude` names machines, so it drops every mux on one. A listed mux that is
  not installed there surfaces as unreachable rather than being dropped, because a name
  the user wrote is a name they meant. **Tests:**
  `a_machine_can_be_given_several_muxes`, `one_mux_on_a_machine_keeps_the_bare_id`,
  `excluding_a_machine_drops_every_mux_on_it`,
  `a_mux_list_parses_from_toml_beside_a_bare_name`,
  `a_mux_list_drops_blanks_and_repeats`,
  `a_source_id_names_its_mux_only_when_the_machine_serves_several`,
  `the_machine_half_and_localness_survive_qualification`,
  `a_qualified_source_still_addresses_a_session`,
  `a_qualified_transport_keeps_reaching_the_same_machine`,
  `a_qualified_local_transport_is_still_this_box`,
  `every_mux_on_this_box_pins_ahead_of_every_remote`. **Live-verified** (local psmux and
  local zellij listed side by side, switching between them in place).
- **FR-A8** — A polled host cannot wedge on one unanswered command. Every command in
  a poll sweep runs under a fixed per-command budget, because the poll ticker only
  advances once the sweep returns: a timed-out listing surfaces as that host's error
  (the nav shows it unreachable), and a timed-out pane query still emits an EMPTY panes
  answer, since a card whose panes never arrive would keep its spinner forever.
  **Tests:** `a_hung_listing_ends_the_sweep_as_an_error_not_a_freeze`,
  `a_hung_pane_query_ends_the_sweep_with_an_empty_answer`.
- **FR-A9** — No mux list needs configuring, on any machine. A machine that named no
  mux is asked which of the ones xmux SUPPORTS it has, and each one that answers becomes a
  source. The candidate set is what xmux can drive, and each candidate is asked with the
  same identity probe a configured mux gets, so a binary carrying a mux's name while being
  another mux is not counted as that mux: where psmux answers, a `tmux` that also answers
  is psmux's own alias of itself (which names itself by the name it was invoked under, so
  no probe can tell it apart) and is dropped. A WRITTEN value is never probed, keeping
  FR-A7's rule that a name the user wrote stays visible even when it is missing; a machine
  where nothing answers keeps the mux it was assumed to run, so the nav names what is
  unreachable rather than showing nothing.
  **Tests:** `discovery_only_looks_for_muxes_xmux_can_drive`,
  `a_machine_offers_every_supported_mux_it_actually_has`,
  `where_psmux_answers_a_tmux_is_its_own_alias`,
  `a_binary_that_answers_as_another_mux_is_not_that_mux`,
  `a_machine_with_no_mux_installed_discovers_none`,
  `a_written_mux_is_taken_verbatim_and_auto_is_what_the_box_has`,
  `the_conventional_mux_leads_the_discovered_list`,
  `a_box_where_nothing_answered_still_offers_its_conventional_mux`,
  `only_a_machine_that_named_no_mux_is_xmuxs_to_decide`.
- **FR-A10** — A REMOTE machine is discovered AFTER launch, asynchronously, and its
  answer only ADDS. The app paints the sources the config names first (a remote probe is
  an ssh round trip per mux, and nothing may wait for that), then each machine's answer
  arrives and every mux it reports that the machine does not already serve becomes a
  scanning card on the spot. An added source's id is always qualified (`prod:zellij`)
  while the mux already served keeps the id it was painted with: that id is what the
  frozen order, the persisted selection, and anything the user typed are keyed to, so
  nothing is renamed and nothing is removed. New cards APPEND, so a card the user is
  looking at does not move because another machine answered. **Tests:**
  `a_discovered_mux_becomes_a_source_on_the_spot`,
  `a_discovered_source_appends_and_leaves_the_selection_put`,
  `muxes_found_forwards_the_add_to_the_loop`,
  `machine_serves_asks_by_machine_and_mux_not_by_id`. **Live-verified** (a real ssh host
  running both tmux and zellij: the nav paints its tmux card at once, and `jupiter06/zellij`
  with its session appears about eight seconds later, on a Windows box where ssh has no
  ControlMaster multiplexing).

## B. The switcher — "see the list, decide whether & where to move"

- **FR-B1** — The nav renders ONE CARD PER SESSION across every reachable source,
  most recently used first: a context line `{host}/{mux}` over a detail line
  `{session}/{index}:{name}` naming the session's focused window. The list is flat, with
  no window or pane rows: xmux aggregates and switches, and the mux itself already shows
  its own windows. **Tests:** `session_card_context_shows_host_mux_session`,
  `session_card_shows_the_focused_window_name`, `panes_are_not_selectable`,
  `parse_panes_*` (data), switcher render tests (`dump_*`).
- **FR-B2** — Render-first: the host skeleton paints instantly; each source's
  sessions and each session's panes stream in independently.
  **Tests:** `connect_all_sources_connects_remote_hosts`,
  `apply_source_result_turns_scanning_into_sessions`, `apply_panes_*`.
- **FR-B3** — The terminal view shows the confirmed session's live grid and follows
  the cursor. A switch keeps the prior grid on screen until the new one is ready
  (stale-while-revalidate); only the first launch, before any grid exists, shows a
  blank view. The `scanning…` / `loading…` state hints live in the nav, not here.
  **Tests:** `render_terminal_view_draws_live_grid`,
  `render_terminal_view_none_grid_is_blank_not_attaching`,
  `terminal_view_target_follows_cursor`, `dump_screen_renders_the_live_grid`.
- **FR-B4** — Navigation: up/down/home/end/pgup/pgdn; fuzzy filter over
  `<source>/<name>`; manual `prefix r` rescan. **Tests:** `filter_narrows`,
  `up_down_and_hjkl_move_linearly`, `navigation_wraps_around`,
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
- **FR-B9** — The nav's bottom row is a status line, not a screen-wide footer. At
  rest it names only the prefix; the states that outrank it (a refusal, scan progress,
  an active filter) take the row while they apply. Arming the prefix widens the PAINT
  to the whole window so the cheatsheet floats over the view border and the live grid,
  leaving the layout alone so no card shifts. **Tests:**
  `hint_bar_shows_the_prefix_at_rest_and_its_keys_when_armed`,
  `the_armed_hint_bar_floats_across_the_whole_window`,
  `armed_hint_bar_fits_a_narrow_nav`,
  `arming_the_prefix_marks_the_frame_dirty_so_the_hint_bar_swaps`,
  `long_flash_wraps_in_narrow_hint_bar_instead_of_clipping`.
- **FR-B10** — Every unselected card carries a 0-based number in its gutter, on the
  row of the session it addresses, and `prefix <digit>` jumps to it. The selected card
  shows none: the accent bar beside it already says you are there. Selecting a card
  moves nothing on its rows - the number column stays spent and the connector stays
  drawn - so a name holds its column as the selection passes over it. The popup stays
  open so the number can grow, and accepts a digit only while the result still addresses
  a real session, so one-, two-, and three-digit numbers behave identically. Each edit
  moves the selection; `Enter` keeps it, `Esc` returns to where the jump started.
  **Tests:** `every_unselected_card_carries_its_0_based_number_beside_its_session`,
  `selecting_a_card_never_moves_its_session_name`,
  `a_digit_opens_the_jump_popup_and_lands_on_that_card`,
  `a_jump_walks_into_a_two_digit_number`,
  `a_jump_never_holds_a_number_no_session_carries`,
  `cancelling_a_jump_restores_the_starting_card`, `a_jump_past_the_last_card_is_inert`.

- **FR-B11** — Every colour xmux paints is an ANSI-16 slot, so the TERMINAL THEME
  resolves the hue and the whole UI recolours with the user's own scheme. What the
  sixteen slots cannot say is said with an attribute: the selected card is REVERSE VIDEO,
  the terminal swapping its own pair, which is what a theme itself means by "selected".
  A background xmux picked instead would be wrong on every theme it was not picked for,
  and it cannot be computed from the terminal's own background either, since a terminal
  is free to answer no colour query at all. `[ui] selection-style` names a background
  anyway, in the same colour vocabulary as the view border, and `xmux doctor` reports
  which of the two is in effect because it is invisible on a screenshot. **Tests:**
  `every_colour_xmux_chooses_is_an_ansi_slot`,
  `the_default_selection_is_the_terminals_own_reverse_video`,
  `the_selected_card_is_painted_in_the_terminals_own_reverse_video`,
  `a_selection_style_names_one_background`,
  `a_named_selection_style_is_a_plain_background`,
  `the_report_says_which_of_the_two_paints_the_selection`. **Live-verified** (in a real
  terminal every cell of the selected card comes back inverted with no colour of its own,
  the unselected cards come back on ANSI indices 2/6/8, and the hint bar on 0/15/4; with
  `[ui] selection-style = "#2d4f6b"` set, both card rows come back that colour).

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
  `(empty)`, a once-connected host keeps its last-known cards on a transient drop, and
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
  `selection_from_window_row_target`, `active_window_probe_refreshes_focused_window_line`.
- **FR-C5** — No silent loss: every lowered switch/select command logs its exact argv
  and result through `tracing`; a failed attach logs `attach_failed` (warn) and returns
  to the nav rather than being swallowed; each driver logs its show decision and the
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
  `discover_all_newest_then_name_order`, `discover_all_tie_break_by_name`.
- **FR-D5** — The app launches directly into the persistent split view (nav +
  terminal view) with the cursor preselected — the persisted last session if set,
  else a local-first recency preselect. There is no separate picker mode; `prefix q`
  quits. **Tests:** `launch_preselects_top_row`,
  `streaming_keeps_local_preselect_when_untouched`,
  `rebuild_holds_a_user_moved_session_against_the_preselect`.

## E. Session management

xmux aggregates and switches; it does not edit what a mux already edits. Starting a
session is the one mutation it keeps, because a reachable host with no sessions has
nothing to switch to until one exists.

- **FR-E1** — Create a session on a HOST card (`prefix n`), then it appears in the
  nav. On a session card the action is refused with a flash naming where to press it.
  **Tests:** `create_*`, `new_session_*` (mux), `create_on_unreachable_host_refused`,
  `n_on_a_session_card_refuses_with_a_flash`.
- **FR-E2** — There is no rename, kill, or window/pane command — not on a key, not
  in a modal, not on the wire, and not in the mux command vocabulary. **Tests:**
  `parse_ctl_op_new_session_is_the_only_lifecycle_verb`,
  `resolve_nav_action_keys_require_prefix`.
- **FR-E3** — Create runs off the key path so a slow ssh round-trip never freezes
  rendering or the control channel. The committing key folds through `State::apply`
  into a `Command::RunOp(MuxOp)` the run loop spawns off-loop. **Tests:**
  `slow_op_is_deferred_off_the_key_path`, `*deferred*`, `apply_*` (the RunOp folds).

## F. Control channel

- **FR-F1** — A per-instance local socket (`ctl-<name>.sock`) drives the running app
  headlessly. Its navigation/display verbs — `ping`, `dump`, `status`,
  `switch <source>/<session>`, `focus <terminal|nav>`, `rescan`, `quit`,
  `width <delta>` (a signed column delta, not an absolute width), `toggle-auto-hide` —
  and its one session-lifecycle verb, `new-session` (sessions addressed
  `<source>/<session>`), parse to a domain `Action`. There are no kill/rename/window
  verbs: xmux aggregates and switches, so editing a session stays with the mux. Raw
  key/text injection stays behind the unstable `raw:` namespace (`raw:key` /
  `raw:keys` / `raw:text`), reserved for tests. A command-level failure replies
  `err: …` and `xmux send` exits non-zero. **Tests:**
  `parse_ctl_op_semantic_verbs`, `parse_ctl_op_new_session_is_the_only_lifecycle_verb`,
  `parse_ctl_op_raw_namespace_is_test_only_surface`, `parse_ctl_op_rejects_malformed`,
  `parse_request_cases`, `parse_key_*`, `control_end_to_end`,
  `dispatch_resolves_semantic_verbs_to_op_cmds`.
- **FR-F2** — There is one unified socket, not a separate app socket: `switch <address>`
  is a first-class ctl verb resolving to `Action::Switch`. **Tests:**
  `control_end_to_end`, `dispatch_resolves_semantic_verbs_to_op_cmds`,
  `parse_ctl_op_semantic_verbs`.
- **FR-F3** — Every instance takes a NAME at startup: an auto-generated
  `<adjective>-<noun>` whose walk skips names live instances hold (a crashed
  instance's undialable marker is reused), or an explicit `--name` validated to 1-32
  characters of `[a-z0-9-]` so it is always a legal path segment and Windows pipe
  name. Socket discovery enumerates the `ctl-*.sock` markers, newest by mtime first
  then by name. `xmux send <id>` resolves `id` against LIVE instances only — exact
  name, then unique name prefix, with `-` for the sole one — and refuses ambiguity by
  naming the candidates. `xmux instances` shows each (name, pid, cwd, tty, displayed
  session, focus). **Tests:** `sanitize_name_accepts_safe_names_and_refuses_the_rest`,
  `nth_name_pairs_do_not_repeat_within_a_pass`,
  `pick_free_name_skips_a_live_marker_but_reuses_a_dead_one`,
  `discover_all_newest_then_name_order`, `discover_all_tie_break_by_name`,
  `live_instances_filters_out_dead_markers`,
  `resolve_target_takes_a_name_then_a_unique_prefix`,
  `resolve_target_refuses_an_ambiguous_prefix`,
  `resolve_target_dash_takes_the_sole_instance`, `socket_path_format`.
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
- **UC-3 — Survey, then stay put.** Look around the nav, then quit; the current
  session is untouched. *(FR-B5)* — Test: `control_end_to_end` (quit).
- **UC-4 — Find one session among many, then go.** Filter to narrow, Enter on the
  visible match. *(FR-B4, FR-B6)* — Tests: the FR-B6 set.
- **UC-5 — The remote is down — don't leave me in the dark.** An unreachable host shows
  `⚠ unreachable`; a failed attach is logged and the nav stays usable.
  *(FR-A2, FR-B7, FR-C5)* — **live-verified** (tracing log entry).
- **UC-6 — Deep in a remote, get back home.** Native detach (`prefix d`) inside the
  remote returns control to the local app's split view; pick local or another host.
  *(FR-C2, FR-D1)*
- **UC-7 — Spin up a throwaway on a remote and switch to it.** Create on the
  host's card, then switch to it. *(FR-E1, FR-C2)*
- **UC-8 — Survey what's running everywhere before deciding.** The nav shows every
  session on every host with its focused window; the terminal view previews the selection.
  *(FR-B1, FR-B3, FR-B8)*
- **UC-9 — Drive xmux from a script.** Control channel: dump, inject keys, signal a
  switch. *(FR-F1, FR-F2)* — Tests: `control_end_to_end`, the semantic-verb set.
- **UC-10 — Switch in either direction, local↔remote↔local.** The app re-attaches
  whatever the next target is, local or remote, in any order, with no picker between.
  *(FR-C2, FR-D1)*
- **UC-11 — Go straight to the session I can already see.** Read the number off the
  card, `prefix <digit>`, and the selection is there; keep typing for a number past 9.
  *(FR-B10, FR-C1)*

## Out of scope (documented elsewhere)

- The seamless-cross-host-switch design and its accepted limitations (single live app,
  inter-client repaint flash, Windows ssh latency): `docs/superpowers/` planning
  material.
