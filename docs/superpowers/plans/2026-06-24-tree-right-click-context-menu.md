# Tree Pane Right-Click Context Menu Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a press-hold-release right-click context menu over the tree pane that surfaces each tree node's actions (open / new / rename / kill / split / reconnect) for mouse users.

**Architecture:** The menu is a fourth modal state on the `Switcher` (alongside `input`, `pending_kill`, `show_help`). The cockpit's existing SGR-mouse scan loop drives the gesture exactly like the existing divider drag (`dragging_divider`): a right-button press over a tree row opens the menu, motion while held sets the hovered item, button-up acts on the hovered item (or cancels). Menu actions delegate to the existing keyboard-action methods (`open_new`, `open_input`, `arm_kill`, `queue_split`, `request_rescan`) — the menu is a launcher, not a reimplementation.

**Tech Stack:** Rust, ratatui (TestBackend for headless render tests), crossterm SGR mouse, tokio. No new dependencies.

## Global Constraints

- Branch: `feat/rust-rewrite`. Do not push (commit only; user gates the push).
- UI copy is **English** (matches the existing help text / flash messages — e.g. "cannot rename a host", "fuzzy filter"). Menu labels are English.
- The mux pane right-click is **NOT touched** — it keeps forwarding to the child mux. This feature is tree-pane only.
- Reuse the existing modal pattern and popup helpers (`centered_rect`, `popup_clear_rect`, `Block::bordered`); add no new UI framework.
- `cargo` and `clippy` must be invoked via the real toolchain binaries on this box (the rustup shim is blocked): use `~/.rustup/toolchains/<tc>/bin/cargo.exe`. `cargo test` does NOT rebuild the bin — that's fine here, all tests are lib tests.
- AS-IS: no history/delta comments ("was X", "now Y", "removed"); comments explain why current code is shaped as it is.
- clippy clean (`-D warnings`), every test green before a task's commit.

---

### Task 1: Menu model + per-row-type items

**Files:**
- Modify: `src/ui/switcher.rs` (add types near the `RowRef`/`Input` definitions ~line 196-267; add two fields to `struct Switcher` ~line 269-318 and `Switcher::blank` ~line 321-350)
- Test: `src/ui/switcher.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Produces:
  - `enum MenuItem { Open, NewSession, Reconnect, NewWindow, Rename, Kill, SplitVertical, SplitHorizontal }` with `fn label(self) -> &'static str`
  - `pub enum MenuOutcome { None, Handled, FocusTerminal }`
  - `struct Menu { target: RowRef, rect: Rect, items: Vec<MenuItem>, hovered: Option<usize> }`
  - `fn menu_items(target: &RowRef) -> Vec<MenuItem>`
  - `Switcher` fields: `menu: Option<Menu>`, `screen_area: Rect`

- [ ] **Step 1: Write the failing test**

Add to the `tests` module:

```rust
#[test]
fn menu_items_by_row_type() {
    use super::MenuItem::*;
    let host = RowRef::Host { source: "h".into(), unreachable: false };
    assert_eq!(menu_items(&host), vec![NewSession, Reconnect]);

    let s = sess("h", "api", 1, false, 0);
    assert_eq!(
        menu_items(&RowRef::Session(s.clone())),
        vec![Open, Rename, Kill, NewWindow]
    );
    assert_eq!(
        menu_items(&RowRef::Window { sess: s, window: 1 }),
        vec![Open, SplitVertical, SplitHorizontal, Rename, Kill]
    );
    assert!(menu_items(&RowRef::Pane).is_empty());
    assert!(menu_items(&RowRef::Loading).is_empty());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `~/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin/cargo.exe test -p xmux menu_items_by_row_type`
Expected: FAIL — `cannot find function menu_items` / `MenuItem` unresolved.

- [ ] **Step 3: Write minimal implementation**

Add after the `RowRef` enum (~line 209):

```rust
/// One context-menu entry. The variant drives the action taken on release; the
/// label is the row text. English to match the rest of the tree UI.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum MenuItem {
    Open,
    NewSession,
    Reconnect,
    NewWindow,
    Rename,
    Kill,
    SplitVertical,
    SplitHorizontal,
}

