# xmux Rust rewrite — PROGRESS

Port of xmux (stateless cross-environment tmux/psmux session-switcher TUI) from Go
to Rust with feature parity, then optimization + UX improvement.

- **Branch:** `feat/rust-rewrite` (Rust at repo root; Go reference preserved in `legacy-go/`).
- **Stack:** `ratatui` + `crossterm` (TUI), `tokio` (async concurrency), `clap` (CLI),
  `serde`+`toml` (config), `interprocess` (cross-platform control socket).
- **Method:** TDD per module — port the Go behavior + its test contract, keep
  `cargo build`/`cargo test`/`cargo clippy` green, commit after each coherent chunk.

This doc is the resume point. Each run: read this + `git log --oneline -20`, continue
the next unchecked module.

---

## Crate layout (target)

`xmux` is a lib + bin (mirrors Go `internal/` packages + `cmd/xmux`):

- `src/lib.rs` — module roots
- `src/session.rs` — data types (`Session`, `Pane`, `WindowPanes`, address, parse_target)
- `src/mux.rs` — argv builders + output parsers (tmux/psmux)
- `src/config.rs` — TOML config + ssh-config Host discovery
- `src/source.rs` — local/ssh boundary: argv assembly, ssh transport, quoting, runner, classify
- `src/discovery.rs` — concurrent bounded fan-out scan (tokio)
- `src/manage.rs` — lifecycle ops (create/kill/rename/panes/capture/select)
- `src/attach.rs` — terminal handover; nest guard; same-server-teleport vs cross-server-detach plan
- `src/control.rs` — per-pid local-socket control channel (key/text/dump/ping) + client
- `src/ui/tree.rs` — pure tree model (groups, sort-by-recency, fuzzy filter, add/remove/rename)
- `src/ui/ansi.rs` — pane ANSI SGR → ratatui `Text`/`Span` (faithful, no attribute bleed)
- `src/ui/switcher.rs` — ratatui two-pane navigator + live preview + keys/input/kill/mouse
- `src/env.rs` — resolved runtime (sources, lookups, scan/deepScan, ops)
- `src/main.rs` — clap CLI: (default) home, popup, ls, attach, doctor, ctl, version

---

## Module enumeration & responsibilities (from Go source)

