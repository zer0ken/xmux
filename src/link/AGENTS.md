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
- A POLL source is enumerated ONCE per spawn and the task then parks. It keeps no cadence
  of its own: a sweep that repeats on a timer is a connection to that machine that repeats
  on a timer, whether or not anyone is waiting for the answer. Re-enumerating is
  abort-and-respawn, and only something the user did raises it.
- The parked task is what keeps the channel LIVE, and that is why it parks rather than
  returning. A finished task reads as a dropped channel, so every path that ensures a
  host's channel would enumerate it again, and a keystroke on a selected card would
  become a connection.
- Ensuring a channel is not a request. It opens one a host does not have and leaves a
  host that has one exactly as it stands, which is what lets the input paths call it on
  every keystroke.
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
- What the login is FOR rides the login's own session. Registering a key is a remote
  command on that ssh, not a connection opened afterwards, because a side that cannot
  multiplex has no afterwards - and there the key is the whole point, since it ends the
  password the next probe could not supply. Composing that command may make this machine
  a key pair, so it is composed on the login's thread and never on the runtime's.
- A side that cannot multiplex still runs the login. ssh asks about the host key BEFORE it
  authenticates and writes the answer to `known_hosts`, so accepting a key is a login
  whose whole result outlives the connection; recording the values is another. Only the
  reuse is lost, so only the reuse is refused: a host that then needs a password is asked
  again on the next probe, which is the truth about that machine on that platform rather
  than a reason to have refused the login.
- The login is an exchange xmux has on the user's behalf, not a screen. The PTY exists
  because ssh reads a password from a terminal and from nowhere else; nothing renders it
  and nothing typed reaches it. What the pane collected is what answers: the host-key
  question once, the password once and only if the pane carried one.
- The verdict is the child's exit code. A wrong password only means ssh asks again, so
  recognised auth-failure text only names a failure the exit already established.
- Because the verdict is that exit code, the remote command the login carries MUST end by
  reporting the AUTHENTICATION and nothing else, in a word every shell family has. What it
  carries rides along without a vote. A locked host's shell family is unknown by
  construction: the probe that would have read it never got past the refusal that locked
  the card, so a word only one family has (`true`) turns an accepted password into a
  refused one on a remote from another family. A carried step that failed is reported by
  the next probe telling the truth about the host, not by a login that looks refused.
- A prompt the pane's values cannot answer ENDS the login, because nobody is there to
  answer it: a second password prompt is an auth failure, and a password prompt with no
  password in the pane is a server asking for what the pane is missing. A prompt that is
  neither is left to ssh and ends the login on the idle budget.
- The conversation runs on its own thread, because every part of it - opening the PTY,
  spawning ssh, reading it - waits on something the single runtime thread must not wait
  on. The runtime holds only the handle that cancels it.

## Common Pitfalls

- Do not do display or PTY work here; that belongs to `src/display`.
- Do not block: the reader and writer run on their own threads and communicate
  with the app loop over channels.
- Do not answer a failure with a request. A channel that died, a probe that was refused,
  and an attachment that EOF'd are all states to report, never reasons to reconnect.

## Before Editing

- Decide whether the change is metadata (here), display PTY (`src/display`), or
  transport dispatch (the host axis).
- For a new event, add the event variant, its arm in the state's event apply, and
  its effect follow-up together.

## Verification

- Check that ensure and reap stay idempotent, and that the new event reaches the
  nav through the state rather than through a side channel.
