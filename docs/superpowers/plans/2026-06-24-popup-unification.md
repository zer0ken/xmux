# Popup Unification Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Present every action that needs extra input as a centered modal popup, remove the bottom input pane and the footer kill-confirm, route all popups through one opaque primitive, and let modal popups be dragged by their border.

**Architecture:** `render_popup` is already the shared, opaque popup renderer (verified: 0 residue over a colored grid). A new `render_modal_popup` picks the active modal (help / confirm / input), sizes it to its content, centers it shifted by a `popup_offset`, caches the drawn rect, and draws it through `render_popup`. The bottom-pane layout branch and the footer kill-confirm are deleted. A border press on a modal popup starts a move-drag handled in the cockpit's mouse chain, updating `popup_offset`.

**Tech Stack:** Rust, ratatui (TestBackend for headless render assertions), crossterm, tokio.

## Global Constraints

- Build/test with the real toolchain, not the blocked rustup shim:
  `TC="$HOME/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin"` then
  `RUSTC="$TC/rustc.exe" RUSTDOC="$TC/rustdoc.exe" "$TC/cargo.exe" <cmd>`.
- `cargo test` does not rebuild the binary; tests run against the lib.
- AS-IS codebase: no history narration in comments/docs.
- Keep clippy clean (`cargo clippy --all-targets`).
- Key routing (`handle_key`, `handle_input_key`, `feed_help_key`, `resolve_kill`) and the cockpit `is_inputting()` focus lock are UNCHANGED — only the render surface and mouse drag change.

---

### Task 1: Input (and filter) as a centered popup; remove the bottom pane

Introduces the shared modal-popup render path (`render_modal_popup`,
`offset_centered`, `popup_offset`, `popup_rect`) and moves both help and the
input onto it. Deletes the bottom input pane.

**Files:**
- Modify: `src/ui/switcher.rs`

**Interfaces:**
- Produces: `Switcher::render_modal_popup(&mut self, frame, area)`; fields
  `popup_offset: (i16, i16)`, `popup_rect: Rect`; module fn
  `offset_centered(w, h, area, offset) -> Rect`; `Switcher::input_lines(&self) -> (String, Vec<Line>)`; `Switcher::help_lines(&self) -> (String, Vec<Line>)`.
- Consumes: existing `render_popup`, `centered_rect`, `self.input`, `self.show_help`.

- [ ] **Step 1: Add the two fields to the struct and `blank()`**

In the `Switcher` struct (near `screen_area`), add:

```rust
    /// Drag offset (cells) applied to a modal popup's centered position. Reset
    /// to (0,0) when a popup opens; updated while its border is dragged (Task 4).
    popup_offset: (i16, i16),
    /// The drawn rect of the active modal popup (help/input/confirm), cached
    /// each render so a mouse press can hit-test its border. `Rect::default()`
    /// ⇒ no modal popup open.
    popup_rect: Rect,
```

In `blank()` (near `screen_area: Rect::default(),`) add:

```rust
            popup_offset: (0, 0),
            popup_rect: Rect::default(),
```

- [ ] **Step 2: Write the failing test for the centered input popup**

Add to the `tests` module (near the other input tests):

```rust
    #[tokio::test]
    async fn input_renders_as_a_centered_popup_not_the_bottom_pane() {
        let mut h = Harness::new(sample());
        h.ch('/').await; // open the filter input
        let buf = h.buf();
        let w = buf.area.width;
        let last = buf.area.height - 1;
        // The entry field is NO LONGER on the bottom row.
        let bottom: String = (0..w).map(|x| buf[(x, last)].symbol()).collect();
        assert!(!bottom.contains('❯'), "entry must not be on the bottom row anymore:\n{bottom}");
        // It is in a centered bordered box somewhere in the middle rows.
        let whole: String = (0..buf.area.height)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .map(|(x, y)| buf[(x, y)].symbol().to_string())
            .collect();
        assert!(whole.contains('❯'), "entry field present in a popup");
        assert!(whole.contains("Esc to cancel"), "popup shows the Esc hint");
    }
```

- [ ] **Step 3: Run it to confirm it fails**

