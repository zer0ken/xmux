# Live full-attach terminal-view — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the cockpit's one-attach-at-a-time / detach-to-reattach model with a single persistent terminal application that keeps N live attaches alive across selections, renders the selected session's real terminal in a sidebar+terminal-view Overlay, and switches instantly.

**Architecture:** Generalize the single-attach PTY proxy (`src/proxy/run.rs`) into a reusable per-Attachment unit (control thread owning writer+master, output pump, shared vt100 `Grid`) managed by an `AttachRegistry` (Address→Attachment, LRU-capped). A shared `AppState = Passthrough{fg} | Overlay` gates exactly one stdout writer at a time. The switcher's static `capture-pane` snapshot is replaced by a live `Grid→ratatui` bridge; a 500ms cursor dwell triggers a lazy attach with a left→right progress animation. The cockpit drives this state machine; the control socket is repurposed to inject keys/dump for headless testing.

**Tech Stack:** ratatui 0.30.1, crossterm 0.29 (event-stream), vt100 0.16, portable-pty 0.9, tokio 1.52.3 (`current_thread` runtime), serde 1.0.228 + serde_ignored 0.1.14 + toml 1.1.2, interprocess 2.4.2 (named pipes on Windows), async-trait 0.1.89, anyhow 1.0.102.

## Global Constraints

- Branch `feat/rust-rewrite`. Commit per task (local only — DO NOT push/merge; that needs explicit user approval).
- Windows build uses the REAL toolchain (rustup shim is blocked): prepend `C:\Users\hrlee\.rustup\toolchains\stable-x86_64-pc-windows-msvc\bin` to PATH and set `RUSTC`/`RUSTDOC` to that bin's `rustc.exe`/`rustdoc.exe`. Verify each task with `cargo test` AND `cargo clippy --all-targets` (clippy must be 0 warnings). NEVER pipe cargo through `tail`/`head` (masks the exit code) — redirect to a file and check `$?`. `cargo test` does NOT rebuild the binary (stale-binary trap) — run `cargo build` before exercising the binary.
- NEVER do blocking I/O (`write_all`, `resize`, blocking FS, `Command::status()`) on the `current_thread` async loop. Each kept Attachment owns its writer+master on a dedicated control thread (the B1 `pty_control_loop`+`MasterSink` pattern); N attaches = N control threads + N pumps.
- NEVER hold a process-global `StdoutLock` across a blocking op or while another thread draws — per-write lock only. In any state, only ONE writer touches real stdout.
- ConPTY: never drop the master mid-session on the loop; drop it on its owning control thread at teardown; pre-24H2 `ClosePseudoConsole` may block that thread (bounded join).
- Self-mirror: never live-attach a session whose active pane runs `xmux`.
- xmux must still refuse to run inside a mux (keep `attach::nest_guard`).
- Testing must NEVER disrupt the user's live local psmux session: drive only the throwaway remote `jupiter06`; never select/attach the live `local/xmux`. Live raw-passthrough screen handover is a human-only visual gate.
- `interprocess` = named pipes on Windows (not AF_UNIX); `xmux ctl` streams over one connection.

---

## File Structure

| File | Responsibility |
|------|----------------|
| `src/config.rs` (modify) | Add the optional `[ui]` table → `UiConfig { prefix, keep_cap }` (prefix default `"C-g"`, keep_cap default 6 / min 2) to `Config`. Loaded via the existing `load`/`load_verbose` so unknown keys still warn. |
| `src/proxy/run.rs` (modify) | `parse_prefix` sources the prefix from a config string (drop `prefix_from_env`/`XMUX_PREFIX`). Extract the reusable per-Attachment unit (`Attachment`, `spawn_attachment`, the live-stdout-owner gate generalizing `overlay_active`). |
| `src/proxy/screen.rs` (modify) | Add the `Grid→ratatui` cell bridge (`Grid::render_into`, vt100 cell → ratatui `Style`) and a cursor accessor. |
| `src/proxy/input.rs` (modify) | Extend `InAction`/`InputMachine` prefix actions: `s` → OpenOverlay, `q` → Quit (keep double-tap-literal + paste guard). |
| `src/proxy/registry.rs` (create) | `AttachRegistry`: `Address → Attachment`, LRU eviction protecting foreground+cursor, reap-on-EOF, bounded parallel teardown. |
| `src/proxy/app.rs` (create) | `AppState` (`Passthrough{fg}` \| `Overlay`) shared atomically; the per-pump live-owner write gate; the Overlay→Passthrough transition handoff (restore_bytes + status-bar paint). |
| `src/ui/switcher.rs` (modify) | Terminal view (live `Grid` render) replaces the capture snapshot; rename surviving `preview_*` → `terminal_view_*`; remove `preview_cache`/capture state; dwell progress animation; status-bar content; help only in Overlay. |
| `src/ui/run.rs` (modify) | Animation tick (~33ms, active only while a dwell is pending); remove capture `Cmd::Preview`/`spawn_capture`; keep streaming probes + control socket. |
| `src/ui/tree.rs` (modify) | `order_groups`: local group(s) pinned first, remote groups by max session `last_attached` desc. |
| `src/cockpit.rs` (modify) | Replace the one-attach loop with the persistent app (registry + AppState + state machine); repurpose the control socket to drive it. Remove `signal_cockpit_switch` / `PopupAction::SignalCockpit` / popup→cockpit path. |
| `src/source.rs` (no change) | Reused as-is: `attach_command` gives uniform local+remote attach argv. |
| `src/main.rs` (modify) | Remove the `popup` subcommand + `run_popup`; keep `current_thread` runtime + `nest_guard`. |
| `src/env.rs` (modify) | Thread `UiConfig` (prefix → proxy, keep_cap → registry) off `Config`. |

---

### Task 1: Config `[ui]` table — `UiConfig { prefix, keep_cap }`

**Files:**
- Modify: `src/config.rs:9-17` (add `ui` field to `Config`), add `UiConfig` struct + defaults + a `keep_cap()` clamp accessor.
- Test: `src/config.rs` (`#[cfg(test)] mod tests`).

**Interfaces:**
- Consumes: nothing (first task).
- Produces:
  - `pub struct UiConfig { pub prefix: String, pub keep_cap: usize }` with `#[derive(Debug, Clone, Deserialize)]` and a manual `Default` (prefix `"C-g"`, keep_cap `6`).
  - Field `pub ui: UiConfig` on `Config` (`#[serde(default)]`).
  - `impl Config { pub fn ui_prefix(&self) -> &str; pub fn keep_cap(&self) -> usize }` where `keep_cap()` returns `self.ui.keep_cap.max(2)`.

- [ ] **Step 1: Write the failing test**

Add to `src/config.rs` `mod tests`:

```rust
#[test]
fn ui_table_defaults_and_overrides() {
    // Missing [ui] → defaults: prefix "C-g", keep_cap 6.
    let missing = std::env::temp_dir().join("xmux-ui-absent-xyz.toml");
    let cfg = load(&missing).unwrap();
    assert_eq!(cfg.ui_prefix(), "C-g");
    assert_eq!(cfg.keep_cap(), 6);

    // Explicit [ui] overrides both.
    let path = write_temp(
        r#"
[ui]
prefix = "C-Space"
keep_cap = 10
"#,
        "ui-override.toml",
    );
    let cfg = load(&path).unwrap();
    assert_eq!(cfg.ui_prefix(), "C-Space");
    assert_eq!(cfg.keep_cap(), 10);
}

#[test]
fn ui_keep_cap_clamped_to_min_two() {
    // keep_cap must hold foreground + cursor, so values below 2 clamp up to 2.
    let path = write_temp(
        r#"
[ui]
keep_cap = 1
"#,
        "ui-clamp.toml",
    );
    let cfg = load(&path).unwrap();
    assert_eq!(cfg.keep_cap(), 2, "keep_cap below 2 must clamp to 2");
}

#[test]
fn ui_unknown_key_still_warns() {
    // serde_ignored must still surface a typo'd key under [ui].
    let path = write_temp(
        r#"
[ui]
keep_cap = 6
bogus = "nope"
"#,
        "ui-unknown.toml",
    );
    let (cfg, warnings) = load_verbose(&path).unwrap();
    assert_eq!(cfg.keep_cap(), 6);
    assert_eq!(warnings, vec![r#"unknown key "ui.bogus""#.to_string()]);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p xmux config::tests::ui_ 2> test.err; echo "exit=$?"; cat test.err`
Expected: FAIL — `no method named ui_prefix`/`keep_cap`, `no field ui`.

- [ ] **Step 3: Write minimal implementation**

In `src/config.rs`, add the field to `Config` (after `exclude`):

```rust
    #[serde(default)]
    pub ui: UiConfig,
```

Add the struct + impls (after the `LocalConfig` block):

```rust
/// The optional `[ui]` table: xmux's own prefix and the kept-attachment cap.
#[derive(Debug, Clone, Deserialize)]
pub struct UiConfig {
    /// xmux's prefix spec (e.g. `C-g`, `C-Space`), config-only like tmux's
    /// `set -g prefix`. Parsed by `proxy::run::parse_prefix`.
    #[serde(default = "default_prefix")]
    pub prefix: String,
    /// How many live Attachments to keep. Clamped to a minimum of 2 via
    /// [`Config::keep_cap`] (must hold foreground + cursor).
    #[serde(default = "default_keep_cap")]
    pub keep_cap: usize,
}

fn default_prefix() -> String {
    "C-g".to_string()
}

fn default_keep_cap() -> usize {
    6
}

impl Default for UiConfig {
    fn default() -> Self {
        UiConfig {
            prefix: default_prefix(),
            keep_cap: default_keep_cap(),
        }
    }
}
```

Add to `impl Config` (alongside `local_bin`):

```rust
    /// xmux's configured prefix spec.
    pub fn ui_prefix(&self) -> &str {
        &self.ui.prefix
    }

    /// The kept-attachment cap, clamped to a minimum of 2.
    pub fn keep_cap(&self) -> usize {
        self.ui.keep_cap.max(2)
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p xmux config::tests::ui_ 2> test.err; echo "exit=$?"; cat test.err`
Expected: PASS (3 tests).

- [ ] **Step 5: Verify clippy**

Run: `cargo clippy --all-targets 2> clippy.err; echo "exit=$?"; grep -c warning clippy.err`
Expected: exit=0, 0 warnings.

- [ ] **Step 6: Commit**

```bash
git add src/config.rs
git commit -m "feat(config): add [ui] table with prefix and keep_cap"
```

---

### Task 2: Prefix from config (drop `XMUX_PREFIX` env)

**Files:**
- Modify: `src/proxy/run.rs:59-88` (keep `parse_prefix`, remove `prefix_from_env`), `src/proxy/run.rs:568-574` (the `prefix_from_env_parses_and_defaults` test name/body).
- Modify: `src/env.rs` (expose `ui_prefix` so the cockpit can build a `ProxyConfig`).
- Test: `src/proxy/run.rs` (`#[cfg(test)] mod tests`).

**Interfaces:**
- Consumes: `Config::ui_prefix(&self) -> &str` (Task 1).
- Produces:
  - `parse_prefix(spec: Option<&str>) -> u8` (unchanged signature, still recognises `C-<letter>`, `C-Space`, defaults to `0x07`).
  - `Env` gains `pub ui_prefix: String` (the resolved prefix spec) so the cockpit reads it without re-loading config.
  - `prefix_from_env` is REMOVED — no remaining caller after this task.

- [ ] **Step 1: Update the existing test (failing build)**

Replace the `prefix_from_env_parses_and_defaults` test in `src/proxy/run.rs` with:

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

Add to `src/env.rs` `mod tests` (inside an existing or a new test):

```rust
    #[test]
    fn env_carries_configured_prefix() {
        let mut cfg = Config::default();
        cfg.ui.prefix = "C-a".into();
        assert_eq!(cfg.ui_prefix(), "C-a");
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p xmux proxy::run::tests::parse_prefix 2> test.err; echo "exit=$?"; cat test.err`
Expected: FAIL — `prefix_from_env` still referenced (in `cockpit.rs:354`), or the old test name still present.

- [ ] **Step 3: Remove `prefix_from_env` and its env read**

In `src/proxy/run.rs`, delete:

```rust
/// Read `XMUX_PREFIX` from the environment and parse it.
pub fn prefix_from_env() -> u8 {
    parse_prefix(std::env::var("XMUX_PREFIX").ok().as_deref())
}
```

In `src/env.rs`, add `pub ui_prefix: String` to the `Env` struct and set it in `build_env` (after `local_bin`):

```rust
    let ui_prefix = cfg.ui_prefix().to_string();
```

and add `ui_prefix` to the `Env { .. }` literal. Update the test-only `Env { .. }` constructors in `src/env.rs` `mod tests` to add `ui_prefix: "C-g".into(),`.

In `src/cockpit.rs:353-356`, change the `ProxyConfig` construction (this is the one remaining `prefix_from_env` caller — it is rewritten fully in Task 12, but for now make it compile):

```rust
        let cfg = proxy::run::ProxyConfig {
            prefix: proxy::run::parse_prefix(Some(&self.env.ui_prefix)),
            action_key: b's',
        };
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p xmux proxy::run::tests::parse_prefix env::tests::env_carries 2> test.err; echo "exit=$?"; cat test.err`
Expected: PASS.

- [ ] **Step 5: Verify build + clippy (whole crate, to catch the removed symbol)**

Run: `cargo build 2> build.err; echo "exit=$?"; cargo clippy --all-targets 2> clippy.err; echo "clippy=$?"; grep -c warning clippy.err`
Expected: build exit=0, clippy=0, 0 warnings.

- [ ] **Step 6: Commit**

```bash
git add src/proxy/run.rs src/env.rs src/cockpit.rs
git commit -m "refactor(proxy): source prefix from config, drop XMUX_PREFIX env"
```

---

### Task 3: `Grid→ratatui` cell bridge + cursor accessor

**Files:**
- Modify: `src/proxy/screen.rs` (add `render_into`, `cursor`, and the vt100→ratatui mappers).
- Test: `src/proxy/screen.rs` (`#[cfg(test)] mod tests`).

**Interfaces:**
- Consumes: nothing new (uses vt100 0.16 `Screen::cell(row,col) -> Option<&vt100::Cell>`, `Screen::cursor_position() -> (u16 row, u16 col)`, `Screen::size() -> (u16 rows, u16 cols)`, `Screen::hide_cursor() -> bool`; `vt100::Cell::{has_contents, contents() -> &str, fgcolor()/bgcolor() -> vt100::Color, bold/italic/underline/inverse -> bool}`; `vt100::Color::{Default, Idx(u8), Rgb(u8,u8,u8)}`).
- Produces (on `impl Grid`):
  - `pub fn cursor(&self) -> (u16, u16)` — returns `(col, row)` (ratatui x,y order) of the vt100 cursor, clamped to the grid size.
  - `pub fn hide_cursor(&self) -> bool` — passthrough to the screen.
  - `pub fn render_into(&self, buf: &mut ratatui::buffer::Buffer, area: ratatui::layout::Rect)` — writes a top-left clip of the grid's cells into `area` of `buf`, mapping each vt100 cell's symbol + fg/bg/attrs to a ratatui `Cell`. Cells beyond the grid or `area` are skipped.
  - free fn `pub fn vt_color_to_ratatui(c: vt100::Color) -> ratatui::style::Color`.

