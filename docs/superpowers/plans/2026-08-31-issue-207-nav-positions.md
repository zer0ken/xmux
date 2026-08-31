# Issue 207: nav placement on all four sides and a cycling shortcut

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend the nav placement to the four directions left/right/top/bottom, and land in one pass the four [ui] position settings, the per-frame resolution rule, a `prefix p` shortcut that cycles the position at runtime, and persistence of the position value.

**Architecture:** All rendering draws only inside the rect that `compute_regions` hands out, so changing only the split to four variants keeps the in-region rules (card flow, hint bar, status line) independent of the position. The position rides in `NavSize` as a fourth component. Resolution rule: pinned > auto(wide/narrow) > force > wide.

**Tech Stack:** Rust, cargo test. Test locations: the Harness in `src/ui/switcher/tests.rs` and `tests_support.rs`, `src/app/runtime/tests.rs`, and the config tests in `src/provision/config.rs`.

---

## Design Decisions (Four Design Items)

**1. Shortcut shape and the value it changes: a single `prefix p`, a cycling key, changing auto and force together as one value.**

- The key is a single `prefix p`. It avoids the taken keys (h, l, t, q, ?, n, r, Tab, Esc, Ctrl-arrows, 0-9), it is the initial of "position", and it parallels `prefix t` (the auto-hide toggle).
- The cycle order is one step clockwise: left → top → right → bottom → (unpin) → automatic. In the auto state the first step goes to the clockwise neighbor of the **current effective position**, so no press is ever an invisible no-op (in a narrow window whose effective position is top, the first press moves to right).
- The value changed is **both** of "auto toggle, force value"; in implementation terms it is a single pinned override value (`Option<NavPosition>`). Rationale: under the resolution rule force is ignored while auto is on, so a key that changes only force has no effect, and a key that toggles only auto cannot pick the position. `Some(dir)` means auto off plus force dir; `None` means follow the [ui] settings. This is exactly the shape of the `auto_hide_nav` precedent (a runtime toggle value is saved to the file and beats the config default). The fifth step unpins, leaving a keyboard path back to auto.
- The focus rows of the hint bar cheatsheet and the help modal are written to follow the arrow pair that the current position points to (so that a right attachment shows "→/↓ focus nav"). The chrome receives the position and computes the wording.

**2. Mirror symmetry: only the split (what goes on which side of the view border) flips in the mirror; the layout inside the nav region is identical at all four positions.**

- Rationale: all rendering draws only inside the rect that `compute_regions` hands out (render_nav_list, render_nav_columns, nav_row_lines, hit testing, and the scrollbar all use only in-region coordinates). Changing only the split order in `compute_regions` to four variants therefore keeps the **in-region rules for card flow, hint bar, offscreen card counts, and the status line entirely independent of the position**. The right column is the same vertical list as the left column, and the bottom band is the same down-then-right flow as the top band (making cards read upward through a vertical mirror is a layout no terminal UI uses).
- **Status line decision: in all four positions, the bottom row of the nav region.** That is, with a bottom attachment the status line is the bottom row of the screen, not the row adjacent to the view border. Rationale: (a) `split_nav` keeps today's rule exactly, so the code change is zero. (b) Today's three placements (left column, top band) already all use "the lowest on-screen row among the rows the nav owns" as the status line, and the rules in keybind.md and FR-B9 are worded "The nav's bottom row is its status line". (c) Making the row adjacent to the border the status line would squeeze it between the terminal and the cards, where it reads like a header, departing from the terminal convention that the status line sits on the bottom edge of the screen. (d) On auto-hide there is no awkwardness in the "window's bottom rows" that the hint bar borrows (`hint_bar_rect`) and the bottom attachment's status line being the same row.
- View border dragging mirrors its math according to the position: left `width = col-1`, right `width = window_cols - col`, top `height = row-1`, bottom `height = window_rows - row` (col/row are 1-based). The resize **keys** keep the nav's own size semantics regardless of position (h/Ctrl-← narrower, l/Ctrl-→ wider, Ctrl-↑ shorter, Ctrl-↓ taller). Axis selection is already shape based (`resize_axis` gates on the cached layout), so it stands as is.
- The view border line is drawn only by rect shape (vertical rule/horizontal rule), so it is unchanged. The active half cue (top half = nav focus, left half = nav focus) is a focus signal, not a directional arrow, so it stays independent of the position.

