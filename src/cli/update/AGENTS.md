# Working Notes: /src/cli/update

## Purpose

`update` is the `xmux update` command and the record of the newest released version.
It detects how xmux was installed and updates it the way that method expects, and it
keeps the answer `doctor` and the startup toast read.

## Module Seams

- Install-method detection is shared with the uninstall command.
- The release path downloads the build for the running platform, verifies it against
  the release checksums, and replaces the binary in place.
- The version notice caches the newest release and refreshes it off the app loop.

## Invariants

- The install method is decided from the running executable's path alone; a path
  that cannot be read is reported as unknown.
- An install the script placed is updated by re-running the script, so the install
  layout is described in one place.
- A script install is recognised by its launcher's marker first, its layout second.
- The release feed is asked at most once a day, and a failure leaves the previous
  answer standing.
