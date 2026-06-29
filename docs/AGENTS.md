# Working Notes: /docs

## Purpose

`docs/` contains repository documentation: public user/developer docs, accepted
architecture decisions, functional requirements, and working planning material
kept under a separate subtree.

## Mental Model

Documentation in this repository is part of the contributor and user interface.
Public-facing documentation should be current English prose. `docs/superpowers/`
is working planning material and is not part of the public documentation
surface.

## Module Seams

- `requirements.md` records functional requirements and their test coverage.
- `keybind.md` documents cockpit prefix behavior for users.
- `adr/` records accepted documentation and architecture decisions.
- `superpowers/` stores working plans and specs used during development.

## Invariants

- Public repository documentation is written in English.
- Durable docs describe current behavior and accepted decisions.
- Planning material stays under `docs/superpowers/` until useful content is
  moved into current public docs.
- ADRs should record decisions, context, and consequences without becoming task
  logs.

## Common Pitfalls

- Do not copy implementation history into user-facing docs.
- Do not treat `docs/superpowers/` as release-ready public documentation.
- Do not add requirements without naming the tests or explicit live-verification
  evidence that covers them.

## Before Editing

- Decide whether the change is user docs, requirements coverage, ADR material,
  or working planning material.
- Keep public docs aligned with the current Rust implementation.
- Check `CONTEXT.md` when documenting module seams or refactoring direction.

## Verification

- Run `rg` for stale planning words in public docs when moving content out of
  `docs/superpowers/`.
- Check requirement IDs and test names against the code before updating
  `requirements.md`.
