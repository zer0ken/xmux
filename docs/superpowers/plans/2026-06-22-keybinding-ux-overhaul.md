# Keybinding & Cockpit UX Overhaul Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Unify the cockpit's tree keybindings into a clean hierarchical model, make the tree↔mux focus and selection behave consistently, and improve the mux display (cursor, mouse, layout).

**Architecture:** `src/ui/switcher.rs` owns the tree state machine and key handling; `src/cockpit.rs` owns the `select!` event loop, focus state, draw, and PTY sizing; `src/proxy/{input,decode,screen,run}.rs` own terminal-focus input, key decoding, the vt100 grid, and PTY attachments. Changes stay within these files, following the existing immediate-mode ratatui + current-thread-tokio patterns. The cockpit is single-threaded; blocking work stays off the loop.

**Tech Stack:** Rust 2021, ratatui, crossterm, vt100, portable-pty, tokio current-thread runtime, cargo test.

## Global Constraints

- Toolchain on this box: the rustup shim is blocked. Run cargo via the real toolchain and set RUSTC/RUSTDOC:
  - `RUSTC="$HOME/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin/rustc.exe" RUSTDOC="$HOME/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin/rustdoc.exe" "$HOME/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin/cargo.exe" <cmd>`
- `cargo test` does NOT rebuild the bin; the lib tests (`cargo test --lib`) are the fast gate. Keep `cargo clippy --all-targets` at zero warnings.
- AS-IS codebase principle: comments/docs describe CURRENT behavior only — no "was X now Y", no change-narration. Update stale comments you touch.
- Every directional function must respond to BOTH the arrow key AND its hjkl equivalent (↑=k, ↓=j, ←=h, →=l). This is already true for plain navigation (Task baseline); keep it true for anything new.
- Do NOT commit or push beyond what each task's commit step says. Never `--no-verify`.
- Headless limits: the full cockpit loop and real-terminal rendering/cursor/mouse cannot be unit-tested. Unit-test the pure switcher/decode/grid logic; mark live-terminal behavior in the human visual-gate checklist at the bottom of `src/cockpit.rs` tests.

---

## Current State (already implemented, GREEN, UNCOMMITTED — Task 0 commits it)

The working tree already contains, passing 283 lib tests + clippy clean:

