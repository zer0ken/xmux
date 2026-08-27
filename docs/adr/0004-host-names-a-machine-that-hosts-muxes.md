# ADR 0004: Host Names a Machine That Hosts Muxes

## Status

Accepted

## Context

The vocabulary had one word doing two jobs and another naming nothing.

`host` meant a machine in some sentences and a machine paired with one mux in
others. The two readings agree only while every machine serves a single mux. As
soon as one serves two, they split: the runtime object keyed `local:zellij` was
called a host while the card above it printed `local` in its host slot, and a
sentence like "each host keeps its own live PTY attachment" was true of the
pairing and false of the machine.

`machine` named the execution axis, but it is a generic word. Every computer is a
machine whether xmux can reach it or not, so the word says nothing about the
model: it does not distinguish the machines xmux offers from the rest of the
world.

## Decision

A HOST is a machine that hosts muxes and that xmux can reach. The `roster`
decides the set: of all the machines there are, the hosts are the ones it names.

A SOURCE is one mux on one host. A host serving several muxes is several sources
under one host, and the host is the half of a source id that survives when the
mux half is dropped.

`machine` names no abstraction. It stays the plain word for the thing in the
world, used when the sentence is about hardware rather than about the model.

The execution axis is the HOST axis, and `Transport` is its trait. A `Transport`
reaches a host; it is not one, and several transports may reach the same host.

## Consequences

A source's section title `{host}/{mux}` is the two halves of a source id, which is
what it always printed.

Every sentence that said host and meant the pairing says source instead: the
per-source PTY attachment, the per-source control client, source events, the
runtime source registry, per-source inventory.

The test for such a sentence is whether it stays true when one host serves two
muxes. If it does not, it is about a source.

Two directory names sat against the vocabulary: `src/machine/` held the host
axis, and `src/host/` held per-source connection management. The later
reorganization resolved the collision by renaming them to `src/transport/` (the
Transport axis) and `src/link/` (per-source connection management), leaving the
domain type `Host` in `src/model/host.rs`.

The scan indicator prints `scanning hosts` while the things it counts are
sources.
