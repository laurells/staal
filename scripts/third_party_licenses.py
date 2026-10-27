#!/usr/bin/env python3
"""Writes the licences of the code staalc's binaries contain.

staalc, libstaalc and the executables `staalc build` makes are linked from staalc's
Rust dependencies. Their licences (MIT, Apache-2.0, ISC, BSD, Zlib, Unicode-3.0,
CDLA-Permissive-2.0) ask that their notices go with copies, so every release archive
and wheel carries this file next to staalc's own LICENSE and NOTICE.

    scripts/third_party_licenses.py <output file> [Cargo.toml]

The manifest is staalc's own unless another is given: node/Cargo.toml, for the Node
addon, which links staalc and the crates that bind it to Node.

It reads `cargo metadata` (the locked versions, for this machine's platform), follows
the normal dependencies of staalc (not its tests' or build scripts'), and for each
crate copies, verbatim, the licence files its package holds. Crates whose licence
files are identical are listed together under one copy. A crate that holds no licence
file but is licensed under the same terms as a crate of its project that does (the
Bytecode Alliance's crates, under Apache-2.0 WITH LLVM-exception) gets that text; a
crate of a project none of whose crates has one gets the project's licence from
scripts/licences/ (napi-rs, for the Node addon); any other crate without one stops
the script, so nothing is left out unnoticed.
"""

import json
import os
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PREFIXES = ("LICEN", "COPYING", "NOTICE", "UNLICENSE")
# Projects whose crates ship no licence file at all: the licence from the project's
# repository, kept in scripts/licences/ (where each came from is in its key).
PROJECT_LICENCES = {
    "https://github.com/napi-rs/napi-rs": ("LICENSE (github.com/napi-rs/napi-rs, tag napi@2.16.17)", "napi-rs.LICENSE"),
}


def licence_files(directory):
    names = sorted(n for n in os.listdir(directory) if n.upper().startswith(PREFIXES) and os.path.isfile(os.path.join(directory, n)))
    out = []
    for n in names:
        with open(os.path.join(directory, n), encoding="utf-8", errors="replace") as f:
            out.append((n, f.read().strip("\n")))
    return out


def main():
    if len(sys.argv) not in (2, 3):
        sys.exit("usage: third_party_licenses.py <output file> [Cargo.toml]")
    manifest = os.path.abspath(sys.argv[2]) if len(sys.argv) == 3 else os.path.join(ROOT, "Cargo.toml")
    host = next(l.split(": ", 1)[1] for l in subprocess.check_output(["rustc", "-vV"], text=True).splitlines() if l.startswith("host: "))
    meta = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--format-version", "1", "--locked", "--filter-platform", host, "--manifest-path", manifest],
        text=True))
    packages = {p["id"]: p for p in meta["packages"]}
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    root = meta["resolve"]["root"]
    linked, todo = set(), [root]
    while todo:
        i = todo.pop()
        if i in linked:
            continue
        linked.add(i)
        for d in nodes[i]["deps"]:
            if any(k["kind"] is None for k in d["dep_kinds"]):
                todo.append(d["pkg"])
    linked.discard(root)
    # staalc itself, when it is a path dependency (the Node addon): its licence is
    # LICENSE and NOTICE, not a third party's.
    linked = {i for i in linked if packages[i]["source"] is not None}

    crates = sorted((packages[i] for i in linked), key=lambda p: (p["name"], p["version"]))
    texts = {}
    for p in crates:
        texts[p["id"]] = licence_files(os.path.dirname(p["manifest_path"]))
    # A crate without its own file: the text of a crate of the same repository and
    # licence that has one.
    own = {i for i, t in texts.items() if t}
    for p in crates:
        if texts[p["id"]]:
            continue
        donor = next((q for q in crates if q["id"] in own and q["license"] == p["license"] and q.get("repository") == p.get("repository")), None)
        if donor is None and p["license"] == "Apache-2.0 WITH LLVM-exception":
            donor = next((q for q in crates if q["id"] in own and q["license"] == p["license"] and q["name"].startswith("cranelift-")), None)
        if donor is None and p.get("repository") in PROJECT_LICENCES:
            name, file = PROJECT_LICENCES[p["repository"]]
            with open(os.path.join(ROOT, "scripts", "licences", file), encoding="utf-8") as f:
                texts[p["id"]] = [(name, f.read().strip("\n"))]
            continue
        if donor is None:
            sys.exit(f"{p['name']} {p['version']} ({p['license']}) holds no licence file, and no crate of its project does: add its licence by hand")
        texts[p["id"]] = [(f"{name} (from {donor['name']} {donor['version']}, same project and licence)", text) for name, text in texts[donor["id"]]]

    groups = {}
    for p in crates:
        key = tuple(texts[p["id"]])
        groups.setdefault(key, []).append(p)

    out = [
        "Third-party software in staalc",
        "",
        *(["The staal Node.js addon contains staalc and the following crates. Each is",
           "listed with its licence, followed by the licence files its package holds,",
           "verbatim. staalc's own licence is in LICENSE and NOTICE."]
          if packages[root]["name"] == "staal-node" else
          ["staalc, libstaalc and the executables `staalc build` makes contain the following",
           "crates. Each is listed with its licence, followed by the licence files its package",
           "holds, verbatim. staalc's own licence is in LICENSE and NOTICE."]),
        "",
        "SQLite, which rusqlite builds into staalc (through libsqlite3-sys), is in the public",
        "domain: https://www.sqlite.org/copyright.html",
        "",
        f"{len(crates)} crates.",
    ]
    for key, members in sorted(groups.items(), key=lambda kv: (kv[1][0]["name"], kv[1][0]["version"])):
        out.append("")
        out.append("=" * 80)
        for p in members:
            repo = f" <{p['repository']}>" if p.get("repository") else ""
            out.append(f"{p['name']} {p['version']}: {p['license']}{repo}")
        out.append("=" * 80)
        for name, text in key:
            out.append("")
            out.append(f"--- {name} ---")
            out.append("")
            out.append(text)
    with open(sys.argv[1], "w", encoding="utf-8") as f:
        f.write("\n".join(out) + "\n")
    print(f"{sys.argv[1]}: {len(crates)} crates, {len(groups)} licence texts")


if __name__ == "__main__":
    main()
