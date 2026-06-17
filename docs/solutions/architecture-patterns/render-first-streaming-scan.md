---
title: "Render-first TUI — paint host skeletons instantly, stream every element in"
category: architecture-patterns
module: xmux
problem_type: architecture_pattern
component: tooling
severity: medium
date: 2026-06-17
last_updated: 2026-06-17
applies_when:
  - "a TUI blocks its first paint on a fan-out data gather (ssh / network / a slow local probe)"
  - "the displayable structure (which hosts exist) is known cheaply, but the data under it is slow"
  - "elements arrive at very different speeds and one slow element should not gate the others"
tags:
  - tui
  - ratatui
  - latency
  - render-first
  - streaming
  - ssh
  - tokio
related_components:
  - env
  - ui/run
  - ui/switcher
---

# Render-first TUI — paint host skeletons instantly, stream every element in

## Context

`xmux` / `xmux popup` built the switcher from a scan that ran *before* the first
`terminal.draw`. An earlier pass narrowed that to a local-only `list-sessions`
seed with the full deep scan merged in afterward as one `Cmd::Scan` — but the
**local** `psmux list-sessions` is ~1.1s on this Windows box (tmux 3.3.6), so the
local probe *still* gated the first frame (~1.3s time-to-first-frame), and the
single coarse merge meant a slow host (or the panes phase) still gated when the
fuller tree appeared. There was no per-element progress: the tree looked empty
until a whole tier landed.

