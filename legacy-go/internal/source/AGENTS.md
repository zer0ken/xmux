# Working Notes: /legacy-go/internal/source

## Purpose

This Go package contains reference source, runner, quoting, and classification
behavior.

## Mental Model

Use it to compare command execution and source behavior with active Rust
`src/source.rs`, `src/env.rs`, `src/model/transport.rs`, and `src/backend/`.

## Module Seams

- Source compatibility behavior maps to Rust source/env code.
- Machine execution semantics map to Rust transport.
- Mux behavior maps to Rust backend.

## Invariants

- Rust `Transport` and `Backend` are the preferred homes for new execution and
  mux semantics.
- Reference source behavior should not create a new active ownership model.

## Common Pitfalls

- Do not add new Rust behavior by copying source logic without choosing the
  correct Rust seam.
- Do not mix quoting, transport, and mux policy when a Rust module already owns
  one of those concerns.

## Before Editing

- Check the matching Rust source/env/transport/backend code.
- Identify whether the behavior is compatibility, execution, or mux semantics.

## Verification

- Prefer Rust source/transport/backend tests for active behavior.
- Run this Go package's tests if this package changes.
