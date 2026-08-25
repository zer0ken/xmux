# Working Notes: /src/cli

## Purpose

`cli` is the command surface: argument parsing and dispatch for the
`ls`/`attach`/`doctor`/`instances`/`send`/`version` commands and the default
interactive app, plus the `update` subcommand that detects how xmux was installed
and delegates to the owning package manager (cargo, winget, Homebrew) or replaces
the binary in place with a checksum-verified build from the latest release.
`cli::run` is the sole entry the binary shim calls; everything below it is
crate-internal.

## Mental Model

The CLI is the outermost layer. It parses argv, resolves the config, the resolved
environment, and instance naming, and dispatches each subcommand to the layer
that owns the behavior: the interactive app (the default), the session listing,
the attach handover, the headless instance and send commands, and the
self-update. A command that needs no config or instance (version, update) runs
without one, so a broken config never blocks it.

## Module Seams

- Dispatch owns parsing and command selection; it composes the config, the
  resolved environment, and the instance control socket as each command needs.
- Update owns the self-update command: installation-method detection, package-manager
  delegation, and in-place replacement with a checksum-verified build.

## Invariants

- `cli::run` is the single public entry the binary shim calls; the layers below
  it are crate-internal.
- A running instance is addressed by NAME (a control socket), never by pid.

## Common Pitfalls

- Do not reach below the documented seams from dispatch; route through the config,
  environment, and control surfaces.

## Before Editing

- Identify whether the change is parsing, dispatch, or a specific subcommand's
  behavior.

## Verification

- Exercise each subcommand's argv parsing and dispatch from the binary shim, and
  the update command's method detection on a host that cannot be touched.
