# Installing xmux

The [README](README.md#1-installation) gives the install commands. This document
covers what they leave out: the platform requirements, how the install script lays
out and pins an install, the prebuilt binaries, building from source, upgrading,
and uninstalling.

## Platforms

| Platform | Builds | Requirement | Note |
|---|---|---|---|
| Windows | `x86_64` | none | |
| macOS | `aarch64`, `x86_64` | none | The script reads the Rosetta translation flag, so an Intel shell on Apple Silicon still gets the native build. |
| Linux | `x86_64`, `aarch64` | glibc 2.35 or newer (Ubuntu 22.04 or a distro of the same age) | |
| Android (Termux) | `aarch64` | Android 7.0 or later | Linked against Android's own libc, so it runs inside Termux, where the Linux builds cannot load. Termux ships without `ssh`; `pkg install openssh` adds it. |

Remote hosts need `ssh` on the machine that runs xmux, and a supported mux on each
host: `tmux`, GNU `screen`, `zellij`, `abduco`, or `tuios` on unix-likes, `psmux`
on Windows, and `herdr` on either.

## Install Script Layout

The CMD script installs nothing of its own: it hands the install to the PowerShell
script from the same release, so all three scripts end in the same install. The
script unpacks the build into a directory named after its version and points a
launcher at that directory. A new version goes into a directory of its own and only
the launcher is replaced, so the binary a running xmux is executing is never written
to. On Windows, where a running image cannot be overwritten, a launcher in use is
renamed aside and the new one takes its place; a later install deletes the renamed file
once nothing holds it.

| | Unix | Windows |
|---|---|---|
| Versions | `~/.local/share/xmux/versions/<version>/` | `%LOCALAPPDATA%\xmux\versions\<version>\` |
| Launcher | `~/.local/bin/xmux`, a symlink | `%LOCALAPPDATA%\xmux\bin\xmux.exe`, a copy |

Beside the launcher the script writes a one-line file naming the directory the
versions live in. `xmux update` reads it to recognise an install the script placed,
and runs the script again rather than writing the binary itself.

The script reports where the launcher went, and adds the launcher directory to `PATH`
when it is not already there. On
unix it appends a marked block to the shell profile; Termux's bash reads `~/.bashrc`
in every new session, so a new session picks it up. On Windows it writes the user
`PATH`, never the machine one, and records the entry it added in a file in the
install directory.

The PowerShell and CMD scripts spell the options `-Version`, `-BinDir`, `-Root`, and
`-NoModifyPath`:

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

## Package Managers

The Homebrew formula and the winget manifest are in [`packaging/`](packaging/). The
formula is published from a `homebrew-xmux` tap under the project owner, and the
crate from crates.io. Linux has no distro-specific package.

## Prebuilt Binaries

Every build is on the [releases](https://github.com/zer0ken/xmux/releases) page:

| Platform | Asset |
|---|---|
| Windows | `xmux-v<version>-x86_64-pc-windows-msvc.exe` |
| macOS | `xmux-v<version>-<arch>-apple-darwin.tar.gz` |
| Linux | `xmux-v<version>-<arch>-unknown-linux-gnu.tar.gz` |
| Android (Termux) | `xmux-v<version>-aarch64-linux-android.tar.gz` |

On Windows, rename the file to `xmux.exe` and move it into a directory on `PATH`.
Elsewhere, extract the archive and move the `xmux` binary onto `PATH`:

```sh
tar -xzf xmux-v<version>-<arch>-<target>.tar.gz
sudo mv xmux /usr/local/bin/
```

In Termux, download the archive inside Termux and extract it into `$PREFIX/bin`,
which is already on `PATH`. Android mounts shared storage without execute
permission, so a binary a browser saved to the Downloads folder cannot run from
there.

macOS may ask to confirm running the binary, because it is not signed with an Apple
developer certificate. Approve it in **System Settings → Privacy & Security**, or
remove the quarantine attribute:

```sh
xattr -d com.apple.quarantine /usr/local/bin/xmux
```

## Building from Source

Install a Rust toolchain from <https://rustup.rs> (`pkg install rust` in Termux,
`brew install rust` with Homebrew). On Debian or Ubuntu the toolchain also needs
`build-essential`, and on Fedora the `Development Tools` group. Then, in the project
directory:

```sh
cargo install --path .
```

This places `xmux` in `~/.cargo/bin`, which Termux does not put on `PATH` by itself.
`cargo build --release` builds the binary into `target/release/` without installing
it.

## Upgrading

`xmux update` reads where the `xmux` binary lives and updates it:

| Install | What `xmux update` runs |
|---|---|
| Install script | The same install script, which writes a new version directory and repoints the launcher |
| Cargo | A checksum-verified build from the release, written over the binary (`--method cargo` runs `cargo install xmux` instead) |
| winget | `winget upgrade --id zer0ken.xmux` |
| Homebrew | `brew upgrade zer0ken/xmux/xmux` |
| A binary copied onto `PATH` by hand | A checksum-verified build from the release, written over that binary |

`xmux update --check` reports what an update would do without installing it.
`xmux update --version 0.9.5` installs a named version, which is also how to go back
to an older one; a package manager picks its own version, so this reaches only the
paths that choose one. `--method cargo|winget|brew|script|self`, or the
`XMUX_UPDATE_METHOD` environment variable, forces a path. A source build is not a
released version, so it is updated the way it was built.

On Windows a running executable cannot be overwritten. A script install is
unaffected, because the new build goes into a directory of its own; the other paths
either rename the running binary aside or finish in the background once every xmux
instance has exited.

xmux asks GitHub once a day which version is newest and records the answer in
`~/.xmux/version.json`. The request runs off the app's own path, so a launch never
waits on it. `[update] check = false` in `config.toml` turns it off.

## Uninstalling

`xmux uninstall` removes xmux the way it was installed, read from where the binary
lives the same way `xmux update` reads it. It prints what it will remove and asks
`Remove xmux? (y/N)`. Only `y` or `yes` removes anything; any other answer, an empty
one, and a run with no terminal to ask leave everything as it is.

| Install | What `xmux uninstall` removes |
|---|---|
| Install script | The version directories, the launcher and the file beside it, and the `PATH` change the script made |
| Cargo | `cargo uninstall xmux` |
| winget | `winget uninstall --id zer0ken.xmux` |
| Homebrew | `brew uninstall xmux` |
| A binary copied onto `PATH` by hand | That binary |

A script install loses only the paths the script wrote:

- each version directory, named after a version and holding the xmux binary
- each launcher that its marker, its link into those versions, or (in the install
  directory's `bin`) its identical bytes show the script placed, with its marker
- the `PATH` change the script made

A directory chosen with `--root` or `--bin-dir` keeps every other file in it, and is
removed only when nothing else is left in it. A launcher-named file nothing proves
the script placed stays, and the command says so. On unix a marked block in the
shell profile is removed when it adds this install's launcher directory, written
exactly; a block for another install stays, and the rest of the profile is left byte
for byte. On Windows the user `PATH` entry is removed only when the script recorded
adding it, and only the one entry spelled exactly as recorded; an install from a
script that kept no such record leaves the entry, and the command prints it. A
directory the command cannot read stops it with an error before anything is removed.

It then asks `Also remove settings and data? (y/N)`, naming `~/.xmux` (state, logs,
and the control sockets) and `~/.config/xmux` (the config file). The default keeps
both, and the command prints where they are.

| Option | Effect |
|---|---|
| `--yes`, `-y` | Answer yes to removing xmux. Settings and data stay. |
| `--purge` | Answer yes to removing settings and data. |

`xmux uninstall` refuses while an xmux instance is running and names it, and it
checks again right before each removal. An instance started while the first question
waited stops the command before anything is removed; one started while the second
question waited keeps the settings and data, and the confirmed program removal still
completes. It changes nothing on remote hosts: a key this machine registered on a
host stays there until `prefix L` in the app removes it.

On Windows a running executable cannot be deleted, so the files are removed, and a
package manager's uninstall runs, in the background once every xmux process has
exited. The command names a log file in the temporary directory that lists every
path that could not be removed, the package manager's output, and a last line once
the removal finished. The user `PATH` is changed before the command exits.
