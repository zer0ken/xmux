# Working Notes: /src/host

## Purpose

`host` owns host connection management: the control-mode reader and writer
machinery, poll task lifecycle, per-host session and window inventory, and the
host events the app folds into the runtime state. It is a METADATA channel only:
the per-session PTY attachments in `src/display` own the pixels.

## Mental Model

Each remote host gets ONE control-mode client, owned and reaped by the host
manager. A reader thread parses control-mode notifications into host events; a
writer thread turns queued commands into the exact bytes to send. The reader
holds no inventory of its own: it parses each session or pane listing block and
carries the result on a host event, using the same carriers the poll path uses. A
pending-reply correlation ties a control command to its reply so the right event
is emitted. The app folds those events into the host's own inventory, the single
owner of per-host session and window inventory, and rebuilds the nav rows from
it.

## Module Seams

The module is split by role: the shared vocabulary (inventory data plus the
command, event, and reply types the threads exchange), the control-mode stdout
line state machine that produces host events, the writer that drains commands to
the child with one in-flight correlation per line, the client owning one
control-mode child with its reader, writer, and stderr threads, the poll task for
muxes with no control stream, and the manager owning each host's metadata channel
and the composed control argv.

- Ensuring a host spawns the control-mode child with an argv composed across the
  two orthogonal axes: the mux supplies the control payload and the transport
  wraps it for local or ssh execution. It never hardcodes a mux verb or
  hand-rolls ssh.
- The manager owns the map of clients plus ensure, reap, and poll-task
  management; a client owns one host's reader and writer threads and channels.
- The host event is the outbound vocabulary the runtime state consumes; the app
  runs the returned effects back against these clients, the registry, and the
  display worker.
- Depends on the mux axis for control-protocol parsing and on the domain types
  for sessions and their panes.

## Invariants

- This is a metadata path only: host events update inventory and selection aids,
  not display grids.
- Ensuring a host is idempotent: re-ensuring a live host is a no-op.
- The control argv is composed from the transport and mux axes; no mux verb or
  ssh invocation is hardcoded here.

## Common Pitfalls

- Do not do display or PTY work here; that belongs to `src/display`.
- Do not block: the reader and writer run on their own threads and communicate
  with the app loop over channels.

## Before Editing

- Decide whether the change is metadata (here), display PTY (`src/display`), or
  transport lowering (the machine axis).
- For a new event, add the event variant, its arm in the state's event apply, and
  its effect follow-up together.

## Verification

- Check that ensure and reap stay idempotent, and that the new event reaches the
  nav through the state rather than through a side channel.