impl MenuItem {
    fn label(self) -> &'static str {
        match self {
            MenuItem::Open => "open",
            MenuItem::NewSession => "new session",
            MenuItem::Reconnect => "reconnect",
            MenuItem::NewWindow => "new window",
            MenuItem::Rename => "rename",
            MenuItem::Kill => "kill",
            MenuItem::SplitVertical => "split vertical",
            MenuItem::SplitHorizontal => "split horizontal",
        }
    }
}

/// What the cockpit must do after a menu release. Most items are handled inside the
/// switcher (they open an input, arm a kill, or queue an op); `FocusTerminal` is the
/// one outcome the cockpit owns (the "open" item moves focus to the mux pane).
pub enum MenuOutcome {
    None,
    Handled,
    FocusTerminal,
}

/// An open right-click context menu. `target` is the node it acts on, re-located by
/// identity at release so a tree rebuild during the brief hold cannot misfire on a
/// stale row. `rect` is the bordered box in 0-based screen coords; `hovered` is the
/// item under the mouse, or None (released there = cancel).
struct Menu {
    target: RowRef,
    rect: Rect,
    items: Vec<MenuItem>,
    hovered: Option<usize>,
}

/// The menu entries for a node, by type. Non-selectable rows (pane/loading) get none.
fn menu_items(target: &RowRef) -> Vec<MenuItem> {
    use MenuItem::*;
    match target {
        RowRef::Host { .. } => vec![NewSession, Reconnect],
        RowRef::Session(_) => vec![Open, Rename, Kill, NewWindow],
        RowRef::Window { .. } => vec![Open, SplitVertical, SplitHorizontal, Rename, Kill],
        RowRef::Pane | RowRef::Loading => Vec::new(),
    }
}
```

Add two fields to `struct Switcher` (after `preferred: Option<String>,` ~line 317):

```rust
    /// The open right-click context menu (the fourth modal, like `input` /
    /// `pending_kill` / `show_help`). `None` ⇒ no menu.
    menu: Option<Menu>,
    /// The whole frame area, captured each render so the menu box can be clamped to
    /// the screen at open time (mouse events arrive between renders).
    screen_area: Rect,
```

Add to `Switcher::blank()` (after `preferred: None,` ~line 349):

```rust
            menu: None,
            screen_area: Rect::default(),
```

- [ ] **Step 4: Run test to verify it passes**

Run: `~/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin/cargo.exe test -p xmux menu_items_by_row_type`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/ui/switcher.rs
git commit -m "feat: add tree context-menu model and per-row-type items"
```

---

### Task 2: Gesture logic — open / hover / release / cancel

**Files:**
- Modify: `src/ui/switcher.rs` (add a `// --- context menu` method block after the `// --- mouse` block, ~line 1452; add free fns `menu_rect` and `Menu::item_at` near `centered_rect` ~line 1840)
- Test: `src/ui/switcher.rs` (`tests` module)

**Interfaces:**
- Consumes: `menu_items` (Task 1), `Menu`/`MenuItem`/`MenuOutcome` (Task 1), existing `in_tree`, `set_selected`, `list_state.offset()`, `tree_inner`, `screen_area`, `open_new`, `open_input(InputMode::Rename)`, `arm_kill`, `request_rescan`, `queue_split`, `same_node`.
- Produces:
  - `pub fn menu_active(&self) -> bool`
  - `pub fn menu_open(&mut self, col: u16, row: u16) -> bool` (0-based screen coords; returns true iff a menu opened)
  - `pub fn menu_hover(&mut self, col: u16, row: u16)`
  - `pub fn menu_release(&mut self) -> MenuOutcome`
  - `pub fn menu_cancel(&mut self)`
  - `fn menu_rect(col: u16, row: u16, items: &[MenuItem], area: Rect) -> Rect`
  - `Menu::item_at(&self, col: u16, row: u16) -> Option<usize>`

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module:

