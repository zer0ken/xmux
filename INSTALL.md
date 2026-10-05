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

```batch
curl -fsSL https://github.com/zer0ken/xmux/releases/latest/download/install.cmd -o install.cmd && install.cmd && del install.cmd
```

The first line is for macOS, Linux, WSL and Android Termux, the second for
Windows PowerShell, and the third for Windows CMD. The CMD script installs
nothing of its own: it hands the install to the PowerShell script from the same
release, so all three end in the same install.

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
writes your own user `PATH`, never the machine one, so it needs no elevation,
and records the entry it added in a file in the install directory. Pass
`--no-modify-path` and it prints what to add instead.

The scripts take the same options; the PowerShell and CMD scripts spell them
`-Version`, `-BinDir`, `-Root` and `-NoModifyPath`:

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

```batch
curl -fsSL https://github.com/zer0ken/xmux/releases/latest/download/install.cmd -o install.cmd && install.cmd -Version 0.9.5 && del install.cmd
```

## Package managers

| OS | Command |
|---|---|
| macOS | `brew install zer0ken/xmux/xmux` |
| Windows | `winget install --id zer0ken.xmux` |
| Any OS with Rust | `cargo install xmux` |

A package-manager install does not update itself; run `xmux update`, which hands
the upgrade to that package manager. The winget catalog is updated through a
review in the community winget-pkgs repository, so it can trail the newest
release. See [`packaging/`](packaging/) for the manifests.

## Prerequisites

Running xmux needs `ssh` on the machine that runs it, for remote hosts, and a
supported multiplexer on each host you target: `tmux`, GNU `screen`, `zellij`,
`abduco`, or `tuios` on unix-likes, `psmux` on Windows, and `herdr` on either.
A host's multiplexer is detected from the binary it answers as, so a mix across
your hosts needs no configuration. Termux ships without `ssh`; `pkg install
openssh` adds it. See the [README](README.md) for what the program does and how
to use it.

---

## Windows

### Install script

In PowerShell:

```powershell
irm https://github.com/zer0ken/xmux/releases/latest/download/install.ps1 | iex
```

In CMD:

```batch
curl -fsSL https://github.com/zer0ken/xmux/releases/latest/download/install.cmd -o install.cmd && install.cmd && del install.cmd
```

If you see `The token '&&' is not a valid statement separator`, you are in
PowerShell, not CMD. If you see `'irm' is not recognized as an internal or
external command`, you are in CMD, not PowerShell. A PowerShell prompt starts
with `PS C:\`; a CMD prompt is just `C:\`.

Open a new terminal afterwards, so it picks up the `PATH` the script wrote.

### Package manager

```powershell
winget install --id zer0ken.xmux
```

The winget catalog can trail the newest release, because each version is
reviewed in the community repository before it is listed. With Rust installed,
`cargo install xmux` works too.

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

Builds are published for `x86_64` and `aarch64`, and need glibc 2.35 or newer
(Ubuntu 22.04 or a distro of the same age).

### Package manager

```sh
cargo install xmux
```

This is the universal CLI install and works on any OS with a Rust toolchain
installed; it is enabled by publishing the crate to crates.io (see the release
workflow). Linux has no distro-specific package yet.

### Prebuilt binary

1. Download `xmux-v<version>-x86_64-unknown-linux-gnu.tar.gz` on `x86_64`, or
   `xmux-v<version>-aarch64-unknown-linux-gnu.tar.gz` on `aarch64`, from the
   [releases](https://github.com/zer0ken/xmux/releases) page.
2. Extract it and move the `xmux` binary onto your `PATH`:

```sh
tar -xzf xmux-v<version>-<arch>-unknown-linux-gnu.tar.gz
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

## Android (Termux)

### Install script

In Termux:

```sh
curl -fsSL https://github.com/zer0ken/xmux/releases/latest/download/install.sh | sh
```

