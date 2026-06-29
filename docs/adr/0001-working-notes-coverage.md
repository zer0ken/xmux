# ADR 0001: Working Notes Coverage

## Status

Accepted

## Context

Working Notes are being introduced to give people and agents directory-local
architecture context and editing guardrails before they read or change code.

The first pass covers the directories most involved in the current
rearchitecture. Some smaller directories may not need much explanation yet, but
missing files make it harder to rely on Working Notes as a consistent pre-edit
entry point.

## Decision

The first pass will cover only the core architecture seams:

- repository root
- `src/`
- `src/backend/`
- `src/state/`
- `src/ui/`
- `src/proxy/`
- `src/model/`
- `legacy-go/`
- `docs/`

Before release, every directory in the repository must have a local
`AGENTS.md`, even when the content is short.

## Consequences

The first pass stays focused on the directories with the highest architectural
risk.

The release checklist must include a Working Notes coverage audit so newly
created or lower-risk directories are not left undocumented.
