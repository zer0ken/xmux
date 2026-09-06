# Working Notes: /src/link

## Purpose

`link` owns the live host-facing channels: per-source connection management (the
control-mode reader and writer machinery, poll task lifecycle, per-source session
and the source events the app folds into the runtime
state), the mux operations xmux issues against a live host, and the control-socket
protocol for headless driving. The connection-management part is a METADATA
channel only: the per-session PTY attachments in `src/display` own the pixels.

## Mental Model

Each remote source gets ONE control-mode client, owned and reaped by the source
manager. A reader thread parses control-mode notifications into source events; a
writer thread turns queued commands into the exact bytes to send. The reader
holds no inventory of its own: it parses each session listing block and
carries the result on a source event, using the same carriers the poll path uses. A
pending-reply correlation ties a control command to its reply so the right event
is emitted. The app folds those events into the source's own inventory, the single
owner of per-source session inventory, and rebuilds the nav rows from
it.

The operations concern composes a mux argv through the transport and runs it via an
injected runner to perform the mux actions xmux itself issues (create a session,
read a host's sessions or options); nothing is cached and no state is held. The
control-socket concern is the headless driving protocol: length-framed messages,
request and key parsing, and the ctl client that injects keystrokes and dumps the
rendered screen over a local socket.

## Module Seams

The module is split by role: the shared types (inventory data plus the
command, event, and reply types the threads exchange), the control-mode stdout
line state machine that produces source events, the writer that drains commands to
the child with one in-flight correlation per line, the client owning one
control-mode child with its reader, writer, and stderr threads, the poll task for
muxes with no control stream, and the manager owning each source's metadata channel
and the composed control argv.

- Ensuring a source spawns the control-mode child with an argv composed across the
  two orthogonal axes: the mux supplies the control payload and the transport
  wraps it for local or ssh execution. It never hardcodes a mux verb or
  hand-rolls ssh.
- The manager owns the map of clients plus ensure, reap, and poll-task
  management; a client owns one source's reader and writer threads and channels.
- The source event is outbound event set the runtime state consumes; the app
  runs the returned effects back against these clients, the registry, and the
  display worker.
- Depends on the mux axis for control-protocol parsing and on the domain types
  for sessions.
- The operations concern composes each mux argv across the two axes and runs it
  through the injected runner, exactly like enumeration; it hardcodes no mux verb.
- The control-socket concern speaks semantic verbs and resolves them to domain
  actions at one site, so raw key/text injection stays a low-level namespace.

## Invariants

- The connection-management concern is a metadata path only: source events update
  inventory and selection aids, not display grids.
- Ensuring a source is idempotent: re-ensuring a live source is a no-op.
- The control argv is composed from the transport and mux axes; no mux verb or
  ssh invocation is hardcoded here.
- A sweep logs what CHANGED, not that it ran. An unchanged listing and a failure already
  standing are both counted, never rewritten: a polled source ticks tens of times a minute
  for as long as xmux runs, so a line per tick is a file filled by one silent host. The
  rule is a value the loop folds outcomes into, so it is tested rather than read out of a
  log file afterwards.
- A remote host's REACHABILITY (connected, blocked, or unreachable) is classified by a
  machine probe (`ssh <machine> true`) before any channel opens, not by the control
  reader. The reader's exit reason carries only a protocol `%error` (a "no sessions" /
  "no server" empty mux), so a reachable-but-empty host is told from one that answered;
  a control channel opens only for a machine already known to connect.
- The login is a single PTY ssh carrying the submitted connection values as `-o`
  overrides. Where this side multiplexes, it establishes the ONE authenticated master
  (`ControlMaster=yes` over the shared control socket) that every later `BatchMode`
  channel reuses. The secret rides only the transient command and the PTY writer - never
  stored, logged, or rendered.
- A side that cannot multiplex still runs the login. ssh asks about the host key BEFORE it
  authenticates and writes the answer to `known_hosts`, so accepting a key is a login
  whose whole result outlives the connection; recording the values is another. Only the
  reuse is lost, so only the reuse is refused: a host that then needs a password is asked
  again on the next probe, which is the truth about that machine on that platform rather
  than a reason to have refused the login.
- The login is a CONVERSATION the user watches, not an exchange xmux has on their behalf.
  Its PTY is on screen, its keys reach it, and what the pane already collected is typed
  into it: the host-key question once, the password once and only if the pane carried
  one. Every other prompt is left standing for the person looking at it, which is what
  makes a second factor, a key passphrase, and a prompt in any language something to
  answer rather than something to fail on.
- Nothing decides a login failed from what it read. A wrong password only means ssh will
  ask again, and the user answers that one better than a pattern can; the verdict is the
  child's exit code. Recognised auth-failure text only names a failure the exit already
  established.
- The conversation runs on its own thread, because every part of it - opening the PTY,
  spawning ssh, reading it - waits on something the single runtime thread must not wait
  on, and the frames that make the login watchable are drawn on that thread.

## Common Pitfalls

- Do not do display or PTY work here; that belongs to `src/display`.
- Do not block: the reader and writer run on their own threads and communicate
  with the app loop over channels.

## Before Editing

- Decide whether the change is metadata (here), display PTY (`src/display`), or
  transport dispatch (the host axis).
- For a new event, add the event variant, its arm in the state's event apply, and
  its effect follow-up together.

## Verification

- Check that ensure and reap stay idempotent, and that the new event reaches the
  nav through the state rather than through a side channel.
