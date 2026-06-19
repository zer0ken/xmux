# Control-mode re-architecture — Implementation Plan

REQUIRED-SUB-SKILL: superpowers:test-driven-development

**Goal**: Re-architect xmux's attach layer onto tmux/psmux **control mode** (`-CC`). xmux holds **one control-mode connection per host** (local + each remote). Each connection enumerates and tracks that host's tree (via `list-sessions`/`list-windows` plus live `%sessions-changed`/`%window-*` notifications) and drives the live view: `switch-client -t <session>` re-points the connection, its `%output` stream feeds a vt100 `Grid`, and the `Grid` is rendered to ratatui in **both** UI states (no raw passthrough). Human keystrokes for the selected session are forwarded to its active pane via batched `send-keys -H`; xmux stays programmatically controllable via the `xmux ctl` socket. The number of child processes is bounded by the number of hosts, not the number of sessions. This closes live-attach review issues #1–#10 and supersedes the per-session PTY-proxy attach model (`Attachment`/`AttachRegistry`/`LiveOwner`/raw-passthrough).

**Architecture**: A single-threaded tokio runtime (`#[tokio::main(flavor = "current_thread")]`) owns the cockpit event loop. All blocking work lives on dedicated OS threads per host:

- `proxy::control_proto` (new) — **pure**, headless-testable wire functions: `%output` octal decode into a reused buffer, line classification + `%begin/%end/%error` framing, the full notification parse table, the `send-keys -H` batched-hex builder, and the `refresh-client -C WxH` formatter. No I/O.
- `host` (new, `src/host.rs`) — the heart: `HostClient` (one per host) owns the `-CC` child process, a **reader thread** (frame + notification dispatch + `%output`→`Grid` via the pure fns), a **writer thread** (drains a FIFO `HostCmd` channel, writes command lines, pushes correlation entries), a per-host `HostInventory` (sessions/windows, attached session id, active pane id), a shared `Grid`, and `connecting` state. Lazy spawn, teardown on `%exit`, lazy reconnect-on-next-selection.
- `host::manager` (new submodule or struct in `host.rs`) — `HostManager`, an `alias → HostClient` map bounded by host count (no cap/eviction). Lazy connect, reconnect, teardown-all.
- `proxy::screen::Grid` (kept) — the vt100 `Grid` + `render_into`/`resize`/`cursor`/`vt_cell_style` bridge, rendered to ratatui in both states.
- `ui::switcher::Switcher` (modified) — the tree/nav/filter/rename/kill model, fed from `HostInventory` deltas via the kept `apply_*` methods; gains a per-session spinner and a `select = attach` cursor hook; loses the dwell + Enter-picks/Esc-cancels flow.
- `proxy::input::InputMachine` (kept) — prefix interception in Passthrough; its `Forward(bytes)` now feeds the `send-keys -H` builder.
- `proxy::decode::KeyDecoder` (kept) — drives Overlay navigation.
- `control` + `ui::run::serve_control`/`dispatch`/`dump_switcher` (kept, extended) — `xmux ctl` key/text/dump, where `dump` now captures the live `Grid` view.
- `cockpit::run_cockpit` (rewritten) — wires `HostManager` + the control socket + the two-state (`Overlay`/`Passthrough`) chrome over a shared selected session; enters/clears/restores the alternate screen.

**Tech Stack**: Rust 2021, tokio (current_thread), ratatui 0.30 + crossterm 0.29, vt100 0.16, portable-pty 0.9 (kept only for `Child`/`CommandBuilder` to spawn the piped control child — **no ConPTY/openpty**), interprocess 2.4 (control socket), clap 4, anyhow, async-trait, thiserror.

## Global Constraints

- Branch `feat/rust-rewrite`. **Per-task local commits only — DO NOT push or merge.**
- **Windows toolchain (rustup shim blocked).** Set the real toolchain on PATH and as `RUSTC`/`RUSTDOC` before every cargo invocation (Bash tool):
  ```bash
  export PATH="/c/Users/hrlee/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin:$PATH"
  export RUSTC="/c/Users/hrlee/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin/rustc.exe"
  export RUSTDOC="/c/Users/hrlee/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin/rustdoc.exe"
  ```
