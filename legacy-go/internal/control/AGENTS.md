# Working Notes: /legacy-go/internal/control

## Purpose

This Go package contains reference control protocol and server behavior.

## Mental Model

Active control behavior lives in Rust `src/control.rs` and `src/ui/run.rs`.
The Rust control surface favors semantic `Operation` verbs, with raw injection
kept behind `raw:`.

## Module Seams

- Protocol framing reference maps to Rust control framing.
- Server behavior reference maps to Rust UI control serving and cockpit command
  dispatch.

## Invariants

- Rust ctl semantics are authoritative.
- Raw key/text behavior is low-level compatibility, not the preferred public
  automation path.

## Common Pitfalls

- Do not revive old public raw-key verbs without checking Rust ctl invariants.
- Do not bypass `model::Operation` for user-facing automation.

## Before Editing

- Check Rust control parser and dispatch tests.
- Identify whether behavior is semantic operation or raw injection.

## Verification

- Prefer Rust control and UI run tests.
- Run this Go package's tests if this package changes.