Run: `RUSTC=... RUSTDOC=... cargo.exe test --lib input_renders_as_a_centered_popup_not_the_bottom_pane`
Expected: FAIL (the entry is still on the bottom row).

- [ ] **Step 4: Add `offset_centered` and the content builders**

Near `centered_rect` (module-level fn), add:

```rust
/// `centered_rect` shifted by `offset` (cells) and clamped fully inside `area`.
fn offset_centered(w: u16, h: u16, area: Rect, offset: (i16, i16)) -> Rect {
    let base = centered_rect(w, h, area);
    let max_x = area.x + area.width.saturating_sub(w);
    let max_y = area.y + area.height.saturating_sub(h);
    let x = (base.x as i32 + offset.0 as i32).clamp(area.x as i32, max_x as i32) as u16;
    let y = (base.y as i32 + offset.1 as i32).clamp(area.y as i32, max_y as i32) as u16;
    Rect { x, y, width: w.min(area.width), height: h.min(area.height) }
}

/// A short popup title for an input mode (shown on the box's top border).
fn input_title(mode: InputMode) -> &'static str {
    match mode {
        InputMode::Filter => "filter",
        InputMode::New => "new session",
        InputMode::NewWindow => "new window",
        InputMode::SplitWindow => "split",
        InputMode::Rename => "rename",
    }
}
```

In `impl Switcher`, add the input content builder (replaces `render_input` /
`input_desc_lines`):

```rust
    /// The active input rendered as popup `(title, lines)`: the instructional
    /// label, the `❯ buffer` entry line, and a dim Esc hint.
    fn input_lines(&self) -> (String, Vec<Line>) {
        let input = self.input.as_ref().expect("input active");
        let dim = Style::default().add_modifier(Modifier::DIM);
        let lines = vec![
            Line::from(Span::styled(format!(" {}", input.label.trim()), dim)),
            Line::from(format!(" ❯ {}", input.buffer)),
            Line::from(Span::styled(" Esc to cancel", dim)),
        ];
        (input_title(input.mode).to_string(), lines)
    }
```

- [ ] **Step 5: Refactor `render_help` into `help_lines` + `render_modal_popup`**

Replace the body of `render_help` so it builds and returns `(title, lines)`,
renamed to `help_lines`. Keep the `ROWS`/`kw`/`lines` construction verbatim;
change only the signature and the tail:

```rust
    fn help_lines(&self) -> (String, Vec<Line>) {
        // ... keep the existing `enum HelpRow`, `ROWS`, `kw`, and `lines` build ...
        ("keys".to_string(), lines)
    }
```

(Delete the old tail that computed `inner_w/w/h/rect` and called
`render_popup` — that sizing moves into `render_modal_popup`.)

Add `render_modal_popup`:

```rust
    /// Draws the active modal popup (help / confirm / input) centered, shifted
    /// by `popup_offset`, through the shared opaque `render_popup`, and caches
    /// its rect for drag hit-testing. These modals are mutually exclusive in
    /// normal use; if more than one is set, help wins, then confirm, then input.
    fn render_modal_popup(&mut self, frame: &mut Frame, area: Rect) {
        let (title, lines) = if self.show_help {
            self.help_lines()
        } else if self.input.is_some() {
            self.input_lines()
        } else {
            self.popup_rect = Rect::default();
            return;
        };
        let inner_w = lines.iter().map(Line::width).max().unwrap_or(0) as u16;
        let w = (inner_w + 3).clamp(24, area.width.max(1));
        let h = (lines.len() as u16 + 2).min(area.height.max(1));
        let rect = offset_centered(w, h, area, self.popup_offset);
        self.popup_rect = rect;
        render_popup(frame, area, rect, &title, lines);
    }
```

(The confirm branch is added in Task 2.)

- [ ] **Step 6: Delete the bottom-pane layout + draws + the input divider, and call `render_modal_popup`**

In `render()`:
- In the `tree_width == 0` branch, replace
  `if self.show_help { self.render_help(frame, area); }` with
  `self.render_modal_popup(frame, area);`.
