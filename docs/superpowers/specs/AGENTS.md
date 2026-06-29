# Working Notes: /docs/superpowers/specs

## Purpose

This directory stores development specs used to reason about design work.

## Mental Model

Specs here support implementation work. Public architecture decisions and stable
module rules should be copied into ADRs, requirements, or Working Notes.

## Module Seams

- Keep exploratory design material here.
- Keep accepted contributor guidance in public docs outside `docs/superpowers/`.

## Invariants

- Specs should distinguish facts about the code from candidate design ideas.
- Stable behavior claims should have a verification path.

## Common Pitfalls

- Do not treat exploratory specs as public API documentation.
- Do not let a spec contradict Working Notes without resolving the conflict.

## Before Editing

- Check whether the content belongs in an ADR or requirements doc instead.
- Keep code references concrete.

## Verification

- Re-check referenced modules before reusing a spec for implementation work.
