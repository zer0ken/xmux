# Winget manifest

These files are the [Windows Package Manager](https://github.com/microsoft/winget-pkgs)
community manifest for xmux. They let users install with:

```powershell
winget install --id zer0ken.xmux
```

## Registering

The manifest must be added to the `microsoft/winget-pkgs` repository to go
live. The release workflow does not submit it; that is a maintainer step,
because winget-pkgs requires a signed contribution agreement. The usual flow:

1. Wait for a release, then download its `SHA256SUMS` artifact.
2. Copy the `*.installer.yaml`, `*.locale.*.yaml`, and `*.yaml` files here
   under `manifests/z/zer0ken/xmux/<version>/`, filling
   `InstallerSha256` from the release checksum.
3. Open a pull request against `microsoft/winget-pkgs` with those files.

## Files

- `zer0ken.xmux.yaml` — version manifest
- `zer0ken.xmux.installer.yaml` — the installer (portable binary)
- `zer0ken.xmux.locale.en-US.yaml` — display metadata

For this version the `InstallerSha256` is already filled from the release's
`SHA256SUMS`; for each later release, replace it with the new value before
submitting.