- Delete the `let (main_area, input_layout) = if self.input.is_some() { … } else { (area, None) };` block; use `area` directly for the `cols` split (`Layout::horizontal(...).split(area)`).
- Delete the trailing `if let Some((divider_area, pane_area, desc_lines)) = input_layout { … }` block.
- Replace the trailing `if self.show_help { self.render_help(frame, area); }` with `self.render_modal_popup(frame, area);`.
- Change the divider call to drop the input arg: `self.render_divider(frame, cols[1], terminal_focused);`.

Update `render_divider` signature and drop the input-focus branch:

```rust
    fn render_divider(&self, frame: &mut Frame, area: Rect, terminal_focused: bool) {
        // ... keep the hover branch ...
        let colors: Vec<Color> = if area.height <= 1 {
            vec![active; area.height as usize]
        } else {
            // ... keep the top/bottom split unchanged ...
        };
        // ... keep the bars render ...
    }
```

Delete `render_input`, `render_input_divider`, and `input_desc_lines`
entirely.

- [ ] **Step 7: Replace the obsolete bottom-pane tests**

Delete `input_pane_is_full_width_at_the_screen_bottom`,
`input_prompt_is_two_lines_at_the_bottom_with_esc_hint`, and
`input_description_wraps_to_multiple_lines_when_narrow` (they assert the
removed bottom pane). Keep `input_esc_cancels_without_acting` (behavior
unchanged). The new Task-1 test covers the popup.

- [ ] **Step 8: Build, test, clippy**

Run: `cargo.exe test --lib` then `cargo.exe clippy --all-targets`
Expected: the new test passes; no failures; clippy clean.

- [ ] **Step 9: Commit**

```bash
git add -A && git commit -m "feat: render the input as a centered popup, remove the bottom pane"
```

---

### Task 2: Kill confirm as a centered red popup; remove the footer confirm

**Files:**
- Modify: `src/ui/switcher.rs`

**Interfaces:**
- Produces: `Switcher::confirm_lines(&self) -> (String, Vec<Line>)`.
- Consumes: `render_modal_popup` (Task 1), `self.pending_kill`.

- [ ] **Step 1: Write the failing test**

```rust
    #[tokio::test]
    async fn kill_confirm_is_a_centered_red_popup_not_the_footer() {
        let mut h = Harness::new(sample());
        let build = row_index(&h, |r| matches!(r, RowRef::Session(s) if s.name == "build"));
        h.sw.set_selected(build);
        h.sw.user_moved = true;
        h.key(KeyCode::Char('x')).await; // arm the confirm
        let buf = h.buf();
        let last = buf.area.height - 1;
        let footer: String = (0..buf.area.width).map(|x| buf[(x, last)].symbol()).collect();
        assert!(!footer.contains("[y]es"), "confirm must not be in the footer:\n{footer}");
        // A "kill" cell exists in red, in a centered box (not the footer row).
        let red_kill = (0..last).flat_map(|y| (0..buf.area.width).map(move |x| (x, y)))
            .any(|(x, y)| buf[(x, y)].symbol() == "k" && buf[(x, y)].fg == Color::Red);
        assert!(red_kill, "the confirm popup shows red 'kill' text above the footer");
    }
```

- [ ] **Step 2: Run it to confirm it fails**

Run: `cargo.exe test --lib kill_confirm_is_a_centered_red_popup_not_the_footer`
Expected: FAIL (confirm is still in the footer).

- [ ] **Step 3: Add `confirm_lines` and wire it into `render_modal_popup`**

```rust
    /// The armed kill confirm rendered as popup `(title, lines)`, in red.
    fn confirm_lines(&self) -> (String, Vec<Line>) {
        let red = Style::default().fg(Color::Red);
        let q = match self.pending_kill.as_ref().expect("kill armed") {
            PendingKill::Session(sess) => format!(" kill {}?", sess.address()),
            PendingKill::Window { source, target, .. } => format!(" kill {}/{}?", source, target),
        };
        let lines = vec![
            Line::from(Span::styled(q, red)),
            Line::from(Span::styled(" [y]es / [n]o · Esc cancel", red)),
        ];
        ("kill?".to_string(), lines)
    }
```

