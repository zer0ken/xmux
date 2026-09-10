# Installing xmux

xmux ships as one self-contained binary. The install script is the shortest way
to get it, and every other way gives you the same `xmux` command.

## Install script

```sh
curl -fsSL https://github.com/zer0ken/xmux/releases/latest/download/install.sh | sh
```

```powershell
irm https://github.com/zer0ken/xmux/releases/latest/download/install.ps1 | iex
```

The script reads which OS and architecture it is running on, downloads that
build from the latest release, and refuses to install it unless its SHA-256
matches the checksum the release publishes. It then unpacks the build into a
directory named after the version and points a launcher at that directory.

That layout is what makes an upgrade safe while xmux is running. A new version
goes into a directory of its own and only the launcher is replaced, so the
binary a running xmux is executing is never written to.

| | Unix | Windows |
|---|---|---|
| Versions | `~/.local/share/xmux/versions/<version>/` | `%LOCALAPPDATA%\xmux\versions\<version>\` |
| Launcher | `~/.local/bin/xmux`, a symlink | `%LOCALAPPDATA%\xmux\bin\xmux.exe`, a copy |

Beside the launcher the script writes a one-line file naming the directory the
versions live in. `xmux update` reads it to recognise an install the script
placed, and to run the script again rather than writing the binary itself.

The script adds the launcher directory to your `PATH` when it is not already
there. On unix it appends a marked block to your shell profile; on Windows it
writes your own user `PATH`, never the machine one, so it needs no elevation.
Pass `--no-modify-path` and it prints what to add instead.

Both scripts take the same options:

| Option | Environment variable | Effect |
|---|---|---|
| `--version <x.y.z>` | `XMUX_VERSION` | Install this version rather than the newest release. |
| `--bin-dir <dir>` | `XMUX_BIN_DIR` | Put the launcher somewhere else. |
| `--root <dir>` | `XMUX_INSTALL_ROOT` | Keep the version directories somewhere else. |
| `--no-modify-path` | `XMUX_NO_MODIFY_PATH=1` | Report what to add to `PATH` rather than adding it. |

```sh
curl -fsSL https://github.com/zer0ken/xmux/releases/latest/download/install.sh | sh -s -- --version 0.9.5
```

```powershell
& ([scriptblock]::Create((irm https://github.com/zer0ken/xmux/releases/latest/download/install.ps1))) -Version 0.9.5
```

## Package managers

| OS | Command |
|---|---|
| macOS | `brew install zer0ken/xmux/xmux` |
| Windows, Linux, any OS with Rust | `cargo install xmux` |

There is no winget install: the manifest in
[`packaging/winget`](packaging/winget) is not registered in the community
winget-pkgs repository. See [`packaging/`](packaging/) for the manifests and the
registration steps.

## Prerequisites

Running xmux needs `ssh` on the machine that runs it, for remote hosts, and a
supported multiplexer on each host you target: `tmux`, GNU `screen`, `zellij`,
or `abduco` on unix-likes, and `psmux` on Windows. A host's multiplexer is
detected from the binary it answers as, so a mix across your hosts needs no
configuration. See the [README](README.md) for what the program does and how to
use it.

---

## Windows

### Install script

```powershell
irm https://github.com/zer0ken/xmux/releases/latest/download/install.ps1 | iex
```

Open a new terminal afterwards, so it picks up the `PATH` the script wrote.

### Package manager

There is no winget package: the manifest in
[`packaging/winget`](packaging/winget) is not registered in the community
winget-pkgs repository, so `winget install --id zer0ken.xmux` finds nothing.
With Rust installed, `cargo install xmux` works.

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

### Install script

```sh
curl -fsSL https://github.com/zer0ken/xmux/releases/latest/download/install.sh | sh
```

It picks the Apple Silicon or Intel build from what the machine reports, and
reads the Rosetta translation flag rather than the reported architecture, so an
Intel shell on an Apple Silicon Mac still gets the native build.

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

### Install script

```sh
curl -fsSL https://github.com/zer0ken/xmux/releases/latest/download/install.sh | sh
```

Builds are published for `x86_64` and `aarch64`.

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

`xmux doctor` opens with which xmux is running, where its binary is, and what
owns that install, so an update that lands somewhere unexpected can be traced to
the install it acted on.

To upgrade, run `xmux update`. It reads where the `xmux` binary lives and hands
the upgrade to whatever owns that install:

| Install | What `xmux update` runs |
|---|---|
| Install script | The same install script, which writes a new version directory and repoints the launcher |
| Cargo | `cargo install xmux` |
| winget | `winget upgrade --id zer0ken.xmux` |
| Homebrew | `brew upgrade zer0ken/xmux/xmux` |
| A binary you copied onto your `PATH` yourself | A checksum-verified build from the release, written over that binary |

Preview what an update would do without installing it:

```sh
xmux update --check
```

`xmux update --version 0.9.5` installs a named version, which is also how you go
back to an older one. A package manager picks its own version, so this reaches
only the paths that choose one.

Force a path with `--method cargo|winget|brew|script|self`, or the
`XMUX_UPDATE_METHOD` environment variable. A source build is not a released
version, so update it the way it was built.

On Windows a running executable cannot be overwritten. A script install is
unaffected, because the new build goes into a directory of its own; the other
paths either rename the running binary aside or finish in the background once
every xmux instance has exited.

## New releases

xmux asks GitHub once a day which version is newest and records the answer in
`~/.xmux/version.json`. When a newer version has been released, xmux says so on
startup and `xmux doctor` reports it. The request runs off the app's own path, so
a launch never waits on it, and a launch with no network paints exactly as fast
as one with it.

Turn it off in `config.toml`:

```toml
[update]
check = false
```
