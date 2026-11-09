#!/usr/bin/env python3
"""Lists every `unsafe` in the Rust sources (src/, node/src/) with the justification
written beside it, as the table in docs/security-review/UNSAFE.md:

    python3 scripts/unsafe_inventory.py > /tmp/table.md

A block's justification is its `// SAFETY:` comment; an `unsafe fn`'s is its
`# Safety` doc section. clippy refuses either missing in the library
(`undocumented_unsafe_blocks`, `missing_safety_doc`), so an empty cell here is a bug.
"""
import re
import subprocess

FILES = subprocess.check_output(["git", "ls-files", "src/*.rs", "node/src/*.rs"], text=True).split()
FN = re.compile(r'^\s*(?:pub(?:\([^)]*\))?\s+)?(?:unsafe\s+)?(?:extern\s+"C"\s+)?fn\s+(\w+)')


def justification(lines, i, kind):
    """The comment text above line i (SAFETY for a block, # Safety docs for a fn)."""
    j = i - 1
    com = []
    while j >= 0:
        t = lines[j].strip()
        if t.startswith("//") or t.startswith("#[") or t == "":
            if t.startswith("//"):
                com.insert(0, t.lstrip("/").strip())
            j -= 1
            if t == "" and com:
                break
            continue
        break
    text = " ".join(com)
    if kind == "fn":
        m = re.search(r"# Safety\s*(.*)", text)
        return m.group(1).strip() if m else ""
    m = re.search(r"SAFETY:\s*(.*)", text)
    return m.group(1).strip() if m else ""


def main():
    rows = []
    for f in FILES:
        lines = open(f).read().split("\n")
        fn = None
        in_tests = False
        for i, l in enumerate(lines):
            if re.match(r"\s*#\[cfg\(test\)\]", l):
                in_tests = True
            m = FN.match(l)
            if m:
                fn = m.group(1)
            code = l.split("//")[0]
            if "#[unsafe(" in code:
                continue  # attributes (export_name, no_mangle) are listed with their fn
            if re.search(r"\bunsafe\s+(?:extern\s+\"C\"\s+)?fn\b", code):
                kind = "fn"
            elif re.search(r"\bunsafe\s*\{", code):
                kind = "block"
            elif re.search(r"\bunsafe\s+impl\b", code):
                kind = "impl"
            else:
                continue
            if kind == "fn" and re.search(r"type\s+\w+\s*=", code):
                continue  # a function pointer type, not code
            n = len(re.findall(r"\bunsafe\s*\{", code)) if kind == "block" else 1
            just = justification(lines, i, kind).replace("|", "\\|")
            rows.append((f, i + 1, fn or "", kind + (" (test)" if in_tests else ""), n, just))
    print("| Location | Function | Kind | Justification |")
    print("| --- | --- | --- | --- |")
    for f, ln, fn, kind, n, just in rows:
        k = kind if n == 1 else f"{kind} ×{n}"
        print(f"| `{f}:{ln}` | `{fn}` | {k} | {just} |")
    total = sum(r[4] for r in rows)
    print(f"\n{len(rows)} lines, {total} `unsafe` items; {sum(1 for r in rows if not r[5])} without a written justification.")


if __name__ == "__main__":
    main()