- [ ] **Step 1: Write the failing tests**

Add to `src/proxy/screen.rs` `mod tests`:

```rust
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::Color as RColor;

    #[test]
    fn color_mapping_covers_default_idx_rgb() {
        assert_eq!(vt_color_to_ratatui(vt100::Color::Default), RColor::Reset);
        assert_eq!(vt_color_to_ratatui(vt100::Color::Idx(4)), RColor::Indexed(4));
        assert_eq!(
            vt_color_to_ratatui(vt100::Color::Rgb(10, 20, 30)),
            RColor::Rgb(10, 20, 30)
        );
    }

    #[test]
    fn render_into_writes_cell_symbols_into_buffer() {
        let mut g = Grid::new(24, 80);
        g.feed(b"AB");
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 24));
        g.render_into(&mut buf, Rect::new(0, 0, 80, 24));
        assert_eq!(buf[(0, 0)].symbol(), "A");
        assert_eq!(buf[(1, 0)].symbol(), "B");
    }

    #[test]
    fn render_into_clips_to_area_top_left() {
        // A grid wider than the area renders only the top-left clip; nothing is
        // written past area.width/height.
        let mut g = Grid::new(24, 80);
        g.feed(b"HELLO");
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 24));
        // Narrow 3-wide area: only H E L land.
        g.render_into(&mut buf, Rect::new(0, 0, 3, 1));
        assert_eq!(buf[(0, 0)].symbol(), "H");
        assert_eq!(buf[(2, 0)].symbol(), "L");
        // Column 3 was outside the area and must be untouched (default space).
        assert_eq!(buf[(3, 0)].symbol(), " ");
    }

    #[test]
    fn cursor_reports_position_in_xy_order() {
        let mut g = Grid::new(24, 80);
        g.feed(b"abc"); // cursor advances to col 3, row 0
        assert_eq!(g.cursor(), (3, 0), "cursor is (col, row)");
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p xmux proxy::screen::tests 2> test.err; echo "exit=$?"; cat test.err`
Expected: FAIL — `vt_color_to_ratatui` / `render_into` / `cursor` not found.

- [ ] **Step 3: Write the bridge**

In `src/proxy/screen.rs`, add the imports at the top and the methods to `impl Grid` plus the free fn:

```rust
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color as RColor, Modifier, Style};
```

```rust
    /// The vt100 cursor as ratatui `(x, y)` (col, row), clamped to the grid.
    pub fn cursor(&self) -> (u16, u16) {
        let screen = self.parser.screen();
        let (rows, cols) = screen.size();
        let (row, col) = screen.cursor_position();
        (col.min(cols.saturating_sub(1)), row.min(rows.saturating_sub(1)))
    }

    /// Whether the child has hidden its cursor.
    pub fn hide_cursor(&self) -> bool {
        self.parser.screen().hide_cursor()
    }

    /// Writes a top-left clip of the grid into `area` of `buf`, mapping each
    /// vt100 cell's symbol + colours + attrs to a ratatui cell. Cells past the
    /// grid size or `area` are skipped (the terminal view in Overlay is narrower
    /// than the grid, so it shows a top-left clip).
    pub fn render_into(&self, buf: &mut Buffer, area: Rect) {
        let screen = self.parser.screen();
        let (grid_rows, grid_cols) = screen.size();
        let rows = area.height.min(grid_rows);
        let cols = area.width.min(grid_cols);
        for r in 0..rows {
            for c in 0..cols {
                let Some(vcell) = screen.cell(r, c) else {
                    continue;
                };
                let cell = &mut buf[(area.x + c, area.y + r)];
                if vcell.has_contents() {
                    cell.set_symbol(vcell.contents());
                } else {
                    cell.set_symbol(" ");
                }
                cell.set_style(vt_cell_style(vcell));
            }
        }
    }
```

```rust
/// Maps a vt100 colour to a ratatui colour. `Default` → `Reset` (terminal
/// default), `Idx` → 256-colour index, `Rgb` → true colour.
pub fn vt_color_to_ratatui(c: vt100::Color) -> RColor {
    match c {
        vt100::Color::Default => RColor::Reset,
        vt100::Color::Idx(i) => RColor::Indexed(i),
        vt100::Color::Rgb(r, g, b) => RColor::Rgb(r, g, b),
    }
}

/// Maps a vt100 cell's colours and attributes to a ratatui `Style`.
fn vt_cell_style(cell: &vt100::Cell) -> Style {
    let mut style = Style::default()
        .fg(vt_color_to_ratatui(cell.fgcolor()))
        .bg(vt_color_to_ratatui(cell.bgcolor()));
    let mut m = Modifier::empty();
    if cell.bold() {
        m |= Modifier::BOLD;
    }
    if cell.italic() {
        m |= Modifier::ITALIC;
    }
    if cell.underline() {
        m |= Modifier::UNDERLINED;
    }
    if cell.inverse() {
        m |= Modifier::REVERSED;
    }
    style.add_modifier = m;
    style
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p xmux proxy::screen::tests 2> test.err; echo "exit=$?"; cat test.err`
Expected: PASS (4 new + 3 existing).

- [ ] **Step 5: Verify clippy**

Run: `cargo clippy --all-targets 2> clippy.err; echo "exit=$?"; grep -c warning clippy.err`
Expected: exit=0, 0 warnings.

- [ ] **Step 6: Commit**

```bash
git add src/proxy/screen.rs
git commit -m "feat(proxy): add Grid->ratatui cell bridge and cursor accessor"
```

---

### Task 4: Extract the reusable per-Attachment unit

**Files:**
- Modify: `src/proxy/run.rs` (move `PtyCmd`, `PtySink`, `pty_control_loop`, `MasterSink`, `pump_write` into reuse, add `LiveOwner`, `Attachment`, `spawn_attachment`). Keep `proxy_attach` compiling against the new helpers.
- Modify: `src/proxy/mod.rs` (no new module yet — keep these in `run.rs`).
- Test: `src/proxy/run.rs` (`#[cfg(test)] mod tests`).