```rust
/// The screen (col,row) of the tree row at `idx`, given the current layout.
fn row_screen_pos(h: &Harness, idx: usize) -> (u16, u16) {
    let y = h.sw.tree_inner.y + (idx - h.sw.list_state.offset()) as u16;
    (h.sw.tree_inner.x, y)
}

fn row_index<F: Fn(&RowRef) -> bool>(h: &Harness, pred: F) -> usize {
    h.sw.rows.iter().position(|r| pred(&r.reference)).expect("row exists")
}

#[tokio::test]
async fn menu_open_on_session_does_not_move_cursor() {
    let mut h = Harness::new(sample());
    let before = h.sw.selected;
    let idx = row_index(&h, |r| matches!(r, RowRef::Session(s) if s.name == "build"));
    let (x, y) = row_screen_pos(&h, idx);
    assert!(h.sw.menu_open(x, y), "menu opens over a session row");
    assert!(h.sw.menu_active());
    assert_eq!(h.sw.selected, before, "opening the menu must not move the tree cursor");
}

#[tokio::test]
async fn menu_does_not_open_on_pane_row() {
    let mut h = Harness::new(sample());
    let idx = row_index(&h, |r| matches!(r, RowRef::Pane));
    let (x, y) = row_screen_pos(&h, idx);
    assert!(!h.sw.menu_open(x, y), "no menu over a pane row");
    assert!(!h.sw.menu_active());
}

#[tokio::test]
async fn menu_release_off_menu_cancels() {
    let mut h = Harness::new(sample());
    let idx = row_index(&h, |r| matches!(r, RowRef::Session(_)));
    let (x, y) = row_screen_pos(&h, idx);
    h.sw.menu_open(x, y);
    // No hover set → released off-menu → cancel.
    assert!(matches!(h.sw.menu_release(), MenuOutcome::None));
    assert!(!h.sw.menu_active(), "menu closes on release");
    assert!(h.sw.input.is_none() && h.sw.pending_kill.is_none(), "nothing happened");
}

#[tokio::test]
async fn menu_release_open_focuses_terminal_and_selects_target() {
    let mut h = Harness::new(sample());
    let s = sess("local", "build", 1, false, 100);
    let target = RowRef::Session(s);
    let items = menu_items(&target);
    let open_at = items.iter().position(|i| *i == MenuItem::Open).unwrap();
    h.sw.menu = Some(Menu { target, rect: Rect::new(0, 0, 20, 7), items, hovered: Some(open_at) });
    assert!(matches!(h.sw.menu_release(), MenuOutcome::FocusTerminal));
    assert_eq!(cur_session_name(&h).as_deref(), Some("build"), "cursor moved to target");
}

#[tokio::test]
async fn menu_release_rename_opens_input() {
    let mut h = Harness::new(sample());
    let target = RowRef::Session(sess("local", "build", 1, false, 100));
    let items = menu_items(&target);
    let at = items.iter().position(|i| *i == MenuItem::Rename).unwrap();
    h.sw.menu = Some(Menu { target, rect: Rect::new(0, 0, 20, 7), items, hovered: Some(at) });
    assert!(matches!(h.sw.menu_release(), MenuOutcome::Handled));
    assert!(h.sw.is_inputting(), "rename opens the inline input");
}

#[tokio::test]
async fn menu_release_kill_arms_confirm() {
    let mut h = Harness::new(sample());
    let target = RowRef::Session(sess("local", "build", 1, false, 100));
    let items = menu_items(&target);
    let at = items.iter().position(|i| *i == MenuItem::Kill).unwrap();
    h.sw.menu = Some(Menu { target, rect: Rect::new(0, 0, 20, 7), items, hovered: Some(at) });
    assert!(matches!(h.sw.menu_release(), MenuOutcome::Handled));
    assert!(h.sw.pending_kill.is_some(), "kill arms the y/n confirm");
}

#[tokio::test]
async fn menu_release_split_vertical_queues_op() {
    let mut h = Harness::new(sample());
    let s = sess("local", "editor", 2, true, 200);
    let target = RowRef::Window { sess: s, window: 2 };
    let items = menu_items(&target);
    let at = items.iter().position(|i| *i == MenuItem::SplitVertical).unwrap();
    h.sw.menu = Some(Menu { target, rect: Rect::new(0, 0, 20, 7), items, hovered: Some(at) });
    assert!(matches!(h.sw.menu_release(), MenuOutcome::Handled));
    match h.sw.take_pending_op() {
        Some(PendingOp::SplitWindow { target, vertical, .. }) => {
            assert_eq!(target, "editor:2");
            assert!(vertical, "split vertical");
        }
        other => panic!("expected a vertical SplitWindow op, got {other:?}"),
    }
}

#[tokio::test]
async fn menu_release_stale_target_cancels() {
    let mut h = Harness::new(sample());
    // A target that does not exist in the tree (rebuilt away during the hold).
    let target = RowRef::Session(sess("local", "ghost", 1, false, 0));
    let items = menu_items(&target);
    h.sw.menu = Some(Menu { target, rect: Rect::new(0, 0, 20, 7), items, hovered: Some(0) });
    assert!(matches!(h.sw.menu_release(), MenuOutcome::None), "gone target → no-op");
}

#[test]
fn menu_rect_clamps_into_screen() {
    use super::MenuItem::*;
    let area = Rect::new(0, 0, 80, 24);
    let items = [Open, Rename, Kill];
    // Anchored near the bottom-right corner → shifted up/left to stay on-screen.
    let r = menu_rect(78, 23, &items, area);
    assert!(r.x + r.width <= area.width, "box stays within the right edge");
    assert!(r.y + r.height <= area.height, "box stays within the bottom edge");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `~/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin/cargo.exe test -p xmux menu_`
Expected: FAIL — `no method named menu_open` / `menu_rect` not found.

- [ ] **Step 3: Write minimal implementation**

Add a method block after the `// --- mouse` methods (after `mouse_scroll`, ~line 1452):

