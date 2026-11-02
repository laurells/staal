"""staal.Tool and staal.Host: a program as a tool, with grants from the app per call,
quotas and `once` across calls and across processes, and recovery after a crash.

Usage: STAAL_LIBRARY=/path/to/libstaalc.so python3 -I python/tests/test_tool.py <repo root> <scratch dir>
With STAAL_HAS_FAILPOINTS=1 (a library built with crash injection), a child process is
killed in the middle of a call, and the next host to open the directory finishes it.
"""

import json
import os
import shutil
import subprocess
import sys

root, scratch = sys.argv[1], sys.argv[2]
sys.path.insert(0, os.path.join(root, "python"))
import staal  # noqa: E402

checks = 0


def expect(cond, what):
    global checks
    if not cond:
        raise SystemExit(f"FAILED: {what}")
    checks += 1


def usd(a):
    return {"amount": a, "currency": "USD"}


PROGRAM = os.path.join(scratch, "refund.staal")
with open(PROGRAM, "w") as f:
    f.write('fn main(order: Text, amount: Money, pay: cap payments.refund(each: 500 USD)) -> Text {\n'
            '    let receipt = pay.refund(order, amount)\n'
            '    return "Refunded " + to_text(amount) + " for order " + order\n'
            '}\n')
ADD = os.path.join(scratch, "add.staal")
with open(ADD, "w") as f:
    f.write("fn plus(a: Int, b: Int) -> Int {\n    return a + b\n}\n")


def grants(ctx):
    """The app's policy: each customer's limit, from the app's own records."""
    return {"pay": {"cap": "payments.refund", "each": usd(ctx["limit"]), "uses": 1}}


journals = os.path.join(scratch, "runs")
shutil.rmtree(journals, ignore_errors=True)

# A tool whose grants the app computes for each call.
refund = staal.Tool(PROGRAM, name="refund_order", description="Refund part of an order.",
                    grants=grants, journals=journals,
                    quota={"window": "1h", "calls": 4, "money": usd("120.00")})
expect(refund.name == "refund_order" and refund.per_call_grants, "the tool takes its grants per call")
expect(refund.schema["required"] == ["order", "amount"], "the schema of its arguments")
expect("pay" not in refund.schema["properties"], "a grant is never an argument")
expect(refund.output_schema["properties"]["result"] == {"type": "string"}, "the result's schema")
expect(refund.annotations["readOnlyHint"] is False, "grants unknown until the call claim nothing")

gold, standard = {"limit": "100.00"}, {"limit": "20.00"}
r = refund({"order": "4417", "amount": usd("35.00")}, context=gold)
expect(r.status == "done" and not r.is_error, f"a refund within the gold limit: {r}")
expect(r.value == "Refunded 35.00 USD for order 4417", f"the value: {r.value}")
expect(len(r.run) == 32, "the run's id")
first = r.run
r = refund(order="4418", amount=usd("35.00"), context=standard)
expect(r.status == "failed" and r.text == "'payments.refund' allows at most 20.00 USD per action; this action needs 35.00 USD",
       f"the standard limit, from the app's policy: {r}")
r = refund({"order": "4419", "amount": "35.00"}, context=gold)
expect(r.status == "not_run" and r.run is None and "inputs.amount" in r.text, f"arguments the types refuse: {r}")
# Money held: 35.00 settled, then each run reserves its worst case (here up to 100.00).
r = refund({"order": "4420", "amount": usd("1.00")}, context=gold)
expect(r.status == "not_run" and "can spend up to 100.00 USD" in r.text, f"the money quota holds: {r}")
r = refund({"order": "4420", "amount": usd("1.00")}, context=standard)
expect(r.status == "done", f"a smaller grant fits: {r}")
st = refund.status(first)
expect(st["status"] == "done" and st["result"] == "Refunded 35.00 USD for order 4417", f"status: {st}")
expect(refund.status("0" * 32)["status"] == "unknown", "an unknown run")
try:
    refund.status("nope")
    expect(False, "a malformed run id is refused")
except staal.StaalError:
    expect(True, "a malformed run id is refused")
refund.close()

# The quota is kept in the journal directory: a new host in this process, or in
# another, sees the calls already made.
again = staal.Tool(PROGRAM, name="refund_order", description="Refund part of an order.",
                   grants=grants, journals=journals,
                   quota={"window": "1h", "calls": 4, "money": usd("120.00")})
expect(again({"order": "5", "amount": usd("1.00")}, context=standard).status == "done", "a fourth call")
r = again({"order": "6", "amount": usd("1.00")}, context=standard)
expect(r.status == "not_run" and "quota of 4 calls" in r.text, f"a fifth call is refused across hosts: {r}")
again.close()

