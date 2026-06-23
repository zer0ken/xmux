# hide-tree-on-focus Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `config.toml` `[ui]` setting `hide-tree-on-focus` that, when true, hides the sidebar tree and gives the mux the full terminal width while the mux is focused; default false keeps the tree shown (current behavior).

**Architecture:** Keep the cockpit's existing `tree_width` variable as the EFFECTIVE width that every sizing/render site already consumes. Add a separate `tree_width_natural` holding the user's natural/`prefix h·l` width. One loop-top reconcile sets `tree_width` to `0` (hidden) while the mux is focused + the setting is on, else to `tree_width_natural`, and resizes the PTYs on change. `terminal_view_size` and the switcher's `render` treat `tree_width == 0` as "tree hidden": full-width terminal, no divider. No new threading through the event arms.

**Tech Stack:** Rust, ratatui, crossterm, tokio, serde/toml.

## Global Constraints

- Backward compatible: `hide-tree-on-focus` is optional; default `false` = current behavior (tree always shown). A missing `[ui]` or missing key → false.
- AS-IS code: comments state current behavior, never deltas/history.
- `tree_width == 0` is the sole "tree hidden" sentinel; the runtime natural width is clamped 20..100 (`adjust_tree_width`) so it never reaches 0 on its own.
- Ponytail: shortest correct diff; reuse existing patterns (the existing `tree_width` plumbing, the existing h/l resize block); no new abstractions.
- Build/test on this box use the REAL toolchain (the rustup shim is blocked). Run cargo as:
  `CARGO="$HOME/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin/cargo.exe"; RUSTC="$HOME/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin/rustc.exe" RUSTDOC="$HOME/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin/rustdoc.exe" "$CARGO" test ...`
  Do NOT pipe cargo through `tail` (it masks the exit code).
- Scope: ONLY `hide-tree-on-focus`. The other four settings in the spec (tree-width, style, keys, focus display) are out of scope this round.

---

### Task 1: config — `hide-tree-on-focus` field + accessor

**Files:**
- Modify: `src/config.rs` (`UiConfig` struct, its `Default`, a new accessor)
- Test: `src/config.rs` `#[cfg(test)] mod tests`

**Interfaces:**
- Produces: `Config::ui_hide_tree_on_focus(&self) -> bool` (consumed by Task 3); `UiConfig.hide_tree_on_focus: bool`.

- [ ] **Step 1: Write the failing test**

Add to `src/config.rs` `mod tests`:

```rust
    #[test]
    fn ui_hide_tree_on_focus_round_trip() {
        // Missing file → false.
        let missing = std::env::temp_dir().join("xmux-hide-absent-xyz.toml");
        assert!(!load(&missing).unwrap().ui_hide_tree_on_focus());

        // [ui] present but key missing → false; prefix still loads.
        let path = write_temp("[ui]\nprefix = \"C-g\"\n", "hide-missing.toml");
        let cfg = load(&path).unwrap();
        assert!(!cfg.ui_hide_tree_on_focus());
        assert_eq!(cfg.ui_prefix(), "C-g");

        // Explicit true.
        let path = write_temp("[ui]\nhide-tree-on-focus = true\n", "hide-true.toml");
        let cfg = load(&path).unwrap();
        assert!(cfg.ui_hide_tree_on_focus());
        assert_eq!(cfg.ui_prefix(), "C-g"); // prefix unaffected, still defaults

        // Explicit false.
        let path = write_temp("[ui]\nhide-tree-on-focus = false\n", "hide-false.toml");
        assert!(!load(&path).unwrap().ui_hide_tree_on_focus());
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `"$CARGO" test --quiet ui_hide_tree_on_focus_round_trip`
Expected: FAIL — `no method named ui_hide_tree_on_focus` (won't compile).

- [ ] **Step 3: Write minimal implementation**

In `src/config.rs`, change `UiConfig` to add the field:

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct UiConfig {
    /// xmux's prefix spec (e.g. `C-g`, `C-Space`), config-only like tmux's
    /// `set -g prefix`. Parsed by `proxy::term::parse_prefix`.
    #[serde(default = "default_prefix")]
    pub prefix: String,
    /// When true, focusing the mux hides the tree and gives the mux the full
    /// terminal width; the tree returns when focus returns to it. Default false
    /// keeps the tree shown in both focus states.
    #[serde(rename = "hide-tree-on-focus", default)]
    pub hide_tree_on_focus: bool,
}
```