**3. Focus arrows: keep the pair-level rule "the arrow pair facing the terminal's side names the terminal".**

- Terminal on the nav's right/below (Left, Top): {→, ↓} = terminal, {←, ↑} = nav. Today's behavior as is (no change).
- Terminal on the nav's left/above (Right, Bottom): {←, ↑} = terminal, {→, ↓} = nav. The whole pair flips in the mirror. With the nav attached on the right, `prefix →` under nav focus is a swallow-only no-op (the nav already has focus), and under terminal focus focus moves to the nav. The example the issue asks for holds exactly.
- Why a pair and not a single arrow: today's code uses the fixed pair `Right|Down → FocusTerminal` regardless of layout, and the documentation words it "→ and ↓ both name it". Under a pure geometric rule that allows only the single arrow crossing the border, ↓ would lose terminal focus in the Side layout, breaking the default placement's behavior. The pair rule preserves the two default positions byte for byte and mirror-symmetrizes the other two wholesale.
- The two focus paths (`resolve_nav_key` and `TermInput`) receive the same position value and change together. The invariant "a change on one path is a change on both" is kept.

**4. Persistence: one `~/.xmux/nav_position` file, the best-effort pattern of nav_width/auto_hide_nav.**

- File value: `left|top|right|bottom` (pin) or `auto` (no pin). An absent file means follow the [ui] settings (the same as `auto`). `Runtime::new` reads it, and `prefix p` saves immediately (the same moment as `toggle_auto_hide`).
- The four [ui] settings (auto-nav-position=true, wide-nav-position=left, narrow-nav-position=top, force-nav-position=omitted) provide the defaults, and only a pinned file beats them. The default combination reproduces today's behavior exactly (left column when wide, top band otherwise).
- The wide/narrow judgment uses today's turnover test (`view_layout`: assuming the nav keeps a side column, is the terminal that the column leaves behind the wider horizontally) unchanged as the **fixed criterion**. The resolved position never re-enters the judgment's input, so nothing oscillates at the boundary. This is how the issue's requirement ("the judgment result must not become an input to the judgment again, so that nothing oscillates at the boundary") is satisfied.
- The position rides in `NavSize` as a fourth component. Rationale: the nav size concept in CONTEXT.md has bundled natural/width/height into one value on the grounds that "every consumer reads the value whole", and the position is live geometry of exactly the same kind. This keeps the signatures of `compute_regions`, `terminal_view_size`, `DriverCtx`, and `render` all intact, and the 16 tests on the mux driver side compile unchanged thanks to the `visible()` default (Left).
- The per-frame resolution is folded into the existing nav width/height reconcile block at the loop top (`prepare_and_draw`). A position change resizes the PTY and repaints the whole screen with `clear_screen` (the border line jumps to the opposite side of the screen, so it gets the same treatment as `crossed_hidden`).
- Out-of-scope checks: no control-socket position verb (explicit in the issue), no mouse gesture that moves the position (explicit in the issue), no `model::Action`/`Command` additions. `prefix p` follows the precedent of `display::Action::Height` ("key-driven only, no domain action").

## Plan

Each task ends with the tree compiling and the tests green. Common verification (end of every task): `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`.

### Task 1: Rename the ViewLayout variants to shape names

- [ ] Files: `src/ui/switcher/mod.rs`, `src/ui/switcher/render.rs`, `src/ui/switcher/tests.rs`, `src/app/runtime/input.rs`, `src/app/runtime/tests.rs`
- [ ] Mechanical rename `ViewLayout::Side` → `ViewLayout::Column`, `ViewLayout::Top` → `ViewLayout::Band` (about 35 sites by grep). After the four-position extension `Top` would also name the bottom band, so this is advance cleanup to keep the names honest.
- [ ] Doc comment updates: `ViewLayout` ("Column: the nav is a left or right column of the terminal view. Band: a top or bottom band"), `view_layout` (role unchanged, the as-if fixed judgment), `NavSize.height` ("the band's height"), the Side/Top wording in `compute_regions` and `render`, the `layout()` getter comment, and the `render_view_border` comment in `chrome.rs` (worded as the nav-side/terminal-side signal instead of "top = tree (left)").
- [ ] TDD: no new tests (behavior-preserving rename). Verification: the full suite.
- [ ] Commit: `rename ViewLayout variants to shape names (Column, Band)`

