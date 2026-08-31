# Homebrew tap

This formula lets users install xmux's prebuilt binary with:

```sh
brew install zer0ken/xmux/xmux
```

## Registering

Homebrew pulls formulae from a **tap**, a GitHub repository named
`homebrew-xmux` under the project owner (`github.com/zer0ken/homebrew-xmux`).
The tap is hosted once: create the repository and commit `Formula/xmux.rb` to
it.

After that, the release workflow keeps the formula in sync. It regenerates
`xmux.rb` with the new version and the release's `SHA256SUMS` checksums, then
pushes it into the tap when a `HOMEBREW_TAP_TOKEN` secret is configured on the
project (a PAT able to write to the tap repo). Without the secret, the tap stays
at the last manually committed version and must be updated by hand.

Users run `brew install zer0ken/xmux/xmux` (a tap is fetched automatically when
the command names it).

## Files

- `Formula/xmux.rb`: the formula; installs the prebuilt binary for macOS
  (Apple Silicon and Intel) and Linux (x86_64 and arm64).
