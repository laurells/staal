#!/bin/sh
# Seeds fuzz/corpus/<target> from the repository, as fuzz/README.md describes: programs
# for parse and format, run configs for config, staal.json and staal.lock files for
# manifest, and the JWT test tokens and key set for jwt. journal needs no seed (its
# target writes the start record itself). Files already in a corpus are kept.
#
#   scripts/fuzz_seed.sh
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
c=fuzz/corpus
mkdir -p "$c/parse" "$c/format" "$c/config" "$c/manifest" "$c/jwt" "$c/journal"

# Each seed is named by its SHA-256, as libFuzzer names corpus files, so seeding twice
# adds nothing.
seed() {
    for f in "$@"; do
        h=$( (sha256sum "$f" 2>/dev/null || shasum -a 256 "$f") | cut -d' ' -f1)
        cp "$f" "$dir/$h"
    done
}

dir="$c/parse"; seed $(git ls-files '*.staal')
dir="$c/format"; seed $(git ls-files '*.staal')
dir="$c/config"; seed $(git ls-files 'tests/run/*.json' 'tests/crash/*.json')
dir="$c/manifest"; seed $(git ls-files '*staal.json' '*staal.lock')
dir="$c/jwt"; seed tests/fixtures/jwt/jwks.json
# Each test token as its own seed.
python3 - "$c/jwt" <<'EOF'
import hashlib, json, os, sys
for case in json.load(open("tests/fixtures/jwt/cases.json"))["cases"]:
    b = case["token"].encode()
    open(os.path.join(sys.argv[1], hashlib.sha256(b).hexdigest()), "wb").write(b)
EOF
for t in parse format config manifest jwt journal; do
    echo "$t: $(ls "$c/$t" | wc -l) files"
done
