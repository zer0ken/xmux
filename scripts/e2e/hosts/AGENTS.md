# Working Notes: /scripts/e2e/hosts

## Purpose

The host images the suite reaches over ssh, one per host system, and what runs on them.

## Module Seams

- `fetch-muxes.sh` installs the muxes that ship as release binaries, and pins their
  versions.
- `boot.sh` is the entry point: it runs `sessions.sh` as the remote user, then sshd.
- `sessions.sh` starts the sessions every scenario expects, and `whereami` names the
  session a shell runs in.
- `zellij-new` starts every zellij session the suite needs and returns once its server
  has set the session up.

## Invariants

- tuios and herdr live in the remote user's `~/.local/bin`, reachable only through the
  login profile.
- Every mux runs on both systems, or the README table says why it cannot.
- `sessions.sh` is safe to run again on a host whose sessions partly survive a restart.
- No zellij command runs while a zellij server the suite started is still setting up its
  session: zellij 0.45 panics such a server when any zellij command reaches it. Within
  `sessions.sh`, `zellij-new` waits for each server; against xmux's polls, sshd starts
  only after `sessions.sh` returns, and the suite counts a host as up only once sshd
  writes its pid file.
