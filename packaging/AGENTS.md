# Working Notes: /packaging

## Purpose

What the project contributes to each package-manager channel.

## Module Seams

- `homebrew/` holds the tap formula.
- `winget/` holds the community manifest.
- The README here lists the one-time registration each channel needs.

## Invariants

- The release workflow refreshes the version and checksums here on every release tag, so
  a manifest is not edited by hand for a release.
