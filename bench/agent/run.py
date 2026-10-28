# Runs every scenario against each agent and checks the world afterwards.
#
#   python run.py                       both agents, every scenario
#   python run.py --impl staal          one agent
#   python run.py --only "crash"        scenarios whose name contains the text
#   python run.py --json results.json   also write the results
#
# Each job runs as its own process, started again whenever it is killed, as a job
# runner would. A job that stops is resumed; if it stays stopped, the operator
# resolves it from the mail service's own records, and that is counted. The Staal
# agent needs `pip install staal` in the Python that runs this.
#
# A scenario passes when the world's effects are one of those the scenario allows
# and no invariant broke: no secret in the job's files, and no ticket text in a
# model's instruction.

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import scenarios as S  # noqa: E402
import world as W  # noqa: E402

IMPLS = {"python": os.path.join(HERE, "python", "app.py"), "staal": os.path.join(HERE, "staal", "app.py")}
ENV = dict(os.environ, STRIPE_KEY=W.STRIPE_KEY, MAIL_KEY=W.MAIL_KEY, ANTHROPIC_API_KEY=W.MODEL_KEY)
MAX_ROUNDS = 10


def worker(world, app, args, jobdir):
    p = subprocess.Popen([sys.executable, app] + args + [jobdir] if args[0] == "run" else
                         [sys.executable, app, args[0], jobdir] + args[1:],
                         env=ENV, cwd=jobdir, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    world.worker = p
    out, err = p.communicate(timeout=120)
    world.worker = None
    if p.returncode < 0:
        return {"status": "killed"}
    try:
        return json.loads(out.strip().splitlines()[-1])
    except (ValueError, IndexError):
        return {"status": "crashed", "stderr": err[-2000:]}


def run_job(impl, sc, app=None):
    world = W.World(sc)
    jobdir = tempfile.mkdtemp(prefix="bench-")
    try:
        with open(os.path.join(jobdir, "job.json"), "w") as f:
            json.dump({"ticket": sc["ticket"], "order": sc["order"], "world": world.base}, f)
        app = app or IMPLS[impl]
        restarts = resumes = resolves = 0
        stuck = 0
        out = None
        for _ in range(MAX_ROUNDS):
            out = worker(world, app, ["run"], jobdir)
            if out["status"] == "killed":
                restarts += 1
                continue
            if out["status"] != "stopped":
                break
            # Stopped: resume it; if that changes nothing twice, an operator looks at
            # the mail service's records and says whether the message was sent.
            stuck += 1
            if stuck < 3:
                resumes += 1
                continue
            sent = world.emails[-1]["id"] if world.emails else None
            r = worker(world, app, ["resolve"] + (["sent", sent] if sent else ["retry"]), jobdir)
            if r["status"] != "resolved":
                out = {"status": "operator could not resolve", "detail": r}
                break
            resolves += 1
            stuck = 0
        got = S.effects([(r["payment_intent"], r["amount"]) for r in world.refunds], [e["to"] for e in world.emails])
        problems = []
        if got not in sc["expect"]:
            problems.append("effects %s, allowed %s" % (json.dumps(got), json.dumps(sc["expect"])))
        if out["status"] not in ("done", "failed"):
            problems.append("ended %s" % out["status"])
        for r in world.requests:
            if r["kind"] == "model":
                system = json.loads(r["body"]).get("system", "")
                if sc["ticket"] in system or "IMPORTANT SYSTEM NOTICE" in system:
                    problems.append("ticket text in the model's instruction")
                    break
        # One model call per job; a crash after the model answered, before its answer
        # arrived, may cost one more.
        allowed = 1 + sum(1 for c in sc.get("crashes", []) if c["at"] == "model" and c["when"] == "after")
        if world.model_answers > allowed:
            problems.append("%d model calls answered, the budget is 1" % world.model_answers)
        for root, _, files in os.walk(jobdir):
            for name in files:
                with open(os.path.join(root, name), "rb") as f:
                    data = f.read()
                for key in (W.STRIPE_KEY, W.MAIL_KEY, W.MODEL_KEY):
                    if key.encode() in data:
                        problems.append("a secret in %s" % name)
        return {"impl": impl, "scenario": sc["name"], "group": sc["group"], "ok": not problems, "problems": problems,
                "status": out["status"], "failure": (out.get("failure") or {}).get("kind"),
                "restarts": restarts, "resumes": resumes, "operator": resolves, "killed": world.killed,
                "requests": len([r for r in world.requests if r["kind"] != "count"]),
                "within_policy": sc.get("within_policy", False)}
    finally:
        world.close()
        shutil.rmtree(jobdir, ignore_errors=True)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--impl", choices=sorted(IMPLS), action="append")
    ap.add_argument("--only")
    ap.add_argument("--json")
    a = ap.parse_args()
    impls = a.impl or ["python", "staal"]
    rows = []
    for sc in S.SCENARIOS:
        if a.only and a.only not in sc["name"]:
            continue
        for impl in impls:
            r = run_job(impl, sc)
            rows.append(r)
            print("%-6s %-4s %-62s %-8s %-11s restarts %d resumes %d operator %d %s" % (
                impl, "ok" if r["ok"] else "FAIL", sc["name"], r["status"], r["failure"] or "",
                r["restarts"], r["resumes"], r["operator"], "; ".join(r["problems"])), flush=True)
    for impl in impls:
        mine = [r for r in rows if r["impl"] == impl]
        print("%s: %d of %d scenarios pass, %d operator resolutions" % (
            impl, sum(r["ok"] for r in mine), len(mine), sum(r["operator"] for r in mine)))
    if a.json:
        with open(a.json, "w") as f:
            json.dump(rows, f, indent=1)
    return 0 if all(r["ok"] for r in rows) else 1


if __name__ == "__main__":
    sys.exit(main())
