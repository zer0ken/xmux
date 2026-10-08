# Working Notes: /.github/workflows

## Purpose

The GitHub Actions workflows.

## Module Seams

- `ci.yml` runs format, lint, and the test suite on every pull request and on pushes to
  `main`, on Linux and Windows.
- `e2e.yml` runs the end-to-end suite with the Linux client (`scripts/e2e/`) on every
  pull request and on pushes to `main`, as one parallel job per mux.
- `release.yml` accepts a stable version on `main`, updates and verifies Cargo
  metadata, and atomically pushes a release commit and tag before building and
  publishing. A pushed version tag verifies matching metadata before publication.
  Every binary and the crate use the verified commit; packaging uses its tag.
- `winget-sync.yml` is the daily backstop for the winget submission.
- `issue-label-gate.yml` closes an issue that does not carry exactly one of the
  `ai-generated` and `handmade` labels.

## Invariants

- A pull request merges only when the CI gate and the end-to-end suite pass.
