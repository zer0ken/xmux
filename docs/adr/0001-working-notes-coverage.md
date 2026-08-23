# ADR 0001: Working Notes Coverage

## Status

Accepted

## Context

Working Notes give people and agents directory-local architecture context and
editing guardrails before they read or change code. They are only dependable as
a pre-edit entry point if a reader can assume one exists wherever they land. A
missing file is indistinguishable from a directory with nothing worth saying, so
the reader falls back to reading code and the convention stops paying.

## Decision

Every directory in the repository carries a local `AGENTS.md`, even when the
content is short.

A new directory gets its Working Notes when it is created, using the format
`CONTEXT.md` defines.

## Consequences

Release preparation includes a Working Notes coverage audit, so a directory
added between releases is not left undocumented.

Directories with little architectural risk still carry a short file rather than
none, which keeps the entry point uniform.