- Verify each task with `cargo test` AND `cargo clippy --all-targets` (**0 warnings**). Redirect cargo output to a file and check `$?` — **NEVER pipe through `tail`/`head`** (it masks cargo's exit code):
  ```bash
  cargo test > /c/Users/hrlee/AppData/Local/Temp/claude/C--Projects-xmux/24e67497-fd7c-40e7-899a-445e38aef7e6/scratchpad/out.txt 2>&1; echo "EXIT=$?"
  ```
  Then `Read` the file. `cargo clippy --all-targets -- -D warnings` likewise.
- `cargo test` does NOT rebuild the bin — run `cargo build` before exercising the `xmux` binary live.
- **Control mode is text over pipes** — NO ConPTY, NO `ssh -t`. Local: `psmux -CC attach`; remote: `ssh <host> tmux -CC attach` (no `-t`). One connection per host.
- **Rendering ALWAYS Grid → ratatui** in both Overlay and Passthrough; ratatui owns stdout every frame. No raw-byte passthrough; no `LiveOwner`/single-writer gate; no status-bar clear-detect/repaint.
- Keep `attach::nest_guard`; **no self-mirror guard** (unreachable by construction).
- Decode `%output` into a **REUSED buffer** (no per-line allocation). Batch input into **one** `send-keys -H` per input burst. Flow control via `refresh-client -f pause-after=2`.
- **Test only via throwaway `jupiter06`; never the live local psmux server** (Windows = one shared server). Isolated socket (`-L <sock>`, `env -u PSMUX_SESSION -u TMUX`) for any headless live exercise; wrap non-self-bounding `-CC`/ssh children in a hard OS-level watchdog.
- `xmux ctl` must stay **first-class** and `dump` must render the **live Grid**.

## File Structure

| Path | Disposition | Single responsibility |
|---|---|---|
| `src/proxy/control_proto.rs` | **Create** | Pure wire functions: `%output`/`%extended-output` octal decode into a reused buffer; line classification + framing (`%begin`/`%end`/`%error`); notification parse table; `send-keys -H` batched-hex builder; `refresh-client -C WxH` formatter. No I/O. |
| `src/host.rs` | **Create** | `HostClient`, `HostInventory`, `HostCmd`, `HostEvent`, reader thread (framing + notif dispatch + `%output`→`Grid`), writer thread (FIFO + correlation), lazy spawn/teardown/reconnect; `HostManager` (alias→client map, bounded by host count). |
| `src/source.rs` | **Modify** | Add `control_argv` builder (local `<bin> -CC attach` + `-S <socket>` injection; remote `ssh <host> <bin> -CC attach`, no `-t`). Keep `list_sessions`/`run`/`exec_argv`/`quote`/`remote_command`/`attach_command` for `ls`/`doctor`/`attach`. |
| `src/cockpit.rs` | **Modify (rewrite loop)** | The cockpit event loop: lazy `HostManager`; cursor move (real or ctl) = `select = attach`; fold `HostEvent`s into `select!`; draw every frame from the selected client's `Grid`; toggle Overlay/Passthrough on `prefix s`; quit on `prefix q`/`q`; alt-screen enter/clear/restore. Removes `attach_into_registry`/`handle_switcher_outcome`/`enter_passthrough`/`status_bar_bytes`/dwell poll/eof channel/probe wiring. |
| `src/proxy/app.rs` | **Modify** | Replace stdout-owner `AppState` with `enum AppState { Overlay, Passthrough }` over the shared selected session; remove `prev_fg`/`LiveOwner`/`enter_passthrough` byte-painting/`esc_target`. |
| `src/ui/switcher.rs` | **Modify** | Remove dwell (`DWELL`/`dwell_*`/fill render/`attached` set) and the Enter-picks/Esc-cancels flow (`result`/`chosen`/`on_enter`/`choose`/`take_esc`/`esc_requested`). Add per-session spinner state + a `select = attach` cursor hook. Rewrite footer/help (#8); one-line title (#6); confirm arrow + `j`/`k` nav (#2). Keep tree/nav/filter/rename/kill/ordering/`apply_*`. |
| `src/ui/run.rs` | **Modify** | Keep `serve_control`/`accept_loop`/`handle_conn`/`dispatch`/`dump_switcher` + `ControlHandle`. **Remove** `event_loop`, `Cmd::SourceResult`/`Cmd::Panes`/`Cmd::OpDone`/`Cmd::Mouse`/`Cmd::Resize`, `spawn_probes`/`spawn_panes`/`handle_mouse` (callerless after the cockpit rewrite). `Cmd` shrinks to `Key`/`Dump`. |
| `src/proxy/run.rs` | **Remove (file)** | `Attachment`/`spawn_attachment`/ConPTY pump/`pty_control_loop`/`MasterSink`/`LiveOwner`/`scan_clear`/`pump_write`/test scaffolding all removed. `RawGuard` + `parse_prefix` relocate to `src/proxy/term.rs`. |
| `src/proxy/term.rs` | **Create** | `RawGuard` (paired with alt-screen RAII) + `parse_prefix` (sourced from `[ui] prefix`). The two load-bearing survivors of `run.rs`. |
| `src/proxy/registry.rs` | **Remove (file)** | `AttachRegistry` has no successor; the `HostManager` set lives in `host.rs`. |
| `src/proxy/mod.rs` | **Modify** | `pub mod registry; pub mod run;` removed; `pub mod control_proto; pub mod term;` added. |
| `src/lib.rs` | **Modify** | `pub mod host;` added. |
| `src/config.rs` | **Modify** | Remove `keep_cap` from `UiConfig` (+ `default_keep_cap`/`keep_cap()`/clamp + its tests). Keep `prefix`. |
| `src/env.rs` | **Modify** | Drop `cfg.keep_cap()` threading; cockpit builds clients from `env.srcs`/`env.by_alias`. Keep `EnvOps`/`scan`/`ops`/`ui_prefix` for `ls`/`doctor`. |
| `src/main.rs` | **Modify (small)** | Update the runtime doc comment (no PTY pump/control thread); keep `current_thread`, `nest_guard`, all subcommands. |
| `src/control.rs` | **Keep (+ optional verbs)** | Wire protocol unchanged; the new ctl verbs (`passthrough`/`overlay`/`keys`) parse here if implemented. |
| `src/mux.rs` | **Keep** | `SESSION_FORMAT`/`PANE_FORMAT`/builders/parsers reused to build control-mode command lines and parse `%begin…%end` bodies. |

---

### Task 1: `control_proto` — `%output` octal decode (reused buffer)

The decode of `%output`/`%extended-output` data is the hottest path and the most wire-sensitive; it lands first as a pure function over a caller-owned buffer so no allocation happens per line.

**Files**
- Create: `src/proxy/control_proto.rs`
- Modify: `src/proxy/mod.rs` (add `pub mod control_proto;`)
- Test: inline `#[cfg(test)] mod tests` in `src/proxy/control_proto.rs`

**Interfaces**
- Produces:
  - `pub fn decode_output_into(out: &mut Vec<u8>, data: &[u8])` — clears `out`, then appends decoded bytes; every `\ooo` (exactly 3 octal digits) → one byte, all other bytes literal.
  - `pub fn strip_extended_prefix(data: &[u8]) -> &[u8]` — for `%extended-output`, returns the slice after the single `:` separator (data part), or `data` unchanged if no `:` is present.

**Steps**
- [ ] Write failing test `decode_handles_octal_and_literals` covering the exact `[research §4]` cases:
  ```rust
  #[test]
  fn decode_handles_octal_and_literals() {
      let mut out = Vec::new();
      decode_output_into(&mut out, b"\\134");        // backslash
      assert_eq!(out, b"\\");
      decode_output_into(&mut out, b"\\012");        // LF
      assert_eq!(out, b"\n");
      decode_output_into(&mut out, b"\\015");        // CR
      assert_eq!(out, b"\r");
      decode_output_into(&mut out, b"\\000");        // NUL
      assert_eq!(out, b"\x00");
      decode_output_into(&mut out, b"\\033[31mhi");  // ESC + literal tail
      assert_eq!(out, b"\x1b[31mhi");
      decode_output_into(&mut out, &[0xc3, 0xa9]);   // UTF-8 é passes through raw
      assert_eq!(out, &[0xc3, 0xa9]);
      decode_output_into(&mut out, b"\x7f");          // DEL (0x7f) is NOT escaped
      assert_eq!(out, b"\x7f");
  }
  ```
- [ ] Run (expect FAIL — function does not exist):
  ```bash
  cargo test -p xmux control_proto::tests::decode_handles_octal_and_literals > .../out.txt 2>&1; echo "EXIT=$?"
  ```
  Expected: compile error / unresolved `decode_output_into`.
- [ ] Write failing test `decode_reuses_buffer_no_growth_per_call` (the buffer is cleared and reused, not appended-to across calls; capacity is retained):
  ```rust
  #[test]
  fn decode_reuses_buffer_no_growth_per_call() {
      let mut out = Vec::with_capacity(64);
      decode_output_into(&mut out, b"first");
      let cap = out.capacity();
      decode_output_into(&mut out, b"second");
      assert_eq!(out, b"second", "each call refills from scratch");
      assert!(out.capacity() >= cap, "capacity is retained for reuse");
  }
  ```
- [ ] Write failing test `strip_extended_prefix_drops_age_and_future_args`:
  ```rust
  #[test]
  fn strip_extended_prefix_drops_age_and_future_args() {
      // %extended-output %0 <age> ...future... : <data>  — the reader has already
      // split off "%extended-output %0 "; this fn gets "<age> ... : <data>".
      assert_eq!(strip_extended_prefix(b"512 : \\033[2J"), b"\\033[2J");
      assert_eq!(strip_extended_prefix(b"7 future stuff : payload"), b"payload");
      assert_eq!(strip_extended_prefix(b"no colon here"), b"no colon here");
  }
  ```
- [ ] Minimal impl (grounded in `[research §4]`'s decode algorithm; the `: ` strip per `[research §3]`/`§8`):
  ```rust
  //! Pure control-mode (`-CC`) wire functions: `%output` octal decode, line
  //! framing/classification, the notification parse table, the `send-keys -H`
  //! batched-hex builder, and the `refresh-client -C WxH` formatter. No I/O — every
  //! wire detail is unit-testable headlessly against tmux 3.3.x.

  #[inline]
  fn is_octal_digit(b: u8) -> bool {
      (b'0'..=b'7').contains(&b)
  }

  /// Decodes a `%output` value into `out` (cleared first, capacity retained). Every
  /// `\ooo` 3-digit octal escape becomes one byte; all other bytes pass through.
  pub fn decode_output_into(out: &mut Vec<u8>, data: &[u8]) {
      out.clear();
      let n = data.len();
      let mut i = 0;
      while i < n {
          let b = data[i];
          if b == b'\\'
              && i + 3 < n
              && is_octal_digit(data[i + 1])
              && is_octal_digit(data[i + 2])
              && is_octal_digit(data[i + 3])
          {
              let v = (data[i + 1] - b'0') * 64 + (data[i + 2] - b'0') * 8 + (data[i + 3] - b'0');
              out.push(v);
              i += 4;
          } else {
              out.push(b);
              i += 1;
          }
      }
  }

  /// For `%extended-output`, returns the data part after the single `:` separator
  /// (`[research §3, §8]`); the bytes up to and including the first `:` are the age
  /// and future-args field, ignored. No `:` ⇒ the input is returned unchanged.
  pub fn strip_extended_prefix(data: &[u8]) -> &[u8] {
      match data.iter().position(|&b| b == b':') {
          Some(i) => {
              let rest = &data[i + 1..];
              // tmux emits "<age> ... : <data>" with a space after the colon.
              rest.strip_prefix(b" ").unwrap_or(rest)
          }
          None => data,
      }
  }
  ```
- [ ] Run (expect PASS):
  ```bash
  cargo test -p xmux control_proto::tests > .../out.txt 2>&1; echo "EXIT=$?"
  ```
- [ ] `cargo clippy --all-targets -- -D warnings` (0 warnings).
- [ ] Commit: `feat(proxy): add control-mode %output octal decode into a reused buffer`

---

### Task 2: `control_proto` — line classification + `%begin/%end/%error` framing

**Files**
- Modify: `src/proxy/control_proto.rs`
- Test: inline tests

**Interfaces**
- Produces:
  - `pub enum Line<'a>` — `Begin { num: u64 }`, `End { num: u64 }`, `Error { num: u64 }`, `Output { pane: &'a str, data: &'a str }`, `ExtendedOutput { pane: &'a str, rest: &'a str }`, `Notification(Notif<'a>)`, `Body(&'a str)`. (`Body` is what an in-block non-guard line is; the reader decides Body vs Notification by its IDLE/IN_BLOCK state — this fn classifies the *shape* only.)
  - `pub fn classify(line: &str) -> Line<'_>` — classifies one stdout line (already stripped of the trailing `\n`). A line starting with `%` that is not a recognized control verb is `Notification(Notif::Other)`; a line not starting with `%` is `Body`.
- Consumes: `Notif` (Task 3) — define `Notif` in Task 3 and have `classify` return `Line::Notification(parse_notif(line))`. To keep tasks independently green, Task 2 introduces a **placeholder** `pub enum Notif<'a> { Raw(&'a str) }` and Task 3 expands it; `classify` returns `Notification(Notif::Raw(line))` for any `%…` that is not begin/end/error/output/extended-output. Task 3 replaces that arm.

**Steps**
- [ ] Write failing test `classify_frames_and_output`:
  ```rust
  #[test]
  fn classify_frames_and_output() {
      assert!(matches!(classify("%begin 1363006971 2 1"), Line::Begin { num: 2 }));
      assert!(matches!(classify("%end 1363006971 2 1"), Line::End { num: 2 }));
      assert!(matches!(classify("%error 1363006971 5 0"), Line::Error { num: 5 }));
      match classify("%output %0 hi\\012") {
          Line::Output { pane, data } => { assert_eq!(pane, "%0"); assert_eq!(data, "hi\\012"); }
          _ => panic!("expected Output"),
      }
      match classify("%extended-output %3 512 : \\033[2J") {
          Line::ExtendedOutput { pane, rest } => { assert_eq!(pane, "%3"); assert_eq!(rest, "512 : \\033[2J"); }
          _ => panic!("expected ExtendedOutput"),
      }
      assert!(matches!(classify("0: ksh* (1 panes)"), Line::Body("0: ksh* (1 panes)")));
  }
  ```
- [ ] Run (expect FAIL — `Line`/`classify` undefined).
- [ ] Write failing test `classify_begin_num_is_the_correlator` (the `<num>` is the 2nd field; `<flags>` is ignored per `[research §2]`):
  ```rust
  #[test]
  fn classify_begin_num_is_the_correlator() {
      // %begin <time> <num> <flags> — num is field 2, flags (field 3) ignored.
      assert!(matches!(classify("%begin 999 17 0"), Line::Begin { num: 17 }));
      // A malformed begin (non-numeric num) classifies as an Other notification,
      // never panics.
      assert!(matches!(classify("%begin x y z"), Line::Notification(_)));
  }
  ```
- [ ] Minimal impl (a `%output`/`%extended-output` line is `%<verb> %<pane> <rest>`; split on the first two spaces):
  ```rust
  #[derive(Debug, PartialEq)]
  pub enum Line<'a> {
      Begin { num: u64 },
      End { num: u64 },
      Error { num: u64 },
      Output { pane: &'a str, data: &'a str },
      ExtendedOutput { pane: &'a str, rest: &'a str },
      Notification(Notif<'a>),
      Body(&'a str),
  }

  /// Placeholder until Task 3; `classify` returns this for any unrecognized `%…`.
  #[derive(Debug, PartialEq)]
  pub enum Notif<'a> {
      Raw(&'a str),
  }

  fn frame_num(line: &str) -> Option<u64> {
      // "%<verb> <time> <num> <flags>" — num is the 3rd whitespace field.
      line.split_whitespace().nth(2)?.parse().ok()
  }

  pub fn classify(line: &str) -> Line<'_> {
      if let Some(rest) = line.strip_prefix("%begin ") {
          if let Some(num) = frame_num_after(rest) {
              return Line::Begin { num };
          }
          return Line::Notification(Notif::Raw(line));
      }
      if let Some(rest) = line.strip_prefix("%end ") {
          if let Some(num) = frame_num_after(rest) {
              return Line::End { num };
          }
          return Line::Notification(Notif::Raw(line));
      }
      if let Some(rest) = line.strip_prefix("%error ") {
          if let Some(num) = frame_num_after(rest) {
              return Line::Error { num };
          }
          return Line::Notification(Notif::Raw(line));
      }
      if let Some(rest) = line.strip_prefix("%output ") {
          // "%N <data>" — pane up to first space, data is the remainder verbatim.
          if let Some((pane, data)) = rest.split_once(' ') {
              return Line::Output { pane, data };
          }
      }
      if let Some(rest) = line.strip_prefix("%extended-output ") {
          if let Some((pane, rest)) = rest.split_once(' ') {
              return Line::ExtendedOutput { pane, rest };
          }
      }
      if line.starts_with('%') {
          return Line::Notification(Notif::Raw(line));
      }
      Line::Body(line)
  }

  /// `rest` is the line with the verb+space stripped: "<time> <num> <flags>".
  /// `<num>` is the SECOND field; `<flags>` is ignored (`[research §2]`).
  fn frame_num_after(rest: &str) -> Option<u64> {
      rest.split_whitespace().nth(1)?.parse().ok()
  }
  ```
  (Drop the unused `frame_num` helper — keep only `frame_num_after`.)
- [ ] Run (expect PASS).
- [ ] `cargo clippy --all-targets -- -D warnings`.
- [ ] Commit: `feat(proxy): classify control-mode lines and %begin/%end/%error frames`

---

### Task 3: `control_proto` — notification parse table

**Files**
- Modify: `src/proxy/control_proto.rs`
- Test: inline tests

**Interfaces**
- Produces (replaces the Task-2 placeholder `Notif`):
  ```rust
  pub enum Notif<'a> {
      SessionChanged { id: &'a str, name: &'a str },
      SessionsChanged,
      WindowAdd { window: &'a str },
      WindowClose { window: &'a str },
      WindowRenamed { window: &'a str, name: &'a str },
      WindowPaneChanged { window: &'a str, pane: &'a str },
      SessionWindowChanged { session: &'a str, window: &'a str },
      LayoutChange { window: &'a str },
      Pause { pane: &'a str },
      Continue { pane: &'a str },
      Exit { reason: Option<&'a str> },
      ClientDetached,
      Other,
  }
  pub fn parse_notif(line: &str) -> Notif<'_>;
  ```
- Consumes: `classify` (Task 2) — change its unrecognized-`%` arms to return `Line::Notification(parse_notif(line))`.

**Steps**
- [ ] Write failing test `parse_notif_full_table` (verbatim arg forms from `[research §3]`):
  ```rust
  #[test]
  fn parse_notif_full_table() {
      assert!(matches!(parse_notif("%session-changed $1 work"),
          Notif::SessionChanged { id: "$1", name: "work" }));
      assert!(matches!(parse_notif("%sessions-changed"), Notif::SessionsChanged));
      assert!(matches!(parse_notif("%window-add @4"), Notif::WindowAdd { window: "@4" }));
      assert!(matches!(parse_notif("%window-close @4"), Notif::WindowClose { window: "@4" }));
      assert!(matches!(parse_notif("%window-renamed @4 logs"),
          Notif::WindowRenamed { window: "@4", name: "logs" }));
      assert!(matches!(parse_notif("%window-pane-changed @4 %9"),
          Notif::WindowPaneChanged { window: "@4", pane: "%9" }));
      assert!(matches!(parse_notif("%session-window-changed $1 @4"),
          Notif::SessionWindowChanged { session: "$1", window: "@4" }));
      assert!(matches!(parse_notif("%pause %9"), Notif::Pause { pane: "%9" }));
      assert!(matches!(parse_notif("%continue %9"), Notif::Continue { pane: "%9" }));
      assert!(matches!(parse_notif("%client-detached client0"), Notif::ClientDetached));
      assert!(matches!(parse_notif("%unlinked-window-add @9"), Notif::Other));
  }
  ```
- [ ] Run (expect FAIL — variants/`parse_notif` undefined).
- [ ] Write failing test `parse_notif_exit_reason_optional`:
  ```rust
  #[test]
  fn parse_notif_exit_reason_optional() {
      assert!(matches!(parse_notif("%exit"), Notif::Exit { reason: None }));
      assert!(matches!(parse_notif("%exit too far behind"),
          Notif::Exit { reason: Some("too far behind") }));
  }
  ```
- [ ] Minimal impl (split on whitespace; `%session-changed`/`%session-window-changed`/`%window-renamed` take rejoined-or-single trailing args). Wire it into `classify` (replace the three `Notif::Raw(line)` and the trailing-`%` arms with `Notif::Other`/`parse_notif`).
  ```rust
  pub fn parse_notif(line: &str) -> Notif<'_> {
      let mut it = line.splitn(4, ' ');
      let verb = it.next().unwrap_or("");
      match verb {
          "%session-changed" => match (it.next(), it.next()) {
              (Some(id), Some(name)) => Notif::SessionChanged { id, name },
              _ => Notif::Other,
          },
          "%sessions-changed" => Notif::SessionsChanged,
          "%window-add" => it.next().map_or(Notif::Other, |w| Notif::WindowAdd { window: w }),
          "%window-close" => it.next().map_or(Notif::Other, |w| Notif::WindowClose { window: w }),
          "%window-renamed" => match (it.next(), it.next()) {
              (Some(w), Some(name)) => Notif::WindowRenamed { window: w, name },
              _ => Notif::Other,
          },
          "%window-pane-changed" => match (it.next(), it.next()) {
              (Some(w), Some(p)) => Notif::WindowPaneChanged { window: w, pane: p },
              _ => Notif::Other,
          },
          "%session-window-changed" => match (it.next(), it.next()) {
              (Some(s), Some(w)) => Notif::SessionWindowChanged { session: s, window: w },
              _ => Notif::Other,
          },
          "%layout-change" => it.next().map_or(Notif::Other, |w| Notif::LayoutChange { window: w }),
          "%pause" => it.next().map_or(Notif::Other, |p| Notif::Pause { pane: p }),
          "%continue" => it.next().map_or(Notif::Other, |p| Notif::Continue { pane: p }),
          "%client-detached" => Notif::ClientDetached,
          "%exit" => {
              let reason = line.strip_prefix("%exit").map(str::trim).filter(|s| !s.is_empty());
              Notif::Exit { reason }
          }
          _ => Notif::Other, // %unlinked-*, %client-session-changed, %subscription-changed, etc.
      }
  }
  ```
- [ ] Run (expect PASS). Re-run Task-2 tests (`classify_*`) to confirm the rewired `classify` still passes.
- [ ] `cargo clippy --all-targets -- -D warnings`.
- [ ] Commit: `feat(proxy): parse the control-mode notification table`

---

### Task 4: `control_proto` — `send-keys -H` builder + `refresh-client -C` formatter

**Files**
- Modify: `src/proxy/control_proto.rs`
- Test: inline tests

**Interfaces**
- Produces:
  - `pub fn send_keys_line(pane: &str, bytes: &[u8]) -> String` — one batched `send-keys -t <pane> -H <hex> <hex> …\n` line (each byte a 2-digit lowercase hex arg), newline-terminated. Empty `bytes` ⇒ empty string (caller skips).
  - `pub fn refresh_size_line(cols: u16, rows: u16) -> String` — `refresh-client -C <cols>x<rows>\n` (the `x`-form, `[research §7]`).
  - `pub fn pause_after_line(secs: u32) -> String` — `refresh-client -f pause-after=<secs>\n` (`[research §8]`).
  - `pub fn continue_pane_line(pane: &str) -> String` — `refresh-client -A <pane>:continue\n` (`[research §8]`).

**Steps**
- [ ] Write failing test `send_keys_batches_bytes_as_hex` (`[research §5]` — `-H`, one hex byte per arg, no `-K` in 3.3):
  ```rust
  #[test]
  fn send_keys_batches_bytes_as_hex() {
      // Ctrl-C then "ab": one line, hex per byte.
      assert_eq!(send_keys_line("%0", &[0x03, b'a', b'b']), "send-keys -t %0 -H 03 61 62\n");
      // Up-arrow CSI ESC [ A → 1b 5b 41.
      assert_eq!(send_keys_line("%2", b"\x1b[A"), "send-keys -t %2 -H 1b 5b 41\n");
      assert_eq!(send_keys_line("%0", b""), "", "empty burst yields no command");
  }
  ```
- [ ] Run (expect FAIL).
- [ ] Write failing test `size_and_flow_control_lines` (`[research §7, §8]`):
  ```rust
  #[test]
  fn size_and_flow_control_lines() {
      assert_eq!(refresh_size_line(80, 24), "refresh-client -C 80x24\n");  // x-form, NOT comma
      assert_eq!(pause_after_line(2), "refresh-client -f pause-after=2\n");
      assert_eq!(continue_pane_line("%5"), "refresh-client -A %5:continue\n");
  }
  ```
- [ ] Minimal impl:
  ```rust
  use std::fmt::Write as _;

  /// One batched `send-keys -H` line forwarding `bytes` to `pane` (`[research §5]`).
  /// `-H` is the faithful raw-byte path in 3.3 (no `-K`); each byte is one hex arg.
  pub fn send_keys_line(pane: &str, bytes: &[u8]) -> String {
      if bytes.is_empty() {
          return String::new();
      }
      let mut s = format!("send-keys -t {pane} -H");
      for b in bytes {
          let _ = write!(s, " {b:02x}");
      }
      s.push('\n');
      s
  }

  /// `refresh-client -C WxH` — the `x`-form is correct for 3.3.x (`[research §7]`).
  pub fn refresh_size_line(cols: u16, rows: u16) -> String {
      format!("refresh-client -C {cols}x{rows}\n")
  }

  /// Enables flow control so a firehose pane cannot make the client buffer
  /// unbounded or be killed "too far behind" (`[research §8]`).
  pub fn pause_after_line(secs: u32) -> String {
      format!("refresh-client -f pause-after={secs}\n")
  }

  /// Resumes a paused pane after the renderer caught up (`[research §8]`).
  pub fn continue_pane_line(pane: &str) -> String {
      format!("refresh-client -A {pane}:continue\n")
  }
  ```
- [ ] Run (expect PASS).
- [ ] `cargo clippy --all-targets -- -D warnings`.
- [ ] Commit: `feat(proxy): add send-keys -H batcher and refresh-client command builders`

---

### Task 5: `source` — control-mode child argv builder

The cockpit no longer builds an `ssh -t` attach for the interactive view; it builds the `-CC` child argv. The non-interactive `attach_command` (`xmux attach`) is untouched.

**Files**
- Modify: `src/source.rs`
- Test: inline `#[cfg(test)] mod tests` in `src/source.rs`

**Interfaces**
- Consumes: `Source` (`alias`/`binary`/`remote`/`socket`/`ssh_args`/`remote_command` — all existing).
- Produces:
  - `pub fn control_argv(&self) -> Vec<String>` — the argv to spawn the host's control-mode child. Local: `<bin> [-S <socket>] -CC attach`. Remote: `ssh <ssh_args(false)…> "<bin> -CC attach"` — **no `-t`** (`ssh_args(false)` keeps `BatchMode=yes`/`ConnectTimeout`; `[research §1]`).

**Steps**
- [ ] Write failing test `control_argv_local_plain_and_socket`:
  ```rust
  #[test]
  fn control_argv_local_plain_and_socket() {
      let loc = src("local", "psmux", false, "", "");
      assert_eq!(loc.control_argv(), vec!["psmux", "-CC", "attach"]);
      // A local source on a non-default socket injects -S before -CC.
      let mut s = src("local", "tmux", false, "linux", "");
      s.socket = Some("/tmp/tmux-1000/work".into());
      assert_eq!(s.control_argv(), vec!["tmux", "-S", "/tmp/tmux-1000/work", "-CC", "attach"]);
  }
  ```
- [ ] Run (expect FAIL — `control_argv` undefined).
- [ ] Write failing test `control_argv_remote_no_tty`:
  ```rust
  #[test]
  fn control_argv_remote_no_tty() {
      let rem = src("prod", "tmux", true, "linux", "");
      let got = rem.control_argv();
      assert_eq!(got[0], "ssh");
      assert!(!got.iter().any(|s| s == "-t"), "control mode is pipes, never ssh -t: {got:?}");
      assert!(got.iter().any(|s| s.contains("BatchMode=yes")), "{got:?}");
      assert_eq!(got.last().unwrap(), "tmux -CC attach");
  }
  ```
- [ ] Minimal impl (mirror `exec_argv`'s local `-S` injection; remote wraps with `ssh_args(false)` + `remote_command`):
  ```rust
  /// The argv that spawns this source's control-mode (`-CC`) child. Control mode is
  /// a text protocol over pipes, so the remote form uses NO `ssh -t` (`[research
  /// §1]`); the local form injects `-S <socket>` exactly as [`Source::exec_argv`]
  /// does when xmux targets a non-default mux server.
  pub fn control_argv(&self) -> Vec<String> {
      if !self.remote {
          let mut v = vec![self.binary.clone()];
          if let Some(sock) = self.socket.as_deref().filter(|s| !s.is_empty()) {
              v.push("-S".into());
              v.push(sock.to_string());
          }
          v.push("-CC".into());
          v.push("attach".into());
          return v;
      }
      let mut args = self.ssh_args(false); // BatchMode + ConnectTimeout, no -t
      args.push(remote_command(&argv(&[&self.binary, "-CC", "attach"])));
      let mut v = vec!["ssh".to_string()];
      v.extend(args);
      v
  }
  ```
  Add a tiny local `fn argv(parts: &[&str]) -> Vec<String>` in `source.rs` (or inline a `vec![…]`) — `mux::argv` is private. Inline is simplest:
  ```rust
  args.push(remote_command(&[self.binary.clone(), "-CC".into(), "attach".into()]));
  ```
- [ ] Run (expect PASS).
- [ ] `cargo clippy --all-targets -- -D warnings`.
- [ ] Commit: `feat(source): build the -CC control-mode child argv (no ssh -t)`

---

### Task 6: `proxy::term` — relocate `RawGuard` + `parse_prefix`; alt-screen RAII (#1)

`run.rs` is gutted next, so its two survivors move first, and `RawGuard` is extended to own the alternate screen (issue #1: enter + clear on start, restore on exit/panic).

**Files**
- Create: `src/proxy/term.rs`
- Modify: `src/proxy/mod.rs` (add `pub mod term;`)
- Test: inline tests in `src/proxy/term.rs` (the `parse_prefix` cases; `TermGuard` enter/restore is asserted by construction + an #[ignore] console smoke).

**Interfaces**
- Produces:
  - `pub struct TermGuard;` with `pub fn enter() -> anyhow::Result<Self>` — enables raw mode AND enters the alternate screen (crossterm `EnterAlternateScreen`); `Drop` runs `LeaveAlternateScreen` + `disable_raw_mode` (restores the user's pre-launch screen on normal return AND panic — release builds unwind, see `Cargo.toml` `panic` note).
  - `pub fn parse_prefix(spec: Option<&str>) -> u8` — moved verbatim from `run.rs`.
- Consumes: `crossterm::terminal::{enable_raw_mode, disable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen}`, `crossterm::execute`.

**Steps**
- [ ] Write failing test `parse_prefix_recognises_specs_and_defaults` (moved from `run.rs`):
  ```rust
  #[test]
  fn parse_prefix_recognises_specs_and_defaults() {
      assert_eq!(parse_prefix(Some("C-g")), 0x07);
      assert_eq!(parse_prefix(Some("C-Space")), 0x00);
      assert_eq!(parse_prefix(Some("c-a")), 0x01);
      assert_eq!(parse_prefix(None), 0x07);
      assert_eq!(parse_prefix(Some("garbage")), 0x07);
  }
  ```
- [ ] Run (expect FAIL — module/fn not present yet in `term.rs`).
- [ ] Minimal impl — create `src/proxy/term.rs` with `parse_prefix` (copied verbatim from `run.rs:48-66`) and:
  ```rust
  use crossterm::execute;
  use crossterm::terminal::{
      disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
  };

  /// RAII guard owning the terminal for the cockpit's lifetime: enables raw mode
  /// and enters the alternate screen on construction (so pre-launch shell output
  /// never bleeds under the UI — issue #1), and on drop leaves the alternate
  /// screen + disables raw mode, restoring the user's pre-launch screen on normal
  /// return AND on a panic (release builds unwind; see Cargo.toml `panic`).
  pub struct TermGuard;

  impl TermGuard {
      pub fn enter() -> anyhow::Result<Self> {
          enable_raw_mode()?;
          execute!(std::io::stdout(), EnterAlternateScreen)?;
          Ok(TermGuard)
      }
  }

  impl Drop for TermGuard {
      fn drop(&mut self) {
          let _ = execute!(std::io::stdout(), LeaveAlternateScreen);
          let _ = disable_raw_mode();
      }
  }
  ```
  Add `pub mod term;` to `src/proxy/mod.rs`.
- [ ] Run (expect PASS).
- [ ] Add an `#[ignore]` console smoke `term_guard_enters_and_restores` documenting the human visual check (alt-screen enter/clear/restore), since raw-mode toggling needs a real console.
- [ ] `cargo clippy --all-targets -- -D warnings` (NOTE: `run.rs` still defines `RawGuard`/`parse_prefix` and is still referenced by `cockpit.rs`/`registry.rs` — those compile until Task 11/12 removes them; `term.rs` coexists. No duplicate-symbol issue since they are different modules.)
- [ ] Commit: `feat(proxy): add TermGuard (raw + alt-screen RAII) and relocate parse_prefix`

---

### Task 7: `host` — `HostInventory` + `HostCmd`/`HostEvent` types

The data carried between threads and into the cockpit lands before the threads themselves, so the cockpit-facing surface is fixed and unit-testable.

**Files**
- Create: `src/host.rs`
- Modify: `src/lib.rs` (add `pub mod host;`)
- Test: inline tests in `src/host.rs`

**Interfaces**
- Produces:
  ```rust
  /// One host's session/window inventory, seeded from list-sessions/list-windows
  /// and kept live by notifications. The cockpit reads it to (re)build the tree.
  pub struct HostInventory {
      pub sessions: Vec<crate::session::Session>,
      pub panes: std::collections::HashMap<String, Vec<crate::session::WindowPanes>>,
      pub attached_session: Option<String>, // name set by the last switch-client
      pub active_pane: Option<String>,      // "%N" of the attached session
  }
  impl HostInventory { pub fn new() -> Self; }

  /// A command for a host's writer thread. The writer builds the exact bytes.
  pub enum HostCmd {
      Send(String),                         // a ready command line (newline-terminated)
      SendKeys { pane: String, bytes: Vec<u8> },
      SwitchClient { target: String },
      Resize { cols: u16, rows: u16 },
      Shutdown,
  }

  /// A parsed event the reader emits to the cockpit's select! loop.
  pub enum HostEvent {
      Connected { host: String },           // first list-sessions returned
      Inventory { host: String },           // sessions/windows changed → rebuild tree
      Output { host: String },              // %output fed the grid → redraw
      Attached { host: String, session: String }, // %session-changed confirmed
      Exited { host: String, reason: Option<String> }, // %exit / EOF → reap
  }
  ```
- Consumes: `crate::session::{Session, WindowPanes}`.

**Steps**
- [ ] Write failing test `inventory_starts_empty`:
  ```rust
  #[test]
  fn inventory_starts_empty() {
      let inv = HostInventory::new();
      assert!(inv.sessions.is_empty());
      assert!(inv.attached_session.is_none());
      assert!(inv.active_pane.is_none());
  }
  ```
- [ ] Run (expect FAIL — module/types undefined).
- [ ] Write failing test `host_event_carries_host` (the cockpit routes by host alias):
  ```rust
  #[test]
  fn host_event_carries_host() {
      let e = HostEvent::Attached { host: "jupiter06".into(), session: "api".into() };
      match e {
          HostEvent::Attached { host, session } => {
              assert_eq!(host, "jupiter06");
              assert_eq!(session, "api");
          }
          _ => panic!("variant"),
      }
  }
  ```
- [ ] Minimal impl — create `src/host.rs` with the structs/enums above (+ `Default` for `HostInventory` delegating to `new`). Add `pub mod host;` to `lib.rs`.
- [ ] Run (expect PASS).
- [ ] `cargo clippy --all-targets -- -D warnings`.
- [ ] Commit: `feat(host): add HostInventory, HostCmd, and HostEvent types`

---

### Task 8: `host` — the reader thread (frame state machine + notif dispatch + `%output`→Grid)

The reader runs the line state machine over the child's stdout: it correlates `%begin…%end|%error` blocks against the in-flight FIFO, decodes `%output` into the reused buffer and feeds the `Grid`, and updates the shared `HostInventory` + emits `HostEvent`s. It is unit-tested by feeding canned protocol bytes through an in-memory reader (no real child).

**Files**
- Modify: `src/host.rs`
- Test: inline tests in `src/host.rs`

**Interfaces**
- Consumes: `control_proto::{classify, parse_notif, decode_output_into, strip_extended_prefix, Line, Notif}`, `crate::proxy::screen::Grid`, `crate::mux::{parse_sessions, parse_panes}`.
- Produces:
  ```rust
  /// The reader's shared state the cockpit also reads.
  pub struct ReaderState {
      pub grid: std::sync::Arc<std::sync::Mutex<Grid>>,
      pub inventory: std::sync::Arc<std::sync::Mutex<HostInventory>>,
      pub connecting: std::sync::Arc<std::sync::atomic::AtomicBool>,
  }

  /// The in-flight command correlation FIFO, shared with the writer.
  pub type InFlight = std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<PendingReply>>>;

  /// What a resolved %begin…%end block means to the reader.
  pub enum PendingReply { ListSessions, ListPanes { address: String }, ActivePane { session: String }, Ignore }

  /// Runs the line state machine over `lines` (an Iterator<Item=String> of stdout
  /// lines, already split on \n), driving `state`, `in_flight`, and emitting events
  /// via `emit`. Returns when the iterator ends (child EOF). Pure over its inputs so
  /// a test feeds canned bytes; the real reader wraps a BufRead.
  pub fn run_reader<E: FnMut(HostEvent)>(
      host: &str,
      lines: impl Iterator<Item = String>,
      state: &ReaderState,
      in_flight: &InFlight,
      mut emit: E,
  );
  ```

**Steps**
- [ ] Write failing test `reader_decodes_output_into_grid`:
  ```rust
  #[test]
  fn reader_decodes_output_into_grid() {
      let state = test_state(80, 24);
      let in_flight: InFlight = Default::default();
      let mut events = Vec::new();
      let lines = vec!["%output %0 HELLO\\012WORLD".to_string()].into_iter();
      run_reader("jupiter06", lines, &state, &in_flight, |e| events.push(e));
      let g = state.grid.lock().unwrap();
      let mut buf = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 80, 24));
      g.render_into(&mut buf, ratatui::layout::Rect::new(0, 0, 80, 24));
      assert_eq!(buf[(0, 0)].symbol(), "H");
      assert!(events.iter().any(|e| matches!(e, HostEvent::Output { .. })));
  }
  ```
- [ ] Run (expect FAIL).
- [ ] Write failing test `reader_resolves_list_sessions_block_into_inventory` (a `%begin…%end` whose front-of-FIFO entry is `ListSessions`; body is `SESSION_FORMAT` lines → `parse_sessions` → inventory + `Connected`/`Inventory` event):
  ```rust
  #[test]
  fn reader_resolves_list_sessions_block_into_inventory() {
      let state = test_state(80, 24);
      let in_flight: InFlight = Default::default();
      in_flight.lock().unwrap().push_back(PendingReply::ListSessions);
      let mut events = Vec::new();
      let lines = vec![
          "%begin 1 5 0".to_string(),
          "2\t1\t1700000000\tapi".to_string(),
          "%end 1 5 0".to_string(),
      ].into_iter();
      run_reader("jupiter06", lines, &state, &in_flight, |e| events.push(e));
      let inv = state.inventory.lock().unwrap();
      assert_eq!(inv.sessions.len(), 1);
      assert_eq!(inv.sessions[0].name, "api");
      assert_eq!(inv.sessions[0].source, "jupiter06");
      assert!(events.iter().any(|e| matches!(e, HostEvent::Connected { .. })));
      assert!(!state.connecting.load(std::sync::atomic::Ordering::Acquire));
  }
  ```
- [ ] Write failing test `reader_session_changed_sets_attached_and_emits` and `reader_exit_emits_exited`:
  ```rust
  #[test]
  fn reader_session_changed_sets_attached_and_emits() {
      let state = test_state(80, 24);
      let in_flight: InFlight = Default::default();
      let mut events = Vec::new();
      run_reader("jupiter06",
          vec!["%session-changed $1 api".to_string()].into_iter(),
          &state, &in_flight, |e| events.push(e));
      assert_eq!(state.inventory.lock().unwrap().attached_session.as_deref(), Some("api"));
      assert!(events.iter().any(|e| matches!(e, HostEvent::Attached { session, .. } if session == "api")));
  }

  #[test]
  fn reader_exit_emits_exited() {
      let state = test_state(80, 24);
      let in_flight: InFlight = Default::default();
      let mut events = Vec::new();
      run_reader("jupiter06",
          vec!["%exit too far behind".to_string()].into_iter(),
          &state, &in_flight, |e| events.push(e));
      assert!(events.iter().any(|e|
          matches!(e, HostEvent::Exited { reason: Some(r), .. } if r == "too far behind")));
  }
  ```
- [ ] Minimal impl — a state machine with `IDLE`/`IN_BLOCK { num, kind, body }`, a reused decode `Vec<u8>`, and `%sessions-changed`/`%window-*` updating inventory and emitting `Inventory`. Key shape:
  ```rust
  pub fn run_reader<E: FnMut(HostEvent)>(
      host: &str,
      lines: impl Iterator<Item = String>,
      state: &ReaderState,
      in_flight: &InFlight,
      mut emit: E,
  ) {
      use control_proto::{classify, parse_notif, decode_output_into, strip_extended_prefix, Line, Notif};
      let mut decode_buf: Vec<u8> = Vec::with_capacity(4096);
      let mut block: Option<(u64, PendingReply, Vec<String>)> = None; // num, kind, body
      for line in lines {
          // Inside a block, only %end/%error close it; everything else is body
          // (notifications never appear inside a block — [research §2]).
          if let Some((num, _, body)) = block.as_mut() {
              match classify(&line) {
                  Line::End { num: n } | Line::Error { num: n } if n == *num => {
                      let (_, kind, body) = block.take().unwrap();
                      resolve_block(host, kind, &body, state, &mut emit);
                  }
                  _ => body.push(line),
              }
              continue;
          }
          match classify(&line) {
              Line::Begin { num } => {
                  let kind = in_flight.lock().unwrap().pop_front().unwrap_or(PendingReply::Ignore);
                  block = Some((num, kind, Vec::new()));
              }
              Line::Output { pane, data } => {
                  decode_output_into(&mut decode_buf, data.as_bytes());
                  feed_grid(state, pane, &decode_buf);
                  clear_connecting(state);
                  emit(HostEvent::Output { host: host.to_string() });
              }
              Line::ExtendedOutput { pane, rest } => {
                  let data = strip_extended_prefix(rest.as_bytes());
                  decode_output_into(&mut decode_buf, data);
                  feed_grid(state, pane, &decode_buf);
                  clear_connecting(state);
                  emit(HostEvent::Output { host: host.to_string() });
              }
              Line::Notification(n) => dispatch_notif(host, n, state, &mut emit),
              Line::End { .. } | Line::Error { .. } | Line::Body(_) => {} // stray outside a block
          }
      }
      // Iterator ended = child stdout EOF.
      emit(HostEvent::Exited { host: host.to_string(), reason: None });
  }
  ```
  with helpers `resolve_block` (`ListSessions` → `parse_sessions` into `inventory.sessions`, clear `connecting`, emit `Connected`+`Inventory`; `ListPanes` → `parse_panes` into `inventory.panes`, emit `Inventory`; `ActivePane` → parse `display-message` body `PANE=%N …`, set `active_pane`), `dispatch_notif` (`SessionChanged` → set `attached_session` + emit `Attached`; `SessionsChanged`/`WindowAdd|Close|Renamed` → emit `Inventory` (cockpit re-issues `list-sessions`/`list-windows`); `WindowPaneChanged`/`SessionWindowChanged` → set `active_pane`/active window; `Exit`/`ClientDetached` → emit `Exited`; `Pause` → record (cockpit resumes via `continue_pane_line`); `Continue`/`LayoutChange`/`Other` → noop), `feed_grid` (lock grid, `g.feed(bytes)` — pane filtering: v1 feeds the attached session's panes; route all `%output` to the single `Grid`), `clear_connecting` (`connecting.store(false)`).
- [ ] Run (expect PASS).
- [ ] `cargo clippy --all-targets -- -D warnings`.
- [ ] Commit: `feat(host): control-mode reader — framing, notifications, %output → Grid`

---

### Task 9: `host` — the writer thread + `HostClient` spawn/teardown

The writer drains the FIFO `HostCmd` channel, builds exact command bytes (via Task 4's builders), writes them to the child's stdin, and pushes correlation entries onto the in-flight FIFO. `HostClient::spawn` wires the piped child + both threads. Spawn is unit-tested with a fake command (`cmd.exe` echo) that produces no protocol but proves the pipes + thread join; the writer's command-building is tested purely.

**Files**
- Modify: `src/host.rs`
- Test: inline tests in `src/host.rs`

**Interfaces**
- Consumes: `control_proto::{send_keys_line, refresh_size_line, pause_after_line}`, `portable_pty::{CommandBuilder, Child}` (spawn the piped child — **stdin/stdout pipes, no PTY**; use `std::process::Command` with `Stdio::piped()` rather than `portable_pty::openpty`).
- Produces:
  ```rust
  pub struct HostClient {
      pub host: String,
      pub grid: Arc<Mutex<Grid>>,
      pub inventory: Arc<Mutex<HostInventory>>,
      pub connecting: Arc<AtomicBool>,
      cmd_tx: std::sync::mpsc::Sender<HostCmd>,
      child: std::process::Child,
      reader: Option<std::thread::JoinHandle<()>>,
      writer: Option<std::thread::JoinHandle<()>>,
      size: (u16, u16),
  }
  impl HostClient {
      pub fn spawn(host: &str, argv: &[String], cols: u16, rows: u16,
                   events: tokio::sync::mpsc::UnboundedSender<HostEvent>) -> anyhow::Result<HostClient>;
      pub fn list_sessions(&self, bin_argv: &[String]);     // queue list-sessions + push ListSessions
      pub fn switch_client(&self, session: &str);           // queue switch-client + display-message
      pub fn send_keys(&self, pane: &str, bytes: Vec<u8>);  // queue one batched send-keys
      pub fn resize(&mut self, cols: u16, rows: u16);       // queue refresh-client -C + grid.resize
      pub fn teardown(self);                                // Shutdown + kill + bounded join
  }
  /// The writer loop, extracted for unit-testing the command bytes it writes.
  pub fn run_writer<W: std::io::Write>(rx: std::sync::mpsc::Receiver<HostCmd>, w: &mut W, in_flight: &InFlight);
  ```

**Steps**
- [ ] Write failing test `writer_serializes_commands_and_correlates`:
  ```rust
  #[test]
  fn writer_serializes_commands_and_correlates() {
      let (tx, rx) = std::sync::mpsc::channel::<HostCmd>();
      let in_flight: InFlight = Default::default();
      tx.send(HostCmd::Send(crate::proxy::control_proto::pause_after_line(2))).unwrap();
      tx.send(HostCmd::SendKeys { pane: "%0".into(), bytes: vec![0x03] }).unwrap();
      tx.send(HostCmd::Resize { cols: 80, rows: 24 }).unwrap();
      tx.send(HostCmd::Shutdown).unwrap();
      drop(tx);
      let mut out: Vec<u8> = Vec::new();
      run_writer(rx, &mut out, &in_flight);
      let s = String::from_utf8(out).unwrap();
      assert!(s.contains("refresh-client -f pause-after=2\n"));
      assert!(s.contains("send-keys -t %0 -H 03\n"));
      assert!(s.contains("refresh-client -C 80x24\n"));
  }
  ```
- [ ] Run (expect FAIL — `run_writer`/`HostCmd` paths undefined).
- [ ] Write failing test `writer_switch_client_pushes_correlation` (a `SwitchClient` writes `switch-client -t <s>\n` and a `display-message …\n`, and pushes the matching `PendingReply::ActivePane` so the reader can resolve the block):
  ```rust
  #[test]
  fn writer_switch_client_pushes_correlation() {
      let (tx, rx) = std::sync::mpsc::channel::<HostCmd>();
      let in_flight: InFlight = Default::default();
      tx.send(HostCmd::SwitchClient { target: "api".into() }).unwrap();
      tx.send(HostCmd::Shutdown).unwrap();
      drop(tx);
      let mut out: Vec<u8> = Vec::new();
      run_writer(rx, &mut out, &in_flight);
      let s = String::from_utf8(out).unwrap();
      assert!(s.contains("switch-client -t api\n"), "{s}");
      assert!(s.contains("display-message -p -t api -F '#{pane_id}'\n"), "{s}");
      // one ActivePane reply queued for the display-message block.
      assert!(matches!(in_flight.lock().unwrap().front(), Some(PendingReply::ActivePane { .. })));
  }
  ```
- [ ] Minimal impl — `run_writer` matches each `HostCmd`, builds the line(s) via Task-4 builders (and `crate::mux`-derived `switch-client`/`display-message`), writes them, and pushes correlation entries onto `in_flight` for the commands that produce a `%begin…%end` (`list-sessions`/`list-panes`/`display-message`). `send-keys`/`switch-client`/`refresh-client` also produce a `%begin…%end` ack block — push `PendingReply::Ignore` for those so the reader pops one entry per `%begin`. `HostClient::spawn` opens `std::process::Command::new(argv[0]).args(&argv[1..]).stdin(piped).stdout(piped).stderr(piped)` with mux env cleared (`PSMUX_SESSION`/`TMUX`/`TMUX_PANE` removed), wraps stdout in `BufReader::lines()` feeding `run_reader` on the reader thread, and stdin on the writer thread; the connect sequence (`refresh_size_line`, `pause_after_line(2)`, then `list-sessions`) is queued immediately after spawn. `teardown` sends `Shutdown`, `child.kill()`, joins both threads with a bounded wait.
- [ ] Add an `#[ignore]` smoke `host_client_spawns_piped_child` that runs `cmd.exe`/`echo` to prove the pipes + bounded join return (no protocol asserted; real `-CC` is the live gate in Task 18). Wrap in a watchdog per Global Constraints.
- [ ] Run (expect PASS for the pure tests).
- [ ] `cargo clippy --all-targets -- -D warnings`.
- [ ] Commit: `feat(host): writer thread, command correlation, and HostClient spawn/teardown`

---

### Task 10: `host` — `HostManager` (lazy connect, reconnect, teardown-all)

**Files**
- Modify: `src/host.rs`
- Test: inline tests in `src/host.rs`

**Interfaces**
- Consumes: `HostClient`, `crate::source::Source` (`control_argv`), `crate::mux::list_sessions`.
- Produces:
  ```rust
  pub struct HostManager {
      clients: std::collections::HashMap<String, HostClient>,
      events: tokio::sync::mpsc::UnboundedSender<HostEvent>,
  }
  impl HostManager {
      pub fn new(events: tokio::sync::mpsc::UnboundedSender<HostEvent>) -> Self;
      /// Ensures `host`'s client is connected, spawning lazily from its Source. A
      /// no-op if already connected. Returns whether a fresh connect happened.
      pub fn ensure(&mut self, host: &str, src: &crate::source::Source, cols: u16, rows: u16) -> anyhow::Result<bool>;
      pub fn get(&self, host: &str) -> Option<&HostClient>;
      pub fn get_mut(&mut self, host: &str) -> Option<&mut HostClient>;
      pub fn reap(&mut self, host: &str);     // %exit/EOF: drop the client, keep last inventory? (drop client; cockpit keeps last tree via switcher state)
      pub fn resize_all(&mut self, cols: u16, rows: u16);
      pub fn teardown_all(self);
  }
  ```

**Steps**
- [ ] Write failing test `manager_ensure_is_idempotent` (using a test-only `HostClient` constructor or a fake `Source` whose `control_argv` runs a harmless `cmd.exe`/`true`). To keep this headless and deterministic, add `#[cfg(test)] fn insert_fake(&mut self, host: &str)` that inserts a client built from a `cmd.exe`-style no-op argv, and assert `ensure` does not re-spawn when present:
  ```rust
  #[test]
  fn manager_ensure_is_idempotent() {
      let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<HostEvent>();
      let mut mgr = HostManager::new(tx);
      mgr.insert_fake("jupiter06");
      assert!(mgr.get("jupiter06").is_some());
      // ensure on an already-connected host returns Ok(false) (no fresh connect).
      let src = fake_source("jupiter06");
      assert_eq!(mgr.ensure("jupiter06", &src, 80, 24).unwrap(), false);
  }
  ```
- [ ] Run (expect FAIL).
- [ ] Write failing test `manager_reap_drops_client`:
  ```rust
  #[test]
  fn manager_reap_drops_client() {
      let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<HostEvent>();
      let mut mgr = HostManager::new(tx);
      mgr.insert_fake("jupiter06");
      mgr.reap("jupiter06");
      assert!(mgr.get("jupiter06").is_none(), "reaped client is dropped");
  }
  ```
- [ ] Minimal impl — `ensure` returns `Ok(false)` if present, else `HostClient::spawn(host, &src.control_argv(), …)`, queues the connect sequence + first `list-sessions`, inserts, returns `Ok(true)`. `reap`/`teardown_all` call `HostClient::teardown` (bounded join). The fake helpers are `#[cfg(test)]`.
- [ ] Run (expect PASS).
- [ ] `cargo clippy --all-targets -- -D warnings`.
- [ ] Commit: `feat(host): HostManager — lazy connect, reap, teardown-all (bound = host count)`

---

### Task 11: `switcher` — remove dwell + Enter/Esc flow; add spinner + select-hook; rewrite chrome (#2/#5/#6/#7/#8)

The switcher loses the dwell machinery and the "Enter picks / Esc cancels" flow, gains a per-session spinner (driven by an animation tick) and a `select = attach` cursor hook, a one-line title, confirmed arrow + `j`/`k` nav, and a fully rewritten footer/help. The kept tree/nav/filter/rename/kill/ordering/`apply_*` are unchanged.

**Files**
- Modify: `src/ui/switcher.rs`
- Test: inline `#[cfg(test)] mod tests` in `src/ui/switcher.rs` (rewrite the dwell/footer/help/nav tests)

**Interfaces**
- Removed (no successor): `DWELL`, `dwell_start`/`dwell_addr`/`attached` fields, `dwell_candidate`/`rearm_dwell`/`dwell_pending`/`dwell_progress`/`note_attached`/`take_dwell_attach`, `result`/`SwitchResult`/`chosen`/`on_enter`/`choose`/`take_esc`/`esc_requested`/`clear_result`/`should_exit`/`quit`, the dwell fill render block in `render_tree`.
- Produces / kept-and-changed:
  - `pub fn current_attach_target(&self) -> Option<TerminalViewTarget>` — the session/window the cursor is on (the cockpit calls this on every cursor move to `switch-client`). Replaces `take_dwell_attach`. (`TerminalViewTarget` kept.)
  - `pub fn set_spinner(&mut self, addresses: std::collections::HashSet<String>)` — the set of session addresses currently connecting/awaiting-first-output (cockpit pushes this from `HostClient.connecting` + post-switch state).
  - `pub fn tick_spinner(&mut self)` — advances the braille frame index (cockpit calls on the animation tick).
  - `pub fn handle_key(&mut self, ev: KeyEvent)` — kept; arrow + `j`/`k` move; `/`/`n`/`R`/`x`/`r`/`?` unchanged; **`Enter` is a no-op**; `q` sets a kept `wants_quit` flag; `Esc` no longer special (closes input/help only).
  - `pub fn wants_quit(&self) -> bool` — replaces `should_exit`; the cockpit quits on it.
  - `pub fn render(&mut self, frame: &mut Frame, grid: Option<&Grid>)` — kept; one-line title (#6); footer/help rewritten (#8); spinner drawn right of the session name (#5); Overlay-only chrome (title/tree/footer hidden in Passthrough — but the switcher renders only the Overlay chrome; the cockpit draws the Passthrough fullscreen Grid + status bar itself, see Task 14).
- Consumes: `crate::proxy::screen::Grid`.

**Steps**
- [ ] Write failing test `j_k_navigate_like_arrows` (#2 — confirm `j`/`k` move the cursor, matching the arrow arms):
  ```rust
  #[tokio::test]
  async fn j_k_navigate_like_arrows() {
      let mut h = Harness::new(sample());
      h.key(KeyCode::Home).await; // local host
      let at_top = cur_row_label(&h);
      h.ch('j').await; // down
      assert_ne!(cur_row_label(&h), at_top, "j moves the cursor down");
      h.ch('k').await; // back up
      assert_eq!(cur_row_label(&h), at_top, "k moves the cursor up");
  }
  ```
- [ ] Run (expect FAIL — `j`/`k` not yet mapped).
- [ ] Write failing test `enter_is_noop_and_q_quits` (#7):
  ```rust
  #[tokio::test]
  async fn enter_is_noop_and_q_quits() {
      let mut h = Harness::new(sample());
      h.key(KeyCode::Enter).await;
      assert!(!h.sw.wants_quit(), "Enter does nothing");
      h.ch('q').await;
      assert!(h.sw.wants_quit(), "q quits");
  }
  ```
- [ ] Write failing test `cursor_move_yields_attach_target` (#7 — select = attach; replaces dwell):
  ```rust
  #[tokio::test]
  async fn cursor_move_yields_attach_target() {
      let mut h = Harness::new(sample()); // inference preselected (jupiter00)
      let t = h.sw.current_attach_target().expect("a session row yields a target");
      assert_eq!((t.source.as_str(), t.target.as_str()), ("jupiter00", "inference"));
      h.key(KeyCode::Home).await; // host row → its first visible session
      let t = h.sw.current_attach_target().expect("host row targets its first session");
      assert_eq!((t.source.as_str(), t.target.as_str()), ("local", "editor"));
  }
  ```
- [ ] Write failing test `spinner_renders_right_of_connecting_session` (#5):
  ```rust
  #[tokio::test]
  async fn spinner_renders_right_of_connecting_session() {
      let mut h = Harness::new(sample());
      let mut connecting = std::collections::HashSet::new();
      connecting.insert("jupiter00/inference".to_string());
      h.sw.set_spinner(connecting);
      h.draw();
      let tree = h.tree_text();
      // a braille spinner glyph from the U+2800 block appears on the inference row.
      let line = tree.lines().find(|l| l.contains("inference")).unwrap_or("");
      assert!(line.chars().any(|c| ('\u{2800}'..='\u{28ff}').contains(&c)),
          "a braille spinner sits right of a connecting session name:\n{tree}");
  }
  ```
- [ ] Write failing test `footer_and_help_reflect_new_model` (#8 — no stale Enter/Esc/dwell strings; states select=attach):
  ```rust
  #[tokio::test]
  async fn footer_and_help_reflect_new_model() {
      let mut h = Harness::new(sample());
      let footer = h.footer_text();
      assert!(!footer.contains("enter attach"), "Enter is a no-op now:\n{footer}");
      assert!(footer.contains("C-g s") || footer.contains("fullscreen"),
          "footer mentions fullscreen toggle:\n{footer}");
      h.ch('?').await;
      let help = h.text();
      assert!(help.contains("select = attach") || help.contains("move (select = attach)"),
          "help states moving the cursor attaches:\n{help}");
      assert!(!help.contains("dwell") && !help.to_lowercase().contains("previous foreground"),
          "no stale dwell/esc-return strings:\n{help}");
  }
  ```
- [ ] Write failing test `title_is_one_line` (#6):
  ```rust
  #[tokio::test]
  async fn title_is_one_line() {
      let h = Harness::new(sample());
      let out = h.text();
      assert!(out.contains("xmux: cross-host MUX manager"),
          "one-line title:\n{out}");
  }
  ```
- [ ] Minimal impl:
  - Delete the dwell fields/methods/`DWELL`/the fill block in `render_tree`; delete `SwitchResult`/`result`/`chosen`/`on_enter`/`choose`/`take_esc`/`esc_requested`/`clear_result`/`should_exit`/`quit`/`note_attached`.
  - Add fields: `spinner: HashSet<String>`, `spinner_frame: usize`, `wants_quit: bool`.
  - `handle_key`: add `KeyCode::Char('j') => self.move_selection(1)`, `Char('k') => self.move_selection(-1)`; `Char('q') => self.wants_quit = true`; `Enter` → `{}` (no-op); remove the `Esc => esc_requested` arm (Esc only closes input/help via the early returns).
  - Add `current_attach_target` = body of the old `on_focus_changed`'s target computation (reuse `target_for(current_ref())`), returning `Some` only for session/window/host-with-session rows.
  - Add `set_spinner`/`tick_spinner`/`wants_quit`.
  - `render_header`: collapse to one line `xmux: cross-host MUX manager` (change the `Length(2)` constraint to `Length(1)` in `render`).
  - `render_tree`: after the list, draw a braille glyph (`const SPINNER: &[char] = &['⠋','⠙','⠹','⠸','⠼','⠴','⠦','⠧','⠇','⠏'];`) at the end of each row whose session address is in `self.spinner`, using `self.spinner_frame % SPINNER.len()`.
  - `render_footer`/`render_help`: rewrite text to the new model — Overlay footer: `↑/↓ · j/k move (select = attach) · / filter · R rename · x kill · r refresh · ? help · C-g s fullscreen · q quit` (with `fit` fallbacks); help lines updated (drop "Enter attach"/"Esc return to previous foreground"/dwell; add "select = attach", "C-g s fullscreen", "C-g q quit").
  - Update the `Harness` test helpers: drop `result`/`take_dwell_attach` usages; add `cur_row_label`. Rewrite/remove the dwell tests (`dwell_completes_*`, `already_attached_target_skips_dwell`, `cursor_move_resets_dwell`, `non_attachable_row_has_no_dwell`, `enter_attaches_*`, `enter_on_host_*`, `esc_requests_return_not_quit`, `q_quits_the_app`, `quit_leaves_no_choice`, the `selected_node_renders_reverse_video`/filter/kill/create tests stay, adjusting any `result()` assertions to `current_attach_target()`/`wants_quit()`). `mouse_attach`/`mouse_select` (which called `on_enter`) — `mouse_attach` now just selects (no Enter); update `double_click_attaches_current_node`/`single_click_does_not_attach` to assert cursor + `current_attach_target` instead of `result`.
- [ ] Run (expect PASS after the rewrite):
  ```bash
  cargo test -p xmux ui::switcher > .../out.txt 2>&1; echo "EXIT=$?"
  ```
- [ ] `cargo clippy --all-targets -- -D warnings`.
- [ ] Commit: `refactor(ui): switcher select=attach + spinner; drop dwell/Enter-pick/Esc flow`

---

### Task 12: `app` — minimal two-variant `AppState` (no stdout owner)

**Files**
- Modify: `src/proxy/app.rs`
- Test: inline tests in `src/proxy/app.rs`

**Interfaces**
- Removed: `prev_fg`, `LiveOwner` field + import, `esc_target`, `enter_passthrough` byte-painting, the TOCTOU doc, the `Passthrough { fg, fg_id }` payload.
- Produces:
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum AppState { Overlay, Passthrough }

  pub struct App { pub state: AppState }
  impl App {
      pub fn new() -> Self;              // starts in Overlay
      pub fn is_overlay(&self) -> bool;
      pub fn toggle(&mut self);          // Overlay ⇄ Passthrough (prefix s)
  }
  ```

**Steps**
- [ ] Write failing test `app_starts_overlay_and_toggles`:
  ```rust
  #[test]
  fn app_starts_overlay_and_toggles() {
      let mut app = App::new();
      assert!(app.is_overlay());
      app.toggle();
      assert_eq!(app.state, AppState::Passthrough);
      app.toggle();
      assert_eq!(app.state, AppState::Overlay);
  }
  ```
- [ ] Run (expect FAIL — current `App::new` takes a `LiveOwner`; signature changed).
- [ ] Minimal impl — replace the file's body with the two-variant state above; delete the old tests that referenced `LiveOwner`/`prev_fg`/`enter_passthrough`.
- [ ] Run (expect PASS). (This breaks `cockpit.rs` which still uses the old `App` — that is fixed in Task 14; build the *test* for this module in isolation: `cargo test -p xmux proxy::app`.)
- [ ] `cargo clippy --all-targets -- -D warnings` will FAIL until Task 14 rewires `cockpit.rs`. Sequencing note: Tasks 12→13→14 land together as the cockpit-rewrite arc. To keep each commit green, do Task 12's edit but **defer the `cargo clippy --all-targets`/full `cargo test` gate to the end of Task 14** (the three are one logical unit). Commit Task 12 with `cargo test -p xmux proxy::app` green and a note in the message.
- [ ] Commit: `refactor(proxy): minimal Overlay/Passthrough AppState (cockpit rewire in #14)`

---

### Task 13: `ui::run` — drop `event_loop` + probe fan-out; shrink `Cmd`

`event_loop`/`spawn_probes`/`spawn_panes`/`handle_mouse`/`Cmd::SourceResult|Panes|OpDone|Mouse|Resize` have no caller after the cockpit rewrite (verified: only `event_loop`'s own tests and the soon-removed cockpit probe wiring call them). They are removed (AS-IS). `serve_control`/`dispatch`/`dump_switcher`/`ControlHandle` stay.

**Files**
- Modify: `src/ui/run.rs`
- Test: inline tests in `src/ui/run.rs` (keep the control-socket tests; remove the `event_loop_*` tests)

**Interfaces**
- Produces:
  ```rust
  pub enum Cmd { Key(KeyEvent), Dump(oneshot::Sender<String>) }
  pub fn dump_switcher(switcher: &mut Switcher, width: u16, height: u16) -> String; // kept
  pub fn serve_control(path: PathBuf, cmd_tx: mpsc::Sender<Cmd>) -> Option<ControlHandle>; // kept
  // accept_loop/handle_conn/dispatch kept; ControlHandle kept.
  ```
- Removed: `event_loop`, `spawn_probes`, `spawn_panes`, `handle_mouse`, `Cmd::{Mouse,Resize,SourceResult,Panes,OpDone}`, `POLL_INTERVAL`/`ANIM_INTERVAL`/`DOUBLE_CLICK`, the `Backend`/`MouseEvent` imports, the `run_op`/`OpResult` import (now used by the cockpit, kept in `switcher`).

**Steps**
- [ ] Write failing test `dispatch_dump_and_key_still_work` (the surviving control surface is intact):
  ```rust
  #[tokio::test]
  async fn dispatch_dump_and_key_still_work() {
      let (tx, mut rx) = mpsc::channel::<Cmd>(8);
      // key down → a Cmd::Key flows
      let r = dispatch("key down", &tx).await;
      assert_eq!(r, "ok");
      assert!(matches!(rx.recv().await, Some(Cmd::Key(_))));
      // dump → a Cmd::Dump flows (answered by a parallel responder)
      let tx2 = tx.clone();
      tokio::spawn(async move {
          if let Some(Cmd::Dump(reply)) = rx.recv().await { let _ = reply.send("SCREEN".into()); }
      });
      assert_eq!(dispatch("dump", &tx2).await, "SCREEN");
  }
  ```
- [ ] Run (expect this to compile + PASS once the `Cmd` shrink is done; before the edit, the `event_loop_*` tests still reference removed variants — so first delete them, then add this).
- [ ] Minimal impl — delete `event_loop`/`spawn_probes`/`spawn_panes`/`handle_mouse` and the removed `Cmd` variants/consts/imports; delete the `event_loop_*`/`resize_cmd_*`/`StreamOps`/`NoopOps` tests that only exercised the loop (keep `dump_switcher_flattens_buffer`, `control_handle_drop_removes_socket`, `control_end_to_end` — the latter drove `event_loop`; rewrite it to drive `dispatch` directly against a `Cmd` receiver, or keep a minimal in-test consumer loop). `dispatch` keeps `ping`/`dump`/`key`/`text`; (optional) add `passthrough`/`overlay`/`keys` verbs in Task 15.
- [ ] Run (expect PASS). Same green-gate deferral note as Task 12 if `cockpit.rs` is mid-rewrite — but prefer to land Task 13 *after* Task 14 if ordering bites; the suggested order is 12→13→14 with the full gate at the end of 14.
- [ ] `cargo clippy --all-targets -- -D warnings` (full gate may defer to end of Task 14).
- [ ] Commit: `refactor(ui): drop callerless event_loop + probe fan-out; shrink Cmd`

---

### Task 14: `cockpit` — rewrite the loop onto `HostManager` + control socket (#1/#3/#4/#7/#9/#10)

The heart of the wiring. The cockpit owns the `HostManager`, the `Switcher`, the `App` (Overlay/Passthrough), the control socket, and the ratatui terminal. On a cursor move (real or ctl) it ensures the host is connected and `switch-client`s; it folds `HostEvent`s into its `select!`; it draws every frame from the selected client's `Grid`; it toggles state on `prefix s` and quits on `prefix q`/`q`. It enters/clears/restores the alt screen via `TermGuard`.

**Files**
- Modify: `src/cockpit.rs` (rewrite `run_cockpit`; remove `attach_into_registry`/`handle_switcher_outcome`/`enter_passthrough`/`status_bar_bytes`; remove the eof channel/dwell poll/probe wiring; rewrite the tests)
- Modify: `src/env.rs` (drop `cfg.keep_cap()` threading — see Task 17; the cockpit reads `env.srcs`/`env.by_alias`)
- Test: inline `#[cfg(test)] mod tests` in `src/cockpit.rs`

**Interfaces**
- Consumes: `crate::host::{HostManager, HostEvent, HostInventory}`, `crate::proxy::term::{TermGuard, parse_prefix}`, `crate::proxy::app::{App, AppState}`, `crate::proxy::input::{InputMachine, InAction}`, `crate::proxy::decode::KeyDecoder`, `crate::proxy::control_proto::{send_keys_line, refresh_size_line, continue_pane_line}`, `crate::ui::switcher::{Switcher, TerminalViewTarget, run_op, OpResult, Ops}`, `crate::ui::run::{Cmd, serve_control, dump_switcher}`, `crate::env::Env`, `crate::mux::list_sessions`.
- Produces: `pub async fn run_cockpit(env: Arc<Env>) -> i32` (signature unchanged).

**Steps**
- [ ] Write failing test `cockpit_select_attach_drives_switch_client` — a headless test that builds a cockpit-internal helper `on_select(target)` (extracted so it is unit-testable) and asserts it calls `HostManager::ensure` + queues a `switch-client`. Since the full loop needs a terminal, extract the decision logic into a pure helper:
  ```rust
  // Extracted from the loop so the select=attach decision is unit-testable.
  fn select_attach(mgr: &mut HostManager, env: &Env, tgt: &TerminalViewTarget, cols: u16, rows: u16) -> bool;
  ```
  ```rust
  #[tokio::test]
  async fn select_attach_ensures_then_switches() {
      let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<HostEvent>();
      let mut mgr = HostManager::new(tx);
      mgr.insert_fake("jupiter06");                    // pretend connected
      let env = fake_env_with_source("jupiter06");
      let tgt = TerminalViewTarget { source: "jupiter06".into(), target: "api".into() };
      assert!(select_attach(&mut mgr, &env, &tgt, 80, 24),
          "select on a connected host queues a switch-client");
  }
  ```
- [ ] Run (expect FAIL — `select_attach` undefined).
- [ ] Write failing test `prefix_s_toggles_state` (drive the `App` toggle the loop performs on `InAction::OpenOverlay`/the `prefix s` path):
  ```rust
  #[test]
  fn prefix_s_toggles_state() {
      let mut app = App::new();
      assert!(app.is_overlay());
      app.toggle();   // C-g s from Overlay → Passthrough
      assert_eq!(app.state, AppState::Passthrough);
      app.toggle();   // C-g s from Passthrough → Overlay
      assert!(app.is_overlay());
  }
  ```
- [ ] Minimal impl — rewrite `run_cockpit`:
  - `nest_guard`; `let _term = TermGuard::enter()?` (#1: raw + alt-screen; clear before first draw via `term.clear()`).
  - `let size = crossterm::terminal::size()...; let (cols, body_rows) = (w, h-1);`
  - `let (host_tx, mut host_rx) = unbounded_channel::<HostEvent>(); let mut mgr = HostManager::new(host_tx);`
  - `let mut switcher = Switcher::from_sources(env.srcs.iter().map(|s| s.alias.clone()).collect());`
  - `let mut app = App::new();`
  - stdin reader thread (kept verbatim) → `stdin_rx`.
  - `let mut machine = InputMachine::new(parse_prefix(Some(&env.ui_prefix)), b's', b'q', Duration::from_millis(400));`
  - ratatui `Terminal` over stdout (kept).
  - control socket: `serve_control(pick_control_path(&env), cmd_tx)` (kept). **No `spawn_probes`.**
  - At start: ensure the preselected most-recent host is connected + `switch-client` to the preselected session (lazy connect for the cursor's host).
  - The `select!` arms:
    - `host_rx.recv()` → on `Connected`/`Inventory`: pull the client's `HostInventory`, feed `switcher.apply_source_result(host, inv.sessions, None)` + `apply_panes(addr, windows)` per session (rebuild the tree from inventory deltas, #3/#4); on `Output` → set a redraw flag; on `Attached` → clear that session from the spinner set; on `Exited` → `mgr.reap(host)` + if it owned the selected session fall to Overlay/most-recent.
    - `stdin_rx.recv()`:
      - Overlay: `KeyDecoder::feed` → `switcher.handle_key`; after keys, if the cursor moved, `if let Some(tgt) = switcher.current_attach_target() { select_attach(&mut mgr, &env, &tgt, cols, body_rows); }` (#7 select=attach); if `switcher.wants_quit()` → break.
      - Passthrough: `machine.feed` → `InAction::Forward(bytes)` → `mgr.get(sel_host)` → `client.send_keys(active_pane, bytes)` (batched, #6 input); `OpenOverlay` → `app.toggle()` + `term.clear()`; `Quit` → break.
    - `cmd_rx.recv()` (ctl, #10): `Cmd::Key(k)` → same Overlay `handle_key` + select=attach path (so `xmux ctl key down` attaches headlessly); `Cmd::Dump(reply)` → render the current frame to a `TestBackend` and reply (now captures the live Grid — #9). 
    - `event_stream.next()` resize → `mgr.resize_all(c, body)`; `switcher`/`term` resize; push `refresh_size_line` per client (the manager does this).
    - animation tick (braille spinner) → `switcher.tick_spinner()`; rebuild spinner set from `mgr` connecting flags.
  - **Keep create/rename/kill off-loop**: after `switcher.handle_key`, drain `switcher.take_pending_op()` and run it via `run_op(&op, ops.as_ref())` in a detached `tokio::spawn` whose result is sent back over a `mpsc<OpResult>` arm folded into the `select!` → `switcher.apply_op_result(r)` (the `Ops`/`run_op`/`OpResult`/`PendingOp` path in `switcher.rs` is retained; this replaces the removed `Cmd::OpDone` arm with a cockpit-local channel). `env.ops()` supplies the `Arc<dyn Ops>` for these mutations only — NOT for tree probing.
  - Draw: every frame, compute the selected `TerminalViewTarget`; borrow that host's `Grid` (`mgr.get(host).map(|c| c.grid.clone())`); in Overlay `term.draw(|f| switcher.render(f, grid))`; in Passthrough draw the Grid fullscreen at `cols×(rows-1)` + a one-line status bar (`host/session` + active window/pane, info only) — the cockpit draws this directly (the switcher renders only Overlay chrome).
  - On exit: `mgr.teardown_all()` (bounded join); `TermGuard` drop restores the screen.
  - Delete `attach_into_registry`/`handle_switcher_outcome`/`enter_passthrough`/`status_bar_bytes`/the eof channel.
- [ ] Run the full suite (this is the green gate for the 12→13→14 arc):
  ```bash
  cargo build > .../out.txt 2>&1; echo "EXIT=$?"
  cargo test > .../out.txt 2>&1; echo "EXIT=$?"
  ```
- [ ] `cargo clippy --all-targets -- -D warnings` (0 warnings — now the whole tree compiles).
- [ ] Commit: `feat(cockpit): rewrite loop onto HostManager + control socket (select=attach, alt-screen)`

---

### Task 15: `control` + `ui::run` — optional `passthrough`/`overlay`/`keys` ctl verbs (#10)

Additive ctl verbs so a headless test can toggle state and inject raw Passthrough bytes (proving `send-keys` forwarding) without a real terminal.

**Files**
- Modify: `src/ui/run.rs` (`dispatch` + `Cmd`), `src/cockpit.rs` (handle the new `Cmd` variants)
- Test: inline tests in `src/ui/run.rs`

**Interfaces**
- Produces: `Cmd::SetState(AppStateKind)` and `Cmd::Keys(Vec<u8>)` (raw bytes for the Passthrough input path), where `pub enum AppStateKind { Overlay, Passthrough }`. `dispatch` recognizes `passthrough`/`overlay` (→ `Cmd::SetState`) and `keys <hex…>` (→ `Cmd::Keys`).
- Consumes: existing `parse_request`.

**Steps**
- [ ] Write failing test `dispatch_state_and_keys_verbs`:
  ```rust
  #[tokio::test]
  async fn dispatch_state_and_keys_verbs() {
      let (tx, mut rx) = mpsc::channel::<Cmd>(8);
      assert_eq!(dispatch("passthrough", &tx).await, "ok");
      assert!(matches!(rx.recv().await, Some(Cmd::SetState(AppStateKind::Passthrough))));
      assert_eq!(dispatch("keys 1b5b41", &tx).await, "ok"); // ESC [ A
      assert!(matches!(rx.recv().await, Some(Cmd::Keys(b)) if b == vec![0x1b, 0x5b, 0x41]));
  }
  ```
- [ ] Run (expect FAIL).
- [ ] Minimal impl — parse a hex string into bytes (reject odd length / non-hex → `err:`), add the verbs to `dispatch`, add the `Cmd` variants, and handle them in the cockpit loop (`SetState` → set `app.state`; `Keys` → run through the Passthrough input path: `machine.feed` then `send_keys` to the active pane).
- [ ] Run (expect PASS).
- [ ] `cargo clippy --all-targets -- -D warnings`; `cargo test`.
- [ ] Commit: `feat(control): add passthrough/overlay/keys ctl verbs for headless drive`

---

### Task 16: `host` — flow-control resume + resize push (#7 perf, sizing §7)

Wire the backpressure resume (`%pause` → `refresh-client -A %pane:continue` once caught up) and ensure `resize` pushes `refresh-client -C` per client. The reader already records `Pause`; this task acts on it.

**Files**
- Modify: `src/host.rs`
- Test: inline tests in `src/host.rs`

**Interfaces**
- Produces: `HostClient::resume_pane(&self, pane: &str)` — queues `continue_pane_line(pane)`; the reader, on `Notif::Pause { pane }`, emits a `HostEvent::Output`-adjacent signal OR the cockpit resumes after a redraw. Simplest: the reader emits nothing extra; the cockpit, on the next draw after a `Pause` was seen, calls `resume_pane`. To keep it testable, expose `HostInventory.paused_panes: HashSet<String>` set by the reader on `Pause`/cleared on `Continue`.

**Steps**
- [ ] Write failing test `reader_marks_and_clears_paused_pane`:
  ```rust
  #[test]
  fn reader_marks_and_clears_paused_pane() {
      let state = test_state(80, 24);
      let in_flight: InFlight = Default::default();
      run_reader("h", vec!["%pause %3".to_string()].into_iter(), &state, &in_flight, |_| {});
      assert!(state.inventory.lock().unwrap().paused_panes.contains("%3"));
      run_reader("h", vec!["%continue %3".to_string()].into_iter(), &state, &in_flight, |_| {});
      assert!(!state.inventory.lock().unwrap().paused_panes.contains("%3"));
  }
  ```
- [ ] Run (expect FAIL — `paused_panes` field absent).
- [ ] Write failing test `resume_pane_queues_continue_line` (drive `run_writer` with a `HostCmd::Send(continue_pane_line("%3"))`):
  ```rust
  #[test]
  fn resume_pane_queues_continue_line() {
      let (tx, rx) = std::sync::mpsc::channel::<HostCmd>();
      let in_flight: InFlight = Default::default();
      tx.send(HostCmd::Send(crate::proxy::control_proto::continue_pane_line("%3"))).unwrap();
      tx.send(HostCmd::Shutdown).unwrap();
      drop(tx);
      let mut out = Vec::new();
      run_writer(rx, &mut out, &in_flight);
      assert!(String::from_utf8(out).unwrap().contains("refresh-client -A %3:continue\n"));
  }
  ```
- [ ] Minimal impl — add `paused_panes: HashSet<String>` to `HostInventory`; reader sets/clears it on `Pause`/`Continue`; add `HostClient::resume_pane`. In the cockpit draw path, after rendering, drain `paused_panes` of the selected host and `resume_pane` each (the renderer has caught up by then).
- [ ] Run (expect PASS).
- [ ] `cargo clippy --all-targets -- -D warnings`; `cargo test`.
- [ ] Commit: `feat(host): flow-control resume of paused panes after the renderer catches up`

---

### Task 17: `config` + `env` — remove `keep_cap` (decision: AS-IS no unused knob)

The host-bound model has no per-session cap, so the `keep_cap` knob is removed (it would advertise a setting with no effect). `[ui] prefix` stays.

**Files**
- Modify: `src/config.rs` (remove `keep_cap` field/`default_keep_cap`/`keep_cap()`/clamp + the `keep_cap`/clamp tests; the `ui_table_defaults_and_overrides` and `ui_keep_cap_clamped_to_min_two` tests drop their keep_cap halves)
- Modify: `src/env.rs` (no `keep_cap` usage remains after Task 14; confirm)
- Test: inline tests in `src/config.rs`

**Interfaces**
- Removed: `UiConfig.keep_cap`, `default_keep_cap`, `Config::keep_cap`.
- Kept: `UiConfig.prefix`, `Config::ui_prefix`.

**Steps**
- [ ] Write failing test `ui_table_keeps_prefix_drops_keep_cap` (a `[ui] keep_cap = 10` is now an *unknown key* warning, not a field):
  ```rust
  #[test]
  fn ui_table_keeps_prefix_drops_keep_cap() {
      let path = write_temp("[ui]\nprefix = \"C-Space\"\nkeep_cap = 10\n", "ui-no-keepcap.toml");
      let (cfg, warnings) = load_verbose(&path).unwrap();
      assert_eq!(cfg.ui_prefix(), "C-Space");
      assert!(warnings.iter().any(|w| w.contains("ui.keep_cap")),
          "keep_cap is now an unknown key: {warnings:?}");
  }
  ```
- [ ] Run (expect FAIL — `keep_cap` still decodes as a known field, so no warning).
- [ ] Minimal impl — remove the field/default/method/clamp; update the `UiConfig` `Default` impl; delete `ui_keep_cap_clamped_to_min_two`; trim `ui_table_defaults_and_overrides` to prefix-only; `env.rs` confirms no `keep_cap()` call survives (Task 14 removed the cockpit one).
- [ ] Run (expect PASS).
- [ ] `cargo clippy --all-targets -- -D warnings`; `cargo test`.
- [ ] Commit: `refactor(config): remove the keep_cap knob (no per-session cap in the host-bound model)`

---

### Task 18: Remove `proxy/run.rs` + `proxy/registry.rs`; verify no orphans (#9 AS-IS)

The per-session PTY-proxy attach model is now fully replaced; remove the superseded files and grep-verify no dead code, no dangling imports, clippy 0.

**Files**
- Remove: `src/proxy/run.rs`, `src/proxy/registry.rs`
- Modify: `src/proxy/mod.rs` (drop `pub mod run; pub mod registry;`)
- Modify: `src/main.rs` (the runtime doc comment mentions "the PTY output pump … the PTY control thread" — rewrite to describe the per-host reader/writer threads; no behavioral change)
- Test: full suite

**Steps**
- [ ] Grep to confirm no remaining references to the removed symbols:
  ```bash
  # expect ZERO hits in src/ (Grep tool):
  Attachment | spawn_attachment | AttachRegistry | LiveOwner | pty_control_loop | MasterSink | scan_clear | pump_write | DummyChild | fake_attachment | smoke_roundtrip | proxy::run | proxy::registry
  ```
- [ ] Remove the two files; drop the `pub mod` lines; rewrite the `main.rs` runtime comment.
- [ ] Confirm `RawGuard` (now `TermGuard` in `term.rs`) and `parse_prefix` (in `term.rs`) are the only survivors and are referenced only from `cockpit.rs`.
- [ ] Run the full suite + clippy (the gate):
  ```bash
  cargo test > .../out.txt 2>&1; echo "EXIT=$?"
  cargo clippy --all-targets -- -D warnings > .../out.txt 2>&1; echo "EXIT=$?"
  ```
- [ ] Commit: `refactor(proxy): remove the superseded PTY-proxy attach model (run.rs, registry.rs)`

---

### Task 19: Headless live verification on `jupiter06` (#9) + human visual-gate note

Drive the built `xmux` on an isolated psmux socket via `xmux ctl` + `dump`, **jupiter06 only**, never the live local server. Prove: select=attach renders the live Grid; the spinner appears then clears; Passthrough `keys` forwarding echoes in the pane.

**Files**
- No source changes (verification only). Update the HUMAN VISUAL-GATE CHECKLIST comment in `cockpit.rs` tests to the new model.

**Steps**
- [ ] `cargo build` the binary (Global Constraints toolchain).
- [ ] Launch xmux headless under an isolated psmux socket, watchdog-wrapped (Global Constraints), with `XMUX_CONTROL` enabled and the cursor able to reach `jupiter06` sessions. Use `env -u PSMUX_SESSION -u TMUX` to dodge `nest_guard`.
- [ ] Via `xmux ctl <pid> key down` (select = attach), `dump` — assert the dump shows the spinner first, then the jupiter06 session's live content (the Grid renders headlessly now). Drive `passthrough` then `keys <hex of "echo XMUXOK\r">`, then `dump` — assert the pane echoed `XMUXOK`.
- [ ] Record results inline (no source change). Update the `cockpit.rs` HUMAN VISUAL-GATE CHECKLIST to: (1) launch in a real terminal, confirm alt-screen clear (#1) — no pre-launch bleed; (2) Overlay tree + live terminal view right; arrow/`j`/`k` move and the view follows (select=attach), spinner shows while connecting; (3) `C-g s` → fullscreen Passthrough + one-line status bar; type into the session; (4) `C-g s` back to Overlay (same selected session); (5) `C-g q`/`q` clean quit + screen restored; (6) NEVER select `local/xmux`.
- [ ] `cargo test` + `cargo clippy --all-targets -- -D warnings` (final green gate).
- [ ] Commit: `test(cockpit): headless jupiter06 ctl+dump verification; rewrite visual-gate checklist`

---

## Spec / issue coverage map

- **§1 state model (Overlay/Passthrough, Enter no-op, toggle)** → Tasks 11 (no-op Enter, `wants_quit`), 12 (`AppState`), 14 (toggle wiring).
- **§2 concurrency/ownership (per-host threads, no stdout owner, teardown/reap)** → Tasks 8 (reader), 9 (writer/spawn/teardown), 10 (manager reap), 12 (no `LiveOwner`).
- **§3 lifecycle (lazy connect, connect sequence, one connection per host, reconnect)** → Tasks 9 (connect sequence), 10 (lazy connect/reconnect/reap).
- **§4 protocol handling (framing, %output decode, notif table, active-pane)** → Tasks 1–4 (pure), 8 (dispatch + `display-message` active-pane), 9 (`SwitchClient` → `display-message`).
- **§5 selection/spinner/tree** → Task 11 (`current_attach_target`, spinner, tree from inventory), 14 (select=attach wiring).
- **§6 input dual-control + ctl** → Tasks 4 (`send-keys -H`), 14 (Forward → send_keys; ctl key/text/dump), 15 (passthrough/overlay/keys verbs).
- **§7 sizing/perf** → Tasks 4 (`refresh-client -C`/`pause-after`), 9 (connect sequence), 16 (resume), 14 (resize push).
- **§8 rendering (#1 alt-screen, #2 nav, terminal view, #8 help)** → Tasks 6 (#1), 11 (#2/#5/#6/#8), 14 (Grid draw both states).
- **§9 removed/superseded (AS-IS)** → Tasks 12/13/17/18 (app, run, config, run.rs+registry.rs).
- **§10 module impact map** → covered file-by-file in File Structure + each task's Files.
- **§13 open decisions**: `keep_cap` removed (Task 17); local control child only when ≥1 session (Task 14 start logic + manager — a host with zero sessions shows "(empty)" and spawns nothing until selected); `event_loop` removed as callerless (Task 13, confirmed by grep: only its own tests + the removed cockpit probe wiring called it).
- **Issues**: #1 → Task 6/14; #2 → Task 11; #3/#4 → Tasks 8/10/14 (tree from control connection, lazy connect, live Grid); #5 → Task 11; #6 → Tasks 11/14; #7 → Tasks 11/14; #8 → Task 11; #9 → Task 19; #10 → Tasks 13/15/14 (ctl first-class, dump renders live Grid).

## Risks (carried from spec §11)

- `send-keys` fidelity: mouse forwarding out of scope for v1; bracketed paste must be wrapped by xmux (`InputMachine` tracks framing — forward the `ESC[200~…ESC[201~` bytes via `-H`); high/UTF-8 bytes use `-H` (fall back to `-l` only if psmux misbehaves — live gate).
- Remote ssh auth: key/agent auth assumed (no `-t`, `BatchMode=yes`); a host needing a tty surfaces as "unreachable" with the stderr reason.
- psmux/ConPTY `%output` byte fidelity is `[research §10, UNVERIFIED]` — smoke-tested on jupiter06 in Task 19 (decode is byte-exact; any drift is in what psmux emits).
- Watchdog discipline for the non-self-bounding `-CC`/ssh children — bounded teardown joins (Task 9/10) + OS-level watchdog around any live spawn (Task 19).
