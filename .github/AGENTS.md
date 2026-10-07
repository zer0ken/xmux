# Working Notes: /.github

## Purpose

The repository's GitHub automation.

## Module Seams

- `workflows/` owns triggers, job orchestration, and permissions.
- `scripts/` holds support commands shared by workflows.

## Invariants

- Workflow configuration supplies support scripts through environment variables.
