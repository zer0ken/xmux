#!/bin/sh
# xmux installer for macOS and Linux.
#
#   curl -fsSL https://raw.githubusercontent.com/zer0ken/xmux/main/scripts/install/install.sh | sh
#   curl -fsSL .../install.sh | sh -s -- --version 0.9.6
#
# It downloads the release build for this machine, verifies its SHA-256 against
# the release's own SHA256SUMS, unpacks it into a directory named after the
# version, and points a launcher at it. The launcher is a symlink, so an update
# is a relink and the binary a running xmux is executing is never written to.
#
# POSIX sh on purpose: it has to run under dash and busybox ash, not just bash.

set -eu

REPO="zer0ken/xmux"
API_BASE="https://api.github.com/repos/${REPO}"
DOWNLOAD_BASE="https://github.com/${REPO}/releases/download"

# Where the versions live and where the launcher goes. Both are overridable so a
# machine that keeps its tools elsewhere is not forced into these two paths.
ROOT="${XMUX_INSTALL_ROOT:-${XDG_DATA_HOME:-$HOME/.local/share}/xmux}"
BIN_DIR="${XMUX_BIN_DIR:-$HOME/.local/bin}"

VERSION="${XMUX_VERSION:-latest}"
MODIFY_PATH=1
QUIET=0

usage() {
    cat <<'USAGE'
Usage: install.sh [options]

Options:
  --version <x.y.z>   Install this version instead of the latest release.
  --bin-dir <dir>     Put the launcher here (default: ~/.local/bin).
  --root <dir>        Keep the versions here (default: ~/.local/share/xmux).
  --no-modify-path    Do not touch any shell profile; only report what to add.
  --quiet             Print errors only.
  -h, --help          Show this message.

Environment:
  XMUX_VERSION, XMUX_BIN_DIR, XMUX_INSTALL_ROOT  Same as the options above.
  XMUX_NO_MODIFY_PATH=1                          Same as --no-modify-path.
USAGE
}

while [ $# -gt 0 ]; do
    case "$1" in
        --version) VERSION="${2:?--version needs a value}"; shift 2 ;;
        --version=*) VERSION="${1#*=}"; shift ;;
        --bin-dir) BIN_DIR="${2:?--bin-dir needs a value}"; shift 2 ;;
        --bin-dir=*) BIN_DIR="${1#*=}"; shift ;;
        --root) ROOT="${2:?--root needs a value}"; shift 2 ;;
        --root=*) ROOT="${1#*=}"; shift ;;
        --no-modify-path) MODIFY_PATH=0; shift ;;
        --quiet) QUIET=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "install.sh: unknown option $1" >&2; usage >&2; exit 2 ;;
    esac
done

[ "${XMUX_NO_MODIFY_PATH:-0}" = "1" ] && MODIFY_PATH=0

say() { [ "$QUIET" = "1" ] || printf '%s\n' "$*"; }
die() { printf 'install.sh: %s\n' "$*" >&2; exit 1; }

need() {
    command -v "$1" >/dev/null 2>&1 || die "$1 is required but not on PATH"
}

need curl
need tar

# --- what to download --------------------------------------------------------

# The release asset for this machine. The names here mirror the release
# workflow's; a platform with no published build is named rather than guessed at,
# because the alternative is downloading a binary that cannot run.
target_triple() {
    os="$(uname -s)"
    arch="$(uname -m)"
    # A shell running under Rosetta reports x86_64 on an arm64 Mac, so the
    # translation flag decides rather than the reported architecture.
    if [ "$os" = "Darwin" ] && [ "$arch" = "x86_64" ] &&
        [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)" = "1" ]; then
        arch="arm64"
    fi
    case "$os/$arch" in
        Linux/x86_64|Linux/amd64) echo "x86_64-unknown-linux-gnu" ;;
        Linux/aarch64|Linux/arm64) echo "aarch64-unknown-linux-gnu" ;;
        Darwin/arm64|Darwin/aarch64) echo "aarch64-apple-darwin" ;;
        Darwin/x86_64) echo "x86_64-apple-darwin" ;;
        *) die "no xmux release build for $os/$arch; build from source with \`cargo install xmux\`" ;;
    esac
}

resolve_version() {
    if [ "$VERSION" != "latest" ]; then
        printf '%s' "${VERSION#v}"
        return
    fi
    # The tag of the release GitHub itself calls latest, so a prerelease is never
    # picked up by an unpinned install.
    tag="$(curl -fsSL --max-time 60 "${API_BASE}/releases/latest" |
        sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1)"
    [ -n "$tag" ] || die "cannot read the latest release tag from GitHub"
    printf '%s' "${tag#v}"
}

# --- verification ------------------------------------------------------------

# The first of the three tools that is present. A machine with none of them
# cannot verify what it downloaded, and this script will not install what it
# cannot verify.
sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | cut -d' ' -f1
    elif command -v openssl >/dev/null 2>&1; then
        openssl dgst -sha256 "$1" | sed 's/.*= *//'
    else
        die "no SHA-256 tool found (sha256sum, shasum, or openssl); cannot verify the download"
    fi
}

