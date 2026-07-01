# Working Notes: /src/display

## Purpose

`display` is the shared PTY/grid/input display-mechanics layer: PTY attachment
spawn and lifecycle, the off-runtime attach worker, the attachment registry, the
vt/grid state, terminal input decoding, mouse parsing, and terminal setup. It is
mux-agnostic (it names no tmux/psmux verb) and application-agnostic (it holds no
cockpit UI state — the focus/modal state machine lives in `app/focus.rs`).

## Mental Model

The display path runs real attached mux clients. `spawn_attachment` opens a
ConPTY-backed `attach` child; an output pump feeds a `Grid`; the cockpit renders
the selected grid. Input and resize commands are queued to per-attachment control
threads so the async runtime never blocks on PTY operations. The worker moves the
blocking ConPTY open+spawn off the runtime thread and hands finished attachments
back to the cockpit, which owns the registry.

## Module Seams

- `attachment.rs` spawns and manages one PTY attachment (`Attachment`,
  `spawn_attachment`, `PtyEvent`, `PtyCmd`, the control thread and output pump).
- `worker.rs` runs `spawn_attachment` on a dedicated OS thread (`DisplayWorker`,
  `DisplayEnsure`, `DisplayEvent`); it never owns the registry.
- `registry.rs` maps display keys to live attachments and exposes grid/input/
  resize/reap operations (`AttachRegistry`).
- `grid.rs` owns the vt-style grid (`Grid`). `Grid::fingerprint() -> u64` computes
  a content hash over all cell bytes; `cockpit.rs` compares successive fingerprints
  to determine whether a display transition actually changed the visible screen
  content (`display_grid_changed` is emitted only on a hash change).
- `input.rs`, `decode.rs`, `dispatch.rs`, and `mouse.rs` turn terminal input into
  routing decisions or input `Action`s. `term.rs` holds terminal setup helpers
  (prefix parsing, mouse capture, the terminal guard).

## Invariants

- Registry methods must not perform blocking PTY work on the event loop.
- Each attachment coalesces output wakeups so busy sessions cannot enqueue
  unbounded redraw events.
- The metadata control path does not supply display pixels.
- Teardown must signal child/control resources without blocking the runtime.
- The pump answers the child's terminal QUERIES (DSR/DA) itself, since there is no
  real terminal behind the PTY; otherwise the child stalls on startup (empty pane).
- `Grid::render_into` marks each wide (CJK) glyph's trailing cell
  `CellDiffOption::AlwaysUpdate` so ratatui's incremental diff repaints it on a
  wide→narrow transition; ratatui otherwise skips that trailing cell and the
  terminal keeps the old glyph's right half as background residue. This is a
  paint-layer fix — never a full-screen clear (which would flash on every switch).

## Common Pitfalls

- Do not bypass `AttachRegistry` for input, resize, grid lookup, or reap.
- Do not write directly to a PTY from cockpit or UI code.
- Do not treat raw stdout passthrough as compatible with ratatui owning stdout.
- Do not name a mux verb or a cockpit UI-state type here; this layer is
  mux-agnostic and app-agnostic.

## Before Editing

- Identify whether the change concerns attachment lifecycle, grid rendering,
  input routing, or terminal protocol parsing.
- Keep blocking OS calls on dedicated threads or behind existing channels.
- Preserve id/address correlation for `PtyEvent`s.

## Verification

- Run display module tests for registry, input decoding, grid, attachment helper
  behavior, and the worker's off-loop responsiveness.
- Run cockpit tests when changing focus routing, modal routing, or event
  coalescing.
