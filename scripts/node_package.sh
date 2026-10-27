#!/bin/sh
# Builds the `@staal/node` npm package: the Node addon for each platform it holds, its
# JavaScript and types, and the licences, packed with `npm pack` into
# <output folder>/staal-node-X.Y.Z.tgz.
#
#   scripts/node_package.sh [output folder]     (default: target/npm)
#
# By default it holds this machine's addon (scripts/node_addon.sh, with STAAL_ZIGBUILD
# as there). STAAL_NODE_ADDONS names a folder of addons built elsewhere, laid out as
# node_addon.sh writes them (<platform>/staal.node), and packs all of them: the
# release puts every platform's addon in one package. Needs cargo, python3, node and npm.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
out=${1:-"$root/target/npm"}
# Absolute: the packing happens in another folder.
mkdir -p "$out"
out=$(cd "$out" && pwd)
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/bin"
if [ -n "${STAAL_NODE_ADDONS:-}" ]; then
    cp -R "$STAAL_NODE_ADDONS"/. "$stage/bin/"
else
    sh "$root/scripts/node_addon.sh" "$stage/bin"
fi
# The licences of every platform's addon in the package.
targets=""
for d in "$stage"/bin/*/; do
    name=$(basename "$d")
    [ -f "$d/staal.node" ] || { echo "node_package.sh: $name holds no staal.node" >&2; exit 2; }
    case "$name" in
        linux-x64-gnu) t="x86_64-unknown-linux-gnu" ;;
        linux-arm64-gnu) t="aarch64-unknown-linux-gnu" ;;
        darwin-arm64) t="aarch64-apple-darwin" ;;
        darwin-x64) t="x86_64-apple-darwin" ;;
        *) echo "node_package.sh: unknown platform folder $name" >&2; exit 2 ;;
    esac
    targets="${targets:+$targets,}$t"
done
[ -n "$targets" ] || { echo "node_package.sh: no addon to pack" >&2; exit 2; }
cd "$root/node"
cp index.js index.d.ts package.json README.md "$stage/"
cp "$root/LICENSE" "$root/NOTICE" "$stage/"
STAAL_LICENCE_TARGET="$targets" python3 "$root/scripts/third_party_licenses.py" "$stage/LICENSE-THIRD-PARTY.txt" "$root/node/Cargo.toml"
(cd "$stage" && npm pack --silent --pack-destination "$out")
echo "platforms: $targets"
ls "$out"
