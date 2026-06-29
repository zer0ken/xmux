# Working Notes: /legacy-go/cmd

## Purpose

`legacy-go/cmd/` contains Go command entry packages kept for reference.

## Mental Model

Use this directory to compare CLI behavior with the active Rust implementation.
It does not define active behavior by itself.

## Module Seams

- Command wiring belongs under `cmd/xmux/`.
- Shared behavior lives under `legacy-go/internal/`.

## Invariants

- Active CLI behavior is verified in the Rust crate.
- Go command code is reference material.

## Common Pitfalls

- Do not patch only this tree for active CLI behavior.
- Do not assume old command wiring matches the Rust CLI module.

## Before Editing

- Check the matching Rust CLI code first.
- Keep reference edits narrow.

## Verification

- Prefer Rust tests for active behavior.
- Run Go package tests if files in this tree are edited.
