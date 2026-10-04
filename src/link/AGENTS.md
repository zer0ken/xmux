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
- A POLL source is enumerated when something asks for it - the launch scan or an
  explicit re-scan. Its task then keeps re-enumerating the host on a cadence ONLY when
  the transport reuses a connection the machine already holds open (the local box, a WSL
  distribution, an ssh master this side shares), and only while the host keeps answering.
  Over any other path the task runs one enumeration and returns, because there every
  repeat is a fresh login.
- The first failed enumeration ends the task. Re-arming a POLL source is abort-and-respawn,
  and only an explicit re-scan raises it: selecting the card and a probe both leave a
  stopped host as it stands.
- Ensuring a channel is not a request. It opens one a host does not have and leaves a
  host that has one exactly as it stands, which is what lets the input paths call it on
  every keystroke.
- A remote host's REACHABILITY (connected, blocked, or unreachable) is classified by a
  machine probe (`ssh <machine> true`) before any channel opens, not by the control
  reader. The reader's exit reason carries only a protocol `%error` the mux sent on its
  own (a "no sessions" / "no server" empty mux), so a reachable-but-empty host is told
  from one that answered; a control channel opens only for a machine already known to
  connect. An error answering a command xmux sent (a display-tty readback with no record
  file, a client flag an older mux lacks) answers that command and never becomes an exit
  reason.
- A control stream ends with exactly one exit: the mux's own exit notice, or the end of
  a stream that closed without one.
- A mux that ends a control client which had already listed sessions has DETACHED it,
  and the host still serves its other sessions: tmux does this to a control client
  whose attached session was destroyed. The card stands as the mux last reported it and
  the channel is opened once more, which attaches to another session. Only a reopened
  channel that lists sessions again can be reopened on its next detach, so a reopen that
  fails takes the ordinary exit: an empty host when the mux reports no sessions, and
  unreachable otherwise.
- A login starts with one pending process-memory credential for the machine and runs an
  ordinary ssh command through the same execution shape every later command uses. Only a
  successful login promotes that exact credential only when askpass served it; a
  key-authenticated login discards the unused password. Every ssh command carries argv
  and child environment together. No spawn site may separate them.
- A child with a held password forces askpass and permits one password answer. Its
  environment carries an opaque token for a private local broker, never the password.
  The helper answers password and keyboard-interactive password prompts only for an exact
  account-and-host match against the target alias, resolved host name, or host-key alias.
  A destination configured with `ProxyJump` or `ProxyCommand` does not enter the password
  path because the proxy would inherit askpass. It refuses other prompts without consuming the token, along with host-key questions,
  passphrases, passcodes, and one-time codes. Its token remains valid until the child is reaped. A child
  without a held password uses batch mode. A tty attach forces askpass where supported,
  so no password can enter the terminal view. Older Unix clients are detached from the
  controlling terminal; older Windows clients do not enter the password path.
- Only a submitted login whose effective policy is `ask` accepts a new host key. An
  explicit `yes` is never weakened; an unknown key under that policy is unreachable and
  names the fingerprint command. Other commands preserve the user's ssh
  policy, and no command accepts a changed key. Connection sharing remains enabled where
  supported, as an optimization only.
- The login worker runs off the runtime thread, captures ssh's own stdout and stderr,
  removes terminal control sequences and prompts, bounds the result, and categorizes a
  refused password, unreachable host, host-key mismatch, server close after
  authentication, timeout, cancellation, or other failure. That result is separate from
  later probe failures. A refusal that did not receive a held password remains visible.
- A command removes a credential only when that command received its token's password,
  exited with ssh's connection-failure status, and emitted ssh's own authentication
  refusal line. Removal is token-scoped, so a late result cannot remove a newer login.
  Cancellation and replacement remove only the matching pending credential. Removal
  immediately invalidates outstanding tokens and releases the held plaintext. The secret
  is never placed in an argument, environment, log, rendered frame, status, or file. The
  held credential allocation and current password-field allocation are overwritten in
  full when released; transient terminal and IPC buffers remain process memory.
- Registering a key is an ordinary ssh command using the same machine credential. The
  login command reads the remote shell family without assuming POSIX syntax, then the
  family-specific registration runs off the runtime thread. Its outcome is registered,
  skipped with a reason, or failed with ssh's reason, and appears in the completion
  message, log, and host information.

## Common Pitfalls

- Do not do display or PTY work here; that belongs to `src/display`.
- Do not block: the reader and writer run on their own threads and communicate
  with the app loop over channels.
- Do not answer a failure with a request. A channel that died, a probe that was refused,
  and an attachment that EOF'd are all states to report, never reasons to reconnect. A
  detach is not a failure: the mux said over the open stream that it ended this client
  while it keeps serving, and that is the one exit that reopens the channel, once.

## Before Editing

- Decide whether the change is metadata (here), display PTY (`src/display`), or
  transport dispatch (the host axis).
- For a new event, add the event variant, its application-update arm, and its
  effect follow-up together.

## Verification

- Check that ensure and reap stay idempotent, and that the new event reaches the
  nav through the state rather than through a side channel.
