# Working Notes: /src/cli

## Purpose

`cli` is the outermost layer and the command surface. It parses argv, resolves the
config, the environment, and instance naming as each command needs, and dispatches to
the layer that owns the behavior: the interactive app (the default), `ls`, `attach`,
`doctor`, `instances`, `send`, `update`, `uninstall`, and `version`. A command that
needs no config or instance (`version`, `update`, `uninstall`) runs without one, so a
broken config never blocks it.

## Module Seams

- Dispatch owns parsing and command selection and exposes the one public entry the
  binary shim calls; everything below it is crate-internal.
- `update/` owns the update command; its own Working Notes describe it.
- Uninstall owns the removal plan per install method, the two confirmations, and the
  removal itself. It reads the install method through the update command's detection,
  so the two commands never disagree about how xmux was installed.

## Invariants

- Askpass helper mode is handled before logging setup and argument parsing.
- A running instance is addressed by name (its ctl socket), never by pid.
- Uninstall deletes only paths the install provably owns, takes every path
  explicitly, and treats a directory it cannot list as an error rather than an empty
  list, so a removal is never reported done while its target remains.
- The Windows uninstall helper reports what it could not remove in a log the command
  names; the command never reports the deferred part as done.
- `doctor` asks the network nothing and reports a failed source with the same state
  word the app's cards use.

## Verification

- Exercise each subcommand's argv parsing and dispatch from the binary shim, and the
  update command's method detection on a host that cannot be touched.
