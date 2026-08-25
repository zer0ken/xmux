# Installing xmux

xmux ships as one self-contained binary. There are two ways to get it, and
either gives you the same `xmux` command-line program:

- **Prebuilt binary** — download the package for your OS from the
  [releases](https://github.com/zer0ken/xmux/releases) page and put the binary
  on your `PATH`. This is the recommended path: nothing needs to be compiled.
- **From source** — build the Rust project with Cargo. Use this when no
  prebuilt binary matches your platform, or when you want to build a specific
  commit.

## Quick install (package managers)

The fastest one-line installs, per OS:

| OS | Command |
|---|---|
| Windows | `winget install --id zer0ken.xmux` |
| macOS | `brew install zer0ken/xmux/xmux` |
| Linux · any OS with Rust | `cargo install xmux` |

These commands come from the package registrations described below; a couple
need a one-time registration step before they work for everyone. Until then,
the **prebuilt binary** or the `cargo install --path .` command above are
always available. See [`packaging/`](packaging/) for the manifests and the
registration steps.

## Prerequisites

Running xmux needs `ssh` on the machine that runs it, for remote hosts, and a
supported multiplexer on each host you target: `tmux` on unix-likes, `psmux` on
Windows, or `zellij`. A host's multiplexer is detected from the binary it
answers as, so a mix across your hosts needs no configuration. See the
[README](README.md) for what the program does and how to use it.

---

## Windows

### Package manager

```powershell
winget install --id zer0ken.xmux
```

This is enabled by the winget manifest in [`packaging/winget`](packaging/winget);
it is published by submitting that manifest to the community winget-pkgs
repository.

### Prebuilt binary

1. Download
   `xmux-v<version>-x86_64-pc-windows-msvc.exe` from the
   [releases](https://github.com/zer0ken/xmux/releases) page.
2. Rename it to `xmux.exe`.
3. Move it into a directory on your `PATH` (for example a folder you added to
   `PATH` under `C:\Users\you\bin`), or create an alias.

Verify it works by opening a new terminal and running:

```powershell
xmux version
```

### From source

1. Install a Rust toolchain from <https://rustup.rs> (rustup installs Cargo).
2. Open a terminal in the project directory and run:

```powershell
cargo install --path .
```

This builds the release binary and places `xmux` on your `PATH`. To build the
binary without installing it, use `cargo build --release` and copy
`target\release\xmux.exe` wherever you like.

---

## macOS

### Package manager

```sh
brew install zer0ken/xmux/xmux
```

This is enabled by the formula in [`packaging/homebrew`](packaging/homebrew);
it is published by hosting that formula in a `homebrew-xmux` tap under the
project owner.

Prebuilt packages are provided for Apple Silicon (`aarch64`) and Intel
(`x86_64`).

### Prebuilt binary

1. Download `xmux-v<version>-aarch64-apple-darwin.tar.gz` on Apple Silicon, or
   `xmux-v<version>-x86_64-apple-darwin.tar.gz` on Intel, from the
   [releases](https://github.com/zer0ken/xmux/releases) page.
2. Extract it and move the `xmux` binary onto your `PATH`:

```sh
tar -xzf xmux-v<version>-<arch>-apple-darwin.tar.gz
sudo mv xmux /usr/local/bin/
```

Verify it works in a new terminal:

```sh
xmux version
```

> macOS may ask you to confirm running the binary, because it is not signed
> with an Apple developer certificate. This is expected for a binary built by a
> GitHub Actions workflow. You can approve it in **System Settings → Privacy &
> Security**, or remove the quarantine attribute instead:
>
> ```sh
> xattr -d com.apple.quarantine /usr/local/bin/xmux
> ```

### From source

1. Install the Rust toolchain. With [Homebrew](https://brew.sh):

```sh
brew install rust
```

2. From the project directory:

```sh
cargo install --path .
```

This places the `xmux` command on your `PATH` (commonly under
`~/.cargo/bin`). To build without installing, use `cargo build --release`.

---

## Linux

### Package manager

```sh
cargo install xmux
```

This is the universal CLI install and works on any OS with a Rust toolchain
installed; it is enabled by publishing the crate to crates.io (see the release
workflow). Linux has no distro-specific package yet.

Prebuilt packages are provided for `x86_64` (most desktop and server
installations).

### Prebuilt binary

1. Download `xmux-v<version>-x86_64-unknown-linux-gnu.tar.gz` from the
   [releases](https://github.com/zer0ken/xmux/releases) page.
2. Extract it and move the `xmux` binary onto your `PATH`:

```sh
tar -xzf xmux-v<version>-x86_64-unknown-linux-gnu.tar.gz
sudo mv xmux /usr/local/bin/
```

Verify it works in a new terminal:

```sh
xmux version
```

### From source

1. Install a Rust toolchain. On Debian/Ubuntu:

```sh
sudo apt install build-essential curl
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

On Fedora:

```sh
sudo dnf groupinstall "Development Tools"
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

2. From the project directory:

```sh
cargo install --path .
```

This places the `xmux` command on your `PATH` (commonly under
`~/.cargo/bin`). To build without installing, use `cargo build --release`.

---

## Verifying and upgrading

Check the installed version and its health with:

```sh
xmux version
xmux doctor
```

To upgrade, run `xmux update`. It looks at where the `xmux` binary lives and
picks the update that matches how you installed it: a cargo install updates
with `cargo install xmux`, a winget install with `winget upgrade --id
zer0ken.xmux`, and a Homebrew install with `brew upgrade
zer0ken/xmux/xmux`. Any other placement (a prebuilt binary copied onto your
`PATH`) is updated in place from the latest GitHub release, after verifying the
downloaded binary's SHA256 against the release's published checksum.

Preview what an update would do without installing it:

```sh
xmux update --check
```

Force a specific update path with `--method cargo|winget|brew|self`, or the
`XMUX_UPDATE_METHOD` environment variable. A source/dev build (not a released
version) is not overwritten — update it the same way it was built.

To upgrade from a prebuilt binary manually, download the newer package and
replace the binary. To upgrade a Cargo install by hand, re-run
`cargo install --path .` (or, if you installed from crates.io,
`cargo install xmux`).
