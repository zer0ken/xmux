# Working Notes: /legacy-go/internal/attach

## Purpose

This Go package contains reference attach and switch behavior.

## Mental Model

Use it to compare how attach commands and switching decisions were represented
in the Go code. Active attach/display behavior lives in Rust `src/attach.rs`,
`src/cockpit.rs`, `src/display.rs`, and `src/proxy/`.

## Module Seams

- Command construction reference should map to Rust mux/backend/transport code.
- Runtime attach behavior should map to Rust cockpit/display/proxy code.

## Invariants

- Rust owns active attach behavior.
- Reference tests here do not verify Rust behavior.

## Common Pitfalls

- Do not copy attach behavior without checking Rust display attachment rules.
- Do not confuse native attach with the cockpit's live display path.

## Before Editing

- Find the matching Rust attach or display path.
- Check whether a Rust test should be added or updated instead.

## Verification

- Prefer Rust attach/cockpit/proxy tests for active behavior.
- Run this Go package's tests if this package changes.