### Task 2: The NavPosition type and the per-frame resolution rule (TDD)

- [ ] New file: `src/ui/switcher/position.rs`. In `src/ui/switcher/mod.rs`, `mod position; pub use position::{NavPosition, NavPositionSetting, resolve_nav_position};`
- [ ] Tests first (inline `#[cfg(test)]` in position.rs, following the side.rs/columns.rs pattern):
  - `layout()`: Left/Right → `ViewLayout::Column`, Top/Bottom → `ViewLayout::Band`
  - `clockwise()`: Left→Top→Right→Bottom→Left
  - `forward_arrows_face_terminal()`: Left/Top true, Right/Bottom false
  - `parse()`: `"left"|"top"|"right"|"bottom"` (trim, lowercase) → Some, otherwise None. Follow the `#[allow(clippy::should_implement_trait)]` pattern of `FocusTarget::from_str`
  - `resolve_nav_position`: pinned Some → that value (unconditionally). auto → wide if `view_layout(area, natural) == Column`, else narrow. auto off → force unwrap_or(wide). Default settings and no pin: 140x30/natural 48 → Left, 40x100 → Top
  - `NavPositionSetting::default()`: auto=true, wide=Left, narrow=Top, force=None
- [ ] Implementation:
  ```rust
  pub fn resolve_nav_position(
      setting: &NavPositionSetting,
      pinned: Option<NavPosition>,
      area: Rect,
      nav_natural: u16,
  ) -> NavPosition {
      if let Some(p) = pinned { return p; }
      if setting.auto {
          return if view_layout(area, nav_natural) == ViewLayout::Column { setting.wide } else { setting.narrow };
      }
      setting.force.unwrap_or(setting.wide)
  }
  ```
  This transcribes the issue's resolution order (pinned > auto > force > wide) as is.
