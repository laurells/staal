#!/bin/sh
# Tests a staal wheel as a new user gets it: installed with pip into a fresh virtual
# environment, with no Rust on PATH, then used as a command (check, run, repl,
# build) and from Python (the package's own library, through the C interface).
#
#   scripts/wheel_test.sh <wheel> [scratch folder]
#
# PYTHON is the Python to make the environment with (default python3).
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
wheel=$1
scratch=${2:-$(mktemp -d)}
rm -rf "$scratch/env" "$scratch/work"
"${PYTHON:-python3}" -m venv "$scratch/env"
"$scratch/env/bin/pip" install --quiet "$wheel"
mkdir -p "$scratch/work"
cd "$scratch/work"
# Only the environment and the system: no cargo, no rustc.
run() { env -i PATH="$scratch/env/bin:/usr/bin:/bin" HOME="$scratch" "$@"; }
if run sh -c 'command -v cargo || command -v rustc' >/dev/null; then
    echo "wheel_test: Rust is on the clean PATH" >&2
    exit 1
fi
fail() { echo "wheel_test: $*" >&2; exit 1; }
cp "$root/tests/profile/work.staal" "$root/tests/profile/work.json" .
[ "$(run staalc check work.staal)" = "work.staal: ok" ] || fail "check"
[ "$(run staalc run work.staal --config work.json)" = "74" ] || fail "run"
[ "$(printf '1 + 1\n' | run staalc repl)" = "2" ] || fail "repl"
set +e
run staalc run missing.staal --config work.json 2>/dev/null
code=$?
set -e
[ "$code" = 2 ] || fail "exit code $code for a missing file, not 2"
printf 'fn main(n: Int) -> Int {\n    return n * 2\n}\n' > app.staal
echo '{"entry": "main", "inputs": {"n": 21}, "model": {"provider": "script", "responses": []}}' > app.json
# `staalc build` makes executables on Linux only so far.
if [ "$(uname -s)" != Linux ]; then
    echo "wheel_test: staalc build is Linux only: not tested here" >&2
elif command -v cc >/dev/null; then
    run staalc build app.staal -o app >/dev/null || fail "build"
    [ "$(./app run --config app.json)" = "42" ] || fail "the built executable"
else
    echo "wheel_test: no cc here: staalc build not tested" >&2
fi
# The wheel is built with native code (the default features).
run env STAAL_INSTALLED=1 STAAL_HAS_NATIVE=1 "$scratch/env/bin/python" -I "$root/python/tests/test_staal.py" "$root" "$scratch/work"
echo "wheel_test: ok"
