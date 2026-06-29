# Working Notes: /legacy-go/internal/manage

## Purpose

This Go package contains reference management and preview behavior.

## Mental Model

Use it to compare create/rename/kill or preview behavior with active Rust
`src/manage.rs`, `src/ui/ops.rs`, and cockpit/UI paths.

## Module Seams

- Management command behavior maps to Rust manage/backend/transport code.
- Preview behavior maps to Rust display/proxy/UI rendering paths.

## Invariants

- Active management behavior is implemented and tested in Rust.
- Slow or remote management operations should not block UI input/rendering.

## Common Pitfalls

- Do not put active management behavior only in the Go reference.
- Do not move slow operations into Rust UI key handling.

## Before Editing

- Locate the matching Rust management operation and test.
- Preserve the separation between UI intent and side-effecting work.

## Verification

- Prefer Rust manage/UI operation tests for active behavior.
- Run this Go package's tests if this package changes.
