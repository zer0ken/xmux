#!/usr/bin/env bash
# Regenerates the README demo GIFs, and a PNG still of the xmux window, in
# docs/assets from a published xmux release or from a build of this checkout.
#
# usage: scripts/demo/make-gifs.sh [version | --local]
#   version  a published release; defaults to the one in Cargo.toml
#   --local  builds this checkout for Linux in a Rust container and records that build
# needs: docker, node
set -euo pipefail
export MSYS_NO_PATHCONV=1   # Git Bash would rewrite the container paths below

# a native path, because Docker on Windows cannot resolve an msys one
native_pwd() { pwd -W 2>/dev/null || pwd; }
here=$(cd "$(dirname "$0")" && native_pwd)
root=$(cd "$here/../.." && native_pwd)
net=xmux-demo
machines=(laptop gpu-01 web-01)
out="$here/out"
bin="$here/bin"   # the Dockerfile installs an xmux found here instead of downloading one

rm -rf "$bin"
mkdir -p "$bin"
if [ "${1:-}" = "--local" ]; then
  version=local
  echo "building xmux from $root"
  # Named volumes keep the build cache and the crate registry between runs, apart from
  # any other build on the machine.
  docker run --rm -v "$root:/src" -v xmux-demo-target:/target \
    -v xmux-demo-cargo:/usr/local/cargo/registry -w /src -e CARGO_TARGET_DIR=/target \
    rust:1-bookworm sh -c 'cargo build --release --locked && cp /target/release/xmux /src/scripts/demo/bin/xmux'
else
  version=${1:-$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -n 1)}
fi
image="xmux-demo:$version"

teardown() {
  for m in "${machines[@]}"; do docker rm -f "xmux-demo-$m" >/dev/null 2>&1 || true; done
  docker network rm "$net" >/dev/null 2>&1 || true
}
trap teardown EXIT

echo "building $image"
docker build -q --build-arg XMUX_VERSION="$version" -t "$image" "$here" >/dev/null
docker run --rm "$image" xmux --version

teardown
docker network create "$net" >/dev/null
for m in "${machines[@]}"; do
  docker run -d --name "xmux-demo-$m" --hostname "$m" --network "$net" --network-alias "$m" "$image" >/dev/null
done
for m in "${machines[@]}"; do
  docker exec -u dev -w /home/dev "xmux-demo-$m" sh /opt/demo/sessions.sh
done

rm -rf "$out"
for scenario in compare-manual compare-xmux features; do
  docker exec -u dev -w /home/dev xmux-demo-laptop python3 /opt/demo/record.py "$scenario" /tmp/demo
done
docker cp xmux-demo-laptop:/tmp/demo/. "$out"

cd "$here"
npm install --silent --no-audit --no-fund
npx --no-install playwright install chromium >/dev/null
node render.mjs "$out"
docker run --rm -v "$out:/work" "$image" python3 /opt/demo/encode.py /work/manifest.json
cp "$out"/gifs/*.gif "$out"/gifs/*.png "$root/docs/assets/"