Update its `Default`:

```rust
impl Default for UiConfig {
    fn default() -> Self {
        UiConfig {
            prefix: default_prefix(),
            hide_tree_on_focus: false,
        }
    }
}
```

Add the accessor in `impl Config` (next to `ui_prefix`):

```rust
    /// Whether focusing the mux hides the tree (full-width mux). Default false.
    pub fn ui_hide_tree_on_focus(&self) -> bool {
        self.ui.hide_tree_on_focus
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `"$CARGO" test --quiet ui_hide_tree_on_focus_round_trip ui_table_defaults_and_overrides ui_unknown_key_still_warns`
Expected: PASS (3 tests; the existing `[ui]` tests must still pass — the new field is additive and `#[serde(default)]`).

- [ ] **Step 5: Commit**

```bash
git add src/config.rs
git commit -m "feat: add ui.hide-tree-on-focus config field"
```

---

### Task 2: switcher render — full-width terminal when `tree_width == 0`

**Files:**
- Modify: `src/ui/switcher.rs` (`Switcher::render`)
- Test: `src/ui/switcher.rs` `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: nothing new. `render(&mut self, frame, grid, terminal_focused, tree_width)` keeps its signature; only its behavior at `tree_width == 0` changes.
- Produces: when `tree_width == 0`, the terminal view occupies the full `frame.area()` (x=0, full width) with no tree column, no input/footer column, and no divider rule.

- [ ] **Step 1: Write the failing test**

Add to `src/ui/switcher.rs` `mod tests`. (The existing test module already builds `Switcher` instances and renders to a `TestBackend`; mirror an existing render test for setup — e.g. how `from_sources`/`new` is constructed and how a `Terminal::new(TestBackend::new(w, h))` frame is drawn. Use a grid of `None`.)

```rust
    #[test]
    fn render_tree_width_zero_gives_terminal_full_width() {
        // A two-source skeleton is enough; grid None renders the "(attaching…)"
        // placeholder across the whole width when the tree is hidden.
        let mut sw = Switcher::from_sources(vec!["local".into(), "jupiter06".into()]);
        let mut term = Terminal::new(TestBackend::new(40, 10)).unwrap();

        // tree_width == 0 → no tree column, no divider: the terminal view starts at x=0.
        term.draw(|f| sw.render(f, None, true, 0)).unwrap();
        let buf = term.backend().buffer().clone();
        // Column 0 row 0 must NOT be the divider rule '│' (the divider is gone).
        assert_ne!(buf[(0, 0)].symbol(), "│", "divider must be absent when tree hidden");
        // The attaching placeholder text "(attaching…)" begins near x=0 (after its
        // two leading spaces), proving the terminal view owns the left edge.
        let row0: String = (0..40).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        assert!(row0.contains("(attaching…)"), "terminal view fills row 0: {row0:?}");

        // Sanity: with a normal width the divider rule IS present at the tree edge.
        term.draw(|f| sw.render(f, None, true, 20)).unwrap();
        let buf = term.backend().buffer().clone();
        assert_eq!(buf[(20, 0)].symbol(), "│", "divider present at x=tree_width when shown");
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `"$CARGO" test --quiet render_tree_width_zero_gives_terminal_full_width`
Expected: FAIL — with the current 3-column layout, `tree_width == 0` still renders a divider at x=0 (Length(0) tree, Length(1) divider at x=0), so `buf[(0,0)] == "│"`.

- [ ] **Step 3: Write minimal implementation**

In `src/ui/switcher.rs`, at the top of `render`, special-case the hidden tree before the existing layout:

```rust
    pub fn render(
        &mut self,
        frame: &mut Frame,
        grid: Option<&crate::proxy::screen::Grid>,
        terminal_focused: bool,
        tree_width: u16,
    ) {
        let area = frame.area();
        // tree_width == 0 is the "tree hidden" sentinel (mux focused + hide-tree-on-focus):
        // the terminal view owns the whole area — no tree, no input/footer, no divider.
        if tree_width == 0 {
            self.tree_inner = Rect::default();
            self.render_terminal_view(frame, area, grid);
            if let Some(g) = grid {
                if !g.hide_cursor() {
                    frame.set_cursor_position(terminal_cursor_pos(area, g.cursor()));
                }
            }
            if self.show_help {
                self.render_help(frame, area);
            }
            return;
        }
        let cols = Layout::horizontal([
```

(Leave the rest of `render` unchanged.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `"$CARGO" test --quiet render_tree_width_zero_gives_terminal_full_width`
Expected: PASS.

Run the whole switcher module to confirm no regression:
Run: `"$CARGO" test --quiet --lib switcher`
Expected: PASS (all existing switcher tests still green).

- [ ] **Step 5: Commit**

```bash
git add src/ui/switcher.rs
git commit -m "feat: render full-width terminal when tree hidden (width 0)"
```

---

### Task 3: cockpit — sentinel sizing, natural width, focus reconcile, mouse origin

**Files:**
- Modify: `src/cockpit.rs` (`terminal_view_size`, new `reconciled_tree_width`, `run_cockpit` loop, the two h/l resize blocks, the mouse `term_area`)
- Test: `src/cockpit.rs` `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: `Config::ui_hide_tree_on_focus()` (Task 1); the switcher `render` full-width behavior (Task 2).
- Produces: behavior only (no new public API). `terminal_view_size(cols, rows, 0) == (cols.max(1), (rows+1).max(1))`. `reconciled_tree_width(terminal_focused, hide_tree_on_focus, natural) -> u16`.

- [ ] **Step 1: Write the failing tests**

Add to `src/cockpit.rs` `mod tests`:

```rust
    #[test]
    fn terminal_view_size_zero_tree_is_full_width() {
        // Hidden tree (sentinel 0): full cols, no divider subtracted.
        assert_eq!(terminal_view_size(80, 23, 0), (80, 24));
        // Shown tree: cols - tree_width - 1 (divider), height = body_rows + 1.
        assert_eq!(terminal_view_size(80, 23, 48), (31, 24));
        // Degenerate widths clamp to at least 1.
        assert_eq!(terminal_view_size(0, 0, 0), (1, 1));
    }

    #[test]
    fn reconciled_tree_width_hides_only_when_focused_and_enabled() {
        // Tree focused (terminal_focused = false): always the natural width.
        assert_eq!(reconciled_tree_width(false, true, 48), 48);
        assert_eq!(reconciled_tree_width(false, false, 48), 48);
        // Mux focused + setting on: hidden (0).
        assert_eq!(reconciled_tree_width(true, true, 48), 0);
        // Mux focused + setting off: stays shown at natural width.
        assert_eq!(reconciled_tree_width(true, false, 48), 48);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `"$CARGO" test --quiet terminal_view_size_zero_tree_is_full_width reconciled_tree_width_hides_only_when_focused_and_enabled`
Expected: FAIL — `terminal_view_size(80,23,0)` currently returns `(79, 24)` (subtracts the divider); `reconciled_tree_width` does not exist (won't compile).

- [ ] **Step 3a: Implement `terminal_view_size` sentinel**

In `src/cockpit.rs`, replace the body of `terminal_view_size`:

```rust
fn terminal_view_size(cols: u16, body_rows: u16, tree_width: u16) -> (u16, u16) {
    // tree_width == 0 is the "tree hidden" sentinel: the mux takes the full width
    // with no divider column. Otherwise subtract the tree column + the 1-col divider.
    let view_cols = if tree_width == 0 {
        cols.max(1)
    } else {
        cols.saturating_sub(tree_width + 1).max(1)
    };
    (view_cols, (body_rows + 1).max(1))
}
```

- [ ] **Step 3b: Add the pure reconcile helper**

Add near `adjust_tree_width` in `src/cockpit.rs`:

```rust
/// The EFFECTIVE tree width to render and size the mux against. Hidden (0, mux
/// full width) only while the mux is focused AND `hide_tree_on_focus` is set;
/// otherwise the tree's natural width. Pure so the focus/setting interaction is
/// unit-testable; the loop owns the natural width and the PTY resize on change.
fn reconciled_tree_width(terminal_focused: bool, hide_tree_on_focus: bool, natural: u16) -> u16 {
    if terminal_focused && hide_tree_on_focus {
        0
    } else {
        natural
    }
}
```

- [ ] **Step 3c: Read the setting + add the natural width**

In `run_cockpit`, where `tree_width` is initialized (`let mut tree_width = crate::ui::switcher::TREE_WIDTH;`), add below it:

```rust
    // `tree_width` is the EFFECTIVE width (0 = tree hidden, mux full width); every
    // sizing/render site reads it. `tree_width_natural` holds the tree's natural
    // width (what prefix h/l adjusts, restored when the tree is shown again).
    let mut tree_width_natural = tree_width;
    let hide_tree_on_focus = env.cfg.ui_hide_tree_on_focus();
```

- [ ] **Step 3d: Loop-top reconcile + resize on change**

In the `loop {`, immediately AFTER the `switcher.set_spinner_frame(...)` line near the top (and before the `if !app.is_overlay() { switcher.select_active_window(); }` block) add:

```rust
        // Reconcile the effective tree width to the current focus + the hide setting.
        // On a change (focus toggled, or hide flips the width), resize the PTYs to the
        // new mux view size so the mux reflows to/from full width, and mark dirty.
        let want_tree_width = reconciled_tree_width(!app.is_overlay(), hide_tree_on_focus, tree_width_natural);
        if want_tree_width != tree_width {
            tree_width = want_tree_width;
            let (vc, vr) = terminal_view_size(cols, body_rows, tree_width);
            registry.resize_all(vc, vr);
            mgr.resize_all(vc, vr);
            dirty = true;
        }
```

- [ ] **Step 3e: h/l adjusts the NATURAL width**

There are two identical h/l resize blocks in the stdin arm (the main `if app.is_overlay()` branch and the `focus_tree` replay branch). In BOTH, the line is currently:

```rust
                        tree_width = adjust_tree_width(tree_width, wd);
```

Replace it (in both places) with:

```rust
                        tree_width_natural = adjust_tree_width(tree_width_natural, wd);
                        tree_width = tree_width_natural; // h/l only fires in tree focus (tree shown)
```

(The following `terminal_view_size(cols, body_rows, tree_width)` + `resize_all` lines in each block stay unchanged — they now resize to the freshly-adjusted shown width.)

- [ ] **Step 3f: Mouse term_area origin honors the hidden tree**

In the stdin arm's mouse scan, the term area is currently:

```rust
                let (vw, vh) = terminal_view_size(cols, body_rows, tree_width);
                let term_area = ratatui::layout::Rect::new(tree_width + 1, 0, vw, vh);
```

Replace the second line so a hidden tree puts the mux at x=0 (matching `render`):

```rust
                let (vw, vh) = terminal_view_size(cols, body_rows, tree_width);
                let term_x = if tree_width == 0 { 0 } else { tree_width + 1 };
                let term_area = ratatui::layout::Rect::new(term_x, 0, vw, vh);
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `"$CARGO" test --quiet terminal_view_size_zero_tree_is_full_width reconciled_tree_width_hides_only_when_focused_and_enabled`
Expected: PASS.

Run the cockpit module to confirm no regression:
Run: `"$CARGO" test --quiet --lib cockpit`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/cockpit.rs
git commit -m "feat: hide the tree and grow the mux full-width on focus"
```

---

### Task 4: full verification

**Files:** none (verification only)

- [ ] **Step 1: Build**

Run: `RUSTC="$HOME/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin/rustc.exe" RUSTDOC="$HOME/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin/rustdoc.exe" "$CARGO" build`
Expected: builds, 0 errors.

- [ ] **Step 2: Clippy**

Run: `RUSTC=... RUSTDOC=... "$CARGO" clippy --all-targets`
Expected: 0 warnings.

- [ ] **Step 3: Full test suite**

Run: `RUSTC=... RUSTDOC=... "$CARGO" test`
Expected: all tests PASS (prior count + 3 new).

- [ ] **Step 4: Done** — report results; no commit (no code change).

## Self-Review

- **Spec coverage:** Spec §5 (hide-tree-on-focus) + the "이번 회차 구현 범위" slice — config field/accessor (Task 1), switcher render width-0 (Task 2), terminal_view_size sentinel + cockpit reconcile + h/l natural + mouse origin (Task 3), build/clippy/test (Task 4). The other four spec features are explicitly out of scope. Covered.
- **Placeholder scan:** none — every code step shows full code.
- **Type consistency:** `ui_hide_tree_on_focus() -> bool` (Task 1) consumed in Task 3 §3c. `reconciled_tree_width(bool, bool, u16) -> u16` defined and called consistently. `terminal_view_size` signature unchanged. `tree_width` / `tree_width_natural` both `u16`.
- **Note (not unit-testable headlessly):** the live focus-toggle → PTY reflow and the on-screen full-width handover need a real terminal; the headless suite covers the pure helpers + the render layout only. Flagged as a human visual gate.
