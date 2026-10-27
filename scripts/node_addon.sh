#!/bin/sh
# Builds the Node addon (node/) for this machine's platform, into
# <output folder>/<platform>/staal.node, where <platform> is the folder the package's
# index.js loads it from: linux-x64-gnu, linux-arm64-gnu, darwin-arm64 or darwin-x64.
#
#   scripts/node_addon.sh <output folder>
#
# Needs cargo. On Linux, STAAL_ZIGBUILD=1 builds with `cargo zigbuild` for glibc 2.28,
# so the addon loads on Linux systems from 2018 on (RELEASING.md); otherwise it needs
# the glibc of the machine that built it. On macOS, MACOSX_DEPLOYMENT_TARGET sets the
# oldest macOS it runs on.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
[ $# -eq 1 ] || { echo "usage: node_addon.sh <output folder>" >&2; exit 2; }
mkdir -p "$1"
out=$(cd "$1" && pwd)
case "$(uname -s) $(uname -m)" in
    "Linux x86_64") dir="linux-x64-gnu"; target="x86_64-unknown-linux-gnu"; lib="libstaal_node.so" ;;
    "Linux aarch64") dir="linux-arm64-gnu"; target="aarch64-unknown-linux-gnu"; lib="libstaal_node.so" ;;
    "Darwin arm64") dir="darwin-arm64"; target=""; lib="libstaal_node.dylib" ;;
    "Darwin x86_64") dir="darwin-x64"; target=""; lib="libstaal_node.dylib" ;;
    *) echo "node_addon.sh: no Node addon for $(uname -s) $(uname -m) yet" >&2; exit 2 ;;
esac
cd "$root/node"
if [ "${STAAL_ZIGBUILD:-}" = 1 ] && [ -n "$target" ]; then
    cargo zigbuild --release --locked --target "$target.2.28"
    addon="$root/node/target/$target/release/$lib"
else
    cargo build --release --locked
    addon="$root/node/target/release/$lib"
fi
mkdir -p "$out/$dir"
cp "$addon" "$out/$dir/staal.node"
echo "$out/$dir/staal.node"
