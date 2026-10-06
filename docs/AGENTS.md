# Working Notes: /docs

## Purpose

`docs/` holds the documentation that is neither the README nor the glossary, and
these notes state the rules every committed document follows. Documentation is the
standard the code is checked against, not a description of the code as it reads.

## Module Seams

- `principles.md` states every design principle once, with its reason.
- `requirements.md` records functional requirements, one sentence per stable ID.
- `keybind.md` covers what the README's keys and the in-app help leave out.
- `assets/` holds the images the README shows.

## Invariants

- **Working Notes coverage.** Every directory carries an `AGENTS.md`, and a new
  directory gets one when it is created. A missing file is indistinguishable from a
  directory with nothing to say, so release preparation audits coverage.
- **Working Notes format.** Working Notes are titled `Working Notes: <path>` and hold
  `Purpose`, `Module Seams`, and `Invariants`, at most 60 lines. `Common Pitfalls`,
  `Before Editing`, and `Verification` appear only for items that belong to that
  directory alone; rules for the whole repository are stated once, in the root
  `AGENTS.md`. A design decision only one module can verify belongs in that module's
  documentation comments.
- **English public surface.** Everything the project publishes is English: committed
  documentation and code comments, commits, pull requests, issues, and release notes.
  `README.ko.md` is the one exception, the Korean translation kept in step with
  `README.md`. Other-language text found on that surface is translated; temporary
  files outside the repository may use another language.
- **Documentation is the standard.** A document states behavior and design rules, and
  names no test, function, method, field, enum variant, source file, library API, or
  third-party tool the project does not depend on. It may name what the design
  prescribes and the outside world depends on: the two axes and their terms, the
  directory layout, config keys, CLI and ctl verbs, socket names, and the argv of the
  muxes xmux drives. A sentence that would not survive a rename in the source belongs
  in the code.
- **One place per fact.** A principle is stated in `principles.md`, a term in
  `CONTEXT.md`, a requirement in `requirements.md`, and user instructions in the
  README or `keybind.md`; other documents point there instead of repeating it.
- Durable docs describe current behavior only, with no change history.
- A requirement has no coverage line: which tests cover it is the test suite's answer.
