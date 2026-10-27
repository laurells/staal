"""The policy example (examples/policy): the app decides each run's limit, Staal enforces it.

Usage: STAAL_LIBRARY=/path/to/libstaalc.so python3 -I python/tests/test_policy.py <repo root> <scratch dir>
"""

import os
import sys

root, scratch = sys.argv[1], sys.argv[2]
sys.path.insert(0, os.path.join(root, "python"))
sys.path.insert(0, os.path.join(root, "examples", "policy"))
import staal  # noqa: E402
import app  # noqa: E402

lib = staal.Library()
checks = 0


def expect(cond, what):
    global checks
    if not cond:
        raise SystemExit(f"FAILED: {what}")
    checks += 1


def model(amount):
    """A scripted model that proposes a refund of `amount` USD for order 4417."""
    return {"provider": "script", "responses": [
        {"output": {"order": "4417", "amount": {"amount": amount, "currency": "USD"}, "reason": "arrived damaged"},
         "output_tokens": 20}]}


def refused_at(out, limit, needed):
    want = f"'payments.refund' allows at most {limit} USD per action; this action needs {needed} USD"
    return out["status"] == "failed" and out["failure"] == {"kind": "capability", "message": want}


expect(lib.check(app.PROGRAM) == {"status": "ok"}, "the program checks")
gold, standard = {"id": "c-1", "tier": "gold"}, {"id": "c-2", "tier": "standard"}
engines = [None, "interpreter"] + (["native"] if os.environ.get("STAAL_HAS_NATIVE") == "1" else [])
for engine in engines:
    # Within the customer's limit: refunded.
    out = app.handle(lib, gold, 400, "broken", model("300.00"), engine=engine)
    expect(out["status"] == "done" and out["result"] == "Refunded 300.00 USD for order 4417", f"gold refund {engine}: {out}")
    # The same proposal for a standard customer: refused before any refund is made.
    out = app.handle(lib, standard, 400, "broken", model("300.00"), engine=engine)
    expect(refused_at(out, "50.00", "300.00"), f"standard refused {engine}: {out}")
    # The order's own cost limits it too.
    out = app.handle(lib, standard, 30, "broken", model("40.00"), engine=engine)
    expect(refused_at(out, "30.00", "40.00"), f"order cost limits {engine}: {out}")
    out = app.handle(lib, standard, 30, "broken", model("30.00"), engine=engine)
    expect(out["status"] == "done", f"up to the order cost {engine}: {out}")

# A bug in the app that grants more than the code allows still stops at the code's 500 USD.
app.TIER_LIMIT["gold"] = 900
for engine in engines:
    out = app.handle(lib, gold, 1000, "broken", model("600.00"), engine=engine)
    expect(refused_at(out, "500.00", "600.00"), f"code ceiling {engine}: {out}")
    out = app.handle(lib, gold, 1000, "broken", model("500.00"), engine=engine)
    expect(out["status"] == "done", f"up to the code ceiling {engine}: {out}")
app.TIER_LIMIT["gold"] = 500

# With a journal, resuming a finished run gives its answer again and refunds nothing more.
journal = os.path.join(scratch, "policy.journal")
if os.path.exists(journal):
    os.remove(journal)
first = app.handle(lib, gold, 400, "broken", model("120.00"), journal=journal)
expect(first["status"] == "done", f"journaled run: {first}")
config = {"entry": "main", "inputs": {"complaint": "broken"},
          "grants": {"pay": {"cap": "payments.refund", "each": {"amount": "400", "currency": "USD"}}},
          "model": model("120.00")}
again = lib.resume(app.PROGRAM, config, journal)
expect(again["status"] == "done" and again["result"] == first["result"], f"resumed: {again}")
def performed(out):
    return [e["replayed"] for e in out["log"] if e["event"] == "action_performed"]


expect(performed(first) == [False], f"one refund made: {first['log']}")
expect(performed(again) == [True], f"the refund replayed from the journal, not made again: {again['log']}")

print(f"{checks} checks passed")
