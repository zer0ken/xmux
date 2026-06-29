# Working Notes: /legacy-go/internal/config

## Purpose

This Go package contains reference config and ssh-config parsing behavior.

## Mental Model

Use it to compare config defaults, host discovery inputs, and ssh config parsing
against active Rust `src/config.rs`, `src/discovery.rs`, and `src/env.rs`.

## Module Seams

- Config parsing reference maps to Rust config/env assembly.
- SSH config behavior maps to Rust discovery.

## Invariants

- Rust config code owns active parsing behavior.
- Reference behavior should not override current Rust requirements.

## Common Pitfalls

- Do not change only the Go parser for an active config bug.
- Do not assume old defaults match current Rust defaults.

## Before Editing

- Check the matching Rust config and discovery tests.
- Keep reference edits focused on preserved behavior.

## Verification

- Prefer Rust config/discovery tests for active behavior.
- Run this Go package's tests if this package changes.
