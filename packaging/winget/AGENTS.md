# Working Notes: /packaging/winget

## Purpose

The Windows Package Manager community manifest.

## Module Seams

- The version, installer, and default-locale manifests that winget requires.

## Invariants

- Every release submits these to the community repository, which reviews them before
  listing, so the catalog can trail the newest release.