**Interfaces:**
- Consumes: `Grid` (Task 3's `render_into`/`cursor` will be read by later tasks), `pty_control_loop`/`MasterSink`/`PtyCmd` (existing).
- Produces:
  - `pub enum PtyCmd { Input(Vec<u8>), Resize { cols: u16, rows: u16 } }` (made `pub` for the registry).
  - `pub struct LiveOwner(Arc<AtomicU64>)` — a shared generation/owner token. `pub fn new() -> Self`, `pub fn set_owner(&self, id: u64)`, `pub fn set_overlay(&self)`, `pub fn is_owner(&self, id: u64) -> bool`. Overlay state = the sentinel `u64::MAX` (no pump writes stdout). A pump writes iff `is_owner(self_id)`.
  - `pub struct Attachment { pub grid: Arc<Mutex<Grid>>, pub control_tx: std::sync::mpsc::Sender<PtyCmd>, pub size: (u16, u16), pub last_used: Instant, child: Box<dyn portable_pty::Child + Send + Sync>, id: u64 }` with `pub fn resize(&self, cols: u16, rows: u16)`, `pub fn input(&self, bytes: Vec<u8>)`, `pub fn teardown(self)` (drops `control_tx`, kills the child).
  - `pub fn spawn_attachment(argv: &[String], cols: u16, rows: u16, id: u64, live: LiveOwner, stdout: std::io::Stdout, eof_tx: tokio::sync::mpsc::UnboundedSender<u64>) -> anyhow::Result<Attachment>` — opens a PTY at `cols×rows`, spawns `argv` (mux env cleared), starts the control thread (owns writer+master) and the output pump (feeds the shared `Grid`; writes stdout iff `live.is_owner(id)`; sends `id` on `eof_tx` at EOF).

- [ ] **Step 1: Write the failing test**

Add to `src/proxy/run.rs` `mod tests`:

```rust
    use std::sync::atomic::Ordering as AtomicOrdering;

    #[test]
    fn live_owner_gates_by_id_and_overlay() {
        let live = LiveOwner::new();
        live.set_owner(7);
        assert!(live.is_owner(7));
        assert!(!live.is_owner(3), "a non-owner id must not write stdout");
        live.set_overlay();
        assert!(!live.is_owner(7), "in Overlay NO id is the owner");
        assert!(!live.is_owner(3));
        live.set_owner(3);
        assert!(live.is_owner(3));
        assert!(!live.is_owner(7));
    }

    // The full spawn_attachment path needs a real ConPTY (a console), so it is an
    // #[ignore] smoke test driven on demand; the gate logic above is the headless
    // unit. spawn_attachment is exercised live in Task 14.
    #[ignore]
    #[test]
    fn spawn_attachment_feeds_grid_smoke() {
        let live = LiveOwner::new();
        live.set_overlay(); // do not write the test console's stdout
        let (eof_tx, _eof_rx) = tokio::sync::mpsc::unbounded_channel::<u64>();
        let argv: Vec<String> = vec!["cmd.exe".into()];
        let att = spawn_attachment(&argv, 80, 24, 1, live, std::io::stdout(), eof_tx)
            .expect("spawn");
        att.input(b"echo SMOKE\r\n".to_vec());
        std::thread::sleep(Duration::from_millis(800));
        let g = att.grid.lock().unwrap();
        let mut buf = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 80, 24));
        g.render_into(&mut buf, ratatui::layout::Rect::new(0, 0, 80, 24));
        drop(g);
        att.teardown();
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p xmux proxy::run::tests::live_owner 2> test.err; echo "exit=$?"; cat test.err`
Expected: FAIL — `LiveOwner` not found.

- [ ] **Step 3: Add `LiveOwner`**

In `src/proxy/run.rs`, add near the top (after the imports add `use std::sync::atomic::AtomicU64;` and `use std::sync::Mutex;`):

```rust
/// The single shared "who owns raw stdout" token. A pump writes stdout iff it is
/// the current owner. The sentinel `OVERLAY` means ratatui owns stdout and NO
/// pump writes. This generalizes the old `overlay_active: AtomicBool` to a
/// per-Attachment owner check; exactly one writer touches real stdout at a time.
#[derive(Clone)]
pub struct LiveOwner(Arc<AtomicU64>);

const OVERLAY: u64 = u64::MAX;

impl LiveOwner {
    pub fn new() -> Self {
        LiveOwner(Arc::new(AtomicU64::new(OVERLAY)))
    }
    /// Make Attachment `id` the foreground stdout owner (Passthrough).
    pub fn set_owner(&self, id: u64) {
        self.0.store(id, AtomicOrdering::Release);
    }
    /// Enter Overlay: no pump writes stdout (ratatui owns it).
    pub fn set_overlay(&self) {
        self.0.store(OVERLAY, AtomicOrdering::Release);
    }
    /// Whether Attachment `id` may write raw stdout right now.
    pub fn is_owner(&self, id: u64) -> bool {
        self.0.load(AtomicOrdering::Acquire) == id
    }
}

impl Default for LiveOwner {
    fn default() -> Self {
        Self::new()
    }
}
```

(Use `use std::sync::atomic::Ordering as AtomicOrdering;` at the top of the non-test module too, or qualify with `std::sync::atomic::Ordering`. The existing file already imports `Ordering` — reuse it: write `self.0.store(id, Ordering::Release)` etc. and drop the `AtomicOrdering` alias in the impl. Keep the test's `AtomicOrdering` alias local to the test.)

- [ ] **Step 4: Add `PtyCmd` pub, `Attachment`, `spawn_attachment`**

Change `enum PtyCmd` to `pub enum PtyCmd` (line ~105). Add (after `MasterSink`'s impl):

```rust
/// One kept live attach: a ConPTY + attach child + dedicated control thread
/// (owns writer+master) + output pump + a shared `Grid`. The pump feeds the grid
/// always and writes raw stdout iff this attachment is the `LiveOwner`.
pub struct Attachment {
    pub grid: Arc<Mutex<Grid>>,
    pub control_tx: std::sync::mpsc::Sender<PtyCmd>,
    pub size: (u16, u16),
    pub last_used: Instant,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    id: u64,
}

impl Attachment {
    pub fn id(&self) -> u64 {
        self.id
    }
    /// Queue input bytes to the child (FIFO, off the loop).
    pub fn input(&self, bytes: Vec<u8>) {
        let _ = self.control_tx.send(PtyCmd::Input(bytes));
    }
    /// Queue a resize to the child PTY (off the loop) and resize the grid.
    pub fn resize(&self, cols: u16, rows: u16) {
        let _ = self.control_tx.send(PtyCmd::Resize { cols, rows });
        if let Ok(mut g) = self.grid.lock() {
            g.resize(rows, cols);
        }
    }
    /// Tear down: kill the child, drop the control sender so the control thread
    /// drops the master on ITS thread and exits. The pump exits on master EOF.
    pub fn teardown(mut self) {
        let _ = self.child.kill();
        drop(self.control_tx);
        // child + remaining fields drop here; the control thread owns the master.
    }
}

/// Opens a PTY at `cols×rows`, spawns `argv` with mux nesting-guard env cleared,
/// starts the control thread (owns writer+master) and the output pump (always
/// feeds the grid; writes raw stdout iff `live.is_owner(id)`; sends `id` on
/// `eof_tx` at master EOF so the registry can reap it).
pub fn spawn_attachment(
    argv: &[String],
    cols: u16,
    rows: u16,
    id: u64,
    live: LiveOwner,
    stdout: std::io::Stdout,
    eof_tx: tokio::sync::mpsc::UnboundedSender<u64>,
) -> anyhow::Result<Attachment> {
    anyhow::ensure!(!argv.is_empty(), "spawn_attachment: argv must not be empty");
    let pty = native_pty_system();
    let pair = pty.openpty(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    let mut cmd = CommandBuilder::new(&argv[0]);
    for arg in &argv[1..] {
        cmd.arg(arg);
    }
    cmd.env_remove("PSMUX_SESSION");
    cmd.env_remove("TMUX");
    cmd.env_remove("TMUX_PANE");
    let child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader()?;
    let pty_writer = pair.master.take_writer()?;

    let (control_tx, control_rx) = std::sync::mpsc::channel::<PtyCmd>();
    std::thread::spawn(move || {
        pty_control_loop(
            control_rx,
            MasterSink {
                writer: pty_writer,
                master: pair.master,
            },
        )
    });

    let grid = Arc::new(Mutex::new(Grid::new(rows, cols)));
    let pump_grid = grid.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let chunk = &buf[..n];
                    if let Ok(mut g) = pump_grid.lock() {
                        g.feed(chunk);
                    }
                    if live.is_owner(id) {
                        pump_write(&stdout, chunk);
                    }
                }
                Err(_) => break,
            }
        }
        let _ = eof_tx.send(id); // master EOF: ask the registry to reap us
    });

    Ok(Attachment {
        grid,
        control_tx,
        size: (cols, rows),
        last_used: Instant::now(),
        child,
        id,
    })
}
```

Add `use portable_pty::Child;` is not needed — the field uses the fully-qualified `portable_pty::Child`. Confirm `Mutex` and `AtomicU64` are imported at the top.

- [ ] **Step 5: Run to verify it passes**

Run: `cargo test -p xmux proxy::run::tests::live_owner 2> test.err; echo "exit=$?"; cat test.err`
Expected: PASS. (The `#[ignore]` smoke test is not run.)

- [ ] **Step 6: Verify build + clippy**

Run: `cargo build 2> build.err; echo "exit=$?"; cargo clippy --all-targets 2> clippy.err; echo "clippy=$?"; grep -c warning clippy.err`
Expected: build=0, clippy=0, 0 warnings.

- [ ] **Step 7: Commit**

```bash
git add src/proxy/run.rs
git commit -m "feat(proxy): extract reusable per-Attachment unit with live-owner gate"
```

---

### Task 5: `AttachRegistry` — keep cap, LRU eviction, reap

**Files:**
- Create: `src/proxy/registry.rs`.
- Modify: `src/proxy/mod.rs` (add `pub mod registry;`).
- Test: `src/proxy/registry.rs` (`#[cfg(test)] mod tests`).

**Interfaces:**
- Consumes: `Attachment` (`id()`, `grid`, `resize`, `teardown`, `last_used`), `spawn_attachment(..)`, `LiveOwner` (Task 4).
- Produces:
  - `pub struct AttachRegistry { map: HashMap<String, Attachment>, cap: usize, next_id: u64, live: LiveOwner, stdout: std::io::Stdout, eof_tx: tokio::sync::mpsc::UnboundedSender<u64> }`.
  - `pub fn new(cap: usize, live: LiveOwner, eof_tx: tokio::sync::mpsc::UnboundedSender<u64>) -> Self`.
  - `pub fn contains(&self, addr: &str) -> bool`.
  - `pub fn get(&self, addr: &str) -> Option<&Attachment>` and `pub fn touch(&mut self, addr: &str)` (sets `last_used = Instant::now()`).
  - `pub fn id_of(&self, addr: &str) -> Option<u64>`.
  - `pub fn ensure(&mut self, addr: &str, argv: &[String], cols: u16, rows: u16, protect: &[&str]) -> anyhow::Result<u64>` — attaches `addr` if absent (evicting LRU among entries not in `protect` when at cap), returns its id; if present, touches and returns its id.
  - `pub fn reap(&mut self, id: u64)` — removes the entry whose attachment id == `id` (master EOF), tearing it down.
  - `pub fn resize_all(&mut self, cols: u16, rows: u16)`.
  - `pub fn teardown_all(self)` — drains and tears down every attachment (Task 4 `teardown` per entry; bounded — `teardown` never blocks the caller because the control thread owns the master).
  - The eviction unit is exposed for testing: `fn lru_victim(&self, protect: &[&str]) -> Option<String>`.

- [ ] **Step 1: Write the failing tests (eviction logic, no real PTY)**

Because `ensure` spawns a real ConPTY, the headless tests target the pure eviction selection. Create `src/proxy/registry.rs` with a test module that builds a registry, inserts fake `Attachment`s via a test-only constructor, and asserts `lru_victim`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    // A registry whose map we populate directly with fake entries (no PTY), to
    // test the LRU/protect selection in isolation. spawn_attachment is live-only
    // (Task 14).
    fn empty_registry(cap: usize) -> AttachRegistry {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<u64>();
        AttachRegistry::new(cap, crate::proxy::run::LiveOwner::new(), tx)
    }

    #[test]
    fn lru_victim_picks_oldest_unprotected() {
        let mut reg = empty_registry(3);
        let now = Instant::now();
        reg.insert_fake("local/a", 1, now - Duration::from_secs(30));
        reg.insert_fake("jupiter06/b", 2, now - Duration::from_secs(10));
        reg.insert_fake("jupiter06/c", 3, now - Duration::from_secs(20));
        // a is oldest, but protect it: the next-oldest unprotected (c) is the victim.
        assert_eq!(
            reg.lru_victim(&["local/a"]).as_deref(),
            Some("jupiter06/c")
        );
        // With nothing protected, the oldest (a) is the victim.
        assert_eq!(reg.lru_victim(&[]).as_deref(), Some("local/a"));
    }

    #[test]
    fn lru_victim_none_when_all_protected() {
        let mut reg = empty_registry(2);
        let now = Instant::now();
        reg.insert_fake("local/a", 1, now);
        reg.insert_fake("jupiter06/b", 2, now);
        assert!(reg.lru_victim(&["local/a", "jupiter06/b"]).is_none());
    }

    #[test]
    fn reap_removes_by_id() {
        let mut reg = empty_registry(3);
        let now = Instant::now();
        reg.insert_fake("jupiter06/b", 2, now);
        assert!(reg.contains("jupiter06/b"));
        reg.reap(2);
        assert!(!reg.contains("jupiter06/b"), "reap removes the EOF'd attachment");
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p xmux proxy::registry 2> test.err; echo "exit=$?"; cat test.err`
Expected: FAIL — module/file does not exist.

- [ ] **Step 3: Add the module declaration**

In `src/proxy/mod.rs` add (keep alphabetical-ish order):

```rust
pub mod registry;
```

- [ ] **Step 4: Write the registry**

Create `src/proxy/registry.rs`:

```rust
//! The AttachRegistry: an `Address → Attachment` map bounded by the keep cap.
//! When attaching would exceed the cap it evicts the least-recently-used
//! attachment that is neither the foreground nor the current cursor session.
//! A master-EOF reap removes the dead attachment. All blocking PTY work lives on
//! each Attachment's control/pump threads (Task 4), so registry methods never
//! block the event loop.

use std::collections::HashMap;
use std::time::Instant;

use crate::proxy::run::{spawn_attachment, Attachment, LiveOwner};

pub struct AttachRegistry {
    map: HashMap<String, Attachment>,
    cap: usize,
    next_id: u64,
    live: LiveOwner,
    eof_tx: tokio::sync::mpsc::UnboundedSender<u64>,
}

impl AttachRegistry {
    pub fn new(
        cap: usize,
        live: LiveOwner,
        eof_tx: tokio::sync::mpsc::UnboundedSender<u64>,
    ) -> Self {
        AttachRegistry {
            map: HashMap::new(),
            cap: cap.max(2),
            next_id: 1,
            live,
            eof_tx,
        }
    }

    pub fn contains(&self, addr: &str) -> bool {
        self.map.contains_key(addr)
    }

    pub fn get(&self, addr: &str) -> Option<&Attachment> {
        self.map.get(addr)
    }

    pub fn id_of(&self, addr: &str) -> Option<u64> {
        self.map.get(addr).map(Attachment::id)
    }

    pub fn touch(&mut self, addr: &str) {
        if let Some(att) = self.map.get_mut(addr) {
            att.last_used = Instant::now();
        }
    }

    /// The address of the least-recently-used attachment that is not protected,
    /// or `None` when every attachment is protected.
    fn lru_victim(&self, protect: &[&str]) -> Option<String> {
        self.map
            .iter()
            .filter(|(addr, _)| !protect.contains(&addr.as_str()))
            .min_by_key(|(_, att)| att.last_used)
            .map(|(addr, _)| addr.clone())
    }

    /// Ensures `addr` is attached, evicting an unprotected LRU entry first when at
    /// cap. Returns the attachment's id. `protect` lists addresses that must not be
    /// evicted (the foreground + the current cursor session).
    pub fn ensure(
        &mut self,
        addr: &str,
        argv: &[String],
        cols: u16,
        rows: u16,
        protect: &[&str],
    ) -> anyhow::Result<u64> {
        if let Some(att) = self.map.get_mut(addr) {
            att.last_used = Instant::now();
            return Ok(att.id());
        }
        while self.map.len() >= self.cap {
            match self.lru_victim(protect) {
                Some(victim) => {
                    if let Some(att) = self.map.remove(&victim) {
                        att.teardown();
                    }
                }
                None => break, // everything protected; allow a transient over-cap
            }
        }
        let id = self.next_id;
        self.next_id += 1;
        let att = spawn_attachment(
            argv,
            cols,
            rows,
            id,
            self.live.clone(),
            std::io::stdout(),
            self.eof_tx.clone(),
        )?;
        self.map.insert(addr.to_string(), att);
        Ok(id)
    }

    /// Removes the attachment whose id == `id` (its master hit EOF), tearing it
    /// down. A no-op if it was already evicted.
    pub fn reap(&mut self, id: u64) {
        let addr = self
            .map
            .iter()
            .find(|(_, att)| att.id() == id)
            .map(|(addr, _)| addr.clone());
        if let Some(addr) = addr {
            if let Some(att) = self.map.remove(&addr) {
                att.teardown();
            }
        }
    }

    /// Resizes every kept attachment to `cols×rows` (one PtyCmd::Resize each, off
    /// the loop via their control threads).
    pub fn resize_all(&mut self, cols: u16, rows: u16) {
        for att in self.map.values_mut() {
            att.resize(cols, rows);
            att.size = (cols, rows);
        }
    }

    /// Tears down every attachment (on quit). Each `teardown` signals its control
    /// thread and returns immediately; the threads drop their masters off the loop.
    pub fn teardown_all(self) {
        for (_addr, att) in self.map {
            att.teardown();
        }
    }
}

#[cfg(test)]
impl AttachRegistry {
    /// Test-only: insert a fake entry without a real PTY, to exercise the LRU /
    /// protect / reap selection in isolation. The fake Attachment's child + threads
    /// are dummies that are never driven.
    fn insert_fake(&mut self, addr: &str, id: u64, last_used: std::time::Instant) {
        let att = crate::proxy::run::fake_attachment(id, last_used);
        self.map.insert(addr.to_string(), att);
    }

    fn lru_victim_pub(&self, protect: &[&str]) -> Option<String> {
        self.lru_victim(protect)
    }
}
```

Adjust the tests to call the public wrapper: in the test bodies, replace `reg.lru_victim(..)` with `reg.lru_victim_pub(..)` (the method is private; the wrapper exposes it for tests). And add `crate::proxy::run::fake_attachment` (test-only) to `src/proxy/run.rs`:

```rust
/// Test-only: a no-PTY Attachment for registry selection tests. Its control
/// channel sender drops immediately (the receiver is discarded) and its child is
/// a never-spawned dummy. NEVER call input/resize/teardown that touch real I/O on
/// it beyond field reads.
#[cfg(test)]
pub fn fake_attachment(id: u64, last_used: Instant) -> Attachment {
    let (control_tx, _control_rx) = std::sync::mpsc::channel::<PtyCmd>();
    Attachment {
        grid: Arc::new(Mutex::new(Grid::new(24, 80))),
        control_tx,
        size: (80, 24),
        last_used,
        child: Box::new(DummyChild),
        id,
    }
}

#[cfg(test)]
struct DummyChild;
#[cfg(test)]
impl portable_pty::Child for DummyChild {
    fn try_wait(&mut self) -> std::io::Result<Option<portable_pty::ExitStatus>> {
        Ok(None)
    }
    fn wait(&mut self) -> std::io::Result<portable_pty::ExitStatus> {
        Ok(portable_pty::ExitStatus::with_exit_code(0))
    }
    fn kill(&mut self) -> std::io::Result<()> {
        Ok(())
    }
    fn process_id(&self) -> Option<u32> {
        None
    }
}
```

(Verify the `portable_pty::Child` trait method set against 0.9 with `cargo build`; if `Child` requires `process_group_id` or differs, match the compiler's required-methods error exactly. Do not invent methods — let the build dictate them.)

- [ ] **Step 5: Run to verify it passes**

Run: `cargo test -p xmux proxy::registry 2> test.err; echo "exit=$?"; cat test.err`
Expected: PASS (3 tests).

- [ ] **Step 6: Verify build + clippy**

Run: `cargo build 2> build.err; echo "exit=$?"; cargo clippy --all-targets 2> clippy.err; echo "clippy=$?"; grep -c warning clippy.err`
Expected: build=0, clippy=0, 0 warnings.

- [ ] **Step 7: Commit**

```bash
git add src/proxy/registry.rs src/proxy/mod.rs src/proxy/run.rs
git commit -m "feat(proxy): add AttachRegistry with LRU eviction and EOF reap"
```

---

### Task 6: `AppState` + transition handoff

**Files:**
- Create: `src/proxy/app.rs`.
- Modify: `src/proxy/mod.rs` (`pub mod app;`).
- Test: `src/proxy/app.rs` (`#[cfg(test)] mod tests`).

**Interfaces:**
- Consumes: `LiveOwner` (Task 4), `Grid::restore_bytes()` (existing), `Grid::cursor()` (Task 3).
- Produces:
  - `pub enum AppState { Passthrough { fg: String, fg_id: u64 }, Overlay }` (`fg` is the foreground Address).
  - `pub struct App { pub state: AppState, pub prev_fg: Option<(String, u64)>, live: LiveOwner }`.
  - `pub fn new(live: LiveOwner) -> Self` (starts in `Overlay`, `prev_fg = None`).
  - `pub fn enter_overlay(&mut self)` — remembers the current foreground into `prev_fg` (if Passthrough), sets `state = Overlay`, calls `live.set_overlay()`.
  - `pub fn enter_passthrough(&mut self, fg: String, fg_id: u64, restore: &[u8], status_bar: &[u8])` — writes `restore` + `status_bar` under a single per-write stdout lock WHILE pumps are still gated off, then sets `state = Passthrough{fg,fg_id}` and `live.set_owner(fg_id)` (the foreground pump resumes). Returns nothing.
  - `pub fn esc_target(&self) -> Option<(String, u64)>` — `prev_fg.clone()` (what `Esc` returns to; `None` at the initial state → Esc is a no-op).
  - `pub fn is_overlay(&self) -> bool`.

- [ ] **Step 1: Write the failing tests**

Create `src/proxy/app.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::run::LiveOwner;

    #[test]
    fn starts_in_overlay_with_no_prev_fg() {
        let app = App::new(LiveOwner::new());
        assert!(app.is_overlay());
        assert!(app.esc_target().is_none(), "initial Overlay has no Esc target");
    }

    #[test]
    fn overlay_remembers_then_esc_returns_previous() {
        let live = LiveOwner::new();
        let mut app = App::new(live.clone());
        // Enter passthrough on local/a (id 1): live owner becomes 1.
        app.enter_passthrough("local/a".into(), 1, b"", b"");
        assert!(!app.is_overlay());
        assert!(live.is_owner(1));
        // Open overlay: pumps stop writing stdout; prev_fg remembers local/a.
        app.enter_overlay();
        assert!(app.is_overlay());
        assert!(!live.is_owner(1), "overlay gates all pumps off stdout");
        assert_eq!(app.esc_target(), Some(("local/a".to_string(), 1)));
    }

    #[test]
    fn passthrough_switch_changes_live_owner() {
        let live = LiveOwner::new();
        let mut app = App::new(live.clone());
        app.enter_passthrough("local/a".into(), 1, b"", b"");
        app.enter_overlay();
        // Enter on a new session promotes jupiter06/b (id 2) to foreground.
        app.enter_passthrough("jupiter06/b".into(), 2, b"", b"");
        assert!(live.is_owner(2));
        assert!(!live.is_owner(1));
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p xmux proxy::app 2> test.err; echo "exit=$?"; cat test.err`
Expected: FAIL — module does not exist.

- [ ] **Step 3: Add the module declaration**

In `src/proxy/mod.rs`:

```rust
pub mod app;
```

- [ ] **Step 4: Write `AppState`/`App`**

Create `src/proxy/app.rs` (above the test module):

```rust
//! The shared application state machine. Two states only: Passthrough (a
//! foreground attachment owns raw stdout at cols×(rows-1) plus the status bar)
//! and Overlay (ratatui owns stdout, drawing the sidebar + terminal view). The
//! transition handoff guarantees exactly one writer touches real stdout: in
//! Overlay every pump is gated off via `LiveOwner`; entering Passthrough paints
//! the restore + status bar under a per-write lock BEFORE re-enabling the
//! foreground pump.

use std::io::Write;

use crate::proxy::run::LiveOwner;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppState {
    /// `fg` is the foreground Address; `fg_id` its Attachment id.
    Passthrough { fg: String, fg_id: u64 },
    Overlay,
}

pub struct App {
    pub state: AppState,
    /// The foreground to return to on Esc; `None` at the initial Overlay.
    pub prev_fg: Option<(String, u64)>,
    live: LiveOwner,
}

impl App {
    /// Starts in Overlay (the cursor preselected elsewhere), no previous fg.
    pub fn new(live: LiveOwner) -> Self {
        live.set_overlay();
        App {
            state: AppState::Overlay,
            prev_fg: None,
            live,
        }
    }

    pub fn is_overlay(&self) -> bool {
        matches!(self.state, AppState::Overlay)
    }

    /// What Esc returns to (the previous foreground), or None at the initial state.
    pub fn esc_target(&self) -> Option<(String, u64)> {
        self.prev_fg.clone()
    }

    /// Passthrough → Overlay: remember the foreground, gate all pumps off stdout.
    pub fn enter_overlay(&mut self) {
        if let AppState::Passthrough { fg, fg_id } = &self.state {
            self.prev_fg = Some((fg.clone(), *fg_id));
        }
        self.state = AppState::Overlay;
        self.live.set_overlay();
    }

    /// Overlay → Passthrough{fg}: while pumps are still gated off stdout, paint
    /// the foreground restore + status bar under ONE per-write lock; then set the
    /// state and re-enable the foreground pump (it resumes raw writes). This
    /// mirrors the reviewed overlay-close restore in `proxy_attach`.
    pub fn enter_passthrough(&mut self, fg: String, fg_id: u64, restore: &[u8], status_bar: &[u8]) {
        // Pumps are gated off here (we are still Overlay/owner=sentinel), so this
        // single lock cannot race a pump write.
        {
            let out = std::io::stdout();
            let mut lock = out.lock();
            let _ = lock.write_all(restore);
            let _ = lock.write_all(status_bar);
            let _ = lock.flush();
        }
        self.state = AppState::Passthrough { fg, fg_id };
        self.live.set_owner(fg_id);
    }
}
```

- [ ] **Step 5: Run to verify it passes**

Run: `cargo test -p xmux proxy::app 2> test.err; echo "exit=$?"; cat test.err`
Expected: PASS (3 tests).

- [ ] **Step 6: Verify clippy**

Run: `cargo clippy --all-targets 2> clippy.err; echo "exit=$?"; grep -c warning clippy.err`
Expected: exit=0, 0 warnings.

- [ ] **Step 7: Commit**

```bash
git add src/proxy/app.rs src/proxy/mod.rs
git commit -m "feat(proxy): add AppState with one-writer transition handoff"
```

---

### Task 7: `InputMachine` prefix actions — `s` open Overlay, `q` quit

**Files:**
- Modify: `src/proxy/input.rs:6-10` (extend `InAction`), `src/proxy/input.rs:44-82` (handle `s`/`q` after the prefix).
- Test: `src/proxy/input.rs` (`#[cfg(test)] mod tests`).

**Interfaces:**
- Consumes: nothing new (the `ProxyConfig.action_key` is no longer the only command; this task adds a quit key).
- Produces:
  - `pub enum InAction { Forward(Vec<u8>), OpenOverlay, Quit }` — renames `OpenPicker` → `OpenOverlay` (the overlay is now the persistent sidebar, not a one-shot picker), adds `Quit`.
  - `InputMachine::new(prefix: u8, action_key: u8, quit_key: u8, timeout: Duration)` — adds `quit_key` (the cockpit passes `b's'` and `b'q'`).

- [ ] **Step 1: Update tests for the new shape (failing build)**

In `src/proxy/input.rs` `mod tests`, change `fn m()` and the assertions:

```rust
    fn m() -> InputMachine {
        InputMachine::new(0x07, b's', b'q', Duration::from_millis(400))
    }
```

Rename the `OpenPicker` matches to `OpenOverlay` in `prefix_then_action_opens_picker` (keep the test, rename to `prefix_then_s_opens_overlay`):

```rust
    #[test]
    fn prefix_then_s_opens_overlay() {
        let mut im = m();
        let t = Instant::now();
        assert!(im.feed(0x07, t).is_empty(), "prefix is swallowed while arming");
        let out = im.feed(b's', t);
        assert!(matches!(out.as_slice(), [InAction::OpenOverlay]));
    }

    #[test]
    fn prefix_then_q_quits() {
        let mut im = m();
        let t = Instant::now();
        assert!(im.feed(0x07, t).is_empty());
        let out = im.feed(b'q', t);
        assert!(matches!(out.as_slice(), [InAction::Quit]));
    }
```

Update `prefix_inside_bracketed_paste_is_literal` to match `OpenOverlay` instead of `OpenPicker`.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p xmux proxy::input 2> test.err; echo "exit=$?"; cat test.err`
Expected: FAIL — `OpenOverlay`/`Quit` unknown, `new` arity mismatch.

- [ ] **Step 3: Implement**

In `src/proxy/input.rs`, change the enum:

```rust
#[derive(Debug, PartialEq)]
pub enum InAction {
    Forward(Vec<u8>),
    OpenOverlay,
    Quit,
}
```

Add a `quit_key` field and constructor param:

```rust
pub struct InputMachine {
    prefix: u8,
    action_key: u8,
    quit_key: u8,
    timeout: Duration,
    state: State,
    in_paste: bool,
    paste_scan: Vec<u8>,
}

impl InputMachine {
    pub fn new(prefix: u8, action_key: u8, quit_key: u8, timeout: Duration) -> Self {
        Self {
            prefix,
            action_key,
            quit_key,
            timeout,
            state: State::Idle,
            in_paste: false,
            paste_scan: Vec::new(),
        }
    }
```

In the `State::Armed(t)` branch, after the stale-prefix check, replace the action dispatch:

```rust
                self.state = State::Idle;
                if byte == self.action_key {
                    vec![InAction::OpenOverlay]
                } else if byte == self.quit_key {
                    vec![InAction::Quit]
                } else if byte == self.prefix {
                    vec![InAction::Forward(vec![self.prefix])]
                } else {
                    Vec::new()
                }
```

- [ ] **Step 4: Fix the one external caller (`proxy_attach`)**

In `src/proxy/run.rs`, the existing `proxy_attach` uses `InAction::OpenPicker` and `InputMachine::new(cfg.prefix, cfg.action_key, ..)`. `proxy_attach` is REPLACED in Task 12, but it must compile now. Change its `InputMachine::new` call to pass `b'q'` as the quit key and its `match action` arm `InAction::OpenPicker =>` to `InAction::OpenOverlay =>`; add a `InAction::Quit => { mode = Mode::Quitting; }` arm. (These are interim; Task 12 rewrites this path.)

- [ ] **Step 5: Run to verify it passes**

Run: `cargo test -p xmux proxy::input 2> test.err; echo "exit=$?"; cat test.err`
Expected: PASS.

- [ ] **Step 6: Verify build + clippy**

Run: `cargo build 2> build.err; echo "exit=$?"; cargo clippy --all-targets 2> clippy.err; echo "clippy=$?"; grep -c warning clippy.err`
Expected: build=0, clippy=0, 0 warnings.

- [ ] **Step 7: Commit**

```bash
git add src/proxy/input.rs src/proxy/run.rs
git commit -m "feat(proxy): InputMachine prefix actions OpenOverlay (s) and Quit (q)"
```

---

### Task 8: Switcher terminal view (live Grid) replaces capture

**Files:**
- Modify: `src/ui/switcher.rs` — drop `Ops::capture`, `preview_cache`/`preview_cache_order`/`preview_text`/`PREVIEW_CACHE_CAP`/`apply_capture`/`cache_preview`/`preview_capturable`; rename `PreviewTarget`→`TerminalViewTarget`, `preview_target`→`terminal_view_target`, `preview_self`→`terminal_view_self`, `preview_title`→`terminal_view_title`, `PREVIEW_SELF_NOTE`→`TERMINAL_VIEW_SELF_NOTE`; replace `render_preview` with `render_terminal_view` that renders a borrowed live `Grid`.
- Modify: `src/ui/switcher.rs` `Ops` trait — remove `async fn capture`.
- Modify: `src/ui/run.rs` — drop `Cmd::Preview`, `spawn_capture`, `capture_inflight`, the poll-tick capture; remove `Ops::capture` impls from test doubles.
- Modify: `src/env.rs` — remove `EnvOps::capture`.
- Modify: `src/cockpit.rs`, `src/proxy/run.rs` test doubles — remove their `capture` impls.
- Test: `src/ui/switcher.rs` (`mod tests`).

**Interfaces:**
- Consumes: `Grid::render_into`, `Grid::cursor`, `Grid::hide_cursor` (Task 3).
- Produces:
  - `pub struct TerminalViewTarget { pub source: String, pub target: String }` (was `PreviewTarget`; same fields).
  - `Switcher::terminal_view_target(&self) -> TerminalViewTarget`.
  - `Switcher::terminal_view_self(&self) -> bool` (the self-mirror guard, was `preview_self`; true when the focused active pane runs xmux).
  - `Switcher::render_terminal_view(&self, frame: &mut Frame, area: Rect, grid: Option<&Grid>)` — draws the bordered terminal view: if `terminal_view_self`, the self note; else if `grid` is `Some`, `grid.render_into` into the inner area; else a "(attaching…)" placeholder.
  - `Switcher::render(&mut self, frame: &mut Frame, grid: Option<&Grid>)` — the render signature gains the optional live grid for the cursor session (the event loop passes the registry's grid for `terminal_view_target`).
  - `Ops` trait loses `capture`.

- [ ] **Step 1: Write the failing tests**

In `src/ui/switcher.rs` `mod tests`, replace the capture/preview tests. The `Harness::draw` and `dump_switcher` calls must pass `None` for the grid. Add:

```rust
    #[tokio::test]
    async fn terminal_view_target_follows_cursor() {
        let mut h = Harness::new(sample());
        h.key(KeyCode::Home).await; // local host
        let t = h.sw.terminal_view_target();
        assert_eq!((t.source.as_str(), t.target.as_str()), ("local", "editor"));
        h.key(KeyCode::Down).await; // editor
        let t = h.sw.terminal_view_target();
        assert_eq!((t.source.as_str(), t.target.as_str()), ("local", "editor"));
    }

    #[tokio::test]
    async fn self_mirror_guard_suppresses_terminal_view() {
        let groups = vec![Group {
            source: "local".into(),
            err: None,
            sessions: vec![sess("local", "selfsess", 1, true, 500)],
        }];
        let mut panes = HashMap::new();
        panes.insert(
            "local/selfsess".to_string(),
            vec![win(0, "xmux", true, vec![pane(0, true, "xmux")])],
        );
        let h = Harness::new(Scan { groups, panes });
        assert!(
            h.sw.terminal_view_self(),
            "a pane running xmux must be flagged self-mirror (no live attach)"
        );
        assert!(
            h.text().contains("running xmux"),
            "the terminal view shows the self note:\n{}",
            h.text()
        );
    }

    #[tokio::test]
    async fn render_terminal_view_draws_live_grid() {
        use crate::proxy::screen::Grid;
        let mut h = Harness::new(sample());
        h.key(KeyCode::Down).await; // editor (a normal pane)
        let mut g = Grid::new(28, 50);
        g.feed(b"LIVE-GRID-CONTENT");
        // Render with the live grid supplied.
        let sw = &mut h.sw;
        h.term.draw(|f| sw.render(f, Some(&g))).unwrap();
        let out = buffer_text(h.term.backend().buffer());
        assert!(
            out.contains("LIVE-GRID-CONTENT"),
            "the terminal view renders the live grid's contents:\n{out}"
        );
    }
```

Delete the now-obsolete tests: `preview_cache_is_bounded`, `preview_reconnecting_on_revisit`, `preview_shows_loading_until_fetched`, `preview_blank_on_host_without_session`, `preview_suppressed_when_focused_pane_runs_xmux` (replaced above), `preview_captures_when_focused_pane_is_not_xmux`, `preview_target_follows_cursor` (replaced above). Update `Harness::draw` to `self.term.draw(|f| sw.render(f, None)).unwrap();`.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p xmux ui::switcher 2> test.err; echo "exit=$?"; cat test.err`
Expected: FAIL — `terminal_view_target`/`terminal_view_self`/`render(f, Some)` not found; `capture` removed but still referenced.

- [ ] **Step 3: Remove capture from the `Ops` trait + all impls**

In `src/ui/switcher.rs`, delete `async fn capture(&self, source: &str, target: &str) -> anyhow::Result<String>;` from the `Ops` trait. Remove the `capture` impl from: `src/env.rs` `EnvOps`, `src/ui/switcher.rs` `RecordOps`, `src/ui/run.rs` `NoopOps`/`GatedOps`/`StreamOps`, `src/proxy/run.rs` `NoopOps`. Also delete the now-dead `env.rs` test `capture_shares_the_probe_semaphore`.

- [ ] **Step 4: Rename + rewrite the terminal view in `switcher.rs`**

Rename the type and fields throughout `switcher.rs`:
- `PreviewTarget` → `TerminalViewTarget` (and its public re-exports/uses in `run.rs`).
- `preview_target` → `terminal_view_target`, `preview_self` → `terminal_view_self_flag` (field), `preview_title` → `terminal_view_title`, `PREVIEW_SELF_NOTE` → `TERMINAL_VIEW_SELF_NOTE`.
- Delete fields `preview_cache`, `preview_cache_order`, `preview_text`, `dialog`, `poll_kick`, and methods `apply_capture`, `cache_preview`, `preview_cache_len`, `preview_capturable`, `take_poll_kick`, `refresh_preview_self`'s cache bits.

Add accessors:

```rust
    pub fn terminal_view_target(&self) -> TerminalViewTarget {
        self.terminal_view_target.clone()
    }
    pub fn terminal_view_self(&self) -> bool {
        self.terminal_view_self_flag
    }
```

Replace `on_focus_changed` so it only tracks the target + self flag (no cache/dialog/poll):

```rust
    fn on_focus_changed(&mut self) {
        let tgt = match self.current_ref() {
            Some(r) => self.target_for(r),
            None => TerminalViewTarget::default(),
        };
        self.terminal_view_target = tgt.clone();
        if tgt.target.is_empty() {
            self.terminal_view_title = " Terminal ".into();
            self.terminal_view_self_flag = false;
            return;
        }
        self.terminal_view_title = format!(" {} ", tgt.target);
        self.terminal_view_self_flag = self.focused_runs_xmux();
    }
```

Set `TERMINAL_VIEW_SELF_NOTE` to:

```rust
const TERMINAL_VIEW_SELF_NOTE: &str = "  This pane is running xmux.\n\n  Live view hidden here so xmux is not\n  attached inside its own terminal view.";
```

Replace `render_preview` with `render_terminal_view`:

```rust
    fn render_terminal_view(&self, frame: &mut Frame, area: Rect, grid: Option<&crate::proxy::screen::Grid>) {
        let block = Block::bordered().title(self.terminal_view_title.clone());
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if self.terminal_view_self_flag {
            frame.render_widget(Paragraph::new(TERMINAL_VIEW_SELF_NOTE), inner);
            return;
        }
        match grid {
            Some(g) => {
                let buf = frame.buffer_mut();
                g.render_into(buf, inner);
            }
            None => {
                frame.render_widget(Paragraph::new("  (attaching…)").dim(), inner);
            }
        }
    }
```

Change `render` to thread the grid:

```rust
    pub fn render(&mut self, frame: &mut Frame, grid: Option<&crate::proxy::screen::Grid>) {
        // ... unchanged layout ...
        self.render_tree(frame, mid[0]);
        self.render_terminal_view(frame, mid[1], grid);
        // ... input + footer + help unchanged ...
    }
```

Remove `use crate::ui::ansi::ansi_to_text;` if now unused (the live grid does not go through ansi text). Keep `ansi.rs` itself (other callers may exist — check with `cargo build`; if unused crate-wide, leave it, do not delete pre-existing code beyond your change).

- [ ] **Step 5: Update `run.rs` render call sites + remove capture machinery**

In `src/ui/run.rs`:
- `dump_switcher`: change `switcher.render(f)` → `switcher.render(f, None)` (the dump renders the tree skeleton; the live grid is a separate live-only concern, shown as `(attaching…)` in dumps).
- `event_loop`: change `terminal.draw(|f| switcher.render(f))` → `terminal.draw(|f| switcher.render(f, None))`. (Task 12 supplies the real grid in the cockpit's loop; the standalone `event_loop` is exercised headlessly without a registry.)
- Delete `spawn_capture`, `capture_inflight`, the `Cmd::Preview` variant + its match arm, the `take_poll_kick` calls, and the poll-tick `spawn_capture`. Keep `POLL_INTERVAL`/`poll.tick()` only if Task 9 needs the animation tick — for now, delete the poll branch's capture body, leaving the tick as a no-op (Task 9 repurposes it).

Since `Cmd::Preview` is removed, update the `Cmd` enum doc and any match exhaustiveness.

- [ ] **Step 6: Run to verify it passes**

Run: `cargo test -p xmux ui::switcher ui::run 2> test.err; echo "exit=$?"; cat test.err`
Expected: PASS.

- [ ] **Step 7: Verify build + clippy (whole crate — capture removal is cross-cutting)**

Run: `cargo build 2> build.err; echo "exit=$?"; cargo clippy --all-targets 2> clippy.err; echo "clippy=$?"; grep -c warning clippy.err`
Expected: build=0, clippy=0, 0 warnings.

- [ ] **Step 8: Commit**

```bash
git add src/ui/switcher.rs src/ui/run.rs src/env.rs src/cockpit.rs src/proxy/run.rs
git commit -m "feat(ui): live terminal view replaces capture snapshot"
```

---

### Task 9: Dwell trigger (500ms) + left→right progress animation

**Files:**
- Modify: `src/ui/switcher.rs` — add dwell tracking (`dwell_start`, `dwell_addr`), `dwell_pending`, `dwell_progress`, `take_dwell_attach`; overlay the progress fill in `render_tree`.
- Modify: `src/ui/run.rs` — add a `~33ms` animation tick active only while a dwell is pending; on a completed dwell, the cockpit (Task 12) reads the attach request.
- Test: `src/ui/switcher.rs` (`mod tests`).

**Interfaces:**
- Consumes: `TerminalViewTarget` (Task 8), `tree::sort_by_recency` (existing).
- Produces (on `Switcher`):
  - `pub const DWELL: Duration = Duration::from_millis(500);` (module const).
  - `pub fn dwell_pending(&self) -> bool` — true when the cursor rests on an attachable session/window row that is not yet attached-confirmed and the 500ms window has not elapsed.
  - `pub fn dwell_progress(&self, now: Instant) -> f32` — `clamp((now - dwell_start)/500ms, 0, 1)`; `0.0` when no dwell pending.
  - `pub fn note_attached(&mut self, addr: &str)` — marks `addr` as already attached (cap re-visit ⇒ no progress bar, instant view).
  - `pub fn take_dwell_attach(&mut self, now: Instant) -> Option<TerminalViewTarget>` — when a dwell completes (`progress >= 1`) on an attachable, not-yet-attached target, returns it once (clears the dwell) so the loop attaches it; otherwise `None`.
  - cursor moves reset `dwell_start` (already-attached targets do not start a dwell).

- [ ] **Step 1: Write the failing tests**

In `src/ui/switcher.rs` `mod tests`:

```rust
    #[tokio::test]
    async fn dwell_completes_after_500ms_and_yields_attach() {
        let mut h = Harness::new(sample());
        // inference preselected (attachable, not yet attached).
        let start = std::time::Instant::now();
        assert!(h.sw.dwell_pending(), "an unattached focused session starts a dwell");
        assert!(
            h.sw.take_dwell_attach(start).is_none(),
            "no attach before 500ms"
        );
        let done = start + std::time::Duration::from_millis(500);
        let got = h.sw.take_dwell_attach(done).expect("dwell completes");
        assert_eq!((got.source.as_str(), got.target.as_str()), ("jupiter00", "inference"));
        // Taken once: a second call yields nothing until the cursor moves.
        assert!(h.sw.take_dwell_attach(done).is_none());
    }

    #[tokio::test]
    async fn already_attached_target_skips_dwell() {
        let mut h = Harness::new(sample());
        h.sw.note_attached("jupiter00/inference");
        assert!(
            !h.sw.dwell_pending(),
            "an already-attached target shows live at once, no dwell bar"
        );
    }

    #[tokio::test]
    async fn cursor_move_resets_dwell() {
        let mut h = Harness::new(sample());
        let t0 = std::time::Instant::now();
        let _ = h.sw.dwell_progress(t0);
        h.key(KeyCode::Down).await; // move to inference's window
        // After a move, progress restarts near 0 even well past 500ms from t0.
        let later = t0 + std::time::Duration::from_millis(600);
        assert!(
            h.sw.dwell_progress(later) < 1.0,
            "moving the cursor restarts the dwell window"
        );
    }

    #[tokio::test]
    async fn non_attachable_row_has_no_dwell() {
        let mut h = Harness::new(sample());
        h.key(KeyCode::Down).await; // inference -> window
        h.key(KeyCode::Down).await; // -> db-2 (unreachable host, no session target)
        assert!(matches!(
            h.sw.current_ref(),
            Some(RowRef::Host { unreachable: true, .. })
        ));
        assert!(!h.sw.dwell_pending(), "an unreachable host row starts no dwell");
    }

    #[tokio::test]
    async fn self_mirror_session_has_no_dwell() {
        // A session whose active pane runs xmux must not auto-attach (infinite
        // mirror); the terminal view shows the self note instead of a dwell bar.
        let groups = vec![Group {
            source: "local".into(),
            err: None,
            sessions: vec![sess("local", "selfsess", 1, true, 500)],
        }];
        let mut panes = HashMap::new();
        panes.insert(
            "local/selfsess".to_string(),
            vec![win(0, "xmux", true, vec![pane(0, true, "xmux")])],
        );
        let h = Harness::new(Scan { groups, panes });
        assert!(h.sw.terminal_view_self(), "fixture focuses the xmux session");
        assert!(
            !h.sw.dwell_pending(),
            "a self-mirror session must not start a dwell/attach"
        );
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p xmux ui::switcher::tests::dwell ui::switcher::tests::already_attached ui::switcher::tests::cursor_move ui::switcher::tests::non_attachable ui::switcher::tests::self_mirror_session 2> test.err; echo "exit=$?"; cat test.err`
Expected: FAIL — `dwell_pending`/`take_dwell_attach`/`note_attached`/`dwell_progress` not found.

- [ ] **Step 3: Implement dwell state**

In `src/ui/switcher.rs`, add module const + fields:

```rust
/// How long the cursor must rest on an attachable row before xmux attaches it.
pub const DWELL: std::time::Duration = std::time::Duration::from_millis(500);
```

Add to `struct Switcher` (and `blank()`):

```rust
    /// When the cursor last settled on the current attachable target; `None` when
    /// the current row is not an attach candidate.
    dwell_start: Option<std::time::Instant>,
    /// The target the current dwell would attach (its address), set with
    /// `dwell_start`. Cleared once the attach is taken.
    dwell_addr: Option<String>,
    /// Addresses already attached (cap residents): no dwell bar, instant view.
    attached: std::collections::HashSet<String>,
```

In `blank()`: `dwell_start: None, dwell_addr: None, attached: HashSet::new(),`.

In `set_selected`, after `self.on_focus_changed();`, (re)arm the dwell:

```rust
        self.rearm_dwell();
```

Add the methods:

```rust
    /// The address the current row would attach, if it is an attachable, not-yet-
    /// attached session/window. Hosts/panes/loading and already-attached are None.
    fn dwell_candidate(&self) -> Option<String> {
        let addr = match self.current_ref()? {
            RowRef::Session(s) => s.address(),
            RowRef::Window { sess, .. } => sess.address(),
            RowRef::Host { .. } | RowRef::Pane | RowRef::Loading => return None,
        };
        if self.terminal_view_self_flag {
            return None; // active pane runs xmux: self-mirror guard, no auto-attach
        }
        if self.attached.contains(&addr) {
            return None; // already live; shown at once, no dwell
        }
        Some(addr)
    }

    fn rearm_dwell(&mut self) {
        match self.dwell_candidate() {
            Some(addr) => {
                self.dwell_start = Some(std::time::Instant::now());
                self.dwell_addr = Some(addr);
            }
            None => {
                self.dwell_start = None;
                self.dwell_addr = None;
            }
        }
    }

    pub fn dwell_pending(&self) -> bool {
        self.dwell_start.is_some()
    }

    pub fn dwell_progress(&self, now: std::time::Instant) -> f32 {
        match self.dwell_start {
            Some(start) => {
                let elapsed = now.saturating_duration_since(start).as_secs_f32();
                (elapsed / DWELL.as_secs_f32()).clamp(0.0, 1.0)
            }
            None => 0.0,
        }
    }

    pub fn note_attached(&mut self, addr: &str) {
        self.attached.insert(addr.to_string());
        // If the current dwell was for this addr, cancel it (now live).
        if self.dwell_addr.as_deref() == Some(addr) {
            self.dwell_start = None;
            self.dwell_addr = None;
        }
    }

    pub fn take_dwell_attach(&mut self, now: std::time::Instant) -> Option<TerminalViewTarget> {
        if self.dwell_progress(now) < 1.0 {
            return None;
        }
        let addr = self.dwell_addr.take()?;
        self.dwell_start = None;
        let tgt = self.terminal_view_target();
        // Defend against a target/addr drift: only attach the dwell's own target.
        if tgt.target.is_empty() {
            return None;
        }
        self.note_attached(&addr);
        Some(tgt)
    }
```

Note: `note_attached` must be callable while `rearm_dwell` is private; it is `pub` because the cockpit marks an attach confirmed.

- [ ] **Step 4: Overlay the progress fill in `render_tree`**

In `render_tree`, after `frame.render_stateful_widget(list, area, &mut self.list_state);`, add the fill on the selected row's span proportional to `dwell_progress`:

```rust
        // Dwell progress: fill the selected row's background left→right.
        if self.dwell_pending() {
            let progress = self.dwell_progress(std::time::Instant::now());
            if progress > 0.0 {
                let buf = frame.buffer_mut();
                let y = self.tree_inner.y + (self.selected as u16).saturating_sub(self.list_state.offset() as u16);
                if y >= self.tree_inner.y && y < self.tree_inner.bottom() {
                    let fill_w = ((self.tree_inner.width as f32) * progress) as u16;
                    for x in self.tree_inner.x..(self.tree_inner.x + fill_w).min(self.tree_inner.right()) {
                        buf[(x, y)].set_bg(Color::DarkGray);
                    }
                }
            }
        }
```

- [ ] **Step 5: Add the animation tick to `event_loop`**

In `src/ui/run.rs`, change the poll branch so the loop wakes every ~33ms ONLY while a dwell is pending (otherwise the 1s interval). Replace the `poll` interval usage:

```rust
const ANIM_INTERVAL: Duration = Duration::from_millis(33);
```

In the loop, before `tokio::select!`, compute the tick:

```rust
        let tick = if switcher.dwell_pending() {
            ANIM_INTERVAL
        } else {
            POLL_INTERVAL
        };
        let mut anim = tokio::time::interval(tick);
        anim.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
```

(Recreating the interval each iteration is acceptable: it is cheap and keeps the tick rate matched to whether a dwell is pending; the first immediate tick is skipped because we already drew above.) Replace the `_ = poll.tick()` branch body with a no-op that just loops (the redraw at the top of the loop paints the new progress). In the standalone `event_loop` (no registry), `take_dwell_attach` is not acted on — that wiring lives in the cockpit (Task 12). Delete the now-unused `let mut poll = ...` lines.

- [ ] **Step 6: Run to verify it passes**

Run: `cargo test -p xmux ui::switcher ui::run 2> test.err; echo "exit=$?"; cat test.err`
Expected: PASS.

- [ ] **Step 7: Verify build + clippy**

Run: `cargo build 2> build.err; echo "exit=$?"; cargo clippy --all-targets 2> clippy.err; echo "clippy=$?"; grep -c warning clippy.err`
Expected: build=0, clippy=0, 0 warnings.

- [ ] **Step 8: Commit**

```bash
git add src/ui/switcher.rs src/ui/run.rs
git commit -m "feat(ui): 500ms dwell trigger with left-to-right progress animation"
```

---

### Task 10: Tree ordering — local pinned first, remote hosts by recency

**Files:**
- Modify: `src/ui/tree.rs` — add `order_groups`.
- Modify: `src/ui/switcher.rs` — apply `order_groups` in `visible_groups`/`rebuild` so groups paint in recency order. (The global-max preselect already exists at `rebuild`; this only reorders host groups.)
- Test: `src/ui/tree.rs` (`mod tests`).

**Interfaces:**
- Consumes: `Group` (existing), `session::LOCAL_SOURCE` (existing).
- Produces:
  - `pub fn order_groups(groups: &[Group]) -> Vec<Group>` — returns the groups with the local source(s) (`source == LOCAL_SOURCE`) pinned first (in their original relative order), then the remaining (remote) groups sorted by each group's max session `last_attached` descending (a group with no sessions sorts last; ties by source name ascending). Inputs are not mutated.

- [ ] **Step 1: Write the failing test**

In `src/ui/tree.rs` `mod tests`:

```rust
    #[test]
    fn order_groups_pins_local_then_remote_by_recency() {
        let groups = vec![
            Group {
                source: "jupiter00".into(),
                err: None,
                sessions: vec![sess("jupiter00", "a", 100)],
            },
            Group {
                source: "local".into(),
                err: None,
                sessions: vec![sess("local", "w", 50)],
            },
            Group {
                source: "jupiter06".into(),
                err: None,
                sessions: vec![sess("jupiter06", "b", 300)],
            },
            Group {
                source: "deadhost".into(),
                err: Some("refused".into()),
                sessions: vec![],
            },
        ];
        let out = order_groups(&groups);
        let order: Vec<&str> = out.iter().map(|g| g.source.as_str()).collect();
        // local first; then remotes by max last_attached desc (jupiter06=300,
        // jupiter00=100); the empty/unreachable deadhost (no sessions) sorts last.
        assert_eq!(order, vec!["local", "jupiter06", "jupiter00", "deadhost"]);
    }

    #[test]
    fn order_groups_does_not_mutate_input() {
        let groups = sample_groups();
        let first = groups[0].source.clone();
        let _ = order_groups(&groups);
        assert_eq!(groups[0].source, first);
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p xmux ui::tree::tests::order_groups 2> test.err; echo "exit=$?"; cat test.err`
Expected: FAIL — `order_groups` not found.

- [ ] **Step 3: Implement `order_groups`**

In `src/ui/tree.rs`:

```rust
/// Orders host groups for display: the local source(s) pinned first (original
/// relative order), then remote groups by their most-recent session's
/// `last_attached` descending (a group with no sessions sorts last; ties by
/// source name ascending). Inputs are not mutated.
pub fn order_groups(groups: &[Group]) -> Vec<Group> {
    use crate::session::LOCAL_SOURCE;
    let mut local: Vec<Group> = Vec::new();
    let mut remote: Vec<Group> = Vec::new();
    for g in groups {
        if g.source == LOCAL_SOURCE {
            local.push(g.clone());
        } else {
            remote.push(g.clone());
        }
    }
    remote.sort_by(|a, b| {
        let am = a.sessions.iter().map(|s| s.last_attached).max();
        let bm = b.sessions.iter().map(|s| s.last_attached).max();
        // Some(max) before None; higher max first; ties by source name asc.
        bm.cmp(&am).then_with(|| a.source.cmp(&b.source))
    });
    local.into_iter().chain(remote).collect()
}
```

(Note: `Option<i64>` ordering — `None < Some(_)` — so `bm.cmp(&am)` with descending intent puts `Some` highs first and `None` last; verify the deadhost-last assertion passes, which confirms the direction.)

- [ ] **Step 4: Apply ordering in the switcher**

In `src/ui/switcher.rs` `visible_groups`, wrap the returned groups in `tree::order_groups`:

```rust
    fn visible_groups(&self) -> Vec<Group> {
        let groups = if self.filter.is_empty() {
            self.groups.clone()
        } else {
            let filtered = tree::filter_groups(&self.groups, &self.filter);
            if filtered.is_empty() {
                self.groups
                    .iter()
                    .map(|g| Group {
                        source: g.source.clone(),
                        err: g.err.clone(),
                        sessions: Vec::new(),
                    })
                    .collect()
            } else {
                filtered
            }
        };
        tree::order_groups(&groups)
    }
```

- [ ] **Step 5: Run to verify it passes**

Run: `cargo test -p xmux ui::tree ui::switcher 2> test.err; echo "exit=$?"; cat test.err`
Expected: PASS. (The existing `renders_four_level_tree` and preselect tests still hold — `local` already sorted first there; confirm `streaming_auto_advances_to_recent_when_untouched` still passes since preselect is by global max, independent of group order.)

- [ ] **Step 6: Verify clippy**

Run: `cargo clippy --all-targets 2> clippy.err; echo "exit=$?"; grep -c warning clippy.err`
Expected: exit=0, 0 warnings.

- [ ] **Step 7: Commit**

```bash
git add src/ui/tree.rs src/ui/switcher.rs
git commit -m "feat(ui): pin local group first, order remote hosts by recency"
```

---

### Task 11: Remove the `popup` subcommand and the popup→cockpit path

**Files:**
- Modify: `src/main.rs` — remove `Command::Popup`, `run_popup`, the `Popup` match arm, and the now-unused `control_path` helper if it has no other caller (it does: keep it — it is still used? Verify: `control_path` is only used by `run_popup`. Remove it too.).
- Modify: `src/cockpit.rs` — remove `signal_cockpit_switch`, `PopupAction::SignalCockpit`, `decide_popup_action`'s SignalCockpit branch (and `PopupAction` entirely if no remaining caller), `cockpit_pointer`/`write_cockpit_pointer`/`read_cockpit_pointer`/`remove_cockpit_pointer*` if only the popup used them. Verify each symbol's callers before removing.
- Test: `src/cockpit.rs`, `src/main.rs` — delete the corresponding tests (`popup_decision_table`, `signal_cockpit_switch_acks_and_sets_pending`, pointer round-trip tests if the pointer is removed).

**Interfaces:**
- Consumes: nothing new.
- Produces: a smaller surface. `PopupAction`, `decide_popup_action`, `signal_cockpit_switch`, and the cockpit-pointer functions are removed (their only consumer was `run_popup`). The cockpit control socket (`cockpit_accept`/`dispatch_cockpit` `switch`) stays — Task 12 repurposes it to drive the persistent app.

- [ ] **Step 1: Identify dead symbols (grounding, not a code change yet)**

Run: `cargo build 2> build.err` after the removals to let the compiler flag every orphaned reference; remove iteratively until clean. The plan removes:
- `src/main.rs`: `Command::Popup` variant + its `Some(Command::Popup) => ...` arm + `async fn run_popup` + `fn control_path` (popup-only) + the `use xmux::mux;`/`use xmux::manage;` lines IF they become unused (the compiler will say).
- `src/cockpit.rs`: `pub enum PopupAction`, `pub fn decide_popup_action`, `pub async fn signal_cockpit_switch`, `pub fn cockpit_pointer_path`, `write_cockpit_pointer`, `read_cockpit_pointer`, `remove_cockpit_pointer`, `remove_cockpit_pointer_if_ours`, and `serve_cockpit`'s `write_cockpit_pointer` call + `CockpitHandle`'s pointer cleanup.

- [ ] **Step 2: Delete the popup subcommand in `main.rs`**

Remove from the `Command` enum:

```rust
    /// In-mux switcher, bound via `display-popup -E "xmux popup"`.
    Popup,
```

Remove the match arm:

```rust
        Some(Command::Popup) => match interactive_env() {
            Ok(env) => run_popup(Arc::new(env)).await,
            Err(code) => code,
        },
```

Delete `async fn run_popup(...) { ... }` entirely and `fn control_path(...)`. Remove the imports `use xmux::mux;` and `use xmux::manage;` only if the compiler reports them unused (run_direct_attach uses neither; doctor/ls do not — verify).

- [ ] **Step 3: Delete the popup-only cockpit machinery**

In `src/cockpit.rs`, delete `PopupAction`, `decide_popup_action`, `signal_cockpit_switch`, and all four `*_cockpit_pointer*` functions + `cockpit_pointer_path`. In `serve_cockpit`, remove the `let _ = write_cockpit_pointer(...)` line and in `CockpitHandle::drop` remove `remove_cockpit_pointer_if_ours(...)`. Delete the tests `popup_decision_table`, `signal_cockpit_switch_acks_and_sets_pending`, `pointer_round_trip_and_absent`, `pointer_removed_only_if_ours`.

- [ ] **Step 4: Run build to find stragglers**

Run: `cargo build 2> build.err; echo "exit=$?"; cat build.err`
Expected: iterate until exit=0 (remove each flagged orphan; never leave dead code).

- [ ] **Step 5: Run the remaining cockpit/main tests**

Run: `cargo test -p xmux cockpit 2> test.err; echo "exit=$?"; cat test.err`
Expected: PASS (the kept `dispatch_*`, `switch_*`, `loop_*`, `cockpit_socket_switch_sets_pending` tests).

- [ ] **Step 6: Verify clippy**

Run: `cargo clippy --all-targets 2> clippy.err; echo "exit=$?"; grep -c warning clippy.err`
Expected: exit=0, 0 warnings.

- [ ] **Step 7: Commit**

```bash
git add src/main.rs src/cockpit.rs
git commit -m "refactor: remove the popup subcommand and popup->cockpit path"
```

---

### Task 12: Cockpit persistent-app integration

**Files:**
- Modify: `src/cockpit.rs` — replace `cockpit_loop`/`RealAttacher`/`RealPicker`/`run_cockpit` with the persistent app: build a `LiveOwner`, an `AttachRegistry`, an `App`, a `Switcher`, and run ONE event loop that interleaves stdin (decoded through `InputMachine` in Passthrough; raw keys to the switcher in Overlay), the control socket (`key`/`text`/`dump`), terminal resize, dwell-driven attaches, EOF reaps, and the Overlay↔Passthrough transitions.
- Modify: `src/proxy/run.rs` — `proxy_attach` is no longer the cockpit's entry; keep it compiling (its tests stay) but the cockpit no longer calls it. (Do NOT delete `proxy_attach` in this task; the decode→picker tests in `run.rs` still cover the decode path. A later cleanup task may remove it once the cockpit path is the sole entry — out of scope here.)
- Test: `src/cockpit.rs` — a headless state-machine test using the registry with fake attachments and the switcher, driven by injected keys.

**Interfaces:**
- Consumes: `AttachRegistry` (Task 5), `App`/`AppState` (Task 6), `LiveOwner`/`spawn_attachment`/`Attachment` (Task 4), `InputMachine`/`InAction` (Task 7), `Switcher`/`TerminalViewTarget`/`take_dwell_attach`/`note_attached` (Tasks 8-9), `Grid::{render_into, restore_bytes, cursor}` (Tasks 3), `source.attach_command` (existing), `tree::order_groups` (Task 10).
- Produces:
  - `pub async fn run_cockpit(env: Arc<Env>) -> i32` — unchanged signature; new body driving the persistent app.
  - A testable core: `pub async fn cockpit_app(env, ops, registry-builder, control, initial_overlay) -> i32` is NOT required by the spec; instead the headless test drives the switcher + registry transition helpers directly (see Step 1). Keep the loop body in `run_cockpit` but factor the pure transition decisions into small functions tested in Tasks 6/9.

- [ ] **Step 1: Write the failing headless test (state-machine transitions)**

This test does NOT spawn real PTYs (those are Task 14's live gate). It drives the *pure* transition logic: a `Switcher`, an `App`, and the `attach::nest_guard` + addr→argv resolution. Add to `src/cockpit.rs` `mod tests`:

```rust
    #[tokio::test]
    async fn enter_promotes_cursor_to_passthrough_foreground() {
        use crate::proxy::app::{App, AppState};
        use crate::proxy::run::LiveOwner;
        use crate::ui::switcher::{Scan, Switcher};
        use crate::ui::tree::Group;

        let live = LiveOwner::new();
        let mut app = App::new(live.clone());
        let scan = Scan {
            groups: vec![Group {
                source: "jupiter06".into(),
                err: None,
                sessions: vec![Session {
                    source: "jupiter06".into(),
                    name: "api".into(),
                    windows: 1,
                    attached: false,
                    last_attached: 100,
                }],
            }],
            panes: Default::default(),
        };
        let mut sw = Switcher::new(scan);
        // Cursor preselected on jupiter06/api; Enter chooses it.
        sw.handle_key(ratatui::crossterm::event::KeyEvent::new(
            ratatui::crossterm::event::KeyCode::Enter,
            ratatui::crossterm::event::KeyModifiers::NONE,
        ));
        let chosen = sw.result().chosen.expect("Enter chooses the cursor session");
        let addr = chosen.address();
        assert_eq!(addr, "jupiter06/api");
        // The cockpit would attach (fake id 9) and promote to foreground.
        app.enter_passthrough(addr.clone(), 9, b"", b"");
        assert_eq!(
            app.state,
            AppState::Passthrough { fg: "jupiter06/api".into(), fg_id: 9 }
        );
        assert!(live.is_owner(9), "the foreground attachment owns stdout");
    }

    #[tokio::test]
    async fn esc_returns_to_previous_foreground_no_switch() {
        use crate::proxy::app::App;
        use crate::proxy::run::LiveOwner;
        let live = LiveOwner::new();
        let mut app = App::new(live.clone());
        app.enter_passthrough("local/work".into(), 1, b"", b"");
        app.enter_overlay();
        // Esc target is the remembered previous foreground.
        let (addr, id) = app.esc_target().expect("esc returns to previous fg");
        assert_eq!(addr, "local/work");
        app.enter_passthrough(addr, id, b"", b"");
        assert!(live.is_owner(1));
    }
```

- [ ] **Step 2: Run to verify it fails (or compiles against existing types)**

Run: `cargo test -p xmux cockpit::tests::enter_promotes cockpit::tests::esc_returns 2> test.err; echo "exit=$?"; cat test.err`
Expected: FAIL until `run_cockpit` and the `Ops`/registry wiring compile (the test uses only Task 6/8 types, so it may pass early — that is acceptable; the real deliverable is the rewritten `run_cockpit` body that the build below exercises).

- [ ] **Step 3: Rewrite `run_cockpit`**

Replace the `cockpit_loop`/`RealAttacher`/`RealPicker` machinery and `run_cockpit` with a single persistent loop. The structure (grounded in `proxy_attach`'s thread setup and `event_loop`'s select):

```rust
pub async fn run_cockpit(env: Arc<Env>) -> i32 {
    use crate::proxy::app::{App, AppState};
    use crate::proxy::input::{InAction, InputMachine};
    use crate::proxy::registry::AttachRegistry;
    use crate::proxy::run::{parse_prefix, LiveOwner};
    use crate::proxy::screen::Grid;
    use crate::ui::switcher::Switcher;
    use std::io::{Read, Write};
    use std::time::Duration;

    if let Err(e) = attach::nest_guard(attach::in_mux()) {
        eprintln!("xmux: {e}");
        eprintln!("xmux: the cockpit must be your terminal entry, not run inside a mux.");
        return 2;
    }
    let _ = std::fs::create_dir_all(&env.xmux_dir);

    // Raw mode for the whole session (RAII-restored on return/panic).
    let _raw = match crate::proxy::run::RawGuard::enter() {
        Ok(g) => g,
        Err(e) => {
            eprintln!("xmux: {e}");
            return 1;
        }
    };

    let size = ratatui::crossterm::terminal::size().unwrap_or((80, 24));
    let (cols, body_rows) = (size.0, size.1.saturating_sub(1)); // status bar = last row

    let live = LiveOwner::new();
    let (eof_tx, mut eof_rx) = tokio::sync::mpsc::unbounded_channel::<u64>();
    let mut registry = AttachRegistry::new(env.cfg.keep_cap(), live.clone(), eof_tx);
    let mut app = App::new(live.clone());

    // The switcher seeded from the source skeletons; probes stream sessions in.
    let ops = env.ops();
    let mut switcher = Switcher::from_sources(ops.sources());

    // Single stdin reader thread (the proxy pattern): raw host bytes → channel.
    let (stdin_tx, mut stdin_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(256);
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut stdin = stdin.lock();
        let mut buf = [0u8; 256];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if stdin_tx.blocking_send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let mut machine = InputMachine::new(
        parse_prefix(Some(&env.ui_prefix)),
        b's',
        b'q',
        Duration::from_millis(400),
    );

    // ratatui terminal over the real stdout (Overlay draws; Passthrough is raw).
    let mut term = match ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(std::io::stdout())) {
        Ok(t) => t,
        Err(e) => { eprintln!("xmux: {e}"); return 1; }
    };

    // The control + probe channel (repurposed) for headless key/text/dump.
    let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::channel::<crate::ui::run::Cmd>(256);
    let control = pick_control_path(&env);
    let _control_handle = control.and_then(|p| crate::ui::run::serve_control(p, cmd_tx.clone()));
    crate::ui::run::spawn_probes_pub(&ops, &cmd_tx); // kick streaming probes

    let mut event_stream = ratatui::crossterm::event::EventStream::new();
    use futures::StreamExt;

    loop {
        // Draw: in Overlay, ratatui paints sidebar + terminal view (the cursor
        // session's grid). In Passthrough, the foreground pump writes raw — we
        // only paint the status bar via the transition; nothing to draw here.
        if app.is_overlay() {
            let tv_addr = {
                let t = switcher.terminal_view_target();
                (!t.target.is_empty()).then(|| format!("{}/{}", t.source, t.target))
            };
            // Borrow the cursor session's grid (clone the Arc, lock briefly).
            let grid_arc = tv_addr
                .as_deref()
                .and_then(|a| registry.get(a))
                .map(|att| att.grid.clone());
            let _ = match &grid_arc {
                Some(g) => {
                    let guard = g.lock().ok();
                    term.draw(|f| switcher.render(f, guard.as_deref()))
                }
                None => term.draw(|f| switcher.render(f, None)),
            };
        }

        // Dwell-completed attach (Overlay only): attach + keep, mark live.
        if app.is_overlay() {
            if let Some(tgt) = switcher.take_dwell_attach(std::time::Instant::now()) {
                let addr = format!("{}/{}", tgt.source, tgt.target);
                attach_into_registry(&env, &mut registry, &addr, cols, body_rows, &app);
            }
        }

        let tick = if app.is_overlay() && switcher.dwell_pending() {
            Duration::from_millis(33)
        } else {
            Duration::from_millis(250)
        };

        tokio::select! {
            biased;
            Some(id) = eof_rx.recv() => {
                // A session exited: reap it; if it was the foreground, fall to Overlay.
                let was_fg = matches!(&app.state, AppState::Passthrough { fg_id, .. } if *fg_id == id);
                registry.reap(id);
                if was_fg {
                    app.enter_overlay();
                    term.clear().ok();
                }
            }
            Some(bytes) = stdin_rx.recv() => {
                if app.is_overlay() {
                    // Drive the switcher: decode bytes → KeyEvents.
                    let mut decoder = crate::proxy::decode::KeyDecoder::new();
                    for key in decoder.feed(&bytes) {
                        switcher.handle_key(key);
                    }
                    handle_switcher_outcome(&env, &mut registry, &mut switcher, &mut app, &mut term, cols, body_rows);
                } else {
                    // Passthrough: InputMachine intercepts only the prefix.
                    let now = std::time::Instant::now();
                    let mut to_fg: Vec<u8> = Vec::new();
                    let mut open = false;
                    let mut quit = false;
                    for b in bytes {
                        for action in machine.feed(b, now) {
                            match action {
                                InAction::Forward(f) => to_fg.extend_from_slice(&f),
                                InAction::OpenOverlay => open = true,
                                InAction::Quit => quit = true,
                            }
                        }
                    }
                    if let AppState::Passthrough { fg, .. } = &app.state {
                        if !to_fg.is_empty() {
                            if let Some(att) = registry.get(fg) {
                                att.input(to_fg);
                            }
                        }
                    }
                    if quit { break; }
                    if open {
                        app.enter_overlay();
                        term.clear().ok();
                        // Cursor starts on the current foreground session.
                        // (switcher preselect already targets recency; the spec
                        // allows starting on the fg — best-effort: leave preselect.)
                    }
                }
            }
            Some(cmd) = cmd_rx.recv() => {
                use crate::ui::run::Cmd;
                match cmd {
                    Cmd::Key(k) => { switcher.handle_key(k); handle_switcher_outcome(&env, &mut registry, &mut switcher, &mut app, &mut term, cols, body_rows); }
                    Cmd::SourceResult { source, sessions, err } => {
                        let reachable = err.is_none();
                        switcher.apply_source_result(source, sessions.clone(), err);
                        if reachable { crate::ui::run::spawn_panes_pub(&ops, &cmd_tx, sessions); }
                    }
                    Cmd::Panes { address, panes } => switcher.apply_panes(address, panes),
                    Cmd::Dump(reply) => {
                        let sz = term.size().unwrap_or(ratatui::layout::Size { width: 80, height: 24 });
                        let _ = reply.send(crate::ui::run::dump_switcher(&mut switcher, sz.width, sz.height));
                    }
                    Cmd::OpDone(result) => switcher.apply_op_result(result),
                    Cmd::Mouse(_) | Cmd::Resize(_, _) => {}
                }
            }
            Some(Ok(ev)) = event_stream.next() => {
                if let ratatui::crossterm::event::Event::Resize(c, r) = ev {
                    let body = r.saturating_sub(1);
                    registry.resize_all(c, body);
                    let _ = term.resize(ratatui::layout::Rect::new(0, 0, c, r));
                }
            }
            _ = tokio::time::sleep(tick) => { /* wake to repaint dwell progress */ }
        }
    }

    registry.teardown_all();
    0
}
```

Add the two helper fns in `src/cockpit.rs`:

```rust
/// Resolves an `addr` to its attach argv and ensures it is in the registry,
/// protecting the foreground + the cursor session from eviction.
fn attach_into_registry(
    env: &Arc<Env>,
    registry: &mut crate::proxy::registry::AttachRegistry,
    addr: &str,
    cols: u16,
    rows: u16,
    app: &crate::proxy::app::App,
) -> Option<u64> {
    let sess = crate::session::parse_target(addr).ok()?;
    let src = env.by_alias.get(&sess.source)?.clone();
    if crate::attach::nest_guard(crate::attach::in_mux()).is_err() {
        return None;
    }
    let argv = src.attach_command(&sess.name, None);
    let mut protect: Vec<&str> = vec![addr];
    if let crate::proxy::app::AppState::Passthrough { fg, .. } = &app.state {
        protect.push(fg.as_str());
    }
    registry.ensure(addr, &argv, cols, rows, &protect).ok()
}

/// After the switcher processed a key: Enter → promote to Passthrough; quit →
/// (handled by the caller via should_exit); Esc with a previous fg → return.
fn handle_switcher_outcome(
    env: &Arc<Env>,
    registry: &mut crate::proxy::registry::AttachRegistry,
    switcher: &mut crate::ui::switcher::Switcher,
    app: &mut crate::proxy::app::App,
    term: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
    cols: u16,
    rows: u16,
) {
    use std::io::Write;
    // Enter chose a session: attach immediately + promote to foreground.
    let result = switcher.result();
    if let Some(chosen) = result.chosen {
        let addr = chosen.address();
        if let Some(id) = attach_into_registry(env, registry, &addr, cols, rows, app) {
            switcher.note_attached(&addr);
            // Restore the foreground grid + paint a status bar, then go Passthrough.
            let restore = registry
                .get(&addr)
                .and_then(|att| att.grid.lock().ok().map(|g| g.restore_bytes()))
                .unwrap_or_default();
            let status = status_bar_bytes(&addr, registry_kept(registry), env.cfg.keep_cap(), cols, rows);
            app.enter_passthrough(addr, id, &restore, &status);
        }
        switcher.clear_result();
        return;
    }
    // No chosen session but the switcher asked to exit (q): handled in the loop's
    // quit path by checking should_exit — caller breaks. For Esc-return: the
    // switcher's quit() sets exit; distinguish by esc_target presence.
    if switcher.should_exit() {
        if let Some((addr, id)) = app.esc_target() {
            let restore = registry
                .get(&addr)
                .and_then(|att| att.grid.lock().ok().map(|g| g.restore_bytes()))
                .unwrap_or_default();
            let status = status_bar_bytes(&addr, registry_kept(registry), env.cfg.keep_cap(), cols, rows);
            app.enter_passthrough(addr, id, &restore, &status);
            switcher.clear_exit();
            let _ = term; // term is owned by ratatui; Passthrough writes raw next
        }
    }
}

fn registry_kept(registry: &crate::proxy::registry::AttachRegistry) -> usize {
    registry.kept()
}

/// The Passthrough status bar bytes: `host/session · kept N/cap`, painted on the
/// last physical row, wrapped in cursor save/restore. Info only — no shortcuts.
fn status_bar_bytes(addr: &str, kept: usize, cap: usize, cols: u16, rows: u16) -> Vec<u8> {
    let text = format!("{addr} · kept {kept}/{cap}");
    let clipped: String = text.chars().take(cols as usize).collect();
    let mut out = Vec::new();
    out.extend_from_slice(b"\x1b7"); // save cursor
    out.extend_from_slice(format!("\x1b[{};1H", rows + 1).as_bytes()); // last physical row
    out.extend_from_slice(b"\x1b[7m"); // reverse video
    out.extend_from_slice(b"\x1b[K"); // clear line
    out.extend_from_slice(clipped.as_bytes());
    out.extend_from_slice(b"\x1b[0m");
    out.extend_from_slice(b"\x1b8"); // restore cursor
    out
}
```

Add the small public accessors needed:
- In `src/proxy/registry.rs`: `pub fn kept(&self) -> usize { self.map.len() }`.
- In `src/ui/switcher.rs`: `pub fn clear_result(&mut self) { self.result = SwitchResult::default(); self.exit = false; }` and `pub fn clear_exit(&mut self) { self.exit = false; self.result = SwitchResult::default(); }` (distinguish Esc-return vs Enter via `result.chosen` in the caller).
- In `src/ui/run.rs`: `pub fn spawn_probes_pub`/`pub fn spawn_panes_pub` thin wrappers around the existing private `spawn_probes`/`spawn_panes` (or simply make those `pub(crate)`), and ensure `serve_control`, `dump_switcher`, `Cmd` are `pub` (they already are).
- In `src/proxy/run.rs`: make `RawGuard` and `RawGuard::enter` `pub` (currently private) so the cockpit reuses the one RAII guard.

(Note for the implementer: the Esc-vs-q distinction. The switcher's `quit()` is bound to both `q` and `Esc` today. For the cockpit, `q` must quit the whole app and `Esc` must return to the previous foreground. Add a `Switcher` flag: split `handle_key`'s `Esc`/`'q'` so `Esc` sets a new `self.esc_requested = true` (consumed by `pub fn take_esc(&mut self) -> bool`) instead of `quit()`, and `'q'`/quit keeps setting `exit`. Then in the loop: after a key in Overlay, `if switcher.take_esc() { /* esc-return via app.esc_target */ }`, and `if switcher.should_exit() && switcher.result().chosen.is_none() { break }` quits. Implement this split in switcher.rs as part of this task; add a unit test `esc_sets_esc_requested_not_exit` and `q_sets_exit`.)

- [ ] **Step 4: Add the Esc/q split unit tests in switcher.rs**

```rust
    #[tokio::test]
    async fn esc_requests_return_not_quit() {
        let mut h = Harness::new(sample());
        h.key(KeyCode::Esc).await;
        assert!(h.sw.take_esc(), "Esc requests an overlay return");
        assert!(!h.sw.should_exit(), "Esc must not quit the app");
    }

    #[tokio::test]
    async fn q_quits_the_app() {
        let mut h = Harness::new(sample());
        h.ch('q').await;
        assert!(h.sw.should_exit(), "q quits");
        assert!(!h.sw.take_esc(), "q is not an esc-return");
    }
```

- [ ] **Step 5: Run to verify it passes**

Run: `cargo test -p xmux cockpit ui::switcher 2> test.err; echo "exit=$?"; cat test.err`
Expected: PASS.

- [ ] **Step 6: Verify build + clippy (whole crate)**

Run: `cargo build 2> build.err; echo "exit=$?"; cargo clippy --all-targets 2> clippy.err; echo "clippy=$?"; grep -c warning clippy.err`
Expected: build=0, clippy=0, 0 warnings.

- [ ] **Step 7: Commit**

```bash
git add src/cockpit.rs src/proxy/registry.rs src/proxy/run.rs src/ui/switcher.rs src/ui/run.rs
git commit -m "feat(cockpit): persistent app integrating registry, AppState, state machine"
```

---

### Task 12.5: Status-bar clear-leak repaint (spec §4)

The foreground child is sized `cols×(rows-1)` so it never addresses the last
physical row — EXCEPT a full-screen erase-display (`ESC[2J`, `ESC[J`, …) which
the real terminal applies to the whole screen, wiping the xmux status bar. The
foreground (owner) pump must detect a clear sequence in its outgoing chunk and
re-emit the status bar after it. The status-bar bytes already wrap themselves in
cursor save/restore (`ESC7`/`ESC8`), so a re-emit is transparent to the child.

**Files:**
- Modify: `src/proxy/run.rs` — add `scan_clear` (rolling-tail ED detector); add a
  `status_bar: Arc<Mutex<Vec<u8>>>` field to `Attachment` + `set_status_bar`;
  create the shared cell in `spawn_attachment` and re-emit it from the owner pump
  on a detected clear; add the field to the test-only `fake_attachment`.
- Modify: `src/proxy/registry.rs` — `set_status_bar(&self, addr, bytes)` passthrough.
- Modify: `src/cockpit.rs` — push the status bytes into the attachment on each
  Passthrough entry (Enter + Esc-return), before `app.enter_passthrough`.
- Test: `src/proxy/run.rs` (`mod tests`).

**Interfaces:**
- Consumes: `Attachment`, `spawn_attachment`, `pump_write`, `LiveOwner` (Task 4);
  `AttachRegistry` (Task 5); `status_bar_bytes`/`handle_switcher_outcome` (Task 12).
- Produces:
  - free fn `fn scan_clear(tail: &mut Vec<u8>, chunk: &[u8]) -> bool` — true when a
    `ESC [ <digits>? J` completed in `chunk`; keeps a bounded trailing partial-CSI
    in `tail` across calls (mirrors `InputMachine`'s `paste_scan`).
  - `Attachment.status_bar: Arc<Mutex<Vec<u8>>>` (empty until set) +
    `pub fn set_status_bar(&self, bytes: Vec<u8>)`.
  - `AttachRegistry::set_status_bar(&self, addr: &str, bytes: Vec<u8>)`.

- [ ] **Step 1: Write the failing test (the scanner, chunk-split safe)**

Add to `src/proxy/run.rs` `mod tests`:

```rust
    #[test]
    fn scan_clear_detects_ed_in_one_and_across_chunks() {
        // Whole sequence in one chunk.
        let mut tail = Vec::new();
        assert!(scan_clear(&mut tail, b"text\x1b[2Jmore"));

        // Split across two chunks: `ESC [ 2` then `J`.
        let mut tail = Vec::new();
        assert!(!scan_clear(&mut tail, b"abc\x1b[2"), "params unfinished: not yet");
        assert!(scan_clear(&mut tail, b"J xyz"), "completes on the next chunk");

        // `ESC [ J` (no param) is also an erase-display.
        let mut tail = Vec::new();
        assert!(scan_clear(&mut tail, b"\x1b[J"));

        // A non-ED CSI (colour) must NOT trigger a repaint.
        let mut tail = Vec::new();
        assert!(!scan_clear(&mut tail, b"\x1b[31m colored \x1b[0m"));
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p xmux proxy::run::tests::scan_clear 2> test.err; echo "exit=$?"; cat test.err`
Expected: FAIL — `scan_clear` not found.

- [ ] **Step 3: Implement `scan_clear`**

In `src/proxy/run.rs`:

```rust
/// Detects an erase-display sequence `ESC [ <digits>? J` in `chunk`, carrying any
/// trailing unfinished CSI in `tail` so a sequence split across read chunks is
/// still caught. The owner pump re-emits the status bar whenever this returns
/// true (a child sized cols×(rows-1) only reaches the status row via a full
/// clear). The tail is bounded so a stream of bare ESCs cannot grow it.
fn scan_clear(tail: &mut Vec<u8>, chunk: &[u8]) -> bool {
    let mut buf = std::mem::take(tail);
    buf.extend_from_slice(chunk);
    let mut found = false;
    let mut i = 0;
    let mut keep_from = buf.len();
    while i < buf.len() {
        if buf[i] != 0x1b {
            i += 1;
            continue;
        }
        if i + 1 >= buf.len() {
            keep_from = i; // lone trailing ESC: could begin a CSI next chunk
            break;
        }
        if buf[i + 1] != b'[' {
            i += 1;
            continue;
        }
        let mut j = i + 2;
        while j < buf.len() && buf[j].is_ascii_digit() {
            j += 1;
        }
        if j >= buf.len() {
            keep_from = i; // unfinished params: resume from this ESC next chunk
            break;
        }
        if buf[j] == b'J' {
            found = true;
        }
        i = j + 1;
    }
    let mut rest = buf.split_off(keep_from);
    if rest.len() > 16 {
        let n = rest.len() - 16;
        rest.drain(0..n);
    }
    *tail = rest;
    found
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p xmux proxy::run::tests::scan_clear 2> test.err; echo "exit=$?"; cat test.err`
Expected: PASS.

- [ ] **Step 5: Wire the status bar into the Attachment + owner pump**

In `src/proxy/run.rs`, add the field to `Attachment`:

```rust
    pub status_bar: Arc<Mutex<Vec<u8>>>,
```

Add the method to `impl Attachment`:

```rust
    /// Set the status-bar bytes the owner pump re-emits after a clear sequence.
    pub fn set_status_bar(&self, bytes: Vec<u8>) {
        if let Ok(mut g) = self.status_bar.lock() {
            *g = bytes;
        }
    }
```

In `spawn_attachment`, create the shared cell before the pump and clone it in;
add it to the returned struct literal:

```rust
    let status_bar = Arc::new(Mutex::new(Vec::<u8>::new()));
    let pump_status = status_bar.clone();
```

Update the pump's owner-write branch to scan + re-emit (keep a per-pump
`clear_tail` before the loop: `let mut clear_tail: Vec<u8> = Vec::new();`):

```rust
                    if live.is_owner(id) {
                        pump_write(&stdout, chunk);
                        if scan_clear(&mut clear_tail, chunk) {
                            let sb = pump_status.lock().map(|g| g.clone()).unwrap_or_default();
                            if !sb.is_empty() {
                                pump_write(&stdout, &sb);
                            }
                        }
                    }
```

Add `status_bar,` to the `Attachment { .. }` literal. Add `status_bar: Arc::new(Mutex::new(Vec::new())),` to the test-only `fake_attachment`.

In `src/proxy/registry.rs`, add:

```rust
    /// Push the Passthrough status-bar bytes into `addr`'s attachment so its owner
    /// pump can re-emit them after a full-screen clear.
    pub fn set_status_bar(&self, addr: &str, bytes: Vec<u8>) {
        if let Some(att) = self.map.get(addr) {
            att.set_status_bar(bytes);
        }
    }
```

In `src/cockpit.rs` `handle_switcher_outcome`, push the bytes on BOTH Passthrough
entries (Enter promote + Esc-return), immediately before `app.enter_passthrough`:

```rust
            registry.set_status_bar(&addr, status.clone());
```

- [ ] **Step 6: Verify build + clippy**

Run: `cargo build 2> build.err; echo "exit=$?"; cargo clippy --all-targets 2> clippy.err; echo "clippy=$?"; grep -c warning clippy.err`
Expected: build=0, clippy=0, 0 warnings.

- [ ] **Step 7: Commit**

```bash
git add src/proxy/run.rs src/proxy/registry.rs src/cockpit.rs
git commit -m "feat(proxy): repaint status bar after a child full-screen clear"
```

---

### Task 13: Reconcile the scan cache to the in-memory live scan

**Files:**
- Modify: `src/cockpit.rs` — remove the `ScanCache` plumbing (the cross-open cache exists to repaint fast across picker re-opens; the persistent app holds one continuous scan, so a cross-open cache is dead). Remove `scan_cache`, the `cache:` fields on the (now-removed) `RealAttacher`/`RealPicker`, and the `Some(self.cache.clone())` args.
- Modify: `src/ui/run.rs` — remove `ScanCache`, `seed_switcher`'s cache branch, `merge_scan`, `store_snapshot`, `from_cached_scan` usage; keep `Switcher::from_sources`. `run_picker_fed`/`run_switcher` drop the `cache` parameter (verify no remaining caller after Task 12 — the cockpit no longer calls them; `run_switcher` may now be dead — remove it if so).
- Modify: `src/ui/switcher.rs` — remove `from_cached_scan` and its test, `snapshot` if only the cache used it (verify), `from_cached` harness helper.
- Test: delete `scan_cache_round_trips_through_seed`, `scan_cache_survives_a_transient_unreachable`, `merge_scan_takes_fresh_when_reprobe_succeeds`, `from_cached_scan_shows_last_known_and_revalidates`.

**Interfaces:**
- Consumes: nothing new.
- Produces: a simpler surface — `ScanCache`, `merge_scan`, `store_snapshot`, `seed_switcher`'s cache path, `Switcher::from_cached_scan`/`snapshot` (if unused) removed. `run_switcher`/`run_picker_fed` lose their `cache: Option<ScanCache>` parameter (or are removed entirely if Task 12 left them callerless).

- [ ] **Step 1: Find the now-dead cache symbols**

Run: `cargo build 2> build.err` after Task 12 — anything cache-related the cockpit no longer references is a candidate. Confirm callers with Grep before removing.

Run: `cargo run --quiet 2>/dev/null; rg -n "ScanCache|from_cached_scan|merge_scan|store_snapshot|run_switcher|run_picker_fed" src` (use the Grep tool, not shell rg).

- [ ] **Step 2: Remove the cache plumbing**

In `src/ui/run.rs`: delete `pub type ScanCache`, `merge_scan`, `store_snapshot`, the cache branch of `seed_switcher` (reduce it to `Switcher::from_sources(ops.sources())`), and the `cache` parameter from `run_picker_fed`/`run_switcher`. If `run_switcher`/`run_picker_fed`/`proxy_attach` now have no caller, remove them and their tests (verify each with Grep first). Keep `event_loop`, `serve_control`, `dump_switcher`, `spawn_probes`/`spawn_panes`, `Cmd`.

In `src/ui/switcher.rs`: delete `from_cached_scan` + the `from_cached` harness helper + the four cache tests listed above. Keep `snapshot` only if a live caller remains (the cockpit does not snapshot — remove `snapshot` and `snapshot_carries_sessions_and_panes` if dead).

In `src/cockpit.rs`: remove any residual `ScanCache`/`scan_cache` references left after Task 12.

- [ ] **Step 3: Run build to clean up stragglers**

Run: `cargo build 2> build.err; echo "exit=$?"; cat build.err`
Expected: iterate to exit=0.

- [ ] **Step 4: Run the suite**

Run: `cargo test -p xmux 2> test.err; echo "exit=$?"; tail -40 test.err`
Expected: PASS (the removed cache tests are gone; everything else green).

- [ ] **Step 5: Verify clippy**

Run: `cargo clippy --all-targets 2> clippy.err; echo "exit=$?"; grep -c warning clippy.err`
Expected: exit=0, 0 warnings.

- [ ] **Step 6: Commit**

```bash
git add src/ui/run.rs src/ui/switcher.rs src/cockpit.rs
git commit -m "refactor: drop cross-open scan cache, live scan held continuously"
```

---

### Task 14: Integration + headless verification (control-socket driven)

**Files:**
- Create: `tests/cockpit_headless.rs` (integration test) OR add `#[ignore]`-gated tests to `src/cockpit.rs` — prefer an integration test driven over the control socket so it exercises the real `run_cockpit` wiring.
- Modify: none (verification only; fixes loop back into the relevant task's file).
- Test: `tests/cockpit_headless.rs`.

**Interfaces:**
- Consumes: the full binary (`xmux`) + `xmux ctl` control-socket client; the throwaway remote `jupiter06`.
- Produces: a documented headless verification flow + the human visual gate note.

- [ ] **Step 1: Write the control-socket dump assertions (headless, no live psmux disruption)**

Because the persistent cockpit owns the real terminal, drive it inside a psmux pane on a throwaway socket and inject over `xmux ctl`. The automatable assertions use `dump` (the rendered Overlay tree), NOT live attach (which is the human gate). Add to `src/cockpit.rs` `mod tests` an `#[ignore]` test that builds an `Env` with only the throwaway `jupiter06` source and drives the switcher headlessly via `event_loop`-style injection (reuse the existing `control_end_to_end` pattern from `ui::run` tests, but assert the Overlay tree + recency order + filter + a simulated dwell):

```rust
    // Headless cockpit smoke: drives the SWITCHER half of the cockpit (Overlay
    // tree paint, recency order, nav, filter, dwell completion) without a real
    // PTY. Live attach + the raw passthrough screen handover are the human gate
    // (Step 3). Marked #[ignore] — run on demand.
    #[ignore]
    #[tokio::test]
    async fn cockpit_overlay_headless_smoke() {
        use crate::ui::switcher::{Scan, Switcher};
        use crate::ui::tree::Group;
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        // local pinned first, jupiter06 as a remote (the only throwaway target).
        let scan = Scan {
            groups: vec![
                Group { source: "jupiter06".into(), err: None, sessions: vec![
                    Session { source: "jupiter06".into(), name: "probe".into(), windows: 1, attached: false, last_attached: 300 },
                ]},
                Group { source: "local".into(), err: None, sessions: vec![
                    Session { source: "local".into(), name: "work".into(), windows: 1, attached: false, last_attached: 50 },
                ]},
            ],
            panes: Default::default(),
        };
        let mut sw = Switcher::new(scan);
        // Ordering: local pinned first even though jupiter06 is more recent.
        let dump = crate::ui::run::dump_switcher(&mut sw, 100, 30);
        let local_at = dump.find("local").unwrap();
        let jup_at = dump.find("jupiter06").unwrap();
        assert!(local_at < jup_at, "local group pinned above remote:\n{dump}");
        // Filter to probe, Enter applies, the throwaway is visible.
        for c in ['/', 'p', 'r', 'o', 'b', 'e'] {
            sw.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        sw.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let dump = crate::ui::run::dump_switcher(&mut sw, 100, 30);
        assert!(dump.contains("probe"), "filter keeps the throwaway:\n{dump}");
        assert!(!dump.contains("work"), "filter drops local/work:\n{dump}");
        // A completed dwell on the filtered, unattached session yields an attach.
        let now = std::time::Instant::now();
        let got = sw.take_dwell_attach(now + super::super::ui::switcher::DWELL);
        assert_eq!(got.map(|t| t.target), Some("probe".to_string()));
    }
```

(Adjust the `DWELL` path to the correct module reference — `crate::ui::switcher::DWELL`.)

- [ ] **Step 2: Run the headless smoke (on demand)**

Run: `cargo test -p xmux cockpit::tests::cockpit_overlay_headless_smoke -- --ignored --nocapture 2> test.err; echo "exit=$?"; cat test.err`
Expected: PASS.

- [ ] **Step 3: Document the human visual gate (no code)**

Add a comment block at the top of `tests/cockpit_headless.rs` (or the `#[ignore]` test) recording the manual checklist the human runs in a REAL terminal (never headless):
1. Launch `xmux` (the cockpit) in a real terminal. Confirm it starts in Overlay: sidebar tree + terminal view.
2. Cursor to `jupiter06/probe`, hold 500ms — confirm the left→right progress bar fills, then the terminal view goes live.
3. Press Enter — confirm the session promotes to full-screen Passthrough; the status bar shows `jupiter06/probe · kept N/cap` on the last row.
4. Press the prefix (`C-g`) then `s` — confirm it returns to Overlay with the previous foreground remembered; press Esc — confirm it returns to `jupiter06/probe` Passthrough with no switch.
5. Press the prefix then `q` (or `q` in Overlay) — confirm clean quit, terminal restored.
6. NEVER select/attach `local/xmux` (the live session) during this — only the throwaway `jupiter06`.

- [ ] **Step 4: Full suite + clippy gate**

Run: `cargo build 2> build.err; echo "build=$?"; cargo test -p xmux 2> test.err; echo "test=$?"; tail -30 test.err; cargo clippy --all-targets 2> clippy.err; echo "clippy=$?"; grep -c warning clippy.err`
Expected: build=0, test=0 (all non-ignored green), clippy=0, 0 warnings.

- [ ] **Step 5: Commit**

```bash
git add tests/cockpit_headless.rs src/cockpit.rs
git commit -m "test(cockpit): headless overlay smoke + human visual-gate checklist"
```

---

## Notes for the implementer

- **Stale-binary trap:** `cargo test` does not rebuild the `xmux` binary. Before any test that shells out to `xmux`/`xmux ctl`, run `cargo build`.
- **One writer invariant:** the single hardest property is "exactly one writer touches real stdout." It is enforced by `LiveOwner` (Task 4) + the `App::enter_passthrough` handoff (Task 6). Any new stdout write outside a pump or that handoff is a bug — route it through the handoff.
- **Status-bar clear-leak repaint (spec §4 — Task 12.5):** the foreground child is sized `cols×(rows-1)` so it never addresses the last physical row, EXCEPT a full-screen clear (`ESC[2J` / `ESC[…J`) which hits the whole physical screen. Task 12.5 adds the `scan_clear` rolling-tail detector (like `InputMachine`'s `paste_scan`), a `status_bar: Arc<Mutex<Vec<u8>>>` cell on `Attachment` the cockpit updates on each Passthrough entry, and the owner-pump re-emit after a detected clear. Sequence: Task 12 paints the status bar once on entry; Task 12.5 keeps it alive across the child's clears.
- **`portable_pty::Child` trait methods** (Task 5 `DummyChild`): do NOT guess the method set — let `cargo build` report the required methods for 0.9 and implement exactly those.
- **Adversarial review:** the N-attachment concurrency, the live-owner gate, the transition handoff, and teardown warrant an adversarial concurrency review after Task 12 (per the spec's testing strategy) — dispatch `ce-adversarial-reviewer` on the cockpit loop + registry + app diff before the human gate.
