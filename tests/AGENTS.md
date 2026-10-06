# Working Notes: /tests

## Purpose

Integration tests that run against the crate from outside it.

## Module Seams

- The architecture test enforces the Layer Direction table of the root `AGENTS.md` over
  every source file, test modules included.
- The live password test is ignored by default and runs only against a host named by its
  `XMUX_LIVE_PW_*` environment variables.

## Invariants

- An allowed edge is changed in the root `AGENTS.md` table and in the architecture test
  together.
- A test that needs a real host stays ignored by default, so the suite runs offline.
