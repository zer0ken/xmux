# Working Notes: /legacy-go/internal/session

## Purpose

This Go package contains reference session value behavior.

## Mental Model

Use it to compare session address and ordering behavior with active Rust
`src/session.rs`, `src/ui/tree.rs`, and model code.

## Module Seams

- Session value behavior maps to Rust session/model code.
- Tree ordering and filtering maps to Rust UI tree code.

## Invariants

- Rust session/model code owns active value semantics.
- Address formatting should stay consistent with Rust control and selection
  behavior.

## Common Pitfalls

- Do not change reference values without checking Rust address invariants.
- Do not conflate session values with live host or attachment state.

## Before Editing

- Check matching Rust session/model/UI tree tests.
- Confirm whether behavior is pure value logic.

## Verification

- Prefer Rust session/model/UI tree tests for active behavior.
- Run this Go package's tests if this package changes.
