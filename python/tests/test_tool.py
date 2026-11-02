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
import threading

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
expect(once.annotations["idempotentHint"] is False, "a key kept for a time does not make the tool idempotent")
once.close()


def ledger_events(directory):
    with open(os.path.join(directory, "server.ledger")) as f:
        return [json.loads(line) for line in f]


def at_once(n, call):
    """Runs call(i) on n threads released together; the C interface releases the GIL."""
    out, gate = [None] * n, threading.Barrier(n)

    def go(i):
        gate.wait()
        out[i] = call(i)
    threads = [threading.Thread(target=go, args=(i,)) for i in range(n)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    return out


# Two tenants of one app, one journal directory: the same arguments are two
# operations, and neither tenant can see the other's run.
tenants = staal.Tool(PROGRAM, name="refund_tenants", description="d", grants=grants,
                     scope=lambda c: c["tenant"], journals=os.path.join(scratch, "tenants"), once={"by": ["order"]})
acme = dict(gold, tenant="acme")
globex = dict(gold, tenant="globex")
a1 = tenants({"order": "123", "amount": usd("5.00")}, context=acme)
b1 = tenants({"order": "123", "amount": usd("5.00")}, context=globex)
a2 = tenants({"order": "123", "amount": usd("5.00")}, context=acme)
expect(a1.status == b1.status == a2.status == "done", f"all done: {a1} {b1} {a2}")
expect(a1.run != b1.run, "the same arguments in two scopes are two runs")
expect(a1.run == a2.run, "the same scope again is the same run")
expect(tenants.status(a1.run, context=acme)["status"] == "done", "a run is described in its scope")
for other in (tenants.status(a1.run, context=globex), tenants.host.status(a1.run)):
    expect(other["status"] == "unknown" and "result" not in other, f"and unknown anywhere else: {other}")
c1 = tenants({"order": "123", "amount": usd("6.00")}, context=globex)
expect(c1.status == "not_run" and "different arguments" in c1.text, f"a conflict is found within the scope: {c1}")
expect("acme" not in c1.text and a1.run not in c1.text, f"nothing of the other tenant is told: {c1}")
for bad in ("", "a\nb", "x" * 257):
    r = tenants({"order": "124", "amount": usd("5.00")}, context=dict(gold, tenant=bad))
    expect(r.status == "not_run" and "scope" in r.text, f"a malformed scope is refused: {bad!r} {r}")
try:
    tenants({"order": "124", "amount": usd("5.00")}, context=dict(gold, tenant=7))
    expect(False, "a scope that is not text")
except TypeError:
    expect(True, "a scope that is not text")
tenants.close()

# Many identical `once` calls at the same moment: one run, one refund.
race_dir = os.path.join(scratch, "race")
race = staal.Tool(PROGRAM, name="refund_race", description="d", grants=grants, journals=race_dir, once={"by": ["order"]})
rs = at_once(16, lambda i: race({"order": "88", "amount": usd("5.00")}, context=gold))
expect(len({r.run for r in rs}) == 1 and all(r.status in ("done", "in_progress") for r in rs),
       f"simultaneous calls of one operation share one run: {[(r.status, r.run) for r in rs]}")
reserves = [e for e in ledger_events(race_dir) if e["e"] == "reserve"]
expect(len(reserves) == 1, f"one reservation: {reserves}")
expect(race.status(rs[0].run)["status"] == "done", "and it ran")
race.close()

# Many calls racing for a money quota: each holds its worst case (100.00) until it
# settles, so of 120.00 only one can be held at a time, and after it settles (35.00
# spent) no other fits. Exactly one runs, whatever the timing.
quota_dir = os.path.join(scratch, "quota-race")
quota_host = staal.Host(quota_dir, max_concurrent_runs=16)
q = staal.Tool(PROGRAM, name="refund_quota", description="d", grants=grants, host=quota_host,
               quota={"window": "1h", "money": usd("120.00")})
rs = at_once(16, lambda i: q({"order": str(1000 + i), "amount": usd("35.00")}, context=gold))
done = [r for r in rs if r.status == "done"]
expect(len(done) == 1 and all(r.status == "not_run" for r in rs if r.status != "done"),
       f"one call within the quota: {[r.status for r in rs]}")
reserves = [e for e in ledger_events(quota_dir) if e["e"] == "reserve"]
expect(len(reserves) == 1, f"refused calls reserve nothing: {reserves}")
q.close()
quota_host.close()

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

    # A module the program imports changes while a run is unfinished: the run is not
    # resumed under the changed code, and the directory refuses to open, naming it.
    mod_dir = os.path.join(scratch, "module")
    os.makedirs(mod_dir, exist_ok=True)
    main = os.path.join(mod_dir, "refund.staal")
    helper = os.path.join(mod_dir, "wording.staal")
    with open(main, "w") as f:
        f.write('import wording\n\n'
                'fn main(order: Text, amount: Money, pay: cap payments.refund(each: 500 USD)) -> Text {\n'
                '    let receipt = pay.refund(order, amount)\n'
                '    return wording.refunded(order)\n'
                '}\n')
    with open(helper, "w") as f:
        f.write('pub fn refunded(order: Text) -> Text {\n    return "Refunded order " + order\n}\n')
    runs = os.path.join(mod_dir, "runs")
    child = subprocess.run([sys.executable, "-I", "-c", code, os.path.join(root, "python"), main, runs],
                           env=env, capture_output=True, text=True)
    expect(child.returncode < 0, f"killed in the middle of the run: {child.returncode} {child.stderr}")
    run = ledger_events(runs)[0]["run"]
    with open(helper, "w") as f:
        f.write('pub fn refunded(order: Text) -> Text {\n    return "Order " + order + " refunded"\n}\n')
    try:
        staal.Host(runs)
        expect(False, "a changed module refuses the unfinished run")
    except staal.StaalError as e:
        expect(run in str(e) and "cannot be resumed" in str(e), f"names the run: {e}")
    with open(helper, "w") as f:
        f.write('pub fn refunded(order: Text) -> Text {\n    return "Refunded order " + order\n}\n')
    host = staal.Host(runs)
    st = host.status(run)
    expect(st["status"] == "done" and st["result"] == "Refunded order 9", f"resumed under the code it started with: {st}")
    host.close()

print(f"{checks} checks passed")
