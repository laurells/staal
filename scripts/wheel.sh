#!/bin/sh
# Builds the staal wheel for this machine's platform: staalc, libstaalc (to load from
# Python) and libstaalc's static form (which `staalc build` links), from a release
# build, in python/staal/_bin. On Linux, auditwheel gives the wheel the manylinux tag
# the binaries really need, from the glibc symbols they use.
#
#   scripts/wheel.sh [output folder]     (default: target/wheels)
#
# Needs: cargo, and a Python with `build` (and `auditwheel` and `patchelf` on Linux):
# set PYTHON.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
out=${1:-"$root/target/wheels"}
python=${PYTHON:-python3}
cargo build --release --manifest-path "$root/Cargo.toml"
rel="$root/target/release"
bin="$root/python/staal/_bin"
rm -rf "$bin" "$root/python/build"
mkdir -p "$bin" "$out"
case "$(uname -s)" in
    Linux) cp "$rel/staalc" "$rel/libstaalc.so" "$rel/libstaalc.a" "$bin/" ;;
    Darwin) cp "$rel/staalc" "$rel/libstaalc.dylib" "$rel/libstaalc.a" "$bin/" ;;
    *) echo "wheel.sh: no wheel layout for $(uname -s) yet" >&2; exit 2 ;;
esac
# The licence goes in the wheel's metadata: setuptools takes LICENSE and NOTICE from
# beside pyproject.toml.
cp "$root/LICENSE" "$root/NOTICE" "$root/python/"
"$python" "$root/scripts/third_party_licenses.py" "$root/python/LICENSE-THIRD-PARTY.txt"
raw=$(mktemp -d)
"$python" -m build --wheel --outdir "$raw" "$root/python"
case "$(uname -s)" in
    # auditwheel runs patchelf; a virtual environment's tools are beside its Python.
    Linux) PATH="$(dirname "$(command -v "$python")"):$PATH" "$python" -m auditwheel repair --wheel-dir "$out" "$raw"/*.whl ;;
    *) cp "$raw"/*.whl "$out/" ;;
esac
rm -rf "$raw" "$bin" "$root/python/build" "$root/python"/*.egg-info "$root/python/LICENSE" "$root/python/NOTICE" "$root/python/LICENSE-THIRD-PARTY.txt"
ls "$out"
