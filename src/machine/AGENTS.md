# Working Notes: /src/machine

## Purpose

`machine/` is the HOST axis: how a mux argv reaches the server it runs on,
SEPARATE from which mux runs there (that is `src/mux`). It owns argv assembly and
the per-family execution wrapping only, never a server model and never a mux
verb.

## Mental Model

A host family is a `Transport` implementation. The local family runs a command on
this host, injecting the server socket for a non-default mux server; the
ssh family wraps the command in an ssh connection with the right tty, batch-mode,
and multiplexing options; the WSL family runs it inside a distribution on this
box, exec'd through a login shell so the launcher's own command-line parsing
never re-reads the quoting and the user's own mux is on `PATH`. Which family a
host belongs to is read out of its NAME, so a host named after launch reaches
its family without anything extra being threaded alongside it.

Each transport also carries the SOURCE ID it answers as, separate from where it
connects: one host running several muxes is several sources, all reaching the
same place, so the id cannot be the ssh destination.
The plain factories derive the id from the destination; their explicit variants
state it. A source holds one transport and never branches on which family it is:
it calls trait methods. This mirrors the MUX axis, where the mux trait plays the
same role.

## Module Seams

- The module root holds the `Transport` trait, the host kind and its
  construction method (the single construction-time match mapping a kind to a
  concrete transport), the factories that method delegates to plus the variants
  naming the source id explicitly, the lowered-switch execution shape, and the
  boxing impls that let a stored transport pass where a borrowed one is expected.
- Each host family is its own module: the local family issues no remote shell
  command and uses none of the shared vocabulary; the ssh family owns the
  private option assembly (tty, batch mode, multiplexing); the WSL family owns
  its launcher wrapping, and also the provider that lists this box's
  distributions as host names, because listing them is launcher mechanics
  rather than roster policy.
- The shared shell vocabulary renders an argv injection-safe for the POSIX shell
  a family hands its command to. It is the peer of the mux axis's own vocabulary.

The dependency is one-way: the shell-based families import the shared vocabulary,
and nothing in `machine/` imports a mux type or a source.

## Invariants

- `Transport` names no mux and no server model. Remoteness is a semantic
  ssh-versus-local marker only. What the mux sites actually read are the
  capability predicates: whether a display attach runs through a host shell (the
  gate deciding which source names its client's tty) and whether this box's mux
  registry is authoritative (the registry-merge gate). None of the three derives from another, and no code reads
  them to pick a server model.
- The host kind's own query methods are the ONLY code that matches on the
  kind: one maps a kind to a concrete transport, another reads its server socket.
  No match on the kind is scattered across call sites; the trait object carries
  the choice everywhere else.
- The transport lowers four shapes and no more: a non-interactive command, an
  attach into the terminal handover (local socket injection, or a shell session
  that folds the window pre-selection ahead of the attach, which lives here and
  never in the mux or the caller), a control-mode child, and a raw shell command
  (which only the shell-based families answer).
- A family that needs a terminal for the control child arranges one on the HOST
  side, the way the ssh family forces a pty. It never rewrites a mux flag to work
  around a pipe: which control payload runs is the mux's word, not the
  transport's.
- The mux argv always comes from a mux command plan; a transport only decides HOW
  to run it, never WHAT.
- Every untrusted argv element crossing into a remote shell passes through the
  shared quoting, the single injection-safe boundary.

## Common Pitfalls

- Do not add mux-kind knowledge here. If a decision needs the mux, it belongs in
  `src/mux` or the caller, not the transport.
- A boxed transport does not coerce to a borrowed trait object on its own; the
  blanket impl in the module root is what lets a stored transport be passed
  directly. Removing it forces an explicit reborrow at every call site.
- The shared quoting assumes a POSIX remote shell. A `cmd.exe` remote is NOT a
  supported target. Do not weaken the quoting to accommodate one without an
  explicit per-host shell feature.

## Before Editing

- Adding a host family: add its module with a type implementing `Transport`,
  overriding the capability predicates for its own combination rather than
  deriving them from remoteness, add its factory, and add a host-kind variant
  plus one arm in each of the kind's methods. The compiler forces every arm, and
  no match on the kind exists outside the kind itself. If the family needs its
  own host names, make them recognizable from the name alone, the way `local`
  and the WSL prefix are, and refuse that spelling in the families that would
  otherwise claim it.
- Adding per-host execution behavior to an existing family: edit that family
  and keep the shared shell vocabulary where it is.

## Verification

- Pin the exact argv each lowering emits, per family: that argv is the contract
  the rest of the app composes against.
- When touching quoting, exercise it against shell metacharacters and confirm the
  remote command joins quoted.
