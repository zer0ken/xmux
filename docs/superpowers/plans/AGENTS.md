# Working Notes: /docs/superpowers/plans

## Purpose

This directory stores development execution plans.

## Mental Model

Plans are working material. They can explain sequencing for a task, but accepted
repository rules belong in ADRs or Working Notes.

## Module Seams

- Keep plans scoped to concrete development work.
- Put stable architecture rules in `docs/adr/` or `AGENTS.md` files.

## Invariants

- A plan should be understandable without relying on chat history.
- A plan should name verification gates for the work it describes.

## Common Pitfalls

- Do not make plans the only place where a durable invariant is documented.
- Do not update public docs by editing only a plan.

## Before Editing

- Confirm the file is still active working material.
- Prefer a new dated plan when the task scope is materially different.

## Verification

- Check that referenced paths and commands still exist when reusing a plan.
