#!/bin/sh
# Builds the staal wheel for this machine's platform: staalc, libstaalc (to load from
# Python) and libstaalc's static form (which `staalc build` links), from a release
# build, in python/staal/_bin. On Linux, auditwheel gives the wheel the manylinux tag
# the binaries really need, from the glibc symbols they use.
#
#   scripts/wheel.sh [output folder]     (default: target/wheels)
#
# Needs: cargo, and a Python with `build` (and `auditwheel` and `patchelf` on Linux):
# set PYTHON. STAAL_RELEASE_DIR uses binaries built elsewhere instead of building
# them here, such as the ones `cargo zigbuild` makes for older glibc (RELEASING.md);
# AUDITWHEEL_PLAT then names the manylinux tag they were built for.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
out=${1:-"$root/target/wheels"}
python=${PYTHON:-python3}
if [ -n "${STAAL_RELEASE_DIR:-}" ]; then
    rel=$STAAL_RELEASE_DIR
else
    cargo build --release --manifest-path "$root/Cargo.toml"
    rel="$root/target/release"
fi
bin="$root/python/staal/_bin"
raw=$(mktemp -d)
# Whatever happens, nothing built is left in python/.
cleanup() {
    rm -rf "$raw" "$bin" "$root/python/build" "$root/python"/*.egg-info "$root/python/LICENSE" "$root/python/NOTICE" "$root/python/LICENSE-THIRD-PARTY.txt"
}
trap cleanup EXIT
cleanup
mkdir -p "$raw" "$bin" "$out"
case "$(uname -s)" in
    Linux) cp "$rel/staalc" "$rel/libstaalc.so" "$rel/libstaalc.a" "$bin/" ;;
    Darwin)
        cp "$rel/staalc" "$rel/libstaalc.dylib" "$rel/libstaalc.a" "$bin/"
        # Tag the wheel for the oldest macOS the binaries run on (their deployment
        # target), after checking each binary really asks for no newer one.
        if [ -n "${MACOSX_DEPLOYMENT_TARGET:-}" ]; then
            for f in "$rel/staalc" "$rel/libstaalc.dylib"; do
                minos=$(otool -l "$f" | awk '/LC_BUILD_VERSION/ {found=1} found && $1 == "minos" {print $2; exit}')
                "$python" -c 'import sys; a, b = (tuple(int(x) for x in v.split(".")) for v in sys.argv[1:3]); sys.exit(0 if a <= b else 1)' "$minos" "$MACOSX_DEPLOYMENT_TARGET" \
                    || { echo "wheel.sh: $f needs macOS $minos, newer than MACOSX_DEPLOYMENT_TARGET=$MACOSX_DEPLOYMENT_TARGET" >&2; exit 2; }
            done
            major=${MACOSX_DEPLOYMENT_TARGET%%.*}
            minor=${MACOSX_DEPLOYMENT_TARGET#*.}
            [ "$minor" = "$MACOSX_DEPLOYMENT_TARGET" ] && minor=0
            STAAL_WHEEL_PLATFORM="macosx_${major}_${minor}_$(uname -m)"
            export STAAL_WHEEL_PLATFORM
        fi
        ;;
    *) echo "wheel.sh: no wheel layout for $(uname -s) yet" >&2; exit 2 ;;
esac
# The licence goes in the wheel's metadata: setuptools takes LICENSE and NOTICE from
# beside pyproject.toml.
cp "$root/LICENSE" "$root/NOTICE" "$root/python/"
"$python" "$root/scripts/third_party_licenses.py" "$root/python/LICENSE-THIRD-PARTY.txt"
"$python" -m build --wheel --outdir "$raw" "$root/python"
case "$(uname -s)" in
    # auditwheel runs patchelf; a virtual environment's tools are beside its Python.
    Linux) PATH="$(dirname "$(command -v "$python")"):$PATH" "$python" -m auditwheel repair --wheel-dir "$out" "$raw"/*.whl ;;
    *) cp "$raw"/*.whl "$out/" ;;
esac
ls "$out"