- [ ] Add `pub position: NavPosition` to `NavSize`. Fill `visible`/`hidden` with `position: NavPosition::Left` and add a `with_position(self, position)` builder next to `with_height`. State in the `NavSize` docs that the attachment position is the fourth component.
- [ ] Keep compiling: add `position: NavPosition::Left` to the `NavSize { ... }` literals at `src/app/runtime/handlers.rs:56` and `src/app/runtime/input.rs:165` (Task 6 replaces them with the runtime's actual value).
- [ ] Verification: `cargo test nav_position` (all of the new unit tests), the full suite.
- [ ] Commit: `add NavPosition and the per-frame nav position resolution`

### Task 3: Split compute_regions four ways (TDD)

- [ ] Files: `src/ui/switcher/mod.rs`, `src/ui/switcher/tests.rs`, `src/ui/switcher/tests_support.rs`
- [ ] Tests first (next to `compute_regions_side_top_and_hidden` in tests.rs, same style):
  - `compute_regions_right_column`: `Rect::new(0,0,140,30)` + `NavSize::visible(48).with_position(NavPosition::Right)` → `terminal == Rect::new(0,0,91,30)`, `view_border == Rect::new(91,0,1,30)`, `tree == Rect::new(92,0,48,29)`, `hint_bar == Rect::new(92,29,48,1)`
  - `compute_regions_bottom_band`: `Rect::new(0,0,40,100)` + position Bottom → `terminal == Rect::new(0,0,40,59)`, `view_border == Rect::new(0,59,40,1)`, `hint_bar == Rect::new(0,99,40,1)` (this assertion pins the decision that the status line is the bottom row of the screen)
  - The hidden sentinel respects the position: Right + hidden → the full terminal, `layout == Column`, tree/border/hint_bar default
  - Add a Right variant to `hiding_the_nav_leaves_the_layout_where_it_was` (Column both shown and hidden)
- [ ] Implementation (`compute_regions`):
  ```rust
  let layout = nav.position.layout();
  if nav_width == 0 { /* existing sentinel, layout as is */ }
  match nav.position {
      NavPosition::Left  => /* the existing Side branch as is */,
      NavPosition::Right => /* [Min(0), Length(1), Length(nav_width)] horizontal split, tree=c[2] */,
      NavPosition::Top   => /* the existing Top branch as is */,
      NavPosition::Bottom=> /* [Min(0), Length(1), Length(th)] vertical split, tree=r[2] */,
  }
  ```
  `split_nav` is unchanged (the status line decision is honored here). `Regions.layout` is `nav.position.layout()`.
- [ ] Update the existing direct-call tests: `.with_position(NavPosition::Top)` for the portrait cases, `.with_position(NavPosition::Left)` for landscape (or the Left default without stating it). The meaning is unchanged.
- [ ] Harness update: add `pub(crate) fn auto_nav(width: u16, area: Rect) -> NavSize` to `tests_support.rs` (resolves with the default settings and no pin). Making `Harness::draw` and the `portrait()` helper use it lets all of the existing portrait/landscape tests pass unchanged.
- [ ] In `render.rs`, `render` switches to `self.layout = nav.position.layout()` (removing the direct `view_layout` call), with comment updates.
- [ ] Verification: `cargo test compute_regions` and `cargo test hiding_the_nav`, the full suite.
- [ ] Commit: `split compute_regions across the four nav placements`

### Task 4: The four [ui] position settings (TDD)

- [ ] Files: `src/provision/config.rs`
- [ ] Tests first (config.rs test module, next to `ui_auto_hide_nav_round_trip`):
  - no file → `auto_nav_position == true`, `nav_position_setting()` equal to Default
  - `[ui]\nauto-nav-position = false\nwide-nav-position = "right"\nnarrow-nav-position = "bottom"\nforce-nav-position = "top"` → all parsed
  - unknown values (`wide-nav-position = "diagonal"`) → fall back to the defaults, `force-nav-position = ""` → None
- [ ] Implementation: add to `UiConfig` `#[serde(rename = "auto-nav-position", default = "default_true")] pub auto_nav_position: bool`, `#[serde(rename = "wide-nav-position", default = "default_wide_nav_position")] pub wide_nav_position: String`, and likewise `narrow_nav_position`, `force_nav_position` (default empty string). Update `Default for UiConfig` (auto=true, "left", "top", "").
- [ ] `impl UiConfig { pub fn nav_position_setting(&self) -> NavPositionSetting }`: parse each word with `NavPosition::parse`, fall back to each one's default on failure, empty force → None. (Precedent for config.rs interpreting `crate::ui` types: `ui_prefix()`, theme)
- [ ] Verification: `cargo test nav_position_setting` and `cargo test ui_`, the full suite.
- [ ] Commit: `add [ui] nav position settings`

### Task 5: Persist the pinned position (TDD)

- [ ] Files: `src/ui/prefs.rs`
- [ ] Tests first (duplicating the round-trip/garbage pattern of the prefs.rs test module):
  - after `save_nav_position(&dir, Some(NavPosition::Right))`, `load_nav_position` → `Some(Right)`
  - `save_nav_position(&dir, None)` → `"auto"` in the file, load → None
  - absent file/garbage value ("diagonal") → None
- [ ] Implementation: `NAV_POSITION_FILE: &str = "nav_position"`, `load_nav_position(xmux_dir) -> Option<NavPosition>` (absent/"auto"/unparsable → None), `save_nav_position(xmux_dir, Option<NavPosition>)` (Some → the word, None → "auto"). `ui/prefs.rs` references `crate::ui::switcher::NavPosition`.
- [ ] Verification: `cargo test nav_position_save` and `cargo test nav_position_load`, the full suite.
- [ ] Commit: `persist the nav position override`

### Task 6: Runtime wiring: the loop-top resolution and the PTY resize (TDD)

- [ ] Files: `src/app/runtime/mod.rs`, `src/app/runtime/handlers.rs`, `src/app/runtime/input.rs`, `src/app/runtime/tests.rs`
- [ ] Tests first (next to `resize_keys_adjust_height_in_top_layout` in runtime/tests.rs):
  - `loop_top_resolves_the_pinned_nav_position`: `rt.nav_position_pinned = Some(Right)` → after `prepare_and_draw`, `rt.nav_position == Right`; then feed `rt.nav_size()` to `term.draw` and assert `rt.switcher.layout() == Column`
  - `loop_top_resolves_auto_for_a_portrait_backend`: cols=40/body_rows=59, no pin → `rt.nav_position == Top`, layout Band
- [ ] Implementation:
  - three `Runtime` fields: `nav_position: NavPosition`, `nav_position_pinned: Option<NavPosition>`, `nav_pos_setting: NavPositionSetting`
  - `Runtime::new`: `nav_pos_setting = roster.cfg.ui.nav_position_setting()`, `nav_position_pinned = prefs::load_nav_position(...)`, `nav_position = resolve_nav_position(&setting, pinned, Rect::new(0,0,cols, body_rows+1), nav_width_natural)`
  - `nav_size()` includes `position: self.nav_position`
  - the literals at handlers.rs:56 and input.rs:165 → `position: *nav_position` (add `nav_position` to the destructure)
  - the three `DriverCtx` sites (handlers.rs 626, 999, 1066): `.with_position(self.nav_position)` after `.with_height(self.nav_height)`
  - the reconcile block in `prepare_and_draw`:
    ```rust
    let want_position = resolve_nav_position(
        &self.nav_pos_setting, self.nav_position_pinned,
        Rect::new(0, 0, self.cols, self.body_rows.saturating_add(1)),
        self.nav_width_natural,
    );
    if want_nav_width != self.nav_width
        || self.nav_height != self.applied_nav_height
        || want_position != self.nav_position
    {
        let crossed_hidden = ...;
        let crossed_position = want_position != self.nav_position;
        self.nav_position = want_position;   // update the field before the terminal_view_size computation
        ... // the existing nav_width/applied_nav_height updates, resize_all, and clear_screen when crossed_hidden || crossed_position
    }
    ```
    right after it, `self.state.chrome.set_nav_position(self.nav_position)` (the Task 10 chrome field; putting this in here first means Task 10 only changes the drawing).
  - `on_config_check`: add `self.nav_pos_setting = ui.nav_position_setting();` (the reconcile at the next loop top applies it). Keep returning true.
  - `init_size` in `run_app`: `Runtime::new` has already resolved the initial position, so unify on `rt.nav_size()`.
- [ ] Verification: `cargo test loop_top_resolves`, the full suite.
- [ ] Commit: `resolve the nav position at the loop top and resize on change`

### Task 7: Mirror the view border drag math (TDD)

- [ ] Files: `src/app/input.rs`, `src/app/runtime/input.rs`, `src/app/runtime/tests.rs`
- [ ] Tests first (next to `view_border_drag_width_clamps_to_range` in app/input.rs):
  - `view_border_drag_width(91, "C-g", 140, true) == 49`, `(100, "C-g", 140, true) == 40`, `(135, "C-g", 140, true)` → clamped to `nav_width_min("C-g")`
  - `view_border_drag_height(30, 60, true) == 30`, `(58, 60, true)` → clamped to `NAV_HEIGHT_MIN`
  - update the existing left/top-signature call sites (the `window` argument, false)
- [ ] Implementation:
  ```rust
  pub(crate) fn view_border_drag_width(col: u16, ui_prefix: &str, window_cols: u16, nav_on_right: bool) -> u16 {
      let w = if nav_on_right { window_cols.saturating_sub(col) } else { col.saturating_sub(1) };
      w.clamp(nav_width_min(ui_prefix), NAV_WIDTH_MAX)
  }
  pub(crate) fn view_border_drag_height(row: u16, window_rows: u16, nav_on_bottom: bool) -> u16 {
      let h = if nav_on_bottom { window_rows.saturating_sub(row) } else { row.saturating_sub(1) };
      h.clamp(NAV_HEIGHT_MIN, NAV_HEIGHT_MAX)
  }
  ```
- [ ] `handle_mouse_event`: add `nav_position` to the destructure and reflect it in the literals' `position`; in the drag branch, when `regions.layout == Band` call `view_border_drag_height(ev.row, full.height, *nav_position == NavPosition::Bottom)`, otherwise `view_border_drag_width(ev.col, &env.ui_prefix, full.width, *nav_position == NavPosition::Right)`. grab/hover use `regions.view_border.contains`, so they are unchanged.
- [ ] Two runtime tests (shaped as duplicates of the existing `handle_mouse_event_top_layout_border_drag_resizes_height`):
  - bottom: 40x60, pin Bottom, the border is 0-based row 35 (auto height 24) → grab with a press at SGR row 36, drag to SGR row 40 → `nav_height == 20`
  - right: 140x30, pin Right, the border is 0-based col 91 → grab with a press at SGR col 92, drag to SGR col 100 → `nav_width_natural == 40`
- [ ] Verification: `cargo test view_border_drag` and `cargo test border_drag`, the full suite.
- [ ] Commit: `mirror the view-border drag math for right and bottom placements`

### Task 8: The pair rule for the focus arrows (TDD)

- [ ] Files: `src/app/input.rs`, `src/display/input.rs`, `src/display/dispatch.rs` (comments only), `src/app/runtime/input.rs`, `src/app/runtime/tests.rs`
- [ ] Tests first (next to `resolve_nav_prefix_commands` in app/input.rs, after adding a `NavPosition` argument to the `rt` helper, default Left):
  - position Left: `prefix →`/`prefix ↓` → FocusTerminal, `prefix ←`/`prefix ↑` → no action (same as today)
  - position Right: `prefix →`/`prefix ↓` → no action, `prefix ←`/`prefix ↑` → FocusTerminal
  - position Top: `prefix ↓` → FocusTerminal, `prefix ↑` → no action
  - position Bottom: `prefix ↑` → FocusTerminal, `prefix ↓` → no action
  - TermInput: at position Left, `C-g ←` → FocusNav (today), `C-g →` → swallow (today); at position Right, `C-g →` → FocusNav, `C-g ←` → swallow
- [ ] Implementation:
  - add a `resolve_nav_key(..., nav_position: NavPosition)` parameter. The arrow arms:
    ```rust
    let forward = nav_position.forward_arrows_face_terminal();
    ...
    KeyCode::Tab | KeyCode::Char('\t') => Some(Action::FocusTerminal),
    KeyCode::Right | KeyCode::Down if forward => Some(Action::FocusTerminal),
    KeyCode::Left | KeyCode::Up if !forward => Some(Action::FocusTerminal),
    KeyCode::Right | KeyCode::Down | KeyCode::Left | KeyCode::Up => None,
    ```
    keep the existing order, with the Ctrl-arrow arms first.
  - `TermInput::feed(&mut self, bytes, nav_position: NavPosition)`: flip the arrow branch's `matches!(bytes[i + 2], b'C' | b'B')` according to `forward` (`stay` = the terminal-side pair, `leave` = the nav-side pair). Update the module doc and comments.
  - Call sites: pass `self.nav_position` to the `resolve_nav_key(...)` in `handle_nav_bytes`, and in `handle_stdin_bytes` call `self.term_input.feed(&non_mouse, self.nav_position)`.
- [ ] Verification: `cargo test resolve_nav` and `cargo test prefix_then_arrow`, the full suite.
- [ ] Commit: `follow the nav placement in the focus arrow pairs`

### Task 9: The `prefix p` position cycling key (TDD)

- [ ] Files: `src/ui/switcher/position.rs`, `src/display/dispatch.rs`, `src/display/input.rs`, `src/app/input.rs`, `src/app/runtime/mod.rs`, `src/app/runtime/input.rs`, `src/app/runtime/tests.rs`
- [ ] Tests first:
  - position.rs: `step_nav_position`: `(None, Left) → Some(Top)`, `(None, Top) → Some(Right)`, `(Some(Top), _) → Some(Right)`, `(Some(Bottom), _) → None`, `(Some(Left), _) → Some(Top)`
  - app/input.rs: `prefix p` → `Action::CycleNavPosition` (nav focus, any position)
  - display/input.rs: `C-g p` → `CycleNavPosition`, following bytes still forwarded (terminal focus kept, the same shape as `prefix t`)
  - dispatch.rs: `Action::CycleNavPosition.as_action() == None`
  - runtime/tests.rs: after `handle_stdin_bytes(b"\x07p")`, `rt.nav_position_pinned == Some(clockwise(previous effective position))`, and check that the `nav_position` file was written into the fake env's xmux_dir
- [ ] Implementation:
  - in position.rs, `pub fn step_nav_position(pinned: Option<NavPosition>, effective: NavPosition) -> Option<NavPosition>` (the cycle rule of design decision 1)
  - the `display::dispatch::Action::CycleNavPosition` variant, the `as_action` None arm, and the variant doc ("key-driven only, no ctl verb: applied on the input path, like Height")
  - `resolve_nav_key`: `KeyCode::Char('p') => Some(Action::CycleNavPosition)` (placed under `t`)
  - `TermInput::feed`: a `b0 == b'p'` arm (a duplicate of the `t` arm)
  - `handle_nav_bytes`: `Some(Action::CycleNavPosition) => cycle_position = true`, adding a sixth element to the returned tuple. At the call sites (the `handle_stdin_bytes` body and the nav_replay path):
    ```rust
    fn cycle_nav_position(pinned: &mut Option<NavPosition>, effective: NavPosition, xmux_dir: &std::path::Path) {
        let next = step_nav_position(*pinned, effective);
        *pinned = next;
        crate::ui::prefs::save_nav_position(xmux_dir, next);
    }
    ```
    place it next to `toggle_auto_hide`, and use `if cp { cycle_nav_position(&mut self.nav_position_pinned, self.nav_position, &self.env.xmux_dir); *dirty = true; }`. The terminal-focus arm's `Action::CycleNavPosition` gets the same treatment. The reconcile at the next loop top reflects the pin and actually moves `self.nav_position`.
- [ ] Verification: `cargo test step_nav_position` and `cargo test cycle`, the full suite.
- [ ] Commit: `add prefix p to cycle the nav position`

### Task 10: Make the cheatsheet and help modal follow the active arrow pair (TDD)

- [ ] Files: `src/ui/chrome.rs`, `src/ui/modal.rs`, `src/ui/switcher/render.rs`
- [ ] Tests first:
  - chrome.rs: add `"p nav position"` to the order array of `hint_bar_shows_the_prefix_at_rest_and_its_keys_when_armed`, plus a new assertion: `c.set_nav_position(NavPosition::Right); c.set_armed(true);` → the armed bar contains `"→/↓ focus nav"` and `"←/↑ focus terminal"`
  - modal.rs: at position Left the help focus rows are `Enter · C-g →/↓` (terminal) and `C-g ←/↑ · C-g Esc` (nav); at position Right they flip. A small test that concatenates the `help_lines` Lines and inspects them
- [ ] Implementation:
  - a `pub(crate) nav_position: NavPosition` field on `Chrome` (default Left) plus `set_nav_position`. In the armed branch of `hint_bar_text`, compute the focus segment from the position:
    ```rust
    let focus = if self.nav_position.forward_arrows_face_terminal() {
        "←/↑ focus nav · →/↓ focus terminal"
    } else {
        "→/↓ focus nav · ←/↑ focus terminal"
    };
    ```
    put this segment into each rung of the fit ladder, and after `t hide nav` add `p nav position` (long line), `p position` (middle), `p` (short line)
  - `modal::help_lines(prefix, nav_position)`: write the arrows of the two focus rows as the pair computed above, and below the auto-hide row add `HelpRow::Key(format!("{p} p"), "cycle the nav position (left · top · right · bottom · auto)".into())`
  - `render_modal_popup` calls `modal::help_lines(&state.chrome.ui_prefix, state.chrome.nav_position)`
- [ ] Verification: `cargo test hint_bar_shows` and `cargo test help`, the full suite.
- [ ] Commit: `state the active arrow mapping in the cheatsheet and help`

### Task 11: Documentation updates

- [ ] `docs/keybind.md`:
  - Nav navigation section: describe the four placements and the four [ui] settings, the resolution order (pinned > auto > force > wide), and that the wide/narrow judgment is a fixed criterion (the as-if side column). Update the two-placement "left column/top band" wording to the column/band vocabulary
  - Prefix commands table: add a `prefix p` row: "move the nav one side clockwise (left → top → right → bottom → automatic)"
  - Focus section: update to the pair rule ("the arrow pair facing the terminal's side names the terminal")
  - The status line section: state that "the nav's bottom row" is the status line in all four placements, and that with a bottom attachment it is the bottom row of the screen
  - Mouse section: add a sentence that the drag sets the nav size at all four borders
- [ ] `README.md`: add the four keys to the config example plus the persistence sentence ("prefix p moves the nav one side clockwise and remembers the choice in ~/.xmux/nav_position, which wins over these settings until the key cycles back to automatic")
- [ ] `docs/requirements.md`:
  - FR-B14: update to the pair rule (the arrow pair per placement)
  - FR-B15: realign the wording to state that the turnover test is the fixed wide/narrow criterion of the position resolution (the test itself is unchanged)
  - FR-B16: add that the position is the fourth component of the one geometry value and the persistence of the pinned value
  - new FR-B24: the four placements, the [ui] settings and defaults, the resolution order, the `prefix p` cycle and persistence, and the mirror symmetry rules (the in-region layout is invariant across positions, the status line is the bottom row of the nav region, the drag math mirrors, and a position change leaves the terminal view the remainder whole with selection and focus kept)
- [ ] `CONTEXT.md` glossary: "nav view" (the four placements), "terminal view" ("the other region"), "nav size" (the fourth component), "layout turnover" (the fixed criterion of the resolution), "column flow" (generalized to band placements), "view border" (vertical between columns/horizontal between bands), and minimal edits replacing the side/portrait vocabulary with column/band in "scrollbar strip"/"offscreen counts"/"status row fill"/"nav bands"
- [ ] `src/ui/AGENTS.md`: update the arrow invariant to the pair rule and the side/portrait wording to column/band
- [ ] `src/app/AGENTS.md`: add the nav position to the loop-top reconcile invariant ("The effective nav width and the nav attachment are reconciled at the loop top...")
- [ ] Verification: rerun the full gate.
- [ ] Commit: `document the four nav placements`

## Files to Modify

- `src/ui/switcher/mod.rs` - ViewLayout rename, NavSize.position, the four-way compute_regions split, comments
- `src/ui/switcher/render.rs` - derive the layout cache from the position, comments
- `src/ui/switcher/tests.rs` - position tests, explicit positions in the existing direct-call tests
- `src/ui/switcher/tests_support.rs` - the auto_nav helper
- `src/ui/chrome.rs` - the nav_position field/setter, the cheatsheet focus segment, border comments
- `src/ui/modal.rs` - the help_lines arrow pairs plus the `prefix p` row
- `src/ui/prefs.rs` - nav_position load/save and tests
- `src/ui/run.rs` - pass the position to dump_screen/dump_switcher
- `src/provision/config.rs` - the four [ui] keys plus nav_position_setting()
- `src/app/input.rs` - the resolve_nav_key position parameter and the arrow/p arms, the drag mirror functions and tests
- `src/app/runtime/mod.rs` - the Runtime fields, cycle_nav_position, the reconcile
- `src/app/runtime/handlers.rs` - the Runtime::new load/resolution, nav_size, the DriverCtx position, on_config_check
- `src/app/runtime/input.rs` - the NavSize literals, the drag call sites, the tuple flag, the term_input call site
- `src/app/runtime/tests.rs` - the position/drag/cycle tests, updates to the existing tests
- `src/display/input.rs` - the TermInput position parameter, the arrow flip, the `p` arm, the doc
- `src/display/dispatch.rs` - Action::CycleNavPosition plus the as_action None arm
- `docs/keybind.md`, `README.md`, `docs/requirements.md`, `CONTEXT.md`, `src/ui/AGENTS.md`, `src/app/AGENTS.md` - Task 11

## New Files (if any)

- `src/ui/switcher/position.rs` - NavPosition, NavPositionSetting, resolve_nav_position, step_nav_position, and their unit tests. Re-exported from mod.rs via `pub use`

## Risks

- **Blast radius of the ViewLayout rename**: a mechanical change across about 35 sites. The risk is low because the compiler catches every missed spot, but the diff is wide. Task 1 is kept separate, so the later diffs stay narrow.
- **Position resolution in the test harness**: unless `Harness::draw` and `portrait()` resolve with the default settings, all of the existing portrait tests break. The `auto_nav` helper in Task 3 absorbs this. Tests that call `compute_regions` directly must state the position explicitly.
- **Off-by-one in the drag mirror math**: right/bottom flip the sign because they are `window - coordinate`. The tests derive the border coordinates from `compute_regions` (right border 0-based 91, bottom 0-based 35), so a regression shows immediately.
- **Mismatch between the dump path and the live screen**: if the dump does not receive the position, a dump of a pinned placement shows the left layout. Task 6 makes the runtime pass `self.nav_position`. The dump itself keeps the existing simplification of a fixed NAV_WIDTH.
- **Grid-screen mismatch when the PTY resize is missed**: a position change can change both the width and the height of the terminal view. It is folded into the existing reconcile block to keep a single resize owner, and `clear_screen` also runs on `crossed_position`.
- **Default regression**: the combination auto=true, wide=left, narrow=top, absent file must equal today. The unit tests of the resolution rule pin this combination explicitly.
- **Static wording in the cheatsheet/help**: without following the position, the arrow guidance becomes false in the right/bottom placements. Task 10 closes this by making the chrome receive the position every frame.
- **clippy -D warnings**: the exhaustiveness of the new match arms, `parse`'s should_implement_trait, and so on. Follow the precedent pattern of `FocusTarget::from_str`.
- **No scope creep**: the issue explicitly rules out the ctl verb, a mouse position gesture, and doctor reporting. They are not in the plan.
