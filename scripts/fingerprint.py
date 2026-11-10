#!/usr/bin/env python3
"""A second implementation of the source program fingerprint, `staal-program-v3`,
written from its grammar (RUNTIME.md section 11) and not from staalc's code, and the
generator of tests/fixtures/fingerprint/vectors.json, which staalc's tests compare
against (src/identity.rs).

    python3 scripts/fingerprint.py > tests/fixtures/fingerprint/vectors.json

Grammar, with ns(b) = decimal byte length of b, ":", b, ",":

    fingerprint = ns("staal-program-v3") modules project
    modules     = ns("modules") ns(count) { ns(module-path) ns(text) }        sorted by path bytes
    project     = ns("project") ( ns("none") | ns("some") ns(staal.json) lock packages )
    lock        = ns("lock") ( ns("none") | ns("some") ns(staal.lock) )
    packages    = ns("packages") ns(count) { ns(name) ns(git) ns(commit) ns(count) { ns(path) ns(sha256) } }
                                                       packages sorted by name bytes, files by path bytes
"""
import hashlib
import json
import sys

LABEL = "staal-program-v3"


def ns(field: str) -> bytes:
    b = field.encode("utf-8")
    return str(len(b)).encode("ascii") + b":" + b + b","


def utf8_key(s: str) -> bytes:
    return s.encode("utf-8")


def fingerprint(modules, project) -> bytes:
    out = ns(LABEL) + ns("modules") + ns(str(len(modules)))
    for path, text in sorted(modules, key=lambda m: utf8_key(m[0])):
        out += ns(path) + ns(text)
    out += ns("project")
    if project is None:
        return out + ns("none")
    out += ns("some") + ns(project["manifest"]) + ns("lock")
    out += ns("none") if project["lock"] is None else ns("some") + ns(project["lock"])
    packages = sorted(project["packages"], key=lambda p: utf8_key(p["name"]))
    out += ns("packages") + ns(str(len(packages)))
    for p in packages:
        out += ns(p["name"]) + ns(p["git"]) + ns(p["commit"])
        files = sorted(p["files"], key=lambda f: utf8_key(f[0]))
        out += ns(str(len(files)))
        for path, sha in files:
            out += ns(path) + ns(sha)
    return out


def sha(text: str) -> str:
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def package(name, files, commit="a" * 40):
    return {"name": name, "git": f"https://example.com/{name}.git", "commit": commit,
            "files": [[p, sha(c)] for p, c in files]}


MAIN = "fn main() -> Int {\n    return 1\n}\n"
MANIFEST = '{"dependencies":{"sms":{"git":"https://example.com/sms.git","commit":"' + "a" * 40 + '"}}}'
LOCK = '{"packages":{}}'
SMS = package("sms", [("staal.json", '{"package":"sms","authority":{}}'), ("termii.staal", "pub fn f() {}\n")])
PAY = package("pay", [("staal.json", '{"package":"pay","authority":{}}'), ("a/b.staal", "pub fn g() {}\n")], "b" * 40)

NESTED_SMS = package("sms", [("staal.json", '{"package":"sms","authority":{},"dependencies":{"pay":{"git":"https://example.com/pay.git","commit":"' + "b" * 40 + '"}}}'),
                              ("termii.staal", "import pay.a.b\npub fn f() {}\n")])


def same_source(name):
    """A package whose name alone differs from another's."""
    p = package(name, [("staal.json", '{"authority":{}}'), ("m.staal", "pub fn f() {}\n")])
    p["git"] = "https://example.com/x.git"
    return p


VECTORS = [
    ("one file, no project", [["", MAIN]], None),
    ("one file, its text changed by one byte", [["", MAIN.replace("1", "2")]], None),
    ("an empty file", [["", ""]], None),
    ("two modules", [["", "import m\n" + MAIN], ["m", "pub fn f() {}\n"]], None),
    ("two modules given in the other order", [["m", "pub fn f() {}\n"], ["", "import m\n" + MAIN]], None),
    ("non-ASCII path and text", [["", "import été\n" + MAIN], ["été.x", "// π \U0001F600\n"]], None),
    ("a project with no lock and no packages", [["", MAIN]], {"manifest": "{}", "lock": None, "packages": []}),
    ("a project with an empty lock", [["", MAIN]], {"manifest": "{}", "lock": LOCK, "packages": []}),
    ("a project whose manifest changed", [["", MAIN]], {"manifest": "{ }", "lock": None, "packages": []}),
    ("one package", [["", MAIN], ["sms.termii", "pub fn f() {}\n"]], {"manifest": MANIFEST, "lock": LOCK, "packages": [SMS]}),
    ("two packages given out of order", [["", MAIN]], {"manifest": MANIFEST, "lock": LOCK, "packages": [SMS, PAY]}),
    ("two packages given in order", [["", MAIN]], {"manifest": MANIFEST, "lock": LOCK, "packages": [PAY, SMS]}),
    ("a package file at another path", [["", MAIN]],
     {"manifest": MANIFEST, "lock": LOCK, "packages": [package("sms", [("staal.json", '{"package":"sms","authority":{}}'), ("termii2.staal", "pub fn f() {}\n")])]}),
    ("a package file with other content", [["", MAIN]],
     {"manifest": MANIFEST, "lock": LOCK, "packages": [package("sms", [("staal.json", '{"package":"sms","authority":{}}'), ("termii.staal", "pub fn f() { }\n")])]}),
    ("a package at another commit", [["", MAIN]],
     {"manifest": MANIFEST, "lock": LOCK, "packages": [package("sms", [("staal.json", '{"package":"sms","authority":{}}'), ("termii.staal", "pub fn f() {}\n")], "c" * 40)]}),
    ("a package with no files", [["", MAIN]], {"manifest": MANIFEST, "lock": LOCK, "packages": [package("sms", [])]}),
    # sms depends on pay: the lock holds both, the project's manifest names only sms.
    ("nested dependencies", [["", MAIN]], {"manifest": MANIFEST, "lock": LOCK, "packages": [NESTED_SMS, PAY]}),
    ("module path a.b", [["", MAIN], ["a.b", "pub fn f() {}\n"]], None),
    ("module path b.a: the same names in the other order", [["", MAIN], ["b.a", "pub fn f() {}\n"]], None),
    ("package named ab", [["", MAIN]], {"manifest": MANIFEST, "lock": LOCK, "packages": [same_source("ab")]}),
    ("package named ba: the same letters in the other order", [["", MAIN]], {"manifest": MANIFEST, "lock": LOCK, "packages": [same_source("ba")]}),
]


def main():
    vectors = []
    seen = {}
    for name, modules, project in VECTORS:
        text = fingerprint(modules, project).decode("utf-8")
        vectors.append({"name": name, "modules": modules, "project": project, "fingerprint": text, "sha256": sha(text)})
        seen.setdefault(sha(text), []).append(name)
    # Inputs that differ only in the order they are given in are the same program.
    same = [n for n in seen.values() if len(n) > 1]
    assert same == [["two modules", "two modules given in the other order"], ["two packages given out of order", "two packages given in order"]], same
    # The test in src/identity.rs wants every digest distinct, so keep one of each pair.
    vectors = [v for v in vectors if v["name"] not in ("two modules given in the other order", "two packages given in order")]
    json.dump({"generated_by": "scripts/fingerprint.py", "vectors": vectors}, sys.stdout, ensure_ascii=False, indent=1)
    sys.stdout.write("\n")


if __name__ == "__main__":
    main()
