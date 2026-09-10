# Working Notes: /src/cli

## Purpose

`cli` is the command surface: argument parsing and dispatch for the
`ls`/`attach`/`doctor`/`instances`/`send`/`version` commands and the default
interactive app, plus the `update` subcommand that detects how xmux was installed
and hands the upgrade to whatever owns that install: a package manager runs its own
upgrade, an install the install script placed re-runs that script, and a binary the
user copied onto their PATH is replaced with a checksum-verified build from the
release. This directory exposes ONE entry, which the binary shim calls; everything
below it is crate-internal.

`doctor` reports a failed source as the state its own failure text proves, in the
same word the app's cards use: a failure the user could answer inside the app reads
as one, and only a machine that did not answer reads as unreachable.

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
- Update owns the update command: install-method detection, the delegation each
  method needs, in-place replacement with a checksum-verified build, and the recorded
  answer about which version is newest.

## Invariants

- This directory exposes exactly ONE public entry, which the binary shim calls; the
  layers below it are crate-internal.
- A running instance is addressed by NAME (a control socket), never by pid.
- The steps of an install live in the install script, not here. An install that script
  placed is updated by running the script again, so the layout it writes is described
  in one place and cannot drift from what the update does.
- An install the script placed is recognised by the marker it writes beside its
  launcher, before the layout is read at all. The launcher may sit outside the versions
  it points at, and on Windows it is a copy, so its own position leads nowhere. Reading
  the layout stays as the fallback, which is what recognises an install whose marker
  was deleted.
- An install method is decided from the running executable's own path and nothing
  else. A binary whose path cannot be read is reported as unknown rather than assigned
  a method, because each method writes somewhere different.
- `doctor` asks the network nothing. It reports the recorded answer about the newest
  version; `update --check` is the command that asks.
- Whether a newer version exists is asked at most once a day, off the app's own path,
  and a failure to ask leaves the previous answer standing. This is not the roster
  rule: that rule governs the machines the roster names, which xmux reaches over ssh
  and which refuse every retry identically once they refuse one.

## Common Pitfalls

- Do not reach below the documented seams from dispatch; route through the config,
  environment, and control surfaces.

## Before Editing

- Identify whether the change is parsing, dispatch, or a specific subcommand's
  behavior.

## Verification

- Exercise each subcommand's argv parsing and dispatch from the binary shim, and
  the update command's method detection on a host that cannot be touched.
