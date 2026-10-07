# Working Notes: /.github/workflows

## Purpose

The GitHub Actions workflows.

## Module Seams

- `ci.yml` runs format, lint, and the test suite on every pull request and on pushes to
  `main`.
- `e2e.yml` runs the end-to-end suite with the Linux client (`scripts/e2e/`) on every
  pull request and on pushes to `main`, as one parallel job per mux.
- `release.yml` builds, publishes, and refreshes the packaging on a version tag.
- `winget-sync.yml` is the daily backstop for the winget submission.
- `issue-label-gate.yml` closes an issue that does not carry exactly one of the
  `ai-generated` and `handmade` labels.

## Invariants

- A pull request merges only when the CI gate and the end-to-end suite pass.
