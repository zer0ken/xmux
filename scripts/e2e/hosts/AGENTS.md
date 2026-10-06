# Working Notes: /scripts/e2e/hosts

## Purpose

The host images the suite reaches over ssh, one per host system, and what runs on them.

## Module Seams

- `fetch-muxes.sh` installs the muxes that ship as release binaries, and pins their
  versions.
- `sessions.sh` starts the sessions every scenario expects, and `whereami` names the
  session a shell runs in.

## Invariants

- tuios and herdr live in the remote user's `~/.local/bin`, reachable only through the
  login profile.
- Every mux runs on both systems, or the README table says why it cannot.
- `sessions.sh` is safe to run again on a host whose sessions partly survive a restart.