| # | Module | Go origin | Responsibility | Port deps |
|---|--------|-----------|----------------|-----------|
| 1 | session | internal/session | `Session{source,name,windows,attached,last_attached}`, `Pane`, `WindowPanes`, `address()`, `parse_target` (split first `/`, both non-empty) | none |
| 2 | mux | internal/mux | Pure argv builders (`list-sessions`/`list-panes -s`/`attach`/`switch-client`/`detach-client`/`new-session -A -d -P -F`/`capture-pane -p -e`/`select-window`/`select-pane`/`kill-session`/`rename-session`) + format templates + parsers (`parse_sessions`, `parse_panes`: tab-split, name LAST rejoined, tolerate CRLF, skip malformed, group panes by window_index first-seen) | session |
| 3 | config | internal/config | TOML `Config{local.mux, [[hosts]], exclude}`, `load`/`load_verbose` (warn undecoded keys), `local_bin(os)` (auto→psmux/win,tmux/else), `host_specs(aliases)` (merge discovery+config, dedup, exclude, default tmux). `ssh_host_aliases(path)`: parse OpenSSH config `Host` lines, skip globs(`*?`)/negations(`!`), dedup first-seen | none (serde/toml) |
| 4 | source | internal/source | `Source{alias,binary,remote,control_path,os}`, `ssh_args(tty)` (`-t` vs `BatchMode=yes`, `ConnectTimeout=5`, ControlMaster only on non-windows, `-- alias`), `exec_argv`, `attach_command`, `run`, `list_sessions` (reachable-empty→`[]`, unreachable→Err). `Build`. `quote`/`is_shell_safe`/`remote_command` (POSIX single-quote escaping). `is_no_sessions` (ExitErr only; codes 126/127/255 never benign; "no server running"/"no sessions" as line PREFIX). `Runner` trait + `ExitErr{stderr,code}` + exec runner + `mux_clean_env` (strip TMUX*/PSMUX*) | config, mux, session |
| 5 | discovery | internal/discovery | `Result{source,sessions,err}`. `scan_all(srcs, timeout, max_concurrent)`: bounded concurrency, per-source timeout, ORDER-PRESERVING; one dead source never blocks/fails others | source, session (tokio) |
| 6 | manage | internal/manage | `create`/`kill`/`rename`/`panes`/`capture`/`select_window`/`select_pane` over a `Source` (no state) | mux, session, source |
| 7 | attach | internal/attach | `in_mux()` ($TMUX), `nest_guard(in_mux)`, `Execer` trait + OS execer (hand over stdio, wait), `run_attach`. `plan_switch(from_source,from_bin,target)`: same-source→teleport(switch-client), cross-source→detach(detach-client) | mux, session |
| 8 | ui::tree | internal/ui/tree.go | `Group{source,err,sessions}`, `sort_by_recency` (last_attached desc, name asc, stable), `filter_groups` (fuzzy; source-match keeps all; unreachable kept only on source-match), `add_session`/`remove_session`/`rename_session` (immutable transforms), `fuzzy_match` (case-insensitive subsequence) | session |
| 9 | ui::ansi | internal/ui/ansi.go | `ansi_to_*`: SGR→styled spans, re-state FULL style per change (no bleed), drop non-SGR CSI/OSC, escape literal tags. 16-color + 8-bit(38/48;5) + 24-bit(38/48;2) + attrs (bold/dim/italic/underline/blink/reverse/strike) | ratatui |
| 10 | control | internal/control | length-framed socket protocol (`writeFrame`/`readFrame`, maxFrame 16MiB), `parse_key` (named/space/rune-verbatim-case/ctrl+x), `parse_request` (verb lowered, arg verbatim). `Server` (serve/accept/dispatch ping|dump|key|text, on-main barrier), `Client` (dial/do/close), `socket_path(dir,pid)`, `discover` (newest ctl-*.sock, tie→higher pid) | switcher app handle (interprocess) |
| 11 | ui::switcher | internal/ui/switcher.go | ratatui app: header / two-pane (tree TreeView-equiv `treeWidth=48` + preview Pages w/ floating dialog) / hidden input row / footer. Cursor follows preview (poll 1s + kick), per-target cache, loading/reconnecting dialog. Keys: ↑↓ wrap+skip-panes, PgUp/Dn ±10, Home/End, Enter attach (host→recent sess, sess→sess, win→win), n new, R rename, x kill (inline y/n), / filter, r refresh, q/Esc quit. Mouse: click=select, dbl-click=attach, wheel=scroll. Per-level colors. `render_text` → ratatui `TestBackend` buffer flatten (control `dump`) | all above (ratatui, crossterm, tokio) |
| 12 | env | cmd/xmux/env.go | `Env{cfg,srcs,by_alias,local_bin,xmux_dir}`, `build_env`, `scan`, `deep_scan` (concurrent pane fetch), `to_groups`, `ls_lines`, `ops` (New/Kill/Rename/Panes/Capture/Refresh closures over live mux) | config, source, discovery, manage, ui |
| 13 | main | cmd/xmux/main.go + home/ls/doctor/ctl | clap root + subcommands: (default)→run_home (loop scan→switch→attach→detach-back), popup→run_popup (teleport|detach, one shot), ls→run_ls, attach→direct attach, doctor→reachability report, ctl→drive socket, version. `control_hook` (XMUX_CONTROL=0 disables) | all |

