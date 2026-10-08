# Working Notes: /.github/scripts

## Purpose

Shell scripts that more than one workflow runs.

## Module Seams

- The winget submission script, shared by the release workflow and the daily sync, so a
  fix to the submission reaches both.
- The release version script prepares Cargo metadata or verifies it against a tag
  before publication.

## Invariants

- A script here reads its inputs from environment variables and fails fast when one is
  missing.
