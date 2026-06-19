# xmux — Live full-attach terminal-view re-architecture (design)

- Date: 2026-06-19
- Branch: `feat/rust-rewrite`
- Status: approved — proceeding to TDD plan

## Goal

xmux is a persistent terminal application. Layout: a **sidebar** (left, the
session tree), a **terminal view** (right, the live full attach of the selected
session), and a **status bar** (bottom). The terminal view shows the session's
actual live terminal — a vt100 `Grid` fed by a real attach PTY — replacing the
static `capture-pane` text snapshot. Sessions are attached lazily and kept alive
across selections so switching is instant.

## Terminology

UI regions (canonical names — use these in code identifiers and docs):

- **Sidebar** — the left region; its content is the **session tree** (hosts →
  sessions → windows → panes). Toggled hidden/shown.
- **Terminal view** — the right region; the live terminal of the selected
  session. The single persistent main region (full-screen when the sidebar is
  hidden). Never called "pane" (avoids clashing with the mux's own panes) or
  "preview" (it is a live terminal, not a snapshot).
- **Status bar** — the bottom row in the Passthrough state (info only). In the
  Overlay state the bottom carries the shortcut **help/footer** instead.

Runtime concepts:

- **Attachment** — one kept live attach: a ConPTY + attach child + dedicated
  control thread (owns writer+master) + output pump thread + a shared `Grid`.
- **Foreground** — the single Attachment that owns the real terminal (raw
  stdout) while in the Passthrough state.
- **Passthrough state** — sidebar hidden; the foreground Attachment renders raw
  to the real terminal at `cols×(rows-1)`, with the xmux status bar on the last
  physical row.
- **Overlay state** — sidebar shown; ratatui owns the terminal and draws the
  sidebar (left) + the terminal view (right, a live `Grid` render of the cursor
  session).

## 1. State model & transitions

Two states only: **Passthrough** (sidebar hidden) and **Overlay** (sidebar
shown).

- `Passthrough{ fg: Address }` — foreground owns raw stdout (`rows-1`) + the
  xmux status bar. Input → `InputMachine` → foreground PTY; only the xmux prefix
  is intercepted, everything else (including the mux's own prefix) passes
  through.
- `Overlay` — ratatui owns stdout. Sidebar = session tree (filter, rename,
  kill). Terminal view = live `Grid` render of the cursor session. Input drives
  the tree. The footer/help shows the shortcuts.

Transitions:

| From | Event | To |
|------|-------|-----|
| Passthrough | `prefix s` | Overlay (cursor starts on current foreground) |
| Overlay | cursor dwell completes on attachable row | (stays Overlay) attach; terminal view goes live |
| Overlay | `Enter` on session | attach if needed **immediately**, promote to foreground, → Passthrough (full-screen) |
| Overlay | `Esc` (or toggle) | → Passthrough of the **previous** foreground (no switch) |
| Overlay | `q` | quit xmux |
| Passthrough | `prefix q` | quit xmux |
| Passthrough | foreground session exits/killed | → Overlay, foreground cleared |
| (start) | launch | → Overlay, cursor preselected on the globally most-recent session |

"Foreground" is meaningful only in Passthrough; it is the session last entered
full-screen. Opening Overlay remembers it (so `Esc` returns there); `Enter`
replaces it with the cursor session. At the initial state (no foreground yet),
`Esc` is a no-op (nothing to return to) — only `Enter` (pick a foreground) or
`q` (quit) leave the initial Overlay.

## 2. Concurrency & ownership (core)

Generalizes the existing single-attach proxy (`src/proxy/run.rs`) to N kept
Attachments managed by a registry.

### AttachRegistry

`Address → Attachment`, bounded by the keep cap (§3). Holds:

```
Attachment {
  grid:        Arc<Mutex<Grid>>,        // shared: pump feeds, render reads
  control_tx:  Sender<PtyCmd>,          // Input / Resize → control thread
  size:        (cols, rows),
  last_used:   Instant,                 // LRU
  // child + pump/control join handles for teardown
}
```

### Per-Attachment threads (reuse the B1 pattern)

- **Control thread** — `pty_control_loop` + `MasterSink`: owns `writer` +
  `master`, receives `PtyCmd::{Input,Resize}`, applies them in FIFO order.
  Drops the master on channel close (teardown). One per Attachment.
- **Pump thread** — reads the master in chunks; **always** feeds the shared
  `Grid` (so it stays current for restore and the terminal view);
  **additionally** writes raw stdout **iff this Attachment is the live stdout
  owner** (see below).

### Live stdout owner

A single shared state `AppState = Passthrough{ fg } | Overlay` (atomic /
mutex-guarded), read by every pump before each stdout write:

- A pump writes raw stdout **iff** `AppState == Passthrough && self.addr == fg`.
- Otherwise it feeds its `Grid` only.
- In Overlay, **no** pump writes stdout; only ratatui does.

This is the existing `overlay_active` gate generalized to a per-Attachment
owner check. Exactly one writer touches real stdout at any time. Keep the
per-write `StdoutLock` (never hold it across a blocking op).

### Transition handoff (no two writers)

- **Passthrough → Overlay**: set `AppState = Overlay` (pumps stop writing
  stdout) → ratatui enters its draw mode and owns stdout.
- **Overlay → Passthrough{fg}**: while pumps are still gated off stdout, the
  main loop emits `fg.grid.restore_bytes()` + paints the status bar (under the
  per-write lock); then sets `AppState = Passthrough{fg}` so the foreground
  pump resumes raw writes. This mirrors today's reviewed overlay-close restore.

### Teardown

- Per-Attachment: drop `control_tx` → control thread drops the master on its
  own thread (never on the loop) and exits; pump exits on master EOF. Join with
  a timeout off the loop.
- On quit: tear down all N Attachments in parallel (signal all, then bounded
  join). Pre-24H2 Windows `ClosePseudoConsole` can block a control thread — a
  known follow-up (portable-pty 0.9 has no async close); bounded join prevents a
  hung quit.

## 3. Kept-PTY lifecycle

- **Trigger — dwell**: in Overlay, when the cursor rests on an attachable
  session row for **500ms**, attach it (if not already) and keep it. Scrolling
  past rows quickly attaches nothing.
- **Enter — immediate**: `Enter` attaches the cursor session immediately
  (skipping the dwell wait), promotes it to foreground, and switches to
  Passthrough.
- **Uniform local + remote**: both local and remote sessions are live-attached
  the same way. Remote attach is an `ssh -t` + attach (per `source.rs`);
  ControlMaster multiplexes on non-Windows, Windows opens a fresh connection.
  The cost (ssh spawn latency, `window-size latest` resizing the remote window)
  is accepted for v1.
- **Cap (configurable, config file)**: read from the existing
  `~/.config/xmux/config.toml` (`src/config.rs`) — a new optional `[ui]` table
  with `keep_cap` (default **6**, clamped to a minimum of 2: must hold
  foreground + current cursor). Loaded via the existing `config::load`; unknown
  keys still warn via `serde_ignored`. When attaching would exceed the cap,
  evict the **least-recently-used** Attachment that is neither the foreground
  nor the current cursor session. Eviction tears down that Attachment (§2).

  ```toml
  [ui]
  prefix = "C-g"   # xmux's own prefix (config-only, like tmux's set -g prefix)
  keep_cap = 6
  ```
- **Reap**: when a session's mux process exits, its pump hits EOF → the
  Attachment is removed from the registry; if it was the foreground, fall back
  to Overlay.
- **Self-mirror guard**: before attaching, if the session's active pane command
  is `xmux`, skip the live attach and show a note in the terminal view (extends
  the current `preview_self` guard) — prevents infinite self-mirror.

## 4. Rendering

### Passthrough (foreground)

- The foreground child PTY is sized `cols×(rows-1)`. The child believes the
  terminal is `rows-1` tall, so it never addresses the last physical row and its
  scrolling stays within `rows-1` (status bar auto-protected for scroll/CUP).
- xmux paints its **status bar** on the last physical row: `host/session ·
  kept N/cap` — **info only, no shortcut hints**.
- The one leak is a full-screen clear (`ESC[2J` / `ESC[…J`) which the real
  terminal applies to the whole physical screen. The foreground pump scans each
  outgoing chunk for a clear sequence (rolling-tail, like the existing paste
  detector) and, when found, repaints the status bar after the chunk (wrapped in
  cursor save/restore). Otherwise the status bar is fully transparent.

### Overlay

- Sidebar: the session tree (hosts → sessions → windows → panes), filter,
  rename, kill — unchanged behaviorally.
- Terminal view: a live render of the cursor session's `Grid`. A **Grid→ratatui
  bridge** maps each vt100 cell (fg/bg/attrs, true color) to a ratatui `Style`
  and places the cursor. Implemented on `screen.rs` (`Grid` gains a
  cell/contents accessor; `vt100::Screen` exposes per-cell color/attrs and
  cursor position).
- The footer/help (shown only in Overlay) documents both the overlay keys and
  the passthrough prefix hotkeys.

## 5. Dwell progress animation

Visualizes the in-progress dwell on the cursor row in the sidebar.

- On cursor move to an attachable row: `dwell_start = now`; background = normal
  selection highlight.
- During the 500ms window: fill the row background **left→right** as a progress
  bar. `progress = clamp((now − dwell_start)/500ms, 0, 1)`; columns
  `[area.x, area.x + progress×width)` get the fill style.
- On completion (`progress ≥ 1`): restore the row to normal and attach → the
  terminal view goes live.
- On cursor move before completion: reset (fresh row).
- Already-attached session (cap re-visit): no progress bar; the terminal view
  shows it immediately. The bar runs only when a real attach will occur.
- Non-attachable rows (host header / self-mirror / loading): no animation.

**Mechanics**: the event loop gains a short **animation tick (~33ms)** that is
active **only while a dwell is pending** (idle otherwise — no wasted redraws).
Render draws the `List`+`ListState` as today, then overlays the fill by setting
buffer cell backgrounds on the selected row's span.

## 6. Input routing & xmux prefix

- The xmux prefix is configured **only** in `config.toml` (`[ui] prefix`,
  default `C-g`), matching tmux's config-only `set -g prefix` convention. The
  `XMUX_PREFIX` env var is removed (it was unreleased on this branch, so no
  compatibility cost). The existing `parse_prefix` spec parser (`C-g`,
  `C-Space`, …) is reused on the config value.
- `InputMachine` (`src/proxy/input.rs`) extends its prefix actions. In
  Passthrough: `prefix` arms; then `s` → open Overlay, `q` → quit xmux,
  double-tap prefix → send one literal prefix byte (existing). All other bytes
  (including the mux's own prefix) forward to the foreground.
- In Overlay, bytes drive the tree (no prefix needed): `j/k`/`PgUp`/`PgDn` move,
  `Enter` promote, `Esc` return, `/` filter, `c`/`r`/`x` create/rename/kill, `?`
  help, `q` quit.

## 7. Sizing

- All kept Attachments are sized `cols×(rows-1)` (the size they will have as
  foreground). The terminal view in Overlay, being narrower, renders a top-left
  clip of that grid — sufficient for "which session is this". This avoids resize
  thrash on cursor moves and makes `Enter` promotion a zero-resize, instant
  switch.
- On a terminal resize event, resize all kept Attachments to the new
  `cols×(rows-1)` (one `PtyCmd::Resize` each, via their control threads).
- `window-size latest` may resize the real mux window to the attach size; this
  is accepted for v1 (see brief's sizing research).

## 8. Tree ordering & initial selection

Recency uses the existing server-reported `last_attached` (`session.rs`).

- **Group (host) order**: local group(s) pinned to the top; remote hosts sorted
  by their most-recent session's `last_attached`, descending (host with the most
  recent session first). (Current behavior orders groups by `sources()`; this
  adds dynamic remote-host recency ordering.)
- **Within a group**: `last_attached` descending, ties by name ascending
  (existing `tree::sort_by_recency`).
- **Initial cursor**: the globally most-recent session (max `last_attached`),
  independent of display order (existing preselect at `switcher.rs` already
  selects the global max). Start in Overlay with the cursor there.
- **Streaming**: sessions arrive asynchronously; remote-group recency reordering
  preserves the cursor by session identity and respects the existing
  `user_moved` latch so the cursor does not jump during load.

## 9. Removed / superseded (AS-IS: no dead code)

- **Removed** (capture terminal-view path, fully superseded by live attach — no
  remote fallback): the `capture-pane`-based snapshot, `preview_cache` + LRU
  cap, the capture-target polling, the B2 capture semaphore/debounce, and the
  `Ops::capture` consumer in the switcher. Evaluate whether `Ops::capture`
  itself has any remaining caller; remove if not.
- **Removed** (in-mux open path): the `xmux popup` subcommand + `run_popup`
  (`main.rs`), `signal_cockpit_switch` and the popup→cockpit `SignalCockpit`
  control-socket signal path (`cockpit.rs`). Its replacement is the
  Overlay/Passthrough sidebar toggle.
- **Kept**: streaming `list-sessions` / `list-panes` probes that paint the tree
  (render-first scan); `attach::nest_guard` (xmux still refuses to run inside a
  mux); the cockpit control socket for headless test injection (`key`/`text`/
  `dump`/`ping`) — repurposed to drive the persistent app.
- **Reconcile**: the cross-open scan cache (Task2) exists to repaint the
  switcher fast across re-opens; in a persistent app the scan is held
  continuously, so the cross-open cache is reduced to the in-memory live scan —
  simplify, do not leave a dead cache.

## 10. Testing strategy

- Headless via the control socket (`key`/`text`/`dump`): tree paint, recency
  ordering, navigation, filter, dwell→attach trigger, and switch logic.
- N-Attachment concurrency, the live-owner gate, transition handoff, and
  teardown: offline unit tests + an adversarial review of the concurrency.
- **Never disrupt the live local psmux session**: drive only the throwaway
  remote `jupiter06`; never select/attach the user's live `local/xmux`.
- The rendered raw-passthrough screen handover (full-screen visual) is a human
  verification gate (not headless-observable).

## 11. Risks & known follow-ups

- ConPTY teardown blocking on pre-24H2 `ClosePseudoConsole` (per Attachment,
  worse with N) — bounded join; real fix is a portable-pty bump.
- Uniform remote live attach: ssh spawn latency and `window-size latest`
  resizing the remote window; accepted for v1.
- Status-bar repaint depends on detecting clear sequences; chunk-split clears
  handled by rolling-tail scan.
- Terminal-view clip (top-left only) is a v1 simplification; size-to-pane with a
  debounced resize is a later option.

## 12. Module impact map

- `src/cockpit.rs` — replace the one-attach-at-a-time / detach-to-reattach loop
  with the persistent app: AttachRegistry, AppState, transition handoff, control
  socket repurposed. Remove `signal_cockpit_switch` / `SignalCockpit`.
- `src/proxy/run.rs` — extract the reusable per-Attachment unit (control thread +
  pump + grid + live-owner gate); the proxy becomes "spawn/own N Attachments,
  one foreground". `parse_prefix` sources the prefix from config (not the removed
  `XMUX_PREFIX` env var).
- `src/proxy/screen.rs` — add the Grid→ratatui cell bridge + cursor accessor.
- `src/proxy/input.rs` — extend prefix actions (`s` toggle, `q` quit).
- `src/ui/switcher.rs` — terminal view (live `Grid` render) replaces the capture
  snapshot; rename surviving `preview_*` identifiers to `terminal_view_*` (e.g.
  the self-mirror guard); dwell progress animation; status-bar content; help
  only in Overlay; remove `preview_cache`/capture state.
- `src/ui/run.rs` — animation tick; integrate AppState/state machine; remove
  capture commands; keep streaming probes.
- `src/source.rs` — reuse `attach_command`; uniform remote attach.
- `src/main.rs` — remove the `popup` subcommand; keep the `current_thread`
  runtime (off-load blocking, never block the loop); keep `nest_guard`.
- `src/config.rs` — add the optional `[ui]` table (`UiConfig { prefix,
  keep_cap }`; `keep_cap` default 6, min 2; `prefix` default `C-g`) to `Config`;
  threaded to the proxy (prefix) and the AttachRegistry (cap) via `env.rs`.

## Open questions

- `Enter` default = immediate attach + promote to foreground + full-screen
  Passthrough (per the approved Model A); adjustable at spec review.
