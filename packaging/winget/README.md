# Winget manifest

These files are the [Windows Package Manager](https://github.com/microsoft/winget-pkgs)
community manifest for xmux. They let users install with:

```powershell
winget install --id z0k.xmux
```

## Registering

The manifest must be added to the `microsoft/winget-pkgs` repository to go
live. The release workflow does not submit it; that is a maintainer step,
because winget-pkgs requires a signed contribution agreement. The usual flow:

1. Wait for a release, then download its `SHA256SUMS` artifact.
2. Copy the `*.installer.yaml`, `*.locale.*.yaml`, and `*.yaml` files here
   under `manifests/z/z0/z0k/xmux/<version>/`, filling
   `InstallerSha256` from the release checksum.
3. Open a pull request against `microsoft/winget-pkgs` with those files.

## Files

- `z0k.xmux.yaml` — version manifest
- `z0k.xmux.installer.yaml` — the installer (portable binary)
- `z0k.xmux.locale.en-US.yaml` — display metadata

`InstallerSha256` is a placeholder; replace it with the value from the release
`SHA256SUMS` artifact before submitting.