In `render_modal_popup`, insert the confirm branch between help and input:

```rust
        let (title, lines) = if self.show_help {
            self.help_lines()
        } else if self.pending_kill.is_some() {
            self.confirm_lines()
        } else if self.input.is_some() {
            self.input_lines()
        } else {
            self.popup_rect = Rect::default();
            return;
        };
```

- [ ] **Step 4: Remove the footer kill-confirm**

In `footer_text`, delete the two leading `if let Some(PendingKill...)` branches
so it begins with `if !self.flash.is_empty()`.

In `footer_lines`, change the guard from
`if self.flash.is_empty() || self.pending_kill.is_some()` to
`if self.flash.is_empty()`.

In `render_footer`, delete the `if self.pending_kill.is_some() { para = para.style(...Red) }` block; render the paragraph plainly.

- [ ] **Step 5: Update/replace the footer-confirm tests**

Delete `kill_confirm_footer_is_red` and `kill_window_confirm_footer_is_red`.
In `kill_on_window_row_targets_the_window`, replace the footer-cell red
assertion (the `buf[(x, height-1)]` scan for `"k"`) with a scan over the rows
ABOVE the footer for a red `"k"` cell (the popup), mirroring the Task-2 test;
keep the `PendingKill::Window` and the `y`-confirm assertions intact.

- [ ] **Step 6: Build, test, clippy**

Run: `cargo.exe test --lib` then `cargo.exe clippy --all-targets`
Expected: pass; clippy clean.

- [ ] **Step 7: Commit**

```bash
git add -A && git commit -m "feat: render the kill confirm as a centered red popup, remove the footer confirm"
```

---

### Task 3: Opacity regression test across help / input / confirm

**Files:**
- Modify: `src/ui/switcher.rs` (tests)

**Interfaces:**
- Consumes: `render` over a grid; `Color::Indexed`.

- [ ] **Step 1: Write the test**

```rust
    #[tokio::test]
    async fn every_popup_type_is_opaque_over_a_colored_grid() {
        // A grid filled with a blue background; each popup type drawn over it must
        // leave zero interior cells showing the grid's background.
        fn blue_grid() -> crate::proxy::screen::Grid {
            let mut g = crate::proxy::screen::Grid::new(30, 100);
            let mut fill = Vec::from(&b"\x1b[44m"[..]);
            for r in 0..30u16 {
                fill.extend(format!("\x1b[{};1H", r + 1).bytes());
                fill.extend(std::iter::repeat(b'X').take(100));
            }
            g.feed(&fill);
            g
        }
        fn interior_blue(buf: &Buffer) -> usize {
            // Find the box corner, size it, count Indexed(4) cells inside.
            let mut tl = None;
            'o: for y in 0..buf.area.height {
                for x in 0..buf.area.width {
                    if buf[(x, y)].symbol() == "┌" { tl = Some((x, y)); break 'o; }
                }
            }
            let Some((x0, y0)) = tl else { return usize::MAX };
            let mut w = 0;
            while x0 + w < buf.area.width - 1 && buf[(x0 + w, y0)].symbol() != "┐" { w += 1; }
            let mut hgt = 0;
            while y0 + hgt < buf.area.height - 1 && buf[(x0, y0 + hgt)].symbol() != "└" { hgt += 1; }
            let mut n = 0;
            for y in (y0 + 1)..(y0 + hgt) {
                for x in (x0 + 1)..(x0 + w) {
                    if buf[(x, y)].bg == Color::Indexed(4) { n += 1; }
                }
            }
            n
        }

        // help
        let mut h = Harness::new(sample());
        h.sw.show_help();
        let g = blue_grid();
        h.term.draw(|f| h.sw.render(f, Some(&g), true, 0)).unwrap();
        assert_eq!(interior_blue(h.buf()), 0, "help popup interior must be opaque");

        // input
        let mut h = Harness::new(sample());
        h.ch('/').await;
        let g = blue_grid();
        h.term.draw(|f| h.sw.render(f, Some(&g), false, TREE_WIDTH)).unwrap();
        assert_eq!(interior_blue(h.buf()), 0, "input popup interior must be opaque");

        // confirm
        let mut h = Harness::new(sample());
        let build = row_index(&h, |r| matches!(r, RowRef::Session(s) if s.name == "build"));
        h.sw.set_selected(build);
        h.sw.user_moved = true;
        h.sw.arm_kill();
        let g = blue_grid();
        h.term.draw(|f| h.sw.render(f, Some(&g), false, TREE_WIDTH)).unwrap();
        assert_eq!(interior_blue(h.buf()), 0, "confirm popup interior must be opaque");
    }
```

