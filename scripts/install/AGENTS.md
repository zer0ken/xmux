# Working Notes: /scripts/install

## Purpose

The install scripts a release publishes: the POSIX shell script, the PowerShell script,
and the CMD script that hands off to the PowerShell one.

## Module Seams

- Each script reads the OS and architecture, downloads that build, and verifies its
  SHA-256 against the release checksum before installing.
- `INSTALL.md` documents their layout and options.

## Invariants

- The three scripts take the same options and end in the same install.
- A script writes only the user's own `PATH`, never the machine one, so it needs no
  elevation.
- A new version goes into its own directory and only the launcher is repointed, so a
  running xmux is never overwritten.