The build is published for `aarch64` and needs Android 7.0 or later. It is linked
against Android's own libc, so it runs inside Termux, where the Linux builds cannot
load. The script adds `~/.local/bin` to `PATH` in your shell profile, and Termux's
bash reads `~/.bashrc` in every new session, so a new session picks it up.

### Prebuilt binary

Download the archive inside Termux and extract it into Termux's own `bin`
directory, which is already on `PATH`:

```sh
curl -fLO https://github.com/zer0ken/xmux/releases/download/v<version>/xmux-v<version>-aarch64-linux-android.tar.gz
tar -xzf xmux-v<version>-aarch64-linux-android.tar.gz -C "$PREFIX/bin"
```

Android mounts shared storage without execute permission, so a binary saved to
the Downloads folder by a browser cannot run from there.

### From source

```sh
pkg install rust
cargo install xmux
```

`cargo install` places the `xmux` command in `~/.cargo/bin`, which is not on
`PATH` until you add it.

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

To upgrade, run `xmux update`. It reads where the `xmux` binary lives and
updates it:

| Install | What `xmux update` runs |
|---|---|
| Install script | The same install script, which writes a new version directory and repoints the launcher |
| Cargo | A checksum-verified build from the release, written over the binary (`--method cargo` runs `cargo install xmux` instead) |
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

## Uninstalling

`xmux uninstall` removes xmux the way it was installed. It reads where the
`xmux` binary lives, the same way `xmux update` does, prints what it will
remove, and asks `Remove xmux? (y/N)`. Only `y` or `yes` removes anything; an
empty answer, any other answer, and a run with no terminal to ask leave
everything as it is.

| Install | What `xmux uninstall` removes |
|---|---|
| Install script | The version directories, the launcher and the file beside it, and the `PATH` change the script made |
| Cargo | `cargo uninstall xmux` |
| winget | `winget uninstall --id zer0ken.xmux` |
| Homebrew | `brew uninstall xmux` |
| A binary you copied onto your `PATH` yourself | That binary |

A script install loses only the paths the script wrote:

- each version directory, named after a version and holding the xmux binary
- each launcher that its marker, its link into those versions, or (in the install
  directory's `bin`) its identical bytes show the script placed, with its marker
- the `PATH` change the script made

A directory you chose with `--root` or `--bin-dir` keeps every other file in
it, and is removed only when nothing else is left in it. A launcher-named file
nothing proves the script placed stays, and the command says so. On unix a
marked block the script appended to your shell profile is removed when it adds
this install's launcher directory, written exactly; a block for another install
stays, and the rest of the profile is left byte for byte. On Windows the user
`PATH` entry is removed only when the script recorded adding it, and only the one
entry spelled exactly as recorded. An install from a script that kept no such
record leaves the entry, and the command prints it for you to remove. A directory
the command cannot read stops it with an error before anything is removed.

After the program, it asks `Also remove settings and data? (y/N)`, naming the
two directories xmux keeps: `~/.xmux` (state, logs, and the control sockets)
and `~/.config/xmux` (the config file). The default keeps both, and the command
prints where they are.

| Option | Effect |
|---|---|
| `--yes`, `-y` | Answer yes to removing xmux. Settings and data stay. |
| `--purge` | Answer yes to removing settings and data. |

`xmux uninstall` refuses while an xmux instance is running and names it; quit
that instance first. It changes nothing on remote hosts: a key this PC
registered on a host stays there, and `prefix L` in the app removes it, one host
at a time, before you uninstall.

The command checks for running instances again right before each removal. An
instance started while the first question waited stops the command before
anything is removed; one started while the second question waited keeps the
settings and data, and the program removal already confirmed still completes.

On Windows a running executable cannot be deleted, so the files are removed, and
a package manager's uninstall runs, in the background once every xmux process
has exited. The command names a log file in the temporary directory; it lists
every path that could not be removed, the package manager's output, and a last
line once the removal finished. The user `PATH` is changed before the command
exits.

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
