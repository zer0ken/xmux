# Working Notes: /docs

## Purpose

`docs/` contains repository documentation: public user and developer docs,
accepted architecture decisions, and functional requirements.

## Mental Model

Documentation is the standard the code is checked against, not a description of
the code as it currently reads. It states behavior, contracts, and design rules,
so a rename or a refactor in the source is not a documentation change. It is
also part of the contributor and user interface, so it is current English prose.

## Module Seams

- `requirements.md` records functional requirements by stable ID.
- `keybind.md` documents app prefix behavior for users.
- `adr/` records accepted documentation and architecture decisions.

## Invariants

- Public repository documentation is written in English.
- Durable docs describe current behavior and accepted decisions.
- A document names no test, function, method, field, or library API. It may name
  what the design prescribes and what the outside world depends on: the two axes
  and their vocabulary, the directory layout, config keys, CLI and ctl verbs,
  socket names, and the argv of the muxes xmux drives.
- A requirement is a behavior statement with a stable ID, not a coverage record.
- ADRs should record decisions, context, and consequences without becoming task
  logs.

## Common Pitfalls

- Do not copy implementation history into user-facing docs.
- Do not cite a test name, a source file, or an identifier as evidence for a
  requirement: the requirement is the evidence the code is measured against.
- Do not name a third-party tool, plugin, or product that the project does not
  itself depend on.

## Before Editing

- Decide whether the change is user docs, requirements, or ADR material.
- Ask whether the sentence would survive a rename in the source. If it would
  not, it is describing code rather than stating a rule.
- Check `CONTEXT.md` when documenting module seams or refactoring direction.

## Verification

- Confirm the change states behavior a reader can check the app against.
- Confirm no new source identifier, test name, or file path below the directory
  level entered the text.
