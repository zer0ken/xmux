# Working Notes: /legacy-go

## Purpose

`legacy-go/` contains the Go implementation kept for reference. The active
application lives in the Rust crate at the repository root.

## Mental Model

Use this tree to compare behavior, command construction, parsing, and UI test
coverage when the Rust implementation needs a reference point. Changes here
should be rare and should not become the path for new product behavior.

## Module Seams

- `cmd/xmux/` is the Go CLI entry point.
- `internal/source`, `internal/mux`, `internal/session`, and `internal/config`
  contain discovery, mux command, session, and config behavior.
- `internal/ui` contains the Go switcher UI.
- `internal/control`, `internal/attach`, `internal/manage`, and
  `internal/discovery` contain supporting runtime behavior.

## Invariants

- The Rust crate is the active implementation.
- Behavior learned from this tree should be ported intentionally into Rust, not
  wired across language boundaries.
- Legacy tests can explain behavior but do not verify the Rust code.

## Common Pitfalls

- Do not fix a Rust behavior gap by changing only the Go reference.
- Do not assume package names map one-to-one to the Rust module seams.
- Do not copy old UI or control-channel behavior without checking the current
  Rust architecture.

## Before Editing

- Confirm whether the requested change belongs in active Rust code instead.
- If this tree is used as reference, identify the matching Rust module and tests.

## Verification

- Prefer Rust tests for active behavior.
- If this tree is edited, run the relevant Go package tests from `legacy-go/`.
