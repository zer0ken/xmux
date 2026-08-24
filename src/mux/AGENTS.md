# Working Notes: /src/mux

## Purpose

`mux` is the mux family home. It defines mux-specific behavior behind the `Mux`
trait AND holds the pure shared vocabulary every mux argv is built from. A mux
knows the mux binary, server model, enumeration behavior, attach command shape,
control-channel availability, event source, death signal, and window/session
operation plans.

The module root holds the cross-mux surface: the `Mux` trait, the mux registry
(the one list every factory and predicate reads), identity detection, the factory
functions, and the control-protocol trait that hides a mux's control-mode wire
details (line framing and classification, the notification-to-event table, the
size formatter) from the connection layer. The shared vocabulary holds the query format
templates, the argv builders, the row parsers, and the address utilities; the root
re-exports it, so one path names a vocabulary builder and a mux factory alike.

Each concrete mux lives in its own sub-directory, owning BOTH its metadata mux
AND its display driver, and is re-exported from the root:

- `tmux/` owns the tmux mux, the display-tty file helpers, its control argv, its
  driver with its attach helper, and its pure control-mode wire functions behind
  the control-protocol trait. See `tmux/AGENTS.md`.
- `psmux/` owns the psmux mux, its poll cadence, its in-place switch plan, its
  driver with the tty capture and refresh helpers, and the per-host session
  registry that backs enumeration (one server per session, so there is no
  aggregate session listing). See `psmux/AGENTS.md`.
- `zellij/` owns the zellij mux, its poll cadence, the per-session action argv
  every zellij query is addressed with, its driver (which reattaches on every
  session change because no client can be named from outside its own session),
  and its two output shapes: a human session line and a JSON tab listing. See
  `zellij/AGENTS.md`.

Sub-modules pull the shared trait, value types, and imports from the parent. A
mux's driver is constructed by the mux itself, so no caller names a concrete
driver type.

## Mental Model

A mux describes mux vocabulary and classification. A transport lowers host
execution. The `MuxDriver` trait in `src/driver.rs` is the mux-agnostic display
seam; each mux's concrete driver lives in its own family directory and is
constructed by the mux, so a mux owns BOTH its argv, server model, and
enumeration AND its display orchestration. Shared muxes such as tmux use one
aggregate server and a source-level control stream. Per-session muxes such as psmux
and zellij enumerate differently and supply a per-session attach plan.

The command-plan verbs default to tmux-compatible argv, so a tmux-compatible mux
is identity plus a few overrides. A mux that shares no argv with tmux overrides
every verb, and overrides the pane parsing with it: a plan and the shape of what
it prints are one decision, so they move together.

## Module Seams

- Enumeration may use the transport, because it executes on a host.
- Discovery answers "which muxes a host serves", and only ever from the
  registry: the candidate set is what xmux can drive, and each candidate is
  confirmed by the identity probe answering AS that mux. It is called once per
  host, by the environment for this box before the first paint and by the
  runtime for each remote after it, never per source. The psmux-shadows-tmux
  filter lives inside discovery and keys off what ANSWERED, never off an OS: a
  remote's OS is not something xmux knows, since the ssh family's platform field
  is the LOCAL one and gates multiplexing only.
- Plan methods return mux argv or mux intent; they do not decide local versus ssh
  execution. The plan set covers what xmux itself issues: attach, enumerate, read
  panes and options, select a window, and start a session. There is no kill,
  rename, or window-edit plan; the mux owns those. Every mux argv is built from a
  mux and lowered by a transport, never off a bare binary name.
- The generic command builders from the shared vocabulary are called ONLY inside
  the per-mux directories and the shared enumeration helper in the root, each
  plan wrapping one. The pure address vocabulary is callable anywhere.
- The server model, the event source, and the death signal are the classification
  values callers use instead of branching on mux names. The mux constructs the
  source's driver, so mux selection lives in the mux family and never in a central
  match on server model; the wrapper in `src/driver.rs` only resolves it. tmux
  keeps one PTY per source with an in-place switch; psmux switches its client in
  place or reattaches per session; zellij reattaches on every change, since it
  can name no client from outside its own session.

## Invariants

- A reachable empty mux enumerates as an empty list, not an error; unreachable
  sources return an error.
- Every command in a poll sweep runs under a fixed per-command budget. The poll
  ticker only advances after the sweep RETURNS, so one command that never answers
  would freeze that source's whole inventory. A timed-out listing surfaces as the
  source's error; a timed-out pane query still emits an EMPTY panes event, because a
  card whose panes never arrive keeps its spinner forever.
- Transport-specific command wrapping belongs to the host axis.
- A mux answers for its OWN flags, and a flag question added to the trait carries no
  tmux-compatible default. Whether a mux takes a server-socket flag is asked of the mux
  for exactly this reason: zellij refuses an unexpected flag before it reads the verb, so
  a default answer inherited from tmux would make a source permanently unreachable, and a
  mux added later would inherit the same break.
- Where a machine has to know something mux-specific to address a mux correctly, the
  answer is computed where BOTH are known - the source list and the host registry - and
  handed to the machine. The machine axis names no mux, so it can only use what it is
  given; both sites derive their answer from one raw value the same way, which is what
  keeps a source and its host addressing one server.
- Mux methods should stay at the exact behavior surface used by app, source
  metadata, and management code.

## Common Pitfalls

- Do not add a broad capability catalog when only one caller needs a concrete
  plan.
- Do not thread remoteness booleans through mux methods.
- Do not duplicate psmux registry behavior outside the mux and source boundary
  without deciding which module owns it.

## Before Editing

- Identify whether the new behavior is mux semantics, host transport, or UI
  policy.
- Check tmux, psmux, AND zellij behavior when changing trait methods. A new verb
  with a tmux-compatible default is silently wrong for zellij, which refuses
  tmux's flags outright.
- Keep trait additions tied to an end-to-end caller.

## Verification

- Pin the argv a plan emits and the shape it parses back together; they are one
  decision.
- Re-check the connection and app surfaces when the event source, death signal, or
  selection outcome changes.
