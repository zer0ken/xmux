# Working Notes: /legacy-go/internal/mux

## Purpose

This Go package contains reference mux command construction, parsing, and target
handling.

## Mental Model

Use it to compare mux command strings and parser behavior with active Rust
`src/mux.rs` and `src/backend/`.

## Module Seams

- Command construction maps to Rust mux/backend methods.
- Parsing maps to Rust mux parsing and session/window value construction.

## Invariants

- Rust backend and mux code own active mux behavior.
- Transport wrapping belongs outside mux command semantics.

## Common Pitfalls

- Do not mix ssh/local transport decisions into mux command rules.
- Do not copy parser behavior without matching Rust tests.

## Before Editing

- Check the matching Rust mux/backend tests.
- Decide whether the behavior is mux syntax or transport lowering.

## Verification

- Prefer Rust mux/backend/model tests for active behavior.
- Run this Go package's tests if this package changes.
