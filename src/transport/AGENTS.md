# Working Notes: /src/transport

## Purpose

`transport/` is the HOST axis: how a mux argv reaches the machine it runs on, separate
from which mux runs there (`src/mux`). It owns argv assembly and per-implementation
execution wrapping only, never a server model and never a mux verb. The local
implementation runs a command on this machine, the ssh implementation wraps it in an
ssh connection, and the WSL implementation runs it inside a distribution on this
machine. Which implementation a host uses is read out of its name, and each transport
also carries the source id it answers as, so one machine serving several muxes is
several sources at one destination.

## Module Seams

- The module root holds the `Transport` trait, the host kind with the one match that
  maps a kind to a concrete transport, the factories, the switch execution shape, and
  the boxing impl that lets a stored transport pass where a borrowed one is expected.
- Each host implementation is its own module. The WSL module also lists this
  machine's distributions as host names, since that is launcher mechanics rather than
  roster policy.
- The shared shell helpers render an argv injection-safe for the POSIX shell an
  implementation hands its command to, and read which shell family answers a host.
- The credential broker holds a submitted password in process memory and serves it to
  ssh children through askpass.
- The ssh diagnostic reads OpenSSH's own failure text.
- Nothing in `transport/` imports a mux type or a source.

## Invariants

- The mux argv always comes from a mux command plan; a transport decides only HOW to
  run it, never WHAT.
- The capability predicates (remoteness, attach through a host shell, local registry
  authority, connection reuse, shell family) are independent: none derives from
  another, and no code reads them to pick a server model.
- The transport composes a fixed set of command shapes: a non-interactive command,
  an attach into the terminal handover and its command-line form, a control-mode
  child, a raw shell command (only the shell-based implementations answer), and a
  login command, a key-only login check, and closing a shared connection (only ssh
  answers these three). The key-only check holds no credential and shares no master.
- Every untrusted argv element crossing into a remote shell passes through the shared
  quoting, the single injection-safe boundary. A `cmd.exe` remote is not a supported
  target; supporting it means a second rendering chosen by shell family, never a
  weaker quoting.

## Before Editing

- A new host implementation is a module implementing `Transport` that overrides the
  capability predicates for its own combination, a factory, and a host-kind variant
  with one arm in each of the kind's methods. Its host names are recognizable from the
  name alone, as `local` and the WSL prefix are, and refused by the implementations
  that would otherwise claim them.

## Verification

- Pin the exact argv each dispatch emits, per implementation: that argv is the
  contract the rest of the app composes against.
- Exercise quoting changes against shell metacharacters and confirm the remote command
  joins quoted.
