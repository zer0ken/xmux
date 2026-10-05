# Working Notes: /docs

## Purpose

`docs/` holds the repository documentation that is neither the README nor the
glossary: the design principles, the functional requirements, the keybinding detail,
and the images the README shows. These notes also state the rules every committed
document follows.

## Mental Model

Documentation is the standard the code is checked against, not a description of
the code as it currently reads. It states behavior, contracts, and design rules,
so a rename or a refactor in the source is not a documentation change. It is
also part of the contributor and user interface, so it is current English prose.

## Module Seams

- `principles.md` states every design principle once, with its reason.
- `requirements.md` records functional requirements by stable ID.
- `keybind.md` documents key and mouse behavior for users.
- `assets/` holds the images the README shows.

## Invariants

### Working Notes Coverage

Every directory in the repository carries an `AGENTS.md`, even when the content is
short, and a new directory gets one when it is created. Working Notes are only
dependable as a pre-edit entry point if a reader can assume one exists wherever they
land: a missing file is indistinguishable from a directory with nothing worth saying.
Release preparation includes a coverage audit, so a directory added between releases
is not left undocumented.

### Working Notes Format

Working Notes are titled `Working Notes: <path>` and use these sections:

- `Purpose`
- `Mental Model`
- `Module Seams`
- `Invariants`
- `Common Pitfalls`
- `Before Editing`
- `Verification`

They describe the current codebase state. Active refactoring direction is expressed
as invariants, module seams, and pitfalls rather than as change history or phase
narrative.

### English Public Surface

Everything the project publishes is written in English: committed documentation
including Working Notes and code comments, commit messages, pull request titles and
bodies, issue titles, bodies, and comments, and release notes. `README.ko.md` is the
one exception: it is the Korean translation of `README.md` and is kept in step with
it. Release notes are generated from pull request titles, so a pull request title is
written as an English release note line. Text in another language that reaches the
public surface is translated as soon as it is found. Temporary files outside the
repository may use another language.

### Documentation Is the Standard

Durable documentation states behavior and design rules; the code is the subject
checked against it. A document names no test, function, method, field, enum variant,
source file, or library API. It may name what the design itself prescribes and what
the outside world already depends on: the two axes and their terms, the directory
layout a new module must fit, config keys, CLI and ctl verbs, socket names, and the
argv of the muxes xmux drives. It names no third-party authoring tool, plugin, or
product the project does not itself depend on.

A requirement is a behavior statement with a stable ID and no coverage line: which
tests cover it is answered by the test suite. Durable docs describe current behavior
only.

## Common Pitfalls

- Do not copy implementation history into user-facing docs.
- Do not cite a test name, a source file, or an identifier as evidence for a
  requirement: the requirement is the evidence the code is measured against.

## Before Editing

- Decide whether the change is a principle, a requirement, user docs, or a glossary
  term, and state it in that one place.
- Ask whether the sentence would survive a rename in the source. If it would not, it
  describes code and belongs in the code.

## Verification

- Confirm the change states behavior a reader can check the app against.
- Confirm no new source identifier, test name, or file path below the directory
  level entered the text.
