---
title: TUI preview by capture-pane mirrors itself when the target pane runs the TUI
date: 2026-06-17
category: ui-bugs
module: xmux
problem_type: ui_bug
component: tooling
symptoms:
  - "Preview pane shows the switcher nested inside its own preview, repeating to screen depth"
  - "The preview never settles — it re-renders every poll tick (1s) with one more level of nesting"
  - "Triggered when the focused session's active pane is itself running xmux"
root_cause: logic_error
resolution_type: code_fix
severity: medium
tags: [tui, ratatui, capture-pane, preview, recursion, tmux, psmux]
related_components: ["ui/switcher", "ui/run"]
---

# TUI preview by capture-pane mirrors itself when the target pane runs the TUI

## Problem
xmux's right-hand preview pane shows a live `capture-pane` of the focused session's active pane. When that pane is itself running xmux, the capture returns xmux's own screen — so the preview draws the whole switcher (tree + preview) inside its own preview, recursively, refreshing every poll tick and never stabilizing.

## Symptoms
- The preview pane renders `xmux / cross-environment MUX manager / ┌ Hosts · Sessions… ┐` containing another `Preview ·` box containing another, to screen depth (a mirror tunnel).
- It re-renders every 1s (each capture differs because the inner preview just updated), so it looks like an infinite render loop rather than a static nesting.
- Reproduces whenever the focused target's active pane command is `xmux` — most acutely the session xmux itself occupies.

## What Didn't Work
- Reasoning that `display-popup` (a client-side overlay in tmux 3.2+) is never in a pane grid, so `capture-pane` "can't" capture it. True for the popup overlay — but the bug is not the popup. It is any pane whose foreground process is xmux (e.g. `xmux` run directly in a pane as the switcher). Capturing that pane returns the live alternate-screen = xmux. The popup case (pane runs a shell under the overlay) is fine and must keep working.
- Identity-only suppression ("don't preview my own session") would be wrong: under a popup the self-session's active pane is the user's shell, and that preview (the real underlying screen) is exactly what the user wants to see. Suppressing by identity hides a good preview.

## Solution
Discriminate by the **active pane's `pane_current_command`**, which xmux already fetches for the tree. If the pane the target would capture is running xmux, skip the capture and show a note instead of recursing.

- `command_is_xmux(cmd)` — matches `xmux` / `xmux.exe`, case-insensitive.
- `Switcher::focused_pane_command(&RowRef)` — resolves the active pane of the focused session/window target from the cached pane map.
- `on_focus_changed` sets `preview_self = true` for such a target, clears the loading/reconnecting dialog, and puts a `PREVIEW_SELF_NOTE` in the preview text (no `poll_kick`).
- `Switcher::preview_capturable()` returns `false` when there is no target or `preview_self` is set.
- `ui/run.rs` `spawn_capture` early-returns unless `switcher.preview_capturable()`, so neither the immediate fetch nor the 1s poll captures a self-overlay.

Verified live (psmux = tmux 3.3.6 on Windows): an isolated session running xmux shows the note in `capture-pane`; navigating to a session whose pane runs a normal command still shows that pane's real content.

## Why This Works
The true discriminator is "is the pane I am about to capture running this TUI?", not "is this my session?" `pane_current_command` answers exactly that with data already in hand — no extra mux call, no fragile content-sniffing of the captured text, no coupling to the header string. Popup-over-a-shell keeps capturing (shows the original screen); pane-running-xmux is suppressed (breaks the recursion).

## Prevention
- Any TUI that previews terminal content by capturing panes/windows must guard the self-reference: a pane running the previewing program will mirror it. Gate the capture on the target's foreground command, not on window/session identity.
- Headless regression tests (ratatui `TestBackend`): focus a target whose active pane command is the TUI binary → assert the capture is suppressed and a note renders; focus a normal pane → assert it stays capturable. See `preview_suppressed_when_focused_pane_runs_xmux` and `preview_captures_when_focused_pane_is_not_xmux` in `src/ui/switcher.rs`.

## Related Issues
- [[rust-tui-ipc-port-gotchas]] — the ratatui port and the `xmux ctl dump` headless-verification harness used to reproduce and confirm this fix.
