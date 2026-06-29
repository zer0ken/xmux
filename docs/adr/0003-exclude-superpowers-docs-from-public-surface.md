# ADR 0003: Exclude Superpowers Docs From Public Surface

## Status

Accepted

## Context

`docs/superpowers/` contains working planning material produced during active
development. Some of it is useful while the rearchitecture is in progress, but it
is not part of the public documentation experience for an open source release.

The repository documentation policy requires committed public documentation to
be written in English.

## Decision

`docs/superpowers/` is not part of the public documentation surface.

Before release, the published repository state must exclude `docs/superpowers/`.
Any still-useful content from that tree should be moved into current English
documentation elsewhere, such as Working Notes, ADRs, requirements, or
architecture docs.

## Consequences

Development can continue using `docs/superpowers/` while the rearchitecture is
in progress.

Release preparation must include a documentation-public-surface audit that
removes or replaces `docs/superpowers/` before publication.
