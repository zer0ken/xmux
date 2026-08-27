# Working Notes: /src/display

## Purpose

`display` is the shared PTY / grid / input display-mechanics layer: PTY
attachment spawn and lifecycle, the off-runtime attach worker, the attachment
registry, the grid state, terminal input decoding, mouse parsing, terminal setup,
and the terminal handover into a session (the exec that hands the controlling
terminal to the mux client when xmux is not the interactive app). It also names
xmux's own session so it can refuse to mirror itself. It is mux-agnostic (it names
no mux verb at all) and application-agnostic (it holds no app UI state; the focus
and modal state machine lives in `app`).

## Mental Model

The display path runs real attached mux clients. Spawning an attachment opens a
PTY-backed attach child; an output pump feeds a grid; the app renders the
selected grid. Input and resize commands are queued to per-attachment control
threads so the async runtime never blocks on PTY operations. The worker moves the
blocking PTY open and spawn off the runtime thread and hands finished attachments
back to the app, which owns the registry.

## Module Seams

- Attachment spawning and management covers one PTY attachment: its handle, its
  events and commands, its control thread, and its output pump.
- The worker runs that spawn on a dedicated OS thread and hands the result back;
  it never owns the registry.
- The registry maps display keys to live attachments and exposes the grid, input,
  resize, and reap operations.
- The grid owns the terminal-emulation cell state. It also answers a content
  fingerprint, which the runtime compares across successive frames to decide
  whether a display transition actually changed the visible screen; the
  grid-changed log event fires only on a change.
- Input decoding, dispatch, and mouse parsing turn terminal input into routing
  decisions or input actions. It recognizes the kitty protocol's press / repeat /
  release events, so a release is never mis-read as a keypress and a held key's
  repeats never re-arm a consumed ready or toggle an armed one. Terminal setup holds
  the prefix parsing, mouse capture, and the terminal guard.

## Invariants

- Registry methods must not perform blocking PTY work on the event loop.
- Each attachment coalesces output wakeups so busy sessions cannot enqueue
  unbounded redraw events.
- The metadata control path does not supply display pixels.
- An attachment reports the name its own PTY carries as a plain fact about that
  PTY. Whether that name identifies a mux client depends on where the attach child
  actually runs, which is a transport question this layer never answers.
- Teardown must signal child and control resources without blocking the runtime.
- The pump answers the child's terminal QUERIES (device status, device
  attributes) itself, since there is no real terminal behind the PTY; otherwise
  the child stalls on startup and the terminal view stays empty.
- The terminal guard asks the terminal to report key releases (kitty
  report-event-types), because a C0 control byte stream never carries a key-up.
  A terminal that declines the request keeps its legacy encodings: it has no
  Release events to tell a held key's Repeat from a fresh Press, so a repeated
  prefix byte is swallowed (the doubled-prefix literal, which fires on a fresh
  Press, is then unavailable).
- Rendering marks each wide (CJK) glyph's trailing cell as always-update so the
  renderer's incremental diff repaints it on a wide-to-narrow transition;
  otherwise that trailing cell is skipped and the terminal keeps the old glyph's
  right half as background residue. This is a paint-layer fix, never a
  full-screen clear, which would flash on every switch.

## Common Pitfalls

- Do not bypass the registry for input, resize, grid lookup, or reap.
- Do not write directly to a PTY from app or UI code.
- Do not treat raw stdout passthrough as compatible with the renderer owning
  stdout.
- Do not name a mux verb or an app UI-state type here; this layer is mux-agnostic
  and app-agnostic.

## Before Editing

- Identify whether the change concerns attachment lifecycle, grid rendering,
  input routing, or terminal protocol parsing.
- Keep blocking OS calls on dedicated threads or behind existing channels.
- Preserve the id and address correlation carried on PTY events.

## Verification

- Exercise the registry, input decoding, grid, attachment lifecycle, and the
  worker's off-loop responsiveness for the seam you touched.
- Re-check focus routing, modal routing, and event coalescing from the app side
  when the change reaches them.
