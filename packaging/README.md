# Packaging artifacts

The package-manager install commands in the docs are backed by the artifacts in
this directory. Each channel needs a one-time registration outside this
repository before the command works for users; the subdirectories below hold
what the project contributes to each registration.

The release workflow (`.github/workflows/release.yml`) runs on every `v*` tag. It
builds the prebuilt binaries, creates the GitHub release, publishes the crate to
crates.io, and then refreshes the manifests in this directory — the new version
and the real release checksums — committing the result to `main`. So on a normal
release, the only manual steps are bumping the version in `Cargo.toml` and
pushing the `v*` tag.

## One-time setup

- **crates.io**: add a `CARGO_REGISTRY_TOKEN` secret and a `crates-io`
  environment so the `publish-crates` job can run `cargo install xmux`.
- **winget auto-submit**: add a `WINGET_PKGS_TOKEN` secret (a PAT with `repo`
  scope that can write to the `zer0ken/winget-pkgs` fork and open PRs there).
  When set, the release workflow opens the winget-pkgs submission PR for you;
  it still needs the winget-pkgs maintainers to merge it.
- **Homebrew tap**: host the formula in a `homebrew-xmux` tap repo under the
  project owner, and add a `HOMEBREW_TAP_TOKEN` secret (a PAT able to write to
  that tap repo). When the secret is set, the release workflow syncs the
  refreshed formula into the tap; hosting the tap is a one-time step.

## Channels

| Command | Artifact | Registration |
|---|---|---|
| `cargo install xmux` | (none needed) | publish to crates.io via the `publish-crates` release job |
| `winget install --id zer0ken.xmux` | `winget/` | submit the manifest to the microsoft/winget-pkgs community repo |
| `brew install zer0ken/xmux/xmux` | `homebrew/` | host the formula in a `homebrew-xmux` tap repo under the project owner |
