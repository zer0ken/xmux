# Homebrew tap

This formula lets users install xmux's prebuilt binary with:

```sh
brew install zer0ken/xmux/xmux
```

## Registering

Homebrew pulls formulae from a **tap** — a GitHub repository named
`homebrew-xmux` under the project owner. The release workflow does not create
that repo; that is a one-time maintainer step:

1. Create a repository named `homebrew-xmux` under the project owner
   (`github.com/zer0ken/homebrew-xmux`).
2. Copy `xmux.rb` into its `Formula/` directory.
3. On each release, update the `url` version and the two `sha256` values from
   the release's `SHA256SUMS` artifact.

Users then run `brew install zer0ken/xmux/xmux` (a tap is fetched
automatically when the command names it).

## Files

- `xmux.rb` — the formula; installs the prebuilt macOS binary for both Apple
  Silicon and Intel.
