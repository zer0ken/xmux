# Working Notes: /legacy-go/internal

## Purpose

`legacy-go/internal/` contains Go implementation packages kept as behavioral
reference material.

## Mental Model

These packages can clarify parsing, command construction, switcher behavior, and
test cases. Active behavior belongs in Rust.

## Module Seams

- Each subdirectory is a Go package.
- Cross-package behavior here should be mapped to the closest Rust module before
  changing active code.

## Invariants

- Go tests explain reference behavior; Rust tests verify active behavior.
- Keep this tree independent from Rust code.

## Common Pitfalls

- Do not assume Go package boundaries match Rust module boundaries.
- Do not implement active fixes only in the reference tree.

## Before Editing

- Identify the active Rust module related to the behavior.
- Treat edits here as reference maintenance.

## Verification

- Run Rust tests for active behavior changes.
- Run Go tests for edited Go packages.