```rust
    // --- context menu -------------------------------------------------------

    /// True while a context menu is open. The cockpit routes every mouse event to
    /// the menu (press-hold-release) while this holds, like a divider drag.
    pub fn menu_active(&self) -> bool {
        self.menu.is_some()
    }

    /// Right-button press at 0-based screen (col,row): opens that tree row's menu if
    /// it lands on a selectable row that has items. Does NOT move the tree cursor —
    /// the gesture only remembers the target, so no background attach fires mid-hold.
    /// Returns true iff a menu opened (so the cockpit knows to consume the event).
    pub fn menu_open(&mut self, col: u16, row: u16) -> bool {
        if !self.in_tree(col, row) {
            return false;
        }
        let offset = self.list_state.offset();
        let idx = offset + (row.saturating_sub(self.tree_inner.y)) as usize;
        let Some(target) = self
            .rows
            .get(idx)
            .filter(|r| r.selectable())
            .map(|r| r.reference.clone())
        else {
            return false;
        };
        let items = menu_items(&target);
        if items.is_empty() {
            return false;
        }
        let rect = menu_rect(col, row, &items, self.screen_area);
        self.menu = Some(Menu { target, rect, items, hovered: None });
        true
    }

    /// Mouse moved while the menu is held: set the item under the cursor (or None).
    pub fn menu_hover(&mut self, col: u16, row: u16) {
        if let Some(menu) = self.menu.as_mut() {
            menu.hovered = menu.item_at(col, row);
        }
    }

    /// Right-button up: act on the hovered item against the (re-located) target row,
    /// then close the menu. Released off-menu (no hovered item) cancels. The target is
    /// re-found by identity so a rebuild during the hold can't act on a stale node.
    pub fn menu_release(&mut self) -> MenuOutcome {
        let Some(menu) = self.menu.take() else {
            return MenuOutcome::None;
        };
        let Some(i) = menu.hovered else {
            return MenuOutcome::None;
        };
        let item = menu.items[i];
        let Some(idx) = self.rows.iter().position(|r| same_node(&r.reference, &menu.target)) else {
            return MenuOutcome::None;
        };
        // The delegated action methods act on the current cursor, so land it on the
        // target first (consistent with having right-clicked that row).
        self.user_moved = true;
        self.set_selected(idx);
        match item {
            MenuItem::Open => MenuOutcome::FocusTerminal,
            MenuItem::NewSession | MenuItem::NewWindow => {
                self.open_new();
                MenuOutcome::Handled
            }
            MenuItem::Rename => {
                self.open_input(InputMode::Rename);
                MenuOutcome::Handled
            }
            MenuItem::Kill => {
                self.arm_kill();
                MenuOutcome::Handled
            }
            MenuItem::Reconnect => {
                self.request_rescan();
                MenuOutcome::Handled
            }
            MenuItem::SplitVertical | MenuItem::SplitHorizontal => {
                if let RowRef::Window { sess, window } = &menu.target {
                    let target = format!("{}:{}", sess.name, window);
                    let dir = if matches!(item, MenuItem::SplitVertical) { "v" } else { "h" };
                    self.queue_split(Some(sess.source.clone()), Some(sess.clone()), Some(target), dir);
                }
                MenuOutcome::Handled
            }
        }
    }

    /// Close the menu without acting (cockpit watchdog: a keystroke ends the gesture).
    pub fn menu_cancel(&mut self) {
        self.menu = None;
    }
```

