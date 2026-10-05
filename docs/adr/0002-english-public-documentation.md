# ADR 0002: English Public Project Surface

## Status

Accepted

## Context

xmux is an open source project. Everything the repository and its GitHub project
publish is part of the public experience for contributors, users, and coding agents,
and one language keeps it readable to all of them.

Temporary planning or scratch files may exist outside the repository and do not
affect the published project.

## Decision

Everything the project publishes is written in English:

- documentation committed to the repository, including Working Notes and code comments
- commit messages
- pull request titles and bodies
- issue titles, bodies, and comments
- release notes

`README.ko.md` is the one exception: it is the Korean translation of `README.md` and
is kept in step with it.

Temporary files outside the repository may use another language.

## Consequences

Release notes are generated from pull request titles, so a pull request title is
written as an English release note line.

Text in another language that reaches the public surface is translated before it is
published, or as soon as it is found.
