# Working Notes: /src/provision

## Purpose

`provision` resolves what exists on this machine: which machines, mux binaries, and
sources xmux offers. It loads the optional TOML config and merges it with
ssh-config discovery, names the ssh targets a source can reach, probes sources
concurrently to enumerate their sessions, and resolves all of it into the source
list and lookups the commands share, re-resolved on every re-scan.

## Mental Model

Provisioning is the "what is out there" layer, distinct from the axes that act on
it: `transport` decides how a command REACHES a host and `mux` decides what runs
there. Config is the on-disk starting point; the roster answers only which ssh
targets exist; discovery isolates each source so one unreachable mux never blocks
the rest; the resolved environment threads a single source-list answer into both
the source list and the runtime registry so the two cannot disagree on which
sources exist.

## Module Seams

- Config loads the optional TOML and merges it with ssh-config discovery to
  produce the set of hosts and mux binaries to use.
- Roster answers only "which hosts does xmux offer", from one or more providers
  that each yield plain ssh target names.
- The neighbour provider answers "which machines does this OS already reach in one hop",
  from the OS's own network state rather than any VPN's client: the routes to single
  addresses, and the neighbour table. It narrows what those give - an entry naming no
  machine, a hardware address answering for many addresses - then keeps only what
  answers ssh, and names the survivors.
- A provider offers a machine from one of two kinds of evidence, and a roster swap must
  read absence differently for each. A RECORD naming it (`~/.ssh/config`, `[[hosts]]`, the
  wsl list) is authoritative both ways, so a record that stops naming it removes it. A
  PROBE answering is authoritative only one way: it proves the machine is there, and its
  silence proves nothing, because the neighbour probe offers a machine by reaching port 22
  inside a bounded budget that a tunnel hop misses with nothing wrong. So a machine only a
  probe offered is CARRIED into a roster that lost it, with its sources, its provider and
  its address. A machine that really left keeps its card and reports itself unreachable,
  which is what a recorded machine does when it goes offline; both kinds behave alike, and
  no re-scan tears down a card the user is working in.
- A name is asked for in the order of who would know it: the system resolver, which holds
  what someone registered, then the machine itself over mDNS, which knows what it calls
  itself whether or not anyone registered it. A name is adopted only when this machine can
  resolve it back, because the name is also the ssh destination: the bare label first, the
  full name where the label means nothing outside its own domain, and the address where
  neither resolves.
- Those two records are read from the OS itself wherever the OS has a way to be asked:
  netlink on Linux and Android, IP Helper on Windows. Only the unixes with neither are
  asked through a command. The netlink socket is never bound: an app on Android may not
  bind one, and a dump does not need it, which is why `ip` fails there on every
  subcommand while these dumps answer.
- A record the OS REFUSES is not a record that is empty. Android denies the neighbour
  table and allows the routing table, so the refusal is carried rather than flattened:
  where the neighbour table is refused, the link this machine holds an address in is
  asked address by address instead, and `doctor` says which record was refused.
- A route names a machine when it names few enough addresses to be machines. A VPN that
  summarises its peers hands the OS one route for two of them, so a block of up to eight
  is read as its addresses; a wider one is a network and contributes nothing.
- Discovery probes every source concurrently, isolating each so one unreachable
  mux never fails the rest, with bounded concurrency, a per-source timeout, and
  order-preserving results.
- Env resolves the source list and the lookups the commands share, owning the
  concurrent scan and the side-effecting operations over the live mux.

## Invariants

- The roster is separate from the transport axis (how a command reaches a host)
  and from discovery (scanning a source for sessions).
- A provider that cannot run yields an empty list rather than an error.
- The roster names no vendor. A provider reads what the OS knows, so a network xmux has
  never heard of is offered on the same terms as one it has, and installing or removing a
  VPN's own tooling changes nothing about which machines appear.
- This box is told from its neighbours by the connection, not by a list of its own
  addresses: a connection whose two ends carry the same address reached here.
- The resolved source list is the single answer threaded into both the source list
  and the runtime registry.

## Common Pitfalls

- Do not put transport decisions or mux verbs into provisioning; a source builds
  its transport and mux from the resolved config, and execution belongs to them.

## Before Editing

- Decide whether the change is config loading, roster policy, scan behavior, or
  resolution of the runtime view.

## Verification

- Exercise config merge, roster provider failure isolation, discovery fan-out, and
  resolution consistency between the source list and the registry.