# One process uses a directory at a time; this process opening it twice shares it.
h1 = staal.Host(journals)
h2 = staal.Host(journals)
try:
    staal.Host(journals, keep=10)
    expect(False, "other settings for an open directory are refused")
except staal.StaalError as e:
    expect("already open in this process with other settings" in str(e), str(e))
child = subprocess.run([sys.executable, "-I", "-c",
                        "import sys; sys.path.insert(0, sys.argv[1]); import staal; staal.Host(sys.argv[2])",
                        os.path.join(root, "python"), journals], capture_output=True, text=True)
expect(child.returncode != 0 and "another process is using" in child.stderr, f"another process waits, then is refused: {child.stderr}")
h1.close()
h2.close()

# `once`: the same operation called again is one run.
once = staal.Tool(PROGRAM, name="refund_once", description="Refund an order once.",
                  grants=grants, journals=os.path.join(scratch, "once"), once={"by": ["order"]})
a = once({"order": "77", "amount": usd("5.00")}, context=gold)
b = once({"order": "77", "amount": usd("5")}, context=gold)
expect(a.status == "done" and b.status == "done" and a.run == b.run and a.text == b.text, f"one run: {a} {b}")
c = once({"order": "77", "amount": usd("6.00")}, context=gold)
expect(c.status == "not_run" and "different arguments" in c.text and c.run is None, f"another operation under the same key: {c}")
expect(once.annotations["idempotentHint"] is True, "once makes the tool idempotent")
once.close()

# Fixed grants, and a function that takes no capabilities.
fixed = staal.Tool(PROGRAM, name="refund_fixed", description="d", journals=os.path.join(scratch, "fixed"),
                   grants={"pay": {"cap": "payments.refund", "each": usd("10.00")}})
expect(not fixed.per_call_grants and fixed({"order": "1", "amount": usd("9.00")}).status == "done", "fixed grants")
fixed.close()
plus = staal.Tool(ADD, name="plus", description="Adds.", entry="plus", journals=os.path.join(scratch, "plus"), engine="interpreter")
expect(plus(a=2, b=3).value == 5, "a pure function")
expect(plus.annotations == {"readOnlyHint": True, "destructiveHint": False, "idempotentHint": True, "openWorldHint": False},
       f"annotations: {plus.annotations}")
plus.close()
for bad, why in [(dict(grants=grants), "takes no grants"), (dict(colour=1), "unknown argument")]:
    try:
        staal.Tool(ADD, name="x", description="d", entry="plus", journals=os.path.join(scratch, "bad"), **bad)
        expect(False, why)
    except TypeError as e:
        expect(why in str(e), str(e))
try:
    staal.Tool(PROGRAM, name="x", description="d", journals=os.path.join(scratch, "bad"))
    expect(False, "a program that needs grants, given none")
except TypeError as e:
    expect("needs grants" in str(e), str(e))
try:
    staal.Tool(os.path.join(scratch, "missing.staal"), name="x", description="d", journals=os.path.join(scratch, "bad"))
    expect(False, "a missing program")
except staal.StaalError as e:
    expect("missing.staal" in str(e), str(e))

# A crash in the middle of a call: the next host to open the directory finishes the run.
if os.environ.get("STAAL_HAS_FAILPOINTS") == "1":
    crash_dir = os.path.join(scratch, "crash")
    code = ("import sys, json; sys.path.insert(0, sys.argv[1]); import staal\n"
            "t = staal.Tool(sys.argv[2], name='refund_order', description='d', journals=sys.argv[3],\n"
            "               grants=lambda c: {'pay': {'cap': 'payments.refund', 'each': {'amount': '50.00', 'currency': 'USD'}}})\n"
            "t({'order': '9', 'amount': {'amount': '3.00', 'currency': 'USD'}})\n")
    env = dict(os.environ, STAALC_FAILPOINT="after_write:3")
    child = subprocess.run([sys.executable, "-I", "-c", code, os.path.join(root, "python"), PROGRAM, crash_dir],
                           env=env, capture_output=True, text=True)
    expect(child.returncode < 0, f"the child was killed in the middle of the run: {child.returncode} {child.stderr}")
    with open(os.path.join(crash_dir, "server.ledger")) as f:
        events = [json.loads(line) for line in f]
    run = events[0]["run"]
    expect([e["e"] for e in events] == ["reserve"], f"reserved, not settled: {events}")
    host = staal.Host(crash_dir)
    st = host.status(run)
    expect(st["status"] == "done" and st["result"] == "Refunded 3.00 USD for order 9", f"finished when the directory was opened: {st}")
    host.close()

print(f"{checks} checks passed")
