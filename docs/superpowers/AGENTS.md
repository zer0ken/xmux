# Working Notes: /docs/superpowers

## Purpose

`docs/superpowers/` stores working planning material used during development.
It is not part of the public documentation surface.

## Mental Model

This tree can contain task plans, specs, and development notes with narrower
context than public docs. Useful durable content should move into public English
docs, ADRs, requirements, or Working Notes before release.

## Module Seams

- `plans/` stores execution plans.
- `specs/` stores design/specification notes.
- Public documentation belongs outside this subtree.

## Invariants

- Do not treat files here as release-ready public docs.
- Keep public-facing facts outside this subtree when they become durable.
- Preserve enough context for active development work to be resumed.

## Common Pitfalls

- Do not cite this subtree as the source of truth for user behavior.
- Do not leave durable architecture rules only in this subtree.

## Before Editing

- Decide whether the content is working material or public documentation.
- Move stable decisions to ADRs or Working Notes.

## Verification

- For release preparation, audit this subtree and remove or replace public-value
  content with current public docs.
