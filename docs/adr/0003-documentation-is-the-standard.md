# ADR 0003: Documentation Is the Standard, Code Is the Subject

## Status

Accepted

## Context

Repository documentation had grown into a second description of the source: a
requirement carried the names of the tests covering it, Working Notes listed the
files and functions inside each module, and the architecture notes named traits,
fields, and enum variants.

That coupling inverts the relationship between the two. Renaming a test or
moving a function became a documentation change, so every code change carried a
documentation edit, and the edits that were skipped left text describing code
that no longer exists. A document that follows the source cannot also be the
thing the source is measured against.

A second kind of coupling had the same effect from outside: references to
particular authoring tools, plugins, and products used while writing the code.
The project does not depend on them, so they date the documentation without
telling a reader anything about xmux.

## Decision

Durable documentation states behavior and design rules. It is the standard; the
code is the subject checked against it.

A document does not name a test, a function, a method, a field, an enum
variant, a source file, or a library API.

A document may name what the design itself prescribes and what the outside world
already depends on: the two axes and their terms, the directory layout a
new module must fit, config keys, CLI and ctl verbs, socket names, and the argv
of the muxes xmux drives.

A document does not name a third-party authoring tool, plugin, or product that
the project does not itself depend on.

## Consequences

A requirement is a behavior statement with a stable ID and no coverage line.
Which tests cover it is answered by the test suite, not by the requirement.

Working Notes describe what a directory owns and what must hold inside it,
rather than enumerating its files and functions.

The test for any documentation sentence is whether it would survive a rename in
the source. If it would not, it is describing code and belongs in the code.