- [ ] **Step 2: Run it**

Run: `cargo.exe test --lib every_popup_type_is_opaque_over_a_colored_grid`
Expected: PASS (the primitive is already opaque; this locks it in).

- [ ] **Step 3: Commit**

```bash
git add -A && git commit -m "test: lock in popup opacity over a colored grid for all popup types"
```

---

### Task 4: Drag modal popups by their border

**Files:**
- Modify: `src/ui/switcher.rs`, `src/cockpit.rs`

**Interfaces:**
- Produces: `Switcher::popup_drag_active(&self) -> bool`,
  `begin_popup_drag(&mut self, col, row) -> bool`,
  `drag_popup(&mut self, col, row)`, `end_popup_drag(&mut self)`,
  `reset_popup_pos(&mut self)`.
- Consumes: `self.popup_rect`, `self.popup_offset` (Task 1).

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn popup_border_press_then_drag_moves_the_rect() {
        let mut sw = Switcher::new(sample());
        sw.show_help();
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| sw.render(f, None, false, 0)).unwrap();
        let before = sw.popup_rect;
        let (bx, by) = (before.x, before.y); // top-left corner is on the border
        assert!(sw.begin_popup_drag(bx, by), "press on the border grabs");
        sw.drag_popup(bx + 5, by + 3);
        term.draw(|f| sw.render(f, None, false, 0)).unwrap();
        assert_eq!(sw.popup_rect.x, before.x + 5, "moved right by 5");
        assert_eq!(sw.popup_rect.y, before.y + 3, "moved down by 3");
        sw.end_popup_drag();
        assert!(!sw.popup_drag_active());
    }

    #[test]
    fn popup_interior_press_does_not_grab() {
        let mut sw = Switcher::new(sample());
        sw.show_help();
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| sw.render(f, None, false, 0)).unwrap();
        let r = sw.popup_rect;
        assert!(!sw.begin_popup_drag(r.x + 2, r.y + 2), "interior press does not start a drag");
    }

    #[test]
    fn popup_drag_clamps_within_screen() {
        let mut sw = Switcher::new(sample());
        sw.show_help();
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| sw.render(f, None, false, 0)).unwrap();
        let r = sw.popup_rect;
        assert!(sw.begin_popup_drag(r.x, r.y));
        sw.drag_popup(r.x.saturating_sub(50), r.y); // yank far left, past the edge
        term.draw(|f| sw.render(f, None, false, 0)).unwrap();
        assert_eq!(sw.popup_rect.x, 0, "clamped to the left screen edge");
    }
```

- [ ] **Step 2: Run them to confirm they fail**

Run: `cargo.exe test --lib popup_border_press`
Expected: FAIL (`begin_popup_drag` not defined).

- [ ] **Step 3: Add the drag state + methods**

Add the field to the struct (near `popup_rect`) and to `blank()`:

```rust
    /// Active border-drag of a modal popup: the grabbed screen cell and the
    /// `popup_offset` at grab time. `None` ⇒ not dragging.
    popup_drag: Option<PopupDrag>,
```
```rust
            popup_drag: None,
