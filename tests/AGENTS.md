# Working Notes: /tests

## Purpose

Integration tests that run against the crate from outside it.

## Module Seams

- The architecture test enforces the Layer Direction table of the root `AGENTS.md` over
  every source file, test modules included.
- Password login, session discovery, and remote attachment are exercised by the
  isolated Docker hosts in `scripts/e2e/`.

## Invariants

- An allowed edge is changed in the root `AGENTS.md` table and in the architecture test
  together.
- Tests use synthetic inputs, disposable child processes, virtual terminals, or
  container hosts. They never require a user's machines, sessions, or console.
- Every test runs automatically in its supported platform's suite; no manual or
  ignored live-environment gates are required.