Add `Menu::item_at` and the free `menu_rect` near `centered_rect` (~line 1840):

```rust
impl Menu {
    /// The item index at 0-based screen (col,row), or None if outside the item area
    /// (the box's bordered interior, one row per item below the top border).
    fn item_at(&self, col: u16, row: u16) -> Option<usize> {
        let inside_x = col > self.rect.x && col + 1 < self.rect.x + self.rect.width;
        if !inside_x || row <= self.rect.y {
            return None;
        }
        let i = (row - self.rect.y - 1) as usize;
        (i < self.items.len()).then_some(i)
    }
}

/// The bordered menu box for an anchor at 0-based screen (col,row): sized to the
/// widest label (+ borders + a pad cell each side) and item count, clamped so it
/// stays fully inside `area` (shifts up/left near the bottom/right edge).
fn menu_rect(col: u16, row: u16, items: &[MenuItem], area: Rect) -> Rect {
    let inner_w = items.iter().map(|it| it.label().chars().count()).max().unwrap_or(0) as u16;
    let w = (inner_w + 4).min(area.width.max(1));
    let h = (items.len() as u16 + 2).min(area.height.max(1));
    let max_x = (area.x + area.width).saturating_sub(w).max(area.x);
    let max_y = (area.y + area.height).saturating_sub(h).max(area.y);
    Rect {
        x: col.clamp(area.x, max_x),
        y: row.clamp(area.y, max_y),
        width: w,
        height: h,
    }
}
```

Note: the release tests construct `Menu` with a fixed `rect`; `menu_open`'s rect uses `self.screen_area`, which `Harness::new` populates via its initial `draw()` (Task 3 sets `screen_area` in `render`). Task 2's `menu_open` tests rely on Task 3's `screen_area` assignment, so implement Task 2 and Task 3 together if running `menu_open_*` before Task 3 — OR temporarily set `h.sw.screen_area` in the test. To keep tasks independent, add this one line at the top of `menu_open_on_session_does_not_move_cursor` and `menu_does_not_open_on_pane_row` after `Harness::new`: `h.sw.screen_area = Rect::new(0, 0, 100, 30);` then re-`h.draw()` is not needed. (Task 3 makes this redundant but harmless.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `~/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin/cargo.exe test -p xmux menu_`
Expected: PASS (all `menu_*` tests)

- [ ] **Step 5: Commit**

```bash
git add src/ui/switcher.rs
git commit -m "feat: tree context-menu open/hover/release gesture logic"
```

---

### Task 3: Render the menu + help line

**Files:**
- Modify: `src/ui/switcher.rs` — `render` (~line 1456: set `screen_area`, call `render_menu` in both branches), add `render_menu` method, add a help row in `render_help` ROWS (~line 1726)
- Test: `src/ui/switcher.rs` (`tests` module)

