#!/bin/sh
# Packages a release of staalc for this machine's platform, from a release build:
#
#   target/dist/staalc-<version>-<target>.tar.gz   staalc, libstaalc (to load), its
#                                                   static form (for `staalc build`),
#                                                   include/staal.h, README, CHANGELOG,
#                                                   LICENSE and NOTICE
#   target/dist/staalc-<version>-<target>.tar.gz.sha256
#
#   scripts/package.sh [output folder]
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
out=${1:-"$root/target/dist"}
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$root/Cargo.toml" | head -n 1)
target=$(rustc -vV | sed -n 's/^host: //p')
[ -n "$version" ] && [ -n "$target" ] || { echo "package.sh: no version or target" >&2; exit 2; }
cargo build --release --manifest-path "$root/Cargo.toml"
rel="$root/target/release"
name="staalc-$version-$target"
stage=$(mktemp -d)
mkdir -p "$stage/$name/include" "$out"
case "$(uname -s)" in
    Linux) cp "$rel/staalc" "$rel/libstaalc.so" "$rel/libstaalc.a" "$stage/$name/" ;;
    Darwin) cp "$rel/staalc" "$rel/libstaalc.dylib" "$rel/libstaalc.a" "$stage/$name/" ;;
    *) echo "package.sh: no package layout for $(uname -s) yet" >&2; exit 2 ;;
esac
cp "$root/include/staal.h" "$stage/$name/include/"
cp "$root/README.md" "$root/CHANGELOG.md" "$root/LICENSE" "$root/NOTICE" "$stage/$name/"
tar -C "$stage" -czf "$out/$name.tar.gz" "$name"
rm -rf "$stage"
cd "$out"
if command -v sha256sum >/dev/null; then sha256sum "$name.tar.gz" > "$name.tar.gz.sha256"; else shasum -a 256 "$name.tar.gz" > "$name.tar.gz.sha256"; fi
echo "$out/$name.tar.gz"
