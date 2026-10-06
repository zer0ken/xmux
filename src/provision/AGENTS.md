# Working Notes: /src/provision

## Purpose

`provision` resolves what is out there: which machines, mux binaries, and sources xmux
offers, re-resolved on every re-scan. It is distinct from the axes that act on that
answer: `transport` decides how a command reaches a host and `mux` decides what runs
there. Config is the on-disk starting point, the roster answers which ssh targets
exist, discovery enumerates each source in isolation, and the resolved environment
threads one source-list answer into both the source list and the runtime registry.

## Module Seams

- Config loads the optional TOML, merges it with ssh-config discovery, and resolves the
  login pane's starting values and matching host stanza as pure configuration results.
- The roster answers only "which hosts does xmux offer", from providers that each yield
  plain ssh target names.
- The neighbour provider reads the machines this OS already reaches in one hop from the
  OS's own routing and neighbour records, narrows them, keeps what answers ssh, and
  names the survivors.
- The OS records are read through the OS interface where one exists (netlink on Linux
  and Android, IP Helper on Windows); only the unixes with neither are asked through a
  command.
- Discovery probes every source concurrently with bounded concurrency, one budget per
  source shared by first contact and enumeration, and order-preserving results.
- The resolved environment owns the source list, the shared lookups, the credential
  store for the run, the concurrent scan, and the side-effecting operations over the
  live mux.

## Invariants

- All xmux-owned paths and local SSH records share one home resolution on every
  platform: nonempty `HOME`, then nonempty `USERPROFILE`, then the platform profile
  directory. If none resolves, the current directory is used with a warning.
  Config, state, sockets, logs, and xmux-owned registries follow this home, as do SSH
  config, tilde includes, and local keys. Paths owned by other programs follow their
  own rules: psmux uses the platform home (the real profile on Windows), and zellij
  uses `%APPDATA%` on Windows.
- The roster is separate from the transport axis (how a command reaches a host) and
  from discovery (scanning a source for sessions).
- A provider that cannot run yields an empty list rather than an error.
- The roster names no vendor. A provider reads what the OS knows, so a network xmux has
  never heard of is offered on the same terms as one it has, and installing or removing
  a VPN's own tooling changes nothing about which machines appear.
- A machine named by a record leaves when the record stops naming it; a machine only a
  probe offered is carried into a roster that lost it, so no re-scan tears down a card
  the user is working in.
- A record the OS refuses is reported as refused, never flattened into an empty one.
- This box is told from its neighbours by the connection, not by a list of its own
  addresses: a connection whose two ends carry the same address reached here.
- The resolved source list is the single answer threaded into both the source list and
  the runtime registry.

## Common Pitfalls

- Transport decisions and mux verbs do not belong here; a source builds its transport
  and mux from the resolved config, and execution belongs to them.
