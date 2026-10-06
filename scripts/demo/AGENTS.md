# Working Notes: /scripts/demo

## Purpose

The recording setup for the README demo GIFs.

## Module Seams

- The setup stages sessions, an ssh config, and an xmux config inside a container,
  records the terminal, and encodes the GIFs into `docs/assets/`. Its README lists each
  GIF.

## Invariants

- Demo assets are recorded only in this isolated environment, never on a real machine,
  so no real machine name reaches a public asset.