The displayable *structure* — which hosts exist — is known with zero probing
(the resolved source list: local + ssh-config aliases + config hosts). Only the
*data* under each host (its sessions, then each session's panes) is slow. Blocking
the paint on any of it is latency the user can't act on, and merging it all at
once hides the independent progress of fast vs slow hosts.

## Guidance

Separate **structure** (paint immediately) from **data** (stream each element in
independently), and give every element a visible state hint so the tree never
looks empty-or-broken without explanation.

1. **Seed from the source list, synchronously — no probing.**
   `Ops::sources()` returns the resolved aliases; `Switcher::from_sources` builds
   one host-skeleton row per source, each in a `scanning…` state. The first
   `terminal.draw` runs before any probe is polled, so the frame paints in tens
   of ms. (The runtime is `current_thread`, so the spawned probes only run at the
   loop's first `await` — *after* the draw. Draw-before-await is what makes the
   skeleton instant.)

2. **Stream each element independently as its own message.** The coarse
   `Cmd::Scan(full)` is replaced by fine-grained messages on the same unified
   command channel that already carries keys/mouse/preview:
   - `Cmd::SourceResult { source, sessions, err }` — one host's `list-sessions`
     outcome. The event loop spawns one probe per source (`spawn_probes`), so a
     fast host never waits on a slow one.
   - `Cmd::Panes { address, panes }` — one session's `list-panes` outcome. When a
     reachable `SourceResult` lands, the loop spawns one pane probe per session
     (`spawn_panes`), off the first-paint critical path.

3. **Model per-element state and render a hint for each.** The switcher tracks
   `scanning: HashSet<String>` (sources not yet returned) and
   `panes_loaded: HashSet<String>` (session addresses whose panes resolved). The
   tri-state host hint is computed in one place: `scanning…` → then its sessions /
   `(empty)` (reachable, zero sessions) / `⚠ unreachable: <reason>` (from
   `Group.err`). A session shows a dim `loading…` placeholder row until its panes
   land. A footer indicator reads `⟳ scanning hosts N/M…` until every probe
   settles, then clears to the help line.

4. **Apply each message incrementally, preserving the cursor.**
   `apply_source_result` / `apply_panes` mutate just the affected rows and rebuild,
   reusing the existing `same_node` / `row_matching` machinery. Cursor policy: a
   `user_moved` latch is set on any explicit navigation; while it is false the
   recency preselect re-wins (an untouched cursor follows the most-recent session
   as hosts arrive), and once the user moves, streamed updates keep the cursor put.

`r` re-scan (`request_rescan`) resets every host to its skeleton and re-kicks the
probes via a `rescan_kick` flag the event loop reads — the same take-once signal
pattern as the existing `poll_kick`. The switcher never owns `tokio::spawn`/the
channel; it signals, the loop spawns.

## Why This Matters

- **Time-to-first-frame dropped from ~1.36s → ~0.36s** (the latter is the bench's
  upper bound — each poll spawns an `xmux ctl` process — so the real paint is
  faster). The first frame shows every host skeleton with a `scanning…` hint
  before *any* probe returns; the slow local `list-sessions` is now just one more
  streamed probe, not a gate.
- **Independent streaming**: each host's sessions and each session's panes appear
  the moment they return. A fast host is not held behind a slow one, and remote
  ssh panes stream in per-session rather than in one all-or-nothing merge.
- **Clarity**: the tree is never a blank box. Every element explains itself —
  scanning, loading, empty, or unreachable-with-reason — so a slow or dead host
  reads as state, not as a bug.
- Reusing the existing `Cmd` channel means no new transport: the decoupling is
  just more message variants plus a synchronous `Ops::sources()` seed.

## When to Apply

- Any TUI whose first paint waits on a fan-out gather where the *structure* is
  cheap-and-known but the *data* is slow and arrives at uneven speeds.
- When late-arriving data must not break interaction — here the cursor-preserving
  `apply_*` + `user_moved` latch make a streaming tree non-disruptive to navigate.

Do **not** reach for this when the gather is uniformly fast (a single blocking
scan is simpler), or when the UI is meaningless without the full data (then an
honest single loading state beats a half-populated tree). The key precondition is
that the *skeleton alone is useful* — host rows the user can read, navigate, and
even act on (create-on-host) before the data lands.

## Examples

Seeding the switcher from the source list, no probing (`ui/run.rs`):

```rust
pub async fn run_switcher(ops: Arc<dyn Ops>, control: Option<PathBuf>) -> anyhow::Result<SwitchResult> {
    // Host skeletons from the resolved source list — the first frame paints
    // before any probe runs; the loop streams the rest in.
    let mut switcher = Switcher::from_sources(ops.sources());
    // …
}
```

Draw-before-await, then kick one probe per source (`ui/run.rs` `event_loop`):

```rust
loop {
    terminal.draw(|f| switcher.render(f))?;   // skeleton paints first
    if switcher.should_exit() { break; }
    if switcher.take_poll_kick() { spawn_capture(switcher, &ops, &cmd_tx); }
    if switcher.take_rescan_kick() { spawn_probes(&ops, &cmd_tx); }  // one list-sessions per source
    tokio::select! {
        maybe = cmd_rx.recv() => match maybe { Some(cmd) => match cmd {
            Cmd::SourceResult { source, sessions, err } => {
                let reachable = err.is_none();
                switcher.apply_source_result(source, sessions.clone(), err);
                if reachable { spawn_panes(&ops, &cmd_tx, sessions); }  // one list-panes per session
            }
            Cmd::Panes { address, panes } => switcher.apply_panes(address, panes),
            // … keys / mouse / preview / dump …
        }, None => break },
        _ = poll.tick() => spawn_capture(switcher, &ops, &cmd_tx),
    }
}
```

Per-element state hint, computed in one place (`ui/switcher.rs`):

```rust
fn host_hint(&self, g: &Group, scanning: bool) -> Option<String> {
    if scanning {
        Some("scanning…".into())
    } else if let Some(err) = &g.err {
        Some(format!("⚠ unreachable: {err}"))
    } else if g.sessions.is_empty() {
        Some("(empty)".into())
    } else {
        None
    }
}
```

### Verifying the streaming progression headlessly

A ratatui/crossterm app renders under the agent's Bash-tool console (no psmux pty
needed). Drive it through xmux's own control channel and dump at intervals to
watch the tiers stream in:

```bash
"$BIN" popup >run.log 2>&1 &          # control socket served as run_switcher starts
sleep 0.15; "$BIN" ctl dump            # skeletons + scanning… hints (instant)
sleep 1.2;  "$BIN" ctl dump            # sessions streamed in; panes still loading…
sleep 2.5;  "$BIN" ctl dump            # panes resolved; (empty)/unreachable hints settled
```

Two control-channel gotchas when scripting this from git-bash:
`xmux ctl key /` fails (`/` is not a named key — send it via `xmux ctl text /`),
and git-bash rewrites a leading-`/` argument into a Windows path
(`MSYS_NO_PATHCONV=1` disables that). Both are harness quirks, not app bugs.
