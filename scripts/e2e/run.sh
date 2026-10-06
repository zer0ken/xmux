#!/usr/bin/env bash
# Runs the end-to-end suite with the Linux client: builds the images and the xmux of
# this checkout, then runs suite.py inside the client container. Arguments go to
# suite.py; the results land in scripts/e2e/out.
#
# usage: scripts/e2e/run.sh [--os debian,alpine] [--mux tmux,...] [--scenario ...]
#   XMUX_E2E_BIN  a prebuilt static Linux xmux to test instead of building one
# needs: docker
set -euo pipefail
export MSYS_NO_PATHCONV=1   # Git Bash would rewrite the container paths below

# a native path, because Docker on Windows cannot resolve an msys one
native_pwd() { pwd -W 2>/dev/null || pwd; }
here=$(cd "$(dirname "$0")" && native_pwd)
root=$(cd "$here/../.." && native_pwd)
target=x86_64-unknown-linux-musl

echo "building images"
docker build -q -t xmux-e2e-client -f "$here/client/Dockerfile" "$here/client" >/dev/null
for os in debian alpine; do
  docker build -q -t "xmux-e2e-host:$os" -f "$here/hosts/Dockerfile.$os" "$here/hosts" >/dev/null
done

if [ -n "${XMUX_E2E_BIN:-}" ]; then
  bin_dir=$(cd "$(dirname "$XMUX_E2E_BIN")" && native_pwd)
  bin_mount=(-v "$bin_dir:/opt/xmux-bin:ro")
  bin="/opt/xmux-bin/$(basename "$XMUX_E2E_BIN")"
else
  echo "building xmux for $target"
  # A static build runs on every host image; the volumes keep the build incremental.
  docker run --rm -v "$root:/src:ro" -v xmux-e2e-target:/target \
    -v xmux-e2e-cargo:/usr/local/cargo/registry -w /src rust:1-bookworm \
    sh -c "rustup target add $target >/dev/null 2>&1 && cargo build --locked -q --target $target --target-dir /target"
  bin_mount=(-v xmux-e2e-target:/opt/xmux-target:ro)
  bin="/opt/xmux-target/$target/debug/xmux"
fi

# suite.py removes what it starts; this covers a run that was killed.
cleanup() {
  docker rm -f xmux-e2e-runner $(docker ps -aq -f label=xmux-e2e) >/dev/null 2>&1 || true
  docker network rm xmux-e2e >/dev/null 2>&1 || true
}
trap cleanup EXIT
cleanup
mkdir -p "$here/out"
docker run --rm --init --name xmux-e2e-runner \
  -v /var/run/docker.sock:/var/run/docker.sock \
  -v "$here:/e2e:ro" -v "$here/out:/out" "${bin_mount[@]}" \
  -e XMUX_E2E_SELF=xmux-e2e-runner -e XMUX_E2E_BIN="$bin" -e XMUX_E2E_OUT=/out \
  xmux-e2e-client python3 -u /e2e/suite.py "$@"
