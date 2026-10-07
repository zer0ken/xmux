# Working Notes: /src/display

## Purpose

`display` is the PTY, grid, and input mechanics layer behind the driver seam. It runs
real attached mux clients: spawning an attachment opens a PTY-backed attach child, an
output pump feeds a grid, and the app renders the selected grid. Input and resize go
to per-attachment control threads, and a worker moves the blocking PTY open and spawn
off the runtime and hands finished attachments back to the app, which owns the
registry. The layer also holds terminal setup, input decoding, mouse parsing, and the
terminal handover into a session when xmux is not the interactive app, and it names
xmux's own session so it can refuse to mirror itself.

It is mux-agnostic, naming no mux verb, and application-agnostic, holding no app UI
state: the focus and modal state machine lives in `app`.

## Module Seams

- An attachment is one PTY client: its handle, events, commands, control thread, and
  output pump. It owns its command's authentication guard until its child is reaped.
- The worker spawns attachments on a dedicated OS thread and never owns the registry.
- The registry maps display keys to live attachments, parks a fresh attachment while
  it paints, and serves a reaped attachment's last grid until a fresh one installs.
- The grid owns the terminal-emulation cell state, a content fingerprint for detecting
  a visible change, its last written line for naming why a child stopped, and the input
  modes the child set, which outlive a wipe of the cells.
- Input decoding, dispatch, paste splitting, and mouse parsing turn terminal bytes into
  routing decisions or input actions; terminal setup holds prefix parsing, mouse
  capture, bracketed paste, focus reports, and the terminal guard.
- The live child-environment read answers one caller-named variable from a running
  attach child. It names no mux and no variable.

## Invariants

- **Real mux clients.** Display attachments are real mux clients, not reconstructed
  output streams; the metadata control path never supplies display pixels.
- The registry is the only way to reach an attachment for input, resize, grid lookup,
  and reap. App and UI code never write to a PTY directly.
- The renderer owns stdout, so raw stdout passthrough of child output is not an
  option; the one exception is a whole OSC 52 sequence the loop re-emits between
  frames.
- The live child-environment read has two answers, a value or no signal; absence never
  stands in for a value, and a stale exec-time environment counts as no signal.

## Before Editing

- PTY events carry only the attachment id; the address behind an id is looked up in
  the registry, so an event never holds a stale address.

## Verification

- A change to input routing is rechecked from the app side for focus routing, modal
  routing, and event coalescing.
