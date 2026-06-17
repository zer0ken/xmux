---
title: Render-first TUI — paint from a local scan, stream remote hosts in
category: architecture-patterns
module: xmux
problem_type: architecture_pattern
component: tooling
severity: medium
date: 2026-06-17
applies_when:
  - "a TUI blocks its first paint on a slow, fan-out data gather (ssh / network)"
  - "the data sources split into fast-local and slow-remote tiers"
  - "the slow tier dominates time-to-first-frame and is not needed to paint"
tags:
  - tui
  - ratatui
  - latency
  - render-first
  - ssh
  - tokio
related_components:
  - env
  - ui/run
  - ui/switcher
---

# Render-first TUI — paint from a local scan, stream remote hosts in

## Context

`xmux popup` / `xmux` (home) built the switcher from `Env::deep_scan`: a
`list-sessions` across every source — local mux **and every ssh host** — followed
by a `list-panes` for every reachable session. The whole gather ran *before* the
first `terminal.draw`, so the UI showed nothing until it finished. With remote
hosts that is dominated by ssh handshakes (Windows OpenSSH has no ControlMaster,
so each host is a full connect), pushing time-to-first-frame to ~2.5s.

The slow tier (ssh hosts, pane detail) is not needed to paint the first frame —
the local sessions are. Blocking on it is pure latency the user can't act on.

## Guidance

Split the scan into a **fast first-paint tier** and a **background full tier**,
and merge the full tier into the live tree:

1. **Seed from a local-only scan.** `Env::local_scan` probes only non-remote
   sources, `list-sessions` only (no `list-panes`). The switcher is constructed
   from that, so the first frame paints as soon as the *local* probe returns.
2. **Kick the full scan in the background, once, on loop start.** The event loop
   spawns `Ops::refresh()` (the existing full `deep_scan`) and feeds the result
   back into its own command channel as a new `Cmd::Scan(Scan)` — the same
   unified channel that already carries keys, mouse, and preview captures.
3. **Merge without blocking.** `Switcher::apply_scan` replaces `groups`/`panes`,
   rebuilds the row model, and **preserves the focused node** (host by source,
   session/window by address) so the cursor doesn't jump when remotes appear.
   `do_refresh` (the manual `r` re-scan) routes through the same method.

The background task is detached — the same pattern as the existing per-target
preview captures — and the process owns its own lifetime, so there is nothing to
join on quit.

## Why This Matters

- Time-to-first-frame dropped from **~2.4s → ~1.3s** on a box with 2 ssh hosts;
  remote hosts and pane detail now stream in by **~3.7s** *after* the paint
  instead of gating it.
- **Surprising floor (worth remembering):** the residual ~1.3s here is almost
  entirely the **local** `psmux list-sessions`, which measures **~1.1s** on this
  Windows box (tmux 3.3.6) — see [[rust-tui-ipc-port-gotchas]] for the local-mux
  context. The "first frame in tens of ms" goal assumes a fast local mux; when
  the local probe itself is slow, render-first removes the *remote* blocking but
  not the local floor. Pushing below that would need an even earlier empty paint
  with the local scan also streamed in — out of scope here, but the obvious next
  lever if the local mux stays slow.
- Reusing the existing `Cmd` channel + `Ops::refresh` means no new transport and
  no new lifecycle: the merge is just one more message variant.

## When to Apply

- Any TUI whose first paint waits on a fan-out gather where part of the data is
  cheap-and-local and part is expensive-and-remote.
- When the expensive tier can arrive late without breaking interaction (here the
  cursor-preserving `apply_scan` makes a late tree swap non-disruptive).

Do **not** reach for this when the gather is uniformly fast, or when the UI is
meaningless without the full data (then a loading state is more honest than a
half-populated tree).

## Examples

Seeding the switcher (`main.rs`):

```rust
// Paint from a fast local-only scan; the loop runs the full deep scan in the
// background and merges it.
let scan = env.local_scan().await;
run_switcher(scan, ops.clone(), control_path(&env)).await
```

Kicking the background scan once, on event-loop start (`ui/run.rs`):

```rust
{
    let ops = ops.clone();
    let tx = cmd_tx.clone();
    tokio::spawn(async move {
        if let Ok(scan) = ops.refresh().await {
            let _ = tx.send(Cmd::Scan(scan)).await;   // merged in the paint loop
        }
    });
}
```

Cursor-preserving merge (`ui/switcher.rs`):

```rust
pub fn apply_scan(&mut self, scan: Scan) {
    let focus = self.current_ref().cloned();
    self.groups = scan.groups;
    self.panes = scan.panes;
    self.rebuild();
    if let Some(i) = focus.and_then(|f| self.row_matching(&f)) {
        self.set_selected(i);   // keep the focused node if it survived the re-scan
    }
}
```

### Verifying time-to-first-frame headlessly

A ratatui/crossterm app that needs a console still renders under the agent's
Bash-tool console (no psmux pty needed). Drive it through xmux's own control
channel and time the first populated dump:

```bash
"$BIN" popup >run.log 2>&1 &          # control socket served once run_switcher starts
START=$(date +%s.%N)
# poll `xmux ctl dump` until the rendered header ("Hosts …") appears, record elapsed
```

For the *old* (blocking) build the control socket isn't served until `deep_scan`
returns, so the first successful `dump` already measures the blocking time;
for the render-first build the socket comes up after only `local_scan`. The poll
spawns one `xmux ctl` process per iteration (~40ms), so the figure is an upper
bound, not a precise floor — fine for a seconds-scale before/after.