### Behavioral invariants to preserve (verified from Go source/tests)
- `list-panes -s` (whole-session scope, never `-a` — leaks across servers/sockets).
- `capture-pane -p -e` (include ANSI so preview reproduces colors).
- `new-session -A -d -P -F '#{session_name}'` (idempotent, detached, prints assigned name).
- ssh: `-t` for attach, `-o BatchMode=yes` for listing (never hang on prompt), `ConnectTimeout=5`, `-- <alias>` terminator.
- Remote command words shell-quoted (single-quote w/ `'\''` escaping); session names are the untrusted input.
- `is_no_sessions`: ONLY a real command exit (with stderr) can be benign; 126/127/255 always unreachable; marker matched as line prefix.
- scan timeout (6s) > ssh connect timeout (5s); `detail_timeout` 6s; `scan_concurrency` 8.
- Tree recency: most-recent session is the default cursor; host preview = its first (recency-sorted) session.
- Filter dead-end guard (XM-01): non-matching filter falls back to sources-only, never empty.
- Rename refuses a `-`-leading name (getopt eats it → silent no-op).
- Detach-to-home model: cross-server pick = detach-client; home loop re-scans + re-renders.

---

## Port order & status

- [x] 0. Scaffold: Cargo (lib+bin), `.gitignore` `/target/`, Go → `legacy-go/`, build+test green
- [x] 1. session — types + parse_target (3 tests) ✅
- [x] 2. mux — argv builders + parsers (26 tests) ✅
- [x] 3. config — TOML + ssh-config discovery (10 tests) ✅
- [x] 4. source — local/ssh boundary, quoting, runner, classify (14 tests) ✅
- [x] 5. discovery — tokio bounded concurrent scan (6 tests) ✅
- [x] 6. manage — lifecycle ops (11 tests) ✅
- [x] 7. attach — handover + switch plan (8 tests, incl. live Windows OsExecer) ✅
- [x] 8. ui::tree — pure model (19 tests) ✅
- [x] 9. ui::ansi — ANSI → ratatui Text (9 tests) ✅
- [ ] 10. control — socket protocol + client (interprocess)
- [ ] 11. ui::switcher — ratatui TUI (the big one)
- [ ] 12. env — runtime wiring
- [ ] 13. main — clap CLI + home/popup/ls/attach/doctor/ctl
- [ ] 14. Optimize pass — startup/memory measurements; async scan tuning
- [ ] 15. UX pass — help overlay, keybindings, visual hierarchy
- [ ] 16. Final verification + README update; write `C:\Projects\tmp\rust-rewrite\DONE`

---

## References used
- ratatui app structure / event loop — https://ratatui.rs/ , https://docs.rs/ratatui/latest/ratatui/
- ratatui async event stream — https://ratatui.rs/tutorials/counter-async-app/async-event-stream/
- tokio process Command + `tokio::time::timeout` — https://docs.rs/tokio/latest/tokio/process/struct.Command.html
- clap derive subcommands — https://github.com/clap-rs/clap/blob/master/examples/git-derive.rs
- interprocess (cross-platform local socket; named pipe on Windows, AF_UNIX on unix) — https://docs.rs/interprocess/latest/interprocess/

## Notes / decisions
- **Runtime:** lean toward `tokio` current-thread flavor for low startup/memory; the
  discovery scan, preview poller, and control server are all I/O-bound so single-thread
  concurrency suffices. (Confirm when wiring the switcher.)
- **Release profile:** `opt-level="z"`, LTO, 1 codegen unit, panic=abort, strip — size+startup.
- **Cargo.lock committed** (xmux is a binary).
- **Control socket on Windows:** Go used AF_UNIX (Go supports it on Win). Rust std lacks
  Windows AF_UNIX (unstable); use `interprocess` local sockets, or `uds_windows`. Decide at module 10.
- **render dump:** Go drew to a tcell SimulationScreen; Rust equivalent is ratatui
  `TestBackend` (in-memory `Buffer`) → flatten cells to string.
- **legacy-go/** builds clean (`go build ./...` RC=0) — reference preserved.
