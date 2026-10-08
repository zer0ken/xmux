# Working Notes: /src/cli/update

## Purpose

`update` is the `xmux update` command and the record of the newest released version.
It detects how xmux was installed and updates it the way that method expects, and it
keeps the answer `doctor` and the app read.

## Module Seams

- Install-method detection is shared with the uninstall command.
- The release path downloads the build for the running platform, verifies it against
  the release checksums, and replaces the binary in place.
- Startup owns update consent, the device preference, and handover to the new build.
- Release checks finish before the app takes the terminal and record their answer.

## Invariants

- The install method is decided from the running executable's path alone; a path
  that cannot be read is reported as unknown.
- An install the script placed is updated by re-running the script, so the install
  layout is described in one place.
- A script install is recognised by its launcher's marker first, its layout second.
- Startup checks once per launch, with a five-second network timeout. A newer release
  offers update and enable automatic updates (the default), update once, or skip.
- Automatic update consent persists on the device. Successful updates start the new
  build with the original arguments; a failure starts the current build without retry.
- Subcommands skip startup updates. Noninteractive launches never prompt and update
  only with saved consent. Disabling release checks also disables automatic updates.
- A failed check leaves the previous recorded answer standing.