```

Add the struct (near `Menu`):

```rust
#[derive(Clone, Copy)]
struct PopupDrag {
    grab: (u16, u16),
    origin: (i16, i16),
}
```

Add the methods in `impl Switcher`:

```rust
    /// True while a modal popup is being border-dragged; the cockpit routes
    /// every mouse event here until release, like the divider drag.
    pub fn popup_drag_active(&self) -> bool {
        self.popup_drag.is_some()
    }

    /// A left press on the active modal popup's border begins a move-drag.
    /// Returns true iff it grabbed (so the cockpit consumes the event).
    pub fn begin_popup_drag(&mut self, col: u16, row: u16) -> bool {
        let r = self.popup_rect;
        if r.width < 2 || r.height < 2 {
            return false; // no modal popup open
        }
        let inside = col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height;
        let on_border = inside
            && (col == r.x || col == r.x + r.width - 1 || row == r.y || row == r.y + r.height - 1);
        if !on_border {
            return false;
        }
        self.popup_drag = Some(PopupDrag { grab: (col, row), origin: self.popup_offset });
        true
    }

    /// Updates `popup_offset` from the cursor while a border-drag is active.
    pub fn drag_popup(&mut self, col: u16, row: u16) {
        if let Some(d) = self.popup_drag {
            let dx = col as i32 - d.grab.0 as i32;
            let dy = row as i32 - d.grab.1 as i32;
            self.popup_offset = ((d.origin.0 as i32 + dx) as i16, (d.origin.1 as i32 + dy) as i16);
        }
    }

    /// Ends a border-drag.
    pub fn end_popup_drag(&mut self) {
        self.popup_drag = None;
    }

    /// Resets a modal popup to its centered position (called when one opens).
    fn reset_popup_pos(&mut self) {
        self.popup_offset = (0, 0);
        self.popup_drag = None;
    }
```

- [ ] **Step 4: Reset position when a popup opens**

Add `self.reset_popup_pos();` at:
- `show_help` (top of the body),
- `toggle_help` — only when opening: `self.show_help = !self.show_help; if self.show_help { self.reset_popup_pos(); }`,
- `open_input` (after `self.flash.clear();`),
- `open_new` (after `self.flash.clear();`),
- `arm_kill` (top of the body).

- [ ] **Step 5: Run the switcher drag tests**

Run: `cargo.exe test --lib popup_border_press popup_interior_press popup_drag_clamps`
Expected: PASS.

- [ ] **Step 6: Wire the cockpit mouse gate**

In `src/cockpit.rs`, in the mouse-scan loop, right AFTER
`let is_left_press = is_press && (ev.cb & 0x03) == 0;` and BEFORE the
divider-grab start (`if is_left_press && tree_width > 0 && col0 == tree_width`),
insert:

```rust
                            // A modal popup (help/input/confirm) moves when its
                            // border is dragged. Once grabbed it owns every mouse
                            // event until release, like the divider drag / menu hold.
                            if switcher.popup_drag_active() {
                                if !ev.pressed {
                                    switcher.end_popup_drag();
                                } else if !is_wheel {
                                    switcher.drag_popup(col0, ev.row.saturating_sub(1));
                                }
                                dirty = true;
                                i += len;
                                continue;
                            }
                            if is_left_press
                                && switcher.begin_popup_drag(col0, ev.row.saturating_sub(1))
                            {
                                dirty = true;
                                i += len;
                                continue;
                            }
```

- [ ] **Step 7: Build, test, clippy**

Run: `cargo.exe test` then `cargo.exe clippy --all-targets`
Expected: all pass; clippy clean.

- [ ] **Step 8: Commit**

```bash
git add -A && git commit -m "feat: drag modal popups by their border to reposition"
```

---

## Self-Review

- **Spec coverage:** #1 opacity → Task 3 (lock-in, verified already opaque); #2 reusable primitive → Tasks 1-2 route help/input/confirm through `render_popup` via `render_modal_popup`; #3 input/confirm popups + bottom-pane removal → Tasks 1-2; #4 drag → Task 4. All covered.
- **Type consistency:** `render_modal_popup`, `help_lines`, `input_lines`, `confirm_lines` all return `(String, Vec<Line>)`; `offset_centered(w,h,area,offset)` consistent; `popup_offset: (i16,i16)`, `popup_rect: Rect`, `popup_drag: Option<PopupDrag>` consistent across tasks.
- **Placeholder scan:** none.
- **Help label note:** `help_lines` reuses the existing `render_help` body verbatim except the return; the implementer must not re-derive the ROWS.
