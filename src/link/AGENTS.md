# Working Notes: /src/link

## Purpose

`link` owns the live host-facing channels: per-host metadata channels, the mux
operations xmux issues against a live host, the login attempt, and the control-socket
protocol for headless driving.

A CONTROL host gets one control-mode client, owned and reaped by the host manager.
Its reader thread parses notifications and listing blocks into host events, and its
writer thread turns queued commands into bytes, with a pending-reply correlation tying
each command to its reply. A POLL host gets one enumeration task instead. Neither
holds an inventory: the app folds their host events into the host's own inventory,
the single owner, and rebuilds the nav rows from it.

## Module Seams

- The shared types: inventory data plus the command, event, and reply types the
  threads and the app exchange.
- The reader: the control-mode stdout line state machine that produces host events.
- The writer: drains commands to the child with one in-flight correlation per line.
- The client: one control-mode child with its reader, writer, and stderr threads.
- The poll task: enumeration for muxes with no control stream.
- The manager: each host's metadata channel and its composed control argv. Ensuring
  a channel opens one a host lacks and leaves a live one as it stands.
- The operations concern composes each mux argv across the two axes and runs it
  through an injected runner; it caches nothing and holds no state.
- The login concern runs one bounded, cancellable ssh login off the runtime and
  categorizes its result.
- The control-socket concern speaks length-framed messages, parses requests and keys,
  and resolves semantic verbs to domain actions at one site.

## Invariants

- **Metadata only.** Metadata and control clients do not own display pixels: host
  events update inventory and selection aids, never display grids. The per-session PTY
  attachments in `src/display` own the pixels.
- A remote machine's reachability comes from the machine probe before any channel opens;
  a control stream's exit reason carries only what the mux said on its own.
- A control stream ends with exactly one exit. A bare exit notice to a client that had
  listed sessions is a detach and reopens the channel once; any other exit reopens
  nothing.

## Before Editing

- For a new host event, add the event variant, its application-update arm, and its
  effect follow-up together.

## Verification

- Ensure and reap stay idempotent, and a new event reaches the nav through the state
  rather than through a side channel.
