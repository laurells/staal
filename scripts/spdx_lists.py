#!/usr/bin/env python3
"""Writes the SPDX license and exception identifiers `staalc agent check` accepts
(src/agent/spdx_licenses.txt, src/agent/spdx_exceptions.txt) from the SPDX License List
data files, version 3.27.0 (2025-07-01):

    https://raw.githubusercontent.com/spdx/license-list-data/v3.27.0/json/licenses.json
        SHA-256 157789ba91984aecba5c67b84168dbbd41a235b910a7569aa64bf42a88b24290
    https://raw.githubusercontent.com/spdx/license-list-data/v3.27.0/json/exceptions.json
        SHA-256 650f497016c3a979ec4062067625aedfc10211220abb3b587ca1fb05a22c34eb

    python3 scripts/spdx_lists.py licenses.json exceptions.json

Deprecated identifiers are left out. The files are checked against these hashes, so a
different list is never written by accident.
"""
import hashlib
import json
import sys

WANT = {
    "licenses": "157789ba91984aecba5c67b84168dbbd41a235b910a7569aa64bf42a88b24290",
    "exceptions": "650f497016c3a979ec4062067625aedfc10211220abb3b587ca1fb05a22c34eb",
}


def load(path, kind):
    data = open(path, "rb").read()
    got = hashlib.sha256(data).hexdigest()
    if got != WANT[kind]:
        sys.exit(f"{path}: SHA-256 {got}, expected {WANT[kind]}")
    doc = json.loads(data)
    if doc["licenseListVersion"] != "3.27.0":
        sys.exit(f"{path}: list version {doc['licenseListVersion']}")
    key, field = ("licenses", "licenseId") if kind == "licenses" else ("exceptions", "licenseExceptionId")
    return sorted(x[field] for x in doc[key] if not x["isDeprecatedLicenseId"])


def main():
    for path, kind in [(sys.argv[1], "licenses"), (sys.argv[2], "exceptions")]:
        ids = load(path, kind)
        with open(f"src/agent/spdx_{kind}.txt", "w") as f:
            f.write(f"# SPDX License List 3.27.0, {kind}, deprecated identifiers left out (scripts/spdx_lists.py)\n")
            for i in ids:
                f.write(i + "\n")


if __name__ == "__main__":
    main()
