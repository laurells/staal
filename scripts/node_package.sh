#!/bin/sh
# Builds the `@staal/node` npm package for this machine's platform: the Node addon
# (node/, linking staalc), its JavaScript and types, and the licences, packed with
# `npm pack` into <output folder>/staal-node-X.Y.Z.tgz.
#
#   scripts/node_package.sh [output folder]     (default: target/npm)
#
# Needs: cargo, node and npm. STAAL_ZIGBUILD=1 builds with `cargo zigbuild` for glibc
# 2.28, so the package loads on Linux systems from 2018 on (RELEASING.md); otherwise
# it needs the glibc of the machine that built it.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
out=${1:-"$root/target/npm"}
# Absolute: the packing happens in another folder.
mkdir -p "$out"
out=$(cd "$out" && pwd)
case "$(uname -s) $(uname -m)" in
    "Linux x86_64") dir="linux-x64-gnu"; target="x86_64-unknown-linux-gnu" ;;
    *) echo "node_package.sh: no npm package layout for $(uname -s) $(uname -m) yet" >&2; exit 2 ;;
esac
cd "$root/node"
if [ "${STAAL_ZIGBUILD:-}" = 1 ]; then
    cargo zigbuild --release --locked --target "$target.2.28"
    addon="$root/node/target/$target/release/libstaal_node.so"
else
    cargo build --release --locked
    addon="$root/node/target/release/libstaal_node.so"
fi
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/bin/$dir" "$out"
cp "$addon" "$stage/bin/$dir/staal.node"
cp index.js index.d.ts package.json README.md "$stage/"
cp "$root/LICENSE" "$root/NOTICE" "$stage/"
python3 "$root/scripts/third_party_licenses.py" "$stage/LICENSE-THIRD-PARTY.txt" "$root/node/Cargo.toml"
(cd "$stage" && npm pack --silent --pack-destination "$out")
ls "$out"
