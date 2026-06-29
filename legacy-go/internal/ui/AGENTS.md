# Working Notes: /legacy-go/internal/ui

## Purpose

This Go package contains reference switcher UI, tree, ANSI, and behavior tests.

## Mental Model

Use it to compare UI behavior with active Rust `src/ui/`, `src/cockpit.rs`, and
`src/proxy/`. Rust UI behavior is split between pure tree transforms,
interactive switcher state, control dump rendering, and cockpit focus/input
routing.

## Module Seams

- Pure tree behavior maps to Rust `src/ui/tree.rs`.
- Interactive switcher behavior maps to Rust `src/ui/switcher.rs`.
- Terminal/proxy behavior maps to Rust `src/proxy/` and cockpit.

## Invariants

- Rust UI tests verify active UI behavior.
- Modal/menu/input ownership must be checked in Rust before porting behavior.

## Common Pitfalls

- Do not copy reference UI behavior without checking Rust focus and modal
  routing.
- Do not add side effects to Rust pure tree transforms.

## Before Editing

- Locate the matching Rust UI or cockpit surface.
- Check whether behavior is pure tree data, rendering, input routing, or side
  effect.

## Verification

- Prefer Rust UI/cockpit/proxy tests for active behavior.
- Run this Go package's tests if this package changes.