**Interfaces:**
- Consumes: `Menu` (Task 1), `popup_clear_rect`, `screen_area`.
- Produces: `fn render_menu(&self, frame: &mut Frame)`.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn menu_renders_with_hovered_item_reversed() {
    use super::MenuItem::*;
    let mut h = Harness::new(sample());
    let target = RowRef::Session(sess("local", "build", 1, false, 100));
    let items = vec![Open, Rename, Kill, NewWindow];
    // Box at a known spot; hover the second item (rename).
    h.sw.menu = Some(Menu { target, rect: Rect::new(2, 2, 18, 6), items, hovered: Some(1) });
    h.draw();
    let out = h.text();
    assert!(out.contains("open") && out.contains("rename") && out.contains("kill"),
        "menu items render:\n{out}");

    // The hovered row (rename, at box y+1+1 = 4) is reversed across the box interior.
    let buf = h.buf();
    let reversed = (3..19).any(|x| buf[(x, 4u16)].modifier.contains(Modifier::REVERSED));
    assert!(reversed, "the hovered item renders reversed");
}

#[tokio::test]
async fn help_lists_the_right_click_menu() {
    let mut h = Harness::new(sample());
    h.sw.show_help();
    h.draw();
    assert!(h.text().contains("right-click"), "help mentions the right-click menu");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `~/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin/cargo.exe test -p xmux menu_renders_with_hovered_item_reversed help_lists_the_right_click_menu`
Expected: FAIL — menu not drawn / help line absent.

- [ ] **Step 3: Write minimal implementation**

In `render`, capture the area right after `let area = frame.area();` (~line 1463):

```rust
        let area = frame.area();
        self.screen_area = area;
```

In the `tree_width == 0` early-return branch, add the menu render after the `show_help` block (before `return;`, ~line 1485):

```rust
            if self.show_help {
                self.render_help(frame, area);
            }
            self.render_menu(frame);
            return;
```

At the end of the main branch, after the `show_help` block (~line 1519):

```rust
        if self.show_help {
            self.render_help(frame, area);
        }
        self.render_menu(frame);
```

Add the `render_menu` method (after `render_help`, ~line 1775):

```rust
    /// Draws the open context menu as a bordered popup at its anchored rect, the
    /// hovered item reversed. Mirrors `render_help`'s popup (Clear behind + a bordered
    /// Paragraph) but anchored at the click instead of centered.
    fn render_menu(&self, frame: &mut Frame) {
        let Some(menu) = self.menu.as_ref() else {
            return;
        };
        let rect = menu.rect;
        let pad = rect.width.saturating_sub(4) as usize;
        let lines: Vec<Line> = menu
            .items
            .iter()
            .enumerate()
            .map(|(i, it)| {
                let mut style = Style::default();
                if menu.hovered == Some(i) {
                    style = style.add_modifier(Modifier::REVERSED);
                }
                Line::from(Span::styled(format!(" {:<pad$} ", it.label()), style))
            })
            .collect();
        frame.render_widget(Clear, popup_clear_rect(rect, self.screen_area));
        frame.render_widget(
            Paragraph::new(Text::from(lines)).block(Block::bordered()),
            rect,
        );
    }
```

Add a help row in `render_help`'s `ROWS`, in the focus section after `Key("drag the divider", ...)` (~line 1727):

```rust
            Key("right-click a row", "hold for its menu, release on an item"),
```

- [ ] **Step 4: Run test to verify it passes**

Run: `~/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin/cargo.exe test -p xmux menu_renders_with_hovered_item_reversed help_lists_the_right_click_menu`
Expected: PASS

- [ ] **Step 5: Run the full switcher test module + clippy**

Run: `~/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin/cargo.exe test -p xmux`
Run: `~/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin/cargo.exe clippy --all-targets -- -D warnings`
Expected: all green, no clippy warnings.

- [ ] **Step 6: Commit**

```bash
git add src/ui/switcher.rs
git commit -m "feat: render the tree context menu and document it in help"
```

---

### Task 4: Cockpit mouse wiring — open/hold/release in the SGR scan loop

**Files:**
- Modify: `src/cockpit.rs` — extract `kick_rescan` from `handle_tree_bytes` (~line 567-578); add the menu capture block + right-press trigger + watchdog + outcome handling in the stdin mouse-scan loop (~line 1189-1300)
- Test: `src/cockpit.rs` (build + clippy gate; the loop itself is integration, verified live)

**Interfaces:**
- Consumes: `Switcher::{menu_active, menu_open, menu_hover, menu_release, menu_cancel}`, `MenuOutcome` (Tasks 1-2), existing `dispatch_pending_op`, `ensure_current_host`, `spawn_local_enumeration`.
- Produces: `fn kick_rescan(switcher, env, mgr, enum_tx)` (free fn).

- [ ] **Step 1: Extract `kick_rescan` (refactor, no behavior change)**

Replace the rescan block inside `handle_tree_bytes` (~line 567-578):

```rust
    if switcher.take_rescan_kick() {
        // 'r' re-scan: re-enumerate local sources and re-list remote ones.
        for src in &env.srcs {
            if src.remote {
                if let Some(c) = mgr.get(&src.alias) {
                    c.list_sessions();
                }
            } else {
                spawn_local_enumeration(src.clone(), enum_tx.clone());
            }
        }
    }
```

with a call:

```rust
    kick_rescan(switcher, env, mgr, enum_tx);
```

Add the free fn near `ensure_current_host` (~line 475):

```rust
/// Consumes a pending re-scan kick (set by `r` or a menu "reconnect"): re-lists every
/// remote host and re-enumerates every local source — the same probes as first launch.
/// A no-op when no kick is pending. Shared by the key and context-menu paths.
fn kick_rescan(
    switcher: &mut crate::ui::switcher::Switcher,
    env: &Env,
    mgr: &HostManager,
    enum_tx: &tokio::sync::mpsc::UnboundedSender<LocalEnum>,
) {
    if switcher.take_rescan_kick() {
        for src in &env.srcs {
            if src.remote {
                if let Some(c) = mgr.get(&src.alias) {
                    c.list_sessions();
                }
            } else {
                spawn_local_enumeration(src.clone(), enum_tx.clone());
            }
        }
    }
}
```

- [ ] **Step 2: Verify the refactor builds and tests pass (no behavior change)**

Run: `~/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin/cargo.exe test -p xmux`
Expected: PASS (same as before — pure extraction).

- [ ] **Step 3: Add the menu capture block + trigger + watchdog**

In the mouse-scan `while i < bytes.len()` loop, insert the **capture block** immediately after `let col0 = ev.col.saturating_sub(1);` and BEFORE the `if dragging_divider {` block (~line 1205-1206):

```rust
                            // A context menu owns every mouse event until the right
                            // button is released (press-hold-release), exactly like the
                            // divider drag below. Motion sets the hovered item; button-up
                            // acts on it (or cancels if off-menu).
                            if switcher.menu_active() {
                                if !ev.pressed {
                                    match switcher.menu_release() {
                                        crate::ui::switcher::MenuOutcome::FocusTerminal => {
                                            if app.is_overlay() {
                                                app.toggle();
                                            }
                                        }
                                        crate::ui::switcher::MenuOutcome::Handled => {
                                            // A menu item may queue an op (split) or a
                                            // re-scan (reconnect); dispatch them here so it
                                            // works in either focus state, not only overlay.
                                            dispatch_pending_op(&mut switcher, &ops, &op_tx);
                                            kick_rescan(&mut switcher, &env, &mgr, &enum_tx);
                                            ensure_current_host(&mut mgr, &env, &switcher, cols, body_rows, tree_width);
                                        }
                                        crate::ui::switcher::MenuOutcome::None => {}
                                    }
                                    dirty = true;
                                } else if !is_wheel {
                                    switcher.menu_hover(col0, ev.row.saturating_sub(1));
                                    dirty = true;
                                }
                                i += len;
                                continue;
                            }
```

Insert the **right-press trigger** after the idle-motion/hover block (after its `}` ~line 1244, before `if is_wheel && app.is_overlay() {`):

```rust
                            // Right-button press over the tree opens that row's context
                            // menu (press-hold-release). Tree-only: a press over the mux
                            // pane falls through and forwards to the child as before.
                            let is_right_press = is_press && (ev.cb & 0x03) == 2;
                            if is_right_press && in_mux.is_none() {
                                if switcher.menu_open(col0, ev.row.saturating_sub(1)) {
                                    dirty = true;
                                    i += len;
                                    continue;
                                }
                            }
```

Add the **watchdog** next to the divider-drag watchdog (after ~line 1298, the `dragging_divider && !non_mouse.is_empty()` block):

```rust
                // Watchdog: a keystroke (or any non-mouse byte) during a held menu ends
                // the gesture without acting — mirrors the divider-drag watchdog, so a
                // missed button-up can't strand the menu and eat later input.
                if switcher.menu_active() && !non_mouse.is_empty() {
                    switcher.menu_cancel();
                    dirty = true;
                }
```

- [ ] **Step 4: Build + clippy**

Run: `~/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin/cargo.exe build`
Run: `~/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin/cargo.exe clippy --all-targets -- -D warnings`
Expected: builds, no clippy warnings. (If `dispatch_pending_op` / `ensure_current_host` borrow-check against `switcher`/`mgr` in the new block, the borrows are sequential — same as their existing call sites — so they compile.)

- [ ] **Step 5: Full test suite**

Run: `~/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin/cargo.exe test`
Expected: all green.

- [ ] **Step 6: Commit**

```bash
git add src/cockpit.rs
git commit -m "feat: wire tree right-click context menu into the cockpit mouse loop"
```

---

## Live verification (human gate, after Task 4)

Headless tests cover the switcher logic + render; the press-hold-release gesture over a real terminal needs a human eye (the control socket injects keys/dumps, not raw SGR mouse). Check:

1. Launch `xmux`. Right-press a **session** row, hold, drag onto "rename", release → the rename input opens prefilled.
2. Right-press a **window** row, hold, drag onto "split vertical", release → the session splits vertically.
3. Right-press, release **without moving** (or off the box) → nothing happens, menu closes.
4. Right-press a **host** row → "new session" / "reconnect"; reconnect re-scans.
5. Right-click does **nothing new in the mux pane** (still forwards to the child).
6. "open" focuses the mux pane on the clicked session.

---

## Self-Review

**Spec coverage:**
- Tree-only, mux untouched → Task 4 trigger gated on `in_mux.is_none()`; mux forwarding path unchanged. ✓
- press-hold-release → Task 4 capture block (release acts, motion hovers) + Task 2 release/hover. ✓
- Open near the row, items per type → Task 1 `menu_items`, Task 2 `menu_rect`. ✓
- Cursor/focus untouched on open; "open" → mux focus → Task 2 `menu_open` (no cursor move) + `menu_release` FocusTerminal; Task 4 `app.toggle()`. ✓
- Delegate to existing flows (rename→input, kill→y/n, split→op, reconnect→rescan) → Task 2 `menu_release`. ✓
- Cancel off-menu / stale target → Task 2 `menu_release` (hovered None, `same_node` relocate). ✓
- Not over pane/loading/hidden tree → Task 2 `menu_open` (selectable + items guard); hidden tree has no tree column so `in_mux` covers it. ✓

**Placeholder scan:** No TBD/TODO; every step has full code + exact commands. ✓

**Type consistency:** `MenuItem`/`MenuOutcome`/`Menu` defined in Task 1, used unchanged in Tasks 2-4; `menu_open` returns `bool`, `menu_release` returns `MenuOutcome` consistently; `queue_split(Option<String>, Option<Session>, Option<String>, &str)` matches the existing signature (`switcher.rs:1137`); `dispatch_pending_op(&mut Switcher, &Arc<dyn Ops>, &UnboundedSender<OpResult>)` matches its call in `handle_tree_bytes`. ✓

**Known simplification (ponytail):** "reconnect" reuses `request_rescan()` which re-scans *every* host (no per-host kick exists). It does reconnect the clicked host; per-host scoping is the upgrade path if it matters. Flagged for the user.
