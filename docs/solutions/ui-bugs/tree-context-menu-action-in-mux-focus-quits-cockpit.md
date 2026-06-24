---
title: Tree context-menu kill in mux focus switched the display and quit the cockpit
date: 2026-06-24
category: ui-bugs
module: cockpit / switcher (xmux TUI)
problem_type: ui_bug
component: tooling
symptoms:
  - "Right-click 'kill' on a background session while the mux pane is focused quit the entire xmux cockpit"
  - "A menu-opened rename input or kill confirm received no keys (keystrokes went to the mux PTY instead)"
  - "Acting on a non-displayed row via the menu silently switched the displayed/attached session"
root_cause: logic_error
resolution_type: code_fix
severity: high
tags: [context-menu, mouse, focus-state, modal, detach-to-quit, tui]
---

# Tree context-menu kill in mux focus switched the display and quit the cockpit

## Problem
A new tree-pane right-click context menu could run its actions in either focus state (tree-focused/overlay or mux-focused/passthrough). In passthrough, a menu action that moved the tree cursor made the live display follow to the acted-on session — and confirming a "kill" there killed the now-displayed session's PTY, tripping detach-to-quit and exiting the whole cockpit.

## Symptoms
- Right-click "kill" on a *background* session while working in the mux pane → cockpit quits.
- Menu-opened "rename" input / "kill" y-n confirm never received keystrokes (they went to the mux child).
- Any menu action on a non-displayed row silently yanked the display to that session.

## What Didn't Work
- **352 passing unit tests + clippy clean did not catch any of it.** The switcher methods (`menu_open`/`menu_release`) were tested in isolation and never asserted the *cockpit focus state* or the post-release display/attach behavior. The cockpit's single-threaded mouse loop has no unit tests (the codebase only unit-tests pure helpers), so the cross-component interaction was invisible to the suite. An adversarial review (constructing the kill→quit scenario by hand) found it; the tests could not.

## Solution
Make the menu always operate in **tree (overlay) focus**: when a right-press opens the menu while the mux pane has focus, take tree focus immediately.

```rust
// src/cockpit.rs — right-press open trigger
if is_right_press && in_mux.is_none() && switcher.menu_open(col0, ev.row.saturating_sub(1)) {
    // The menu's actions (rename input, kill confirm) are driven by the tree's
    // keyboard path, and a kill confirmed in the mux-focus state would quit the
    // cockpit when the killed PTY exits. So a menu always operates in tree focus.
    if !app.is_overlay() {
        app.toggle();
    }
    dirty = true;
    i += len;
    continue;
}
```

One toggle fixes both failure modes at once. Also added `ensure_current_host(...)` to the menu's "open" (FocusTerminal) arm so it matches the left-click select path.

## Why This Works
The quit came from `detach-to-quit`: the cockpit quits when the *displayed* session's PTY exits, and the displayed-attach id is only computed when `!app.is_overlay()` (mux focused). In tree focus that id is `None`, so killing any session can never quit. The input/confirm stranding came from the cockpit routing keys to the PTY in passthrough and only to the switcher in overlay — so an inline input opened from a passthrough menu could not receive keys. Forcing overlay routes both the keys and the kill through the tree's keyboard path, where the existing modals already work. Right-clicking a sidebar to focus it is also standard, expected UX.

## Prevention
- When adding a **new input path** (mouse gesture, command, API) that reuses existing modal flows, list every implicit assumption those modals rely on (here: "inputs only open in tree focus, so only tree focus routes keys to them") and verify the new path satisfies them.
- For UI features whose correctness depends on **app-level state the unit under test doesn't own** (focus, attach registry), a green component suite is not evidence of integration correctness. Add an adversarial review that constructs the cross-component scenario, or an integration test over the event loop.
- Prefer the single root-cause fix (one focus toggle) over guarding each symptom (separate fixes for quit, for input routing, for confirm routing).

## Related Issues
- Plan: `docs/superpowers/plans/2026-06-24-tree-right-click-context-menu.md`
- Reaffirms the recurring xmux lesson that adversarial/code review catches single-threaded-loop and state-interaction bugs the test suite misses.