# --- PATH --------------------------------------------------------------------

# The profile this shell family reads on login. Only used to report or to append
# the launcher directory; nothing else in the file is touched.
profile_file() {
    case "$(basename "${SHELL:-/bin/sh}")" in
        zsh) [ "$(uname -s)" = "Darwin" ] && echo "$HOME/.zprofile" || echo "$HOME/.zshrc" ;;
        bash) [ "$(uname -s)" = "Darwin" ] && echo "$HOME/.bash_profile" || echo "$HOME/.bashrc" ;;
        fish) echo "$HOME/.config/fish/config.fish" ;;
        *) echo "$HOME/.profile" ;;
    esac
}

on_path() {
    case ":${PATH}:" in
        *":$1:"*) return 0 ;;
        *) return 1 ;;
    esac
}

# Appends the launcher directory to the profile, inside a marked block so a
# second install edits that block rather than appending another copy.
add_to_path() {
    dir="$1"
    profile="$(profile_file)"
    marker_begin="# >>> xmux installer >>>"
    marker_end="# <<< xmux installer <<<"
    if [ -f "$profile" ] && grep -qF "$marker_begin" "$profile"; then
        say "PATH already carries the xmux block in $profile"
        return
    fi
    case "$profile" in
        */config.fish) line="fish_add_path \"$dir\"" ;;
        *) line="export PATH=\"$dir:\$PATH\"" ;;
    esac
    mkdir -p "$(dirname "$profile")"
    {
        printf '\n%s\n' "$marker_begin"
        printf '%s\n' "$line"
        printf '%s\n' "$marker_end"
    } >>"$profile"
    say "added $dir to PATH in $profile"
    say "open a new shell, or run: $line"
}

# --- install -----------------------------------------------------------------

version="$(resolve_version)"
triple="$(target_triple)"
asset="xmux-v${version}-${triple}.tar.gz"
version_dir="${ROOT}/versions/${version}"

say "xmux ${version} for ${triple}"

tmp="$(mktemp -d "${TMPDIR:-/tmp}/xmux-install.XXXXXX")"
# The temporary directory goes whether the install succeeded or failed, so a
# failed download leaves nothing behind to confuse the next run.
trap 'rm -rf "$tmp"' EXIT INT TERM

say "downloading ${asset}"
curl -fsSL --max-time 300 -o "${tmp}/${asset}" "${DOWNLOAD_BASE}/v${version}/${asset}" ||
    die "cannot download ${asset}; check that v${version} has a build for ${triple}"

curl -fsSL --max-time 60 -o "${tmp}/SHA256SUMS" "${DOWNLOAD_BASE}/v${version}/SHA256SUMS" ||
    die "cannot download the checksums for v${version}"

expected="$(grep -F " ${asset}" "${tmp}/SHA256SUMS" | cut -d' ' -f1 | head -n 1)"
[ -n "$expected" ] || die "the release lists no checksum for ${asset}"
actual="$(sha256_of "${tmp}/${asset}")"
[ "$actual" = "$expected" ] ||
    die "checksum mismatch for ${asset}: expected ${expected}, got ${actual}"
say "checksum verified"

tar -xzf "${tmp}/${asset}" -C "$tmp" || die "cannot unpack ${asset}"
[ -f "${tmp}/xmux" ] || die "${asset} does not contain an xmux binary"
chmod +x "${tmp}/xmux"

# The version directory is built beside its final name and moved into place, so a
# reader never sees a half-written version directory under that name.
mkdir -p "${ROOT}/versions"
staging="${ROOT}/versions/.staging.$$"
rm -rf "$staging"
mkdir -p "$staging"
cp "${tmp}/xmux" "${staging}/xmux"
rm -rf "$version_dir"
mv "$staging" "$version_dir"

# The launcher is replaced by renaming a fresh symlink over it, which is atomic.
# The running xmux keeps executing the version directory it was launched from,
# because nothing writes to that file.
mkdir -p "$BIN_DIR"
link_tmp="${BIN_DIR}/.xmux.$$"
ln -sfn "${version_dir}/xmux" "$link_tmp"
mv -f "$link_tmp" "${BIN_DIR}/xmux"

say "installed ${version_dir}/xmux"
say "launcher   ${BIN_DIR}/xmux"

if on_path "$BIN_DIR"; then
    :
elif [ "$MODIFY_PATH" = "1" ]; then
    add_to_path "$BIN_DIR"
else
    say ""
    say "$BIN_DIR is not on PATH. Add it with:"
    say "  export PATH=\"$BIN_DIR:\$PATH\""
fi

# Proof the thing that was installed runs, rather than a claim that it was
# written. A binary that unpacked but cannot execute is a failed install.
installed="$("${version_dir}/xmux" version 2>/dev/null || true)"
[ -n "$installed" ] || die "the installed binary did not run; ${version_dir}/xmux"
say ""
say "$installed"
say "Run \`xmux doctor\` to check what it can reach."
