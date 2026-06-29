# Working Notes: /legacy-go/internal/discovery

## Purpose

This Go package contains reference host discovery behavior.

## Mental Model

Use it to compare source discovery and host selection behavior with active Rust
`src/discovery.rs`, `src/config.rs`, and `src/env.rs`.

## Module Seams

- Discovery reference maps to Rust discovery/config assembly.
- Runtime host state maps to Rust model/host management, not this package.

## Invariants

- Rust discovery owns active behavior.
- Discovery should stay separate from live host process ownership.

## Common Pitfalls

- Do not add runtime host tasks to discovery code.
- Do not assume reference discovery behavior is still the desired product rule.

## Before Editing

- Check current Rust discovery tests and requirements.
- Confirm whether the behavior is config parsing, source assembly, or runtime
  host management.

## Verification

- Prefer Rust discovery/config tests for active behavior.
- Run this Go package's tests if this package changes.
