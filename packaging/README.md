# Packaging artifacts

The package-manager install commands in the docs are backed by the artifacts in
this directory. Each channel needs a one-time registration outside this
repository before the command works for users; the subdirectories below hold
what the project contributes to each registration.

The release workflow (`.github/workflows/release.yml`) builds the prebuilt
binaries and publishes the crate to crates.io on every `v*` tag. The checksums
that some of these manifests must carry are the ones printed by the release's
`SHA256SUMS` artifact.

## Channels

| Command | Artifact | Registration |
|---|---|---|
| `cargo install xmux` | (none needed) | publish to crates.io via the `publish-crates` release job |
| `winget install --id z0k.xmux` | `winget/` | submit the manifest to the microsoft/winget-pkgs community repo |
| `brew install zer0ken/xmux/xmux` | `homebrew/` | host the formula in a `homebrew-xmux` tap repo under the project owner |
