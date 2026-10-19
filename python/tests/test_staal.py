"""Runs Staal programs through the C interface from Python.

Usage: STAAL_LIBRARY=/path/to/libstaalc.so python3 -I python/tests/test_staal.py <repo root> <scratch dir>
"""

import json
import os
import sys

root, scratch = sys.argv[1], sys.argv[2]
sys.path.insert(0, os.path.join(root, "python"))
import staal  # noqa: E402

lib = staal.Library()
checks = 0


def expect(cond, what):
    global checks
    if not cond:
        raise SystemExit(f"FAILED: {what}")
    checks += 1


def case(name):
    with open(os.path.join(root, "tests", "run", name + ".json")) as f:
        cfg = json.load(f)
    want = cfg.pop("expect")
    return cfg, want


expect(lib.version, "a version")

# The clinic program: booked, and refused for a foreign number, on both engines.
clinic = os.path.join(root, "tests", "run", "domain_clinic.staal")
expect(lib.check(clinic) == {"status": "ok"}, "the clinic program checks")
# Every clinic case, on both engines, against the case's written expectation.
clinic_cases = sorted(f[:-5] for f in os.listdir(os.path.join(root, "tests", "run")) if f.startswith("domain_clinic__"))
expect(len(clinic_cases) >= 6, "the clinic cases")
has_native = os.environ.get("STAAL_HAS_NATIVE") == "1"
if not has_native:
    cfg, _ = case("domain_clinic__booked")
    r = lib.run(clinic, cfg, engine="native")
    expect(r["status"] == "error" and "without native code" in r["error"], f"native refused clearly: {r}")
for engine in [None, "interpreter"] + (["native"] if has_native else []):
    for name in clinic_cases:
        cfg, want = case(name)
        out = lib.run(clinic, cfg, engine=engine)
        if "config_error_contains" in want:
            expect(out["status"] == "error" and want["config_error_contains"] in out["error"], f"{name} ({engine}): {out}")
            continue
        if "result" in want:
            expect(out["status"] == "done" and out["result"] == want["result"], f"{name} ({engine}): {out.get('status')} {out.get('result')}")
        else:
            expect(out["status"] == "failed" and out["failure"]["kind"] == want["failure"], f"{name} ({engine}): {out.get('status')} {out.get('failure')}")
            if "message_contains" in want:
                expect(want["message_contains"] in out["failure"]["message"], f"{name} ({engine}): message")
        expect(out["log"][0]["event"] == "run_start", "the log comes back")

# A journaled run, then resumed on the other engine: it replays to the same result.
journal = os.path.join(scratch, "py.journal")
if os.path.exists(journal):
    os.remove(journal)
cfg, want = case("domain_clinic__booked")
cfg.setdefault("adapters", {}).setdefault("simulated", {})["idempotent"] = False
out = lib.run(clinic, cfg, journal=journal)
expect(out["status"] == "done", f"journaled run: {out.get('status')}")
again = lib.resume(clinic, cfg, journal, engine="native" if has_native else "interpreter")
expect(again["status"] == "done" and again["result"] == out["result"], "resume on the other engine replays the result")

# Errors come back as values, never crashes.
expect(lib.check(os.path.join(scratch, "missing.staal"))["status"] == "error", "a missing file is an error")
bad = os.path.join(scratch, "bad.staal")
with open(bad, "w") as f:
    f.write("fn f() -> Int {\n    return true\n}\n")
r = lib.check(bad)
expect(r["status"] == "invalid" and r["errors"][0]["code"] == "E101", f"check errors are listed: {r}")
expect(lib.run(clinic, {"entry": "nope"})["status"] == "error", "a bad config is an error")
expect(lib.run(clinic, cfg, engine="fast")["status"] == "error", "an unknown engine is an error")
expect(lib.resolve(journal, retry=True)["status"] == "error", "nothing to resolve in a finished run")
expect(lib.resolve(journal, retry=True, failed="x")["status"] == "error", "one resolution at a time")
r = lib._call("staal_run", clinic, "{not json", None, None)
expect(r["status"] == "error" and "not valid JSON" in r["error"], "invalid JSON config")
r = lib._call("staal_run", None, "{}", None, None)
expect(r["status"] == "error" and "NULL" in r["error"], "a NULL program path")

print(f"python: {checks} checks passed")
