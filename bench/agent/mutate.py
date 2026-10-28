# Applies each mutation (mutations.py) to a copy of each agent, checks the copy the
# way its language allows (Python: compiles; Staal: `staalc check`), and if it is
# accepted runs every scenario against it. A mutation is caught when the change is
# refused before it runs, or when every scenario still passes: the guarantees held.
#
#   python mutate.py [--json mutations.json]

import argparse
import json
import os
import py_compile
import shutil
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import mutations as M  # noqa: E402
import run as R  # noqa: E402
import scenarios as S  # noqa: E402


def apply(impl, edits):
    d = tempfile.mkdtemp(prefix="mut-")
    shutil.copytree(os.path.join(HERE, impl), d, dirs_exist_ok=True)
    for name, old, new in edits:
        path = os.path.join(d, name)
        with open(path) as f:
            text = f.read()
        if text.count(old) != 1:
            raise SystemExit("mutation does not apply to %s/%s: %r" % (impl, name, old[:60]))
        with open(path, "w") as f:
            f.write(text.replace(old, new))
    return d


def check(impl, d):
    if impl == "python":
        try:
            py_compile.compile(os.path.join(d, "app.py"), doraise=True)
            return None
        except py_compile.PyCompileError as e:
            return str(e)
    import staal
    out = staal.Library().check(os.path.join(d, "agent.staal"))
    if out["status"] == "ok":
        return None
    return "; ".join("%s %s" % (e.get("code"), e.get("message")) for e in out.get("errors", [])) or json.dumps(out)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--json")
    a = ap.parse_args()
    rows = []
    for m in M.MUTATIONS:
        for impl in ["python", "staal"]:
            d = apply(impl, m[impl])
            try:
                refused = check(impl, d)
                broken = []
                if not refused:
                    for sc in S.SCENARIOS:
                        r = R.run_job(impl, sc, app=os.path.join(d, "app.py"))
                        if not r["ok"]:
                            broken.append({"scenario": sc["name"], "problems": r["problems"]})
            finally:
                shutil.rmtree(d, ignore_errors=True)
            verdict = ("refused before it ran" if refused else
                       "guarantees held" if not broken else "%d scenarios broken" % len(broken))
            rows.append({"mutation": m["name"], "control": m.get("control", False), "impl": impl,
                         "refused": refused, "broken": broken, "verdict": verdict})
            print("%-6s %-70s %s" % (impl, m["name"], verdict), flush=True)
            if refused:
                print("         " + refused[:300], flush=True)
            for b in broken[:6]:
                print("         %s: %s" % (b["scenario"], "; ".join(b["problems"])[:200]), flush=True)
    if a.json:
        with open(a.json, "w") as f:
            json.dump(rows, f, indent=1)


if __name__ == "__main__":
    main()