- **unlinked-window detection**: `src/proxy/control_proto.rs` `parse_notif` maps `%unlinked-window-add/close/renamed` to `WindowAdd/Close/Renamed` (→ `HostEvent::Changed`); test `host::tests::reader_unlinked_window_notifications_emit_changed`.
- **no-clear-on-toggle**: `src/cockpit.rs` removed `term.clear()` from the Overlay↔Passthrough focus toggles (both states draw the same split; only the divider colour differs). Module doc + human-gate comment updated.
- **last-selected persistence**: `src/state.rs` (new) with `load_last_session`/`save_last_session` (file `~/.xmux/last_session`). `Switcher` has `preferred: Option<String>` + `set_preferred`; `rebuild()` preselect priority = persisted session → else first session row (local-first via `order_groups`). Cockpit loads it into `set_preferred` and saves the settled session.
- **navigation model (#1, #2)**: `Switcher::handle_key` — ↑/↓/k/j = `move_sibling(∓1)` (same tree level); →/l = `descend`, ←/h = `ascend`. Ctrl+↑/↓ removed. PageUp/Down/Home/End unchanged (flat jumps).
- **partial window-follow (#3 old impl — Task 5 REPLACES it)**: `select_active_window()` + generalized `select_window` (follows from a session row too); scattered calls in the focus-toggle, `LocalEnum::Panes`, and the `Focus` handler. Task 5 removes these scattered calls and unifies them.
- **draw split instrumentation**: cockpit draw logs `render` vs `draw` ms under `XMUX_DEBUG`.

---

## File Structure

- `src/ui/switcher.rs` — tree key handling, `open_new`/rename/kill queueing, render (tree/footer/input/terminal), `select_active_window`, `restore_focus`, dynamic tree width. Most tasks touch this.
- `src/cockpit.rs` — event loop, focus toggle, draw, PTY sizing, tree-width state, the unified passthrough follow.
- `src/proxy/input.rs` — `TermInput` (terminal-focus prefix handling).
- `src/proxy/screen.rs` — `Grid` cursor accessors (already present).
- `src/proxy/run.rs` — `Attachment` (mouse forwarding sink, Task 9).
- `src/mux.rs` — argv builders (add `kill_window`, `rename_window` for Task 2).
- `src/ui/switcher.rs` `Ops` trait + `src/env.rs` `EnvOps` + `src/manage.rs` — mutate ops (add window kill/rename for Task 2).

---

## Task 0: Commit the green baseline

**Files:** all currently-modified files.

- [ ] **Step 1: Verify green**

Run:
```
RUSTC=... RUSTDOC=... cargo.exe test --lib
RUSTC=... RUSTDOC=... cargo.exe clippy --all-targets
```
Expected: `283 passed; 0 failed`; clippy zero warnings.

- [ ] **Step 2: Commit**

```
git add -A
git commit -m "feat: unlinked-window detect, no-clear toggle, last-session persist, level-nav"
```

---

## Task 1: Kill confirmation in RED (#5 colour)

**Files:**
- Modify: `src/ui/switcher.rs` (`render_footer`)
- Test: `src/ui/switcher.rs` tests

The footer shows `kill <addr>? [y]es / [n]o` when `pending_kill` is set. It currently renders in the default style; it must render RED.

- [ ] **Step 1: Write the failing test**

Add to the switcher test module. The `Harness` renders to a `TestBackend`; add a helper to read the footer row's style if one is not present, else assert via the buffer. Use the existing `tree_row_*`/buffer access pattern. Minimal test:

```rust
#[tokio::test]
async fn kill_confirm_footer_is_red() {
    let mut h = Harness::new(sample());
    h.ch('x').await; // arm kill on the selected session
    // The footer is the LAST row; find a cell of the confirm text and assert red fg.
    let buf = h.buffer();
    let y = buf.area.height - 1;
    let cell = (0..buf.area.width)
        .map(|x| &buf[(x, y)])
        .find(|c| c.symbol() == "k") // "kill ...": first 'k'
        .expect("kill confirm text present");
    assert_eq!(cell.fg, ratatui::style::Color::Red, "kill confirm must be red");
}
```

If `Harness` has no `buffer()` accessor, add one returning the last rendered `ratatui::buffer::Buffer` (the harness already renders into a `TestBackend`; expose `terminal.backend().buffer().clone()`).

- [ ] **Step 2: Run and verify RED**

Run: `cargo.exe test --lib kill_confirm_footer_is_red`
Expected: FAIL (footer renders default colour, not red).

- [ ] **Step 3: Implement**

In `render_footer`, when `self.pending_kill` is `Some`, style that line red:

```rust
let text = if let Some(sess) = &self.pending_kill {
    return frame.render_widget(
        Paragraph::new(format!(" kill {}? [y]es / [n]o", sess.address()))
            .style(Style::default().fg(Color::Red)),
        area,
    );
} else if ... // unchanged branches
```

(Keep the other footer branches exactly as-is. `Color` and `Style` are already imported.)

- [ ] **Step 4: Verify GREEN + commit**

Run: `cargo.exe test --lib`; expected 284 passed.
```
git add src/ui/switcher.rs
git commit -m "feat: render kill confirmation in red"
```

---

## Task 2: Level-aware rename / kill (#4, #5 scope)

**Files:**
- Modify: `src/mux.rs` (add `kill_window`, `rename_window` builders + tests)
- Modify: `src/ui/switcher.rs` (`Ops` trait, `PendingOp`, `open_input(Rename)`, `arm_kill`, `run_op`, `apply_op_result`)
- Modify: `src/env.rs` (`EnvOps` impls)
- Modify: `src/manage.rs` (window kill/rename helpers, mirroring the session ones)
- Test: `src/mux.rs`, `src/ui/switcher.rs`

**Interfaces produced:**
- `mux::kill_window(bin, target) -> Vec<String>` (`<bin> kill-window -t <session:window>`)
- `mux::rename_window(bin, target, new_name) -> Vec<String>` (`<bin> rename-window -t <session:window> <new_name>`)
- `Ops::kill_window(&self, source, target) -> Result<()>` and `Ops::rename_window(&self, source, target, new_name) -> Result<()>`
- `PendingOp::KillWindow { source, target }`, `PendingOp::RenameWindow { source, target, new_name }`

Behaviour: on a SESSION row, rename/kill act on the session (existing `Rename`/`Kill`). On a WINDOW row, they act on that window (`session:window` target). On a host row, both flash "cannot rename/kill a host" and no-op. The kill confirm footer (Task 1, red) shows the window target on a window row.

- [ ] **Step 1: mux builders — failing test**

```rust
#[test]
fn kill_and_rename_window_argv() {
    assert_eq!(kill_window("tmux", "api:2"), sv(&["tmux", "kill-window", "-t", "api:2"]));
    assert_eq!(rename_window("tmux", "api:2", "logs"),
        sv(&["tmux", "rename-window", "-t", "api:2", "logs"]));
}
```
Run: `cargo.exe test --lib kill_and_rename_window_argv` → FAIL (not defined).

- [ ] **Step 2: Implement the builders**

In `src/mux.rs`, mirror `kill_session`/`rename_session`:
```rust
/// Kills the window `target` (`session:window`).
pub fn kill_window(bin: &str, target: &str) -> Vec<String> {
    argv(&[bin, "kill-window", "-t", target])
}
/// Renames the window `target` (`session:window`) to `new_name`.
pub fn rename_window(bin: &str, target: &str, new_name: &str) -> Vec<String> {
    argv(&[bin, "rename-window", "-t", target, new_name])
}
```
Verify GREEN.

- [ ] **Step 3: manage.rs helpers**

In `src/manage.rs`, add `kill_window(src, target)` and `rename_window(src, target, new_name)` mirroring the existing `kill`/`rename` (they call `src.run(&mux::kill_window(&src.binary, target))` etc.). Match the existing error handling in that file.

- [ ] **Step 4: Ops trait + EnvOps**

Add to `Ops` (in `switcher.rs`):
```rust
async fn kill_window(&self, source: &str, target: &str) -> anyhow::Result<()>;
async fn rename_window(&self, source: &str, target: &str, new_name: &str) -> anyhow::Result<()>;
```
Implement in `EnvOps` (`env.rs`) using `manage::kill_window`/`rename_window` with `with_timeout(DETAIL_TIMEOUT, ...)`. Update the test `RecordOps` mock in `switcher.rs` to record window kill/rename (add fields).

- [ ] **Step 5: PendingOp + run_op + level-aware open — failing test**

```rust
#[tokio::test]
async fn kill_on_window_row_targets_the_window() {
    let mut h = Harness::new(sample());
    h.key(KeyCode::Home).await;   // local host
    h.key(KeyCode::Right).await;  // → editor (session)
    h.key(KeyCode::Right).await;  // → editor's first window (window 1)
    h.ch('x').await;              // arm kill
    h.ch('y').await;              // confirm
    let op = h.sw.take_pending_op().expect("kill window queued");
    assert!(matches!(op, PendingOp::KillWindow { ref target, .. } if target == "editor:1"));
}
```
Run → FAIL (no `KillWindow`).

- [ ] **Step 6: Implement level-aware kill/rename**

- Add `PendingOp::KillWindow { source, target }`, `PendingOp::RenameWindow { source, target, new_name }`.
- `arm_kill`: if the current row is a `RowRef::Window { sess, window }`, set a `pending_kill_window: Option<(String, String)>` (source, `session:window`) instead of `pending_kill`; the confirm footer renders `kill <source>/<target>?` red. `resolve_kill` queues `KillWindow`. On a session row, keep current `pending_kill` → `Kill`.
- `open_input(Rename)`: on a window row, open the rename input pre-filled with the window name, capturing the `session:window` target; Enter queues `RenameWindow`. On a session row, keep current session rename.
- `run_op`: handle the two new variants → `ops.kill_window`/`ops.rename_window`, returning `OpResult::PanesRefreshed` (re-fetch the session's panes so the tree updates) or a dedicated result. Simplest: after a window kill/rename, return `OpResult::PanesRefreshed { address, panes }` for the parent session so `apply_panes` redraws its subtree.
- `apply_op_result`: `PanesRefreshed` already handled.

Keep the host-row case: `open_new` already refuses unreachable hosts; for rename/kill on a host row, flash "cannot rename/kill a host" and return.

- [ ] **Step 7: Verify GREEN + commit**

Run `cargo.exe test --lib` (all pass) + clippy.
```
git add src/mux.rs src/manage.rs src/env.rs src/ui/switcher.rs
git commit -m "feat: level-aware rename/kill for windows"
```

---

## Task 3: Focus keys — Enter in, prefix-Esc out (#6)

**Files:**
- Modify: `src/cockpit.rs` (`handle_tree_bytes` focus-in keys; help/footer text)
- Modify: `src/proxy/input.rs` (`TermInput` — keep prefix-Esc as the primary focus-out)
- Test: `src/proxy/input.rs`, `src/cockpit.rs`

Current state already focuses the terminal on Enter (and Tab) and returns to the tree on `prefix Left|Right|Tab|Esc`. Change: focus-IN is **Enter only** (drop Tab as a focus-in key, since the unified model reserves Tab-free navigation); focus-OUT keeps **prefix-Esc** as the documented key (prefix-Tab/Left stay as aliases — do not remove, they are harmless and already tested).

- [ ] **Step 1: Failing test (focus-in is Enter, not Tab)**

In `cockpit.rs` tests, `handle_tree_bytes` returns `(focus_terminal, quit)`. Add:
```rust
#[test]
fn enter_focuses_terminal_tab_does_not() {
    // Build the minimal args for handle_tree_bytes (see existing call sites for the
    // exact parameter list); feed Enter then assert focus_terminal=true, feed Tab
    // and assert focus_terminal=false.
}
```
(Model the harness on the existing `handle_tree_bytes` call in `run_cockpit`. If wiring a direct test is too heavy, instead unit-test a small extracted helper `fn is_focus_in(key: KeyCode) -> bool { matches!(key, KeyCode::Enter) }` and use it in `handle_tree_bytes`.)

- [ ] **Step 2: Implement**

In `handle_tree_bytes`, change the focus-in match from `KeyCode::Enter | KeyCode::Char('\t')` to `KeyCode::Enter` only. Leave the prefix-armed block (`Tab → focus_terminal`) as-is OR drop Tab there too for consistency (prefer dropping: focus-in is Enter, focus-out is prefix-Esc). Update the help text (`render_help`) and footer hints (`render_footer`) so they read `Enter focus pane` and `C-g Esc back to the tree` (remove the `→`/`Tab` focus claims — `→` descends now).

- [ ] **Step 3: TermInput — confirm prefix-Esc → tree**

`TermInput::feed` already returns `FocusTree` for `prefix` + `Esc`/Left/Right/Tab. Add/confirm a test `prefix_then_esc_focuses_tree` (mirror `prefix_then_left_or_right_or_esc_focuses_tree`). No code change expected; this just locks the behaviour.

- [ ] **Step 4: Verify GREEN + commit**

```
git add src/cockpit.rs src/proxy/input.rs
git commit -m "feat: focus the terminal on Enter, return on prefix-Esc"
```

---

## Task 4: Unified passthrough selection follow (#8 — replaces the partial #3 impl)

**Files:**
- Modify: `src/cockpit.rs` (loop-level follow; remove scattered follow calls)
- Test: `src/ui/switcher.rs` (`select_active_window` already covered), `src/cockpit.rs`

Invariant: **while in Passthrough (terminal focus), the tree selection always equals the displayed session's active window — no exceptions.** Replace the scattered `select_active_window()` calls (focus-toggle, `LocalEnum::Panes`) and the `Focus`-handler `select_window` call with ONE call at the loop top.

- [ ] **Step 1: Remove the scattered follow calls**

- In `handle`-stdin focus-toggle branches: remove the two `switcher.select_active_window();` calls (keep `app.toggle();`).
- In the `LocalEnum::Panes` arm: remove the `if !app.is_overlay() { switcher.select_active_window(); }` block.
- In `handle_host_event` `HostEvent::Focus`: remove the `if !overlay { switcher.select_window(...) }` call. KEEP `switcher.set_active_window(&host, &session, window)` (it updates the active-window marker + the cached flag that `select_active_window` reads). The `overlay` parameter to `handle_host_event` becomes unused — remove it from the signature and call sites.

- [ ] **Step 2: Add the single loop-top follow**

At the top of the `loop {` body in `run_cockpit`, AFTER `switcher.set_spinner_frame(...)` and BEFORE committing `new_sel`:
```rust
// In passthrough the user no longer drives the tree cursor (stdin goes to the
// PTY), so the tree selection tracks the displayed session's active window — always.
// select_active_window is idempotent (no move when already on the active window or
// when the session's panes are unknown), so calling it each iteration is cheap.
if !app.is_overlay() {
    switcher.select_active_window();
}
```

- [ ] **Step 3: Verify**

`select_active_window` is already unit-tested (`select_active_window_moves_to_cached_active_window`). Run `cargo.exe test --lib` (all pass) + clippy (no unused `overlay`).

Manual gate (real terminal): on a session row, press Enter → cursor jumps to the active window row; switch windows inside the mux (e.g. `prefix n`) → the tree selection follows. Add this to the human visual-gate checklist.

- [ ] **Step 4: Commit**

```
git add src/cockpit.rs
git commit -m "feat: tree selection always follows the displayed window in passthrough"
```

---

## Task 5: Selection transition when the selected node is removed (#11)

**Files:**
- Modify: `src/ui/switcher.rs` (`restore_focus` fallback)
- Test: `src/ui/switcher.rs`

When the focused node disappears across a rebuild (killed window/session, or an external removal via refetch), move the cursor to the PREVIOUS sibling at the same level; if it was the first at that level, move to the PARENT. There is always a parent (the local host never disappears).

- [ ] **Step 1: Failing test**

```rust
#[tokio::test]
async fn removed_window_selection_falls_to_previous_sibling_then_parent() {
    // two windows under jup/api; cursor on window 1. Remove window 1 → cursor to
    // window 0 (previous sibling). Remove window 0 (now the only/topmost) → cursor
    // to the session row (parent).
    let mut sw = Switcher::new(two_window_scan());
    sw.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE)); // → window 0
    sw.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));  // ↓ window 1
    assert!(matches!(sw.current_ref(), Some(RowRef::Window { window: 1, .. })));
    // simulate window 1 removed: re-apply panes without window 1.
    sw.apply_panes("jup/api".into(), vec![
        win(0, "w0", true, vec![pane(0, true, "bash")]),
    ]);
    assert!(matches!(sw.current_ref(), Some(RowRef::Window { window: 0, .. })),
        "removed window → previous sibling");
    // remove window 0 too (session now has no window rows): cursor to the session.
    sw.apply_panes("jup/api".into(), vec![]);
    assert!(matches!(sw.current_ref(), Some(RowRef::Session(s)) if s.name == "api"),
        "topmost removed → parent (session row)");
}
```
Run → FAIL (current `restore_focus` does nothing when the node is gone → cursor falls to the rebuild preselect, not the sibling/parent).

- [ ] **Step 2: Implement the fallback in `restore_focus`**

`restore_focus(&mut self, focus: Option<RowRef>)` currently: if `user_moved` and `row_matching(focus)` is `Some(i)`, select it. Add: when `user_moved` and the focus node is GONE, compute the fallback. Capture, BEFORE the rebuild, the focused node's `(indent, position-among-same-level-rows)` — pass it into `restore_focus`. Simplest: change callers to pass the pre-rebuild `selected` index and the focused `RowRef`; in `restore_focus`, if `row_matching` is `None`, scan the NEW rows for the nearest selectable row at the same indent whose original position is just before the lost one; if none at that indent, pick the parent (nearest preceding row at a shallower indent). Implement a helper:

```rust
/// The row to land on after the focused node vanished: the previous selectable
/// sibling at `indent`, else the nearest preceding selectable row at a shallower
/// indent (the parent). Operates on the freshly rebuilt `self.rows`.
fn fallback_after_removal(&self, indent: usize, prior_selected: usize) -> Option<usize> {
    // previous sibling at the same indent, at a row index < prior_selected
    let prev_sibling = self.rows[..prior_selected.min(self.rows.len())]
        .iter().enumerate().rev()
        .find(|(_, r)| r.indent == indent && r.selectable())
        .map(|(i, _)| i);
    prev_sibling.or_else(|| {
        // parent: nearest preceding selectable row at a shallower indent
        self.rows[..prior_selected.min(self.rows.len())]
            .iter().enumerate().rev()
            .find(|(_, r)| r.indent < indent && r.selectable())
            .map(|(i, _)| i)
    })
}
```
Thread the old `(indent, prior_selected)` from each `restore_focus` caller (they already capture `focus` via `current_ref()`; also capture `self.selected` and `self.rows[selected].indent` before the rebuild). When `row_matching` is `None`, use `fallback_after_removal`; set `user_moved` stays true.

- [ ] **Step 3: Verify GREEN + commit**

Run `cargo.exe test --lib` + clippy.
```
git add src/ui/switcher.rs
git commit -m "feat: move selection to prev sibling or parent when a node is removed"
```

---

## Task 6: Show the mux cursor in passthrough (#9)

**Files:**
- Modify: `src/ui/switcher.rs` (`render`, `render_terminal_view`)
- Modify: `src/proxy/screen.rs` (`Grid` already exposes `cursor()` and `hide_cursor()`)
- Test: `src/ui/switcher.rs`

The grid renders cell content but never positions the terminal cursor, so when focused on the mux the cursor is invisible. When `terminal_focused` and the grid is present and not `hide_cursor()`, set the frame cursor to the grid's cursor offset inside the terminal-view area.

- [ ] **Step 1: Failing test**

`Frame::set_cursor_position` is not observable on `TestBackend` via the buffer. Instead extract the placement as a pure function and test that:
```rust
#[test]
fn mux_cursor_maps_into_terminal_view_area() {
    // grid cursor (col=3,row=2); terminal-view area at x=49,y=0 → frame cursor (52,2).
    let pos = terminal_cursor_pos(ratatui::layout::Rect::new(49, 0, 80, 24), (3, 2));
    assert_eq!(pos, ratatui::layout::Position { x: 52, y: 2 });
    // clamped to the area.
    let pos = terminal_cursor_pos(ratatui::layout::Rect::new(49, 0, 4, 2), (100, 100));
    assert_eq!(pos, ratatui::layout::Position { x: 52, y: 1 });
}
```
Run → FAIL (no `terminal_cursor_pos`).

- [ ] **Step 2: Implement**

Add the pure helper in `switcher.rs`:
```rust
fn terminal_cursor_pos(area: ratatui::layout::Rect, cursor: (u16, u16)) -> ratatui::layout::Position {
    let (col, row) = cursor;
    ratatui::layout::Position {
        x: (area.x + col).min(area.x + area.width.saturating_sub(1)),
        y: (area.y + row).min(area.y + area.height.saturating_sub(1)),
    }
}
```
In `render`, after `render_terminal_view`, when `terminal_focused` and a grid is present and `!grid.hide_cursor()`, call `frame.set_cursor_position(terminal_cursor_pos(terminal_area, grid.cursor()))`. Thread `terminal_focused` + the terminal-view `Rect` into the call (compute the same `mid[2]` Rect used by `render_terminal_view`). Pass the grid to `render` (already passed).

- [ ] **Step 3: Verify GREEN + commit**

Run `cargo.exe test --lib` + clippy. Add to the human gate: "in passthrough the mux cursor is visible and tracks typing."
```
git add src/ui/switcher.rs
git commit -m "feat: show the mux cursor when the terminal is focused"
```

---

## Task 7: Resizable tree width — prefix-Ctrl+←/→ (#7)

**Files:**
- Modify: `src/cockpit.rs` (tree-width state, prefix-armed Ctrl+←/→ handling, pass width to render + `terminal_view_size`, resize attachments)
- Modify: `src/ui/switcher.rs` (`render` takes a tree width; `TREE_WIDTH` becomes the default)
- Test: `src/cockpit.rs` (pure width-adjust helper), `src/ui/switcher.rs`

`TREE_WIDTH` (48) is currently a const used by `switcher::render`'s `Layout` and by `cockpit::terminal_view_size`. Make the width a runtime value owned by the cockpit, adjustable ±1 (clamped, e.g. 20..=100), via `prefix` then Ctrl+←/→ (and prefix then Ctrl+h/l). On change, resize all attachments + control clients (like a terminal resize).

- [ ] **Step 1: Pure clamp helper — failing test**

In `cockpit.rs`:
```rust
#[test]
fn tree_width_adjust_clamps() {
    assert_eq!(adjust_tree_width(48, 1), 49);
    assert_eq!(adjust_tree_width(48, -1), 47);
    assert_eq!(adjust_tree_width(20, -1), 20, "clamped at min");
    assert_eq!(adjust_tree_width(100, 1), 100, "clamped at max");
}
```
Run → FAIL.

- [ ] **Step 2: Implement the helper + constants**

```rust
const TREE_WIDTH_MIN: u16 = 20;
const TREE_WIDTH_MAX: u16 = 100;
fn adjust_tree_width(w: u16, delta: i32) -> u16 {
    (w as i32 + delta).clamp(TREE_WIDTH_MIN as i32, TREE_WIDTH_MAX as i32) as u16
}
```

- [ ] **Step 3: Thread the width through render + sizing**

- Add `tree_width: u16` to `run_cockpit` (init `crate::ui::switcher::TREE_WIDTH`).
- Change `Switcher::render(&mut self, frame, grid, terminal_focused, tree_width: u16)` and use `tree_width` in the horizontal `Layout` (replace the `TREE_WIDTH` constant use). Update all `render`/`dump_overlay` call sites (cockpit draw arm, the `Cmd::Dump` arm, `ui/run.rs::dump_overlay`, and the switcher test `Harness`) to pass a width (`TREE_WIDTH` default in tests).
- Change `cockpit::terminal_view_size(cols, body_rows, tree_width)` to take the width; update its callers + its tests.

- [ ] **Step 4: Handle the resize key (prefix-armed)**

In `handle_tree_bytes`, the `*tree_armed` block currently handles Tab/q. Add: a Ctrl+Left/Right (and Ctrl+h/l) decoded key adjusts the width. Because `handle_tree_bytes` does not own the cockpit's `tree_width`/attachments, return a width-delta signal (extend the return tuple, e.g. `(focus_terminal, quit, width_delta: i32)`), and in the stdin arm apply `tree_width = adjust_tree_width(tree_width, delta)`, then `registry.resize_all` + `mgr.resize_all` with the new `terminal_view_size`, and set `dirty = true`. The decoder yields `KeyEvent { code: Left/Right, modifiers: CONTROL }` for `ESC[1;5D`/`ESC[1;5C`; in the armed block match those (and `Char('h')`/`Char('l')` with CONTROL, plus bare `Char('h')`/`Char('l')` after prefix as a fallback for terminals that send C-h=0x08).

- [ ] **Step 5: Verify GREEN + commit**

Run `cargo.exe test --lib` + clippy. Human gate: "prefix then Ctrl+←/→ narrows/widens the tree one column; the mux pane and its PTY resize to match (no wrap artifacts)."
```
git add src/cockpit.rs src/ui/switcher.rs src/ui/run.rs
git commit -m "feat: resize the tree column with prefix Ctrl+arrows"
```

---

## Task 8: Layout — footer/input under the tree column, mux full height (#12)

**Files:**
- Modify: `src/ui/switcher.rs` (`render` layout)
- Test: `src/ui/switcher.rs`

Currently the body is a vertical split `[Min(0) body, Length(input) input, Length(1) footer]`, and `body` is the horizontal `[tree | divider | terminal]`. So the input + footer span the FULL width below both panes. Change so the **left column** stacks `[tree, input, footer]` and the **terminal occupies the full height** on the right.

New layout:
```
top horizontal: [ Length(tree_width) left | Length(1) divider | Min(0) terminal ]
left vertical:  [ Min(0) tree | Length(input_h) input | Length(1) footer ]
```
The terminal `Rect` is the full-height right column; the tree/input/footer live in the left column.

- [ ] **Step 1: Failing test**

```rust
#[tokio::test]
async fn footer_and_input_live_under_the_tree_only() {
    let mut h = Harness::new(sample()); // 80x24 TestBackend
    h.ch('/').await; // open the filter input
    let buf = h.buffer();
    // The terminal view starts at x = tree_width+1; the footer/input rows in that
    // column must be blank (the mux spans full height there), while the left column's
    // last row carries footer text.
    let term_x = crate::ui::switcher::TREE_WIDTH + 1;
    let last = buf.area.height - 1;
    // left column footer has content:
    assert!((0..crate::ui::switcher::TREE_WIDTH).any(|x| buf[(x, last)].symbol() != " "),
        "footer renders in the tree column");
    // the terminal column's bottom rows are part of the grid area, not the footer:
    // (assert the divider/grid occupies the full height — no global footer there)
}
```
Adjust assertions to your `Harness::buffer()` accessor. The key checks: footer/input text appears only in `x < tree_width`; the divider rule spans the full body height.

Run → FAIL (footer currently spans full width).

- [ ] **Step 2: Implement the new layout in `render`**

```rust
let area = frame.area();
let cols = Layout::horizontal([
    Constraint::Length(tree_width),
    Constraint::Length(1),
    Constraint::Min(0),
]).split(area);
let input_h = if self.input.is_some() { 1 } else { 0 };
let left = Layout::vertical([
    Constraint::Min(0),
    Constraint::Length(input_h),
    Constraint::Length(1),
]).split(cols[0]);
self.render_tree(frame, left[0]);
if input_h == 1 { self.render_input(frame, left[1]); }
self.render_footer(frame, left[2]);
self.render_divider(frame, cols[1], terminal_focused);
self.render_terminal_view(frame, cols[2], grid);
if self.show_help { self.render_help(frame, area); }
```
Note: the divider now spans the full height (`cols[1]` is full-height) — keep `render_divider` filling its area's height. `render_terminal_view` gets the full-height right column.

- [ ] **Step 3: Verify GREEN + commit**

Run `cargo.exe test --lib` + clippy. The Task-6 cursor area and Task-7 width plumbing must use this same terminal Rect (`cols[2]`). Human gate: "filter/rename/footer show under the tree only; the mux fills the full height on the right."
```
git add src/ui/switcher.rs
git commit -m "feat: confine footer/input to the tree column, mux spans full height"
```

---

## Task 9: Forward mouse events to the mux (#10) — LARGEST, do last

**Files:**
- Modify: `src/proxy/term.rs` (`TermGuard` — enable mouse capture)
- Modify: `src/cockpit.rs` (decode mouse from stdin in passthrough; translate to grid coords; forward to the PTY)
- Modify: `src/proxy/input.rs` or a new `src/proxy/mouse.rs` (SGR mouse encode/decode)
- Modify: `src/proxy/run.rs` (`Attachment::input` already forwards bytes — reuse)
- Test: the pure encode/translate functions

Goal: tmux/psmux mouse interactions (status-bar clicks, pane select, scroll) work inside the displayed mux. Trade-off: when mouse capture is on, native Windows-terminal text selection in the mux area is replaced by mux mouse behaviour. Scope: **capture + forward ONLY in Passthrough**; in Overlay, leave the mouse to the tree's existing `mouse_select`/`mouse_scroll` (the tree already handles clicks) and to native selection where it falls outside the tree.

**Design:**
1. `TermGuard::enter` enables mouse capture (`crossterm::event::EnableMouseCapture`) and disables it on drop. Confirm xmux already reads raw stdin (it does); mouse events arrive as `ESC[<...M/m` (SGR) byte sequences.
2. In the stdin handler, when in Passthrough, decode SGR mouse sequences. For a mouse event whose cell is inside the terminal-view `Rect` (`cols[2]` from Task 8), translate the screen `(col,row)` to grid-local `(col - area.x, row - area.y)` and re-encode an SGR mouse event at the grid-local coordinates, then forward to the attachment's PTY (`registry.input(key, bytes)`). Events outside the terminal area (in the tree column) are ignored in passthrough (or switch focus — keep simple: ignore).
3. The mux must have mouse mode on for the child to interpret them (tmux `set -g mouse on`); xmux only forwards — it does not enable mux mouse mode. Document this in the help/README.

- [ ] **Step 1: SGR mouse parse/encode — failing tests**

Add pure functions (new `src/proxy/mouse.rs`, add `pub mod mouse;` to `src/proxy/mod.rs`):
```rust
/// A parsed SGR mouse event: button code, 1-based (col,row), press/release.
pub struct MouseEvent { pub cb: u16, pub col: u16, pub row: u16, pub pressed: bool }
/// Parses one SGR mouse sequence `ESC [ < cb ; col ; row (M|m)`. Returns the event
/// and the total byte length consumed, or None if `data` is not a complete SGR mouse seq.
pub fn parse_sgr_mouse(data: &[u8]) -> Option<(MouseEvent, usize)> { ... }
/// Re-encodes an SGR mouse event at new 1-based (col,row).
pub fn encode_sgr_mouse(ev: &MouseEvent, col: u16, row: u16) -> Vec<u8> { ... }
```
Tests: round-trip `ESC[<0;10;5M` → cb=0,col=10,row=5,pressed=true; re-encode at (3,2) → `ESC[<0;3;2M`; release form `m` → pressed=false; partial input → None.

- [ ] **Step 2: Implement parse/encode** (pure byte work; mirror the DSR parsing style in `proxy/run.rs`). Verify GREEN.

- [ ] **Step 3: Coordinate translation — failing test**

```rust
/// Maps a screen cell to grid-local coords if inside `area`, else None.
fn to_grid_local(area: Rect, col: u16, row: u16) -> Option<(u16, u16)> { ... }
```
Test inside/outside the area. Implement, verify.

- [ ] **Step 4: Wire capture + forwarding**

- `TermGuard`: enable/disable mouse capture (guard against double-enable). Add a test only if the guard exposes a testable seam; otherwise this is a human-gate item.
- Stdin handler (Passthrough branch): before forwarding raw bytes, scan for SGR mouse sequences; for each inside the terminal `Rect`, translate + re-encode + forward to the attachment; pass non-mouse bytes through `TermInput` as today. Keep the terminal `Rect` available in the loop (store the last-rendered terminal area, or recompute from `cols`, `tree_width`, body size).

- [ ] **Step 5: Verify GREEN + commit**

Run `cargo.exe test --lib` + clippy. Human gate (real terminal, mux with `mouse on`): "click the tmux status bar / select a pane / scroll inside the mux works through xmux; native selection no longer fires inside the mux area in passthrough."
```
git add src/proxy/term.rs src/proxy/mouse.rs src/proxy/mod.rs src/cockpit.rs
git commit -m "feat: forward mouse events to the mux in passthrough"
```

---

## After all tasks

- [ ] Run the full suite + clippy: `cargo.exe test --lib && cargo.exe clippy --all-targets`.
- [ ] Run `/simplify` over the cumulative diff (reuse/dedupe/altitude only — no new behaviour, no bug-hunting).
- [ ] Run `compound-engineering:ce-code-review` (or `superpowers:verification-before-completion`) over the diff before declaring done.
- [ ] Update the human visual-gate checklist in `src/cockpit.rs` tests with the new behaviours (nav model, focus keys, passthrough follow, cursor, tree-width, layout, mouse).

## Self-Review notes (gaps to watch)

- Tasks 6, 7, 8 all touch `Switcher::render` and the terminal `Rect`. Do Task 8 (layout) BEFORE Tasks 6/7 if convenient, or keep the terminal `Rect` (`cols[2]`) as the single source the cursor (Task 6) and mouse (Task 9) translate against.
- The `overlay` parameter removed from `handle_host_event` (Task 4) must be removed from ALL call sites (the main loop drains host events in two places).
- `render` signature changes (Tasks 7 add `tree_width`) ripple to `ui/run.rs::dump_overlay` and the switcher test `Harness` — update them in the same task.
- Mouse capture (Task 9) interacts with bracketed paste already handled by `TermInput`; keep paste handling intact.
