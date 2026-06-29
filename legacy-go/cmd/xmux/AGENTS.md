# Working Notes: /legacy-go/cmd/xmux

## Purpose

This package contains the Go `xmux` command implementation kept for reference.

## Mental Model

The Rust `src/cli.rs`, `src/main.rs`, and related modules are the active CLI.
This package can help explain previous command behavior and test coverage.

## Module Seams

- CLI subcommands are wired here.
- Runtime behavior is delegated into `legacy-go/internal/` packages.

## Invariants

- Do not treat this package as the active command entry point.
- Keep reference behavior separate from Rust implementation decisions.

## Common Pitfalls

- Do not copy command behavior without checking current Rust requirements.
- Do not add new user-facing behavior here alone.

## Before Editing

- Locate the matching Rust command path.
- Check whether the Go tests document a behavior missing from Rust tests.

## Verification

- Run the matching Rust tests for active behavior.
- Run this Go package's tests only when this reference package changes.
