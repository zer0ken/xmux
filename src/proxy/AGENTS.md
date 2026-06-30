# Working Notes: /src/proxy

## Purpose

`proxy` owns the terminal-facing display path and low-level input plumbing:
PTY attachments, vt/grid state, input decoding, mouse parsing, focus routing,
attachment registry, and terminal setup helpers.

## Mental Model

The proxy display path runs real attached mux clients. Output pumps feed
`Grid`s; the cockpit renders the selected grid. Input and resize commands are
queued to attachment control threads so the async runtime does not block on PTY
operations.

## Module Seams

- `run.rs` spawns and manages one PTY attachment.
- `registry.rs` maps display keys to live attachments and exposes grid/input/
  resize/reap operations.
- `screen.rs` owns the vt-style grid. `Grid::fingerprint() -> u64` computes a
  content hash over all cell bytes; `cockpit.rs` compares successive fingerprints
  to determine whether a display transition actually changed the visible screen
  content (`display_grid_changed` is emitted only on a hash change).
- `input.rs`, `decode.rs`, `dispatch.rs`, and `mouse.rs` turn terminal input into
  routing decisions or UI events.
- `app.rs` tracks focus and modal routing state.

## Invariants

- Registry methods must not perform blocking PTY work on the event loop.
- Each attachment coalesces output wakeups so busy sessions cannot enqueue
  unbounded redraw events.
- The metadata control path does not supply display pixels.
- Teardown must signal child/control resources without blocking the runtime.

## Common Pitfalls

- Do not bypass `AttachRegistry` for input, resize, grid lookup, or reap.
- Do not write directly to a PTY from cockpit or UI code.
- Do not treat raw stdout passthrough as compatible with ratatui owning stdout.

## Before Editing

- Identify whether the change concerns attachment lifecycle, grid rendering,
  input routing, or terminal protocol parsing.
- Keep blocking OS calls on dedicated threads or behind existing channels.
- Preserve id/address correlation for `PtyEvent`s.

## Verification

- Run proxy module tests for registry, input decoding, screen, and attachment
  helper behavior.
- Run cockpit tests when changing focus routing, modal routing, or event
  coalescing.
