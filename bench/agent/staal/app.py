# The app that runs the Staal agent (agent.staal). It decides the policy (a refund
# limit from the customer's tier and what they paid; which payment and address the
# run may act on), grants it, and runs, resumes or resolves the job's journal.
#
#   python app.py run JOBDIR                 run the job, or resume it after a crash
#   python app.py resolve JOBDIR sent ID     the message in doubt was sent, as ID
#   python app.py resolve JOBDIR retry       it was not sent: send it again
#
# Prints one JSON line {"status": "done"|"failed"|"stopped", ...}; exits 0, 1 or 3.

import json
import os
import sys

import staal

HERE = os.path.dirname(os.path.abspath(__file__))
PROGRAM = os.path.join(HERE, "agent.staal")
TIER_LIMIT_CENTS = {"standard": 5000, "gold": 50000}


def usd(cents):
    return {"amount": "%d.%02d" % divmod(cents, 100), "currency": "USD"}


def config(job):
    order, base = job["order"], job["world"]
    limit = min(TIER_LIMIT_CENTS[order["tier"]], order["paid_cents"])
    return {
        "entry": "main",
        "inputs": {
            "ticket": job["ticket"],
            "order": {"id": order["id"], "payment_intent": order["payment_intent"],
                      "paid": usd(order["paid_cents"]), "email": order["email"]},
        },
        "grants": {
            "refunds": {"cap": "stripe.refunds.create", "uses": 1, "each": usd(limit),
                        "only": [order["payment_intent"]], "needs": {
                            "get": {"cap": "net.http.get", "only": [base + "/v1/payment_intents/*"]},
                            "post": {"cap": "net.http.post", "only": [base + "/v1/refunds"], "idempotency_key": True},
                            "key": {"env": "STRIPE_KEY"},
                            "base": base}},
            "replies": {"cap": "mail.messages.send", "uses": 1, "only": [order["email"].strip().lower()], "needs": {
                            "http": {"cap": "net.http.post", "only": [base + "/emails"]},
                            "key": {"env": "MAIL_KEY"},
                            "base": base,
                            "sender": "support@shop.example"}},
        },
        "model": {"provider": "anthropic", "model": "claude-sonnet-5-5", "api_key": {"env": "ANTHROPIC_API_KEY"},
                  "base_url": base, "retries": 2, "retry_delay_ms": 10},
        "net": {"timeout_ms": 5000},
    }


def main(argv):
    lib = staal.Library()
    jobdir = argv[1]
    journal = os.path.join(jobdir, "run.journal")
    if argv[0] == "resolve":
        if argv[2] == "sent":
            out = lib.resolve(journal, status=200, body=json.dumps({"id": argv[3]}))
        else:
            out = lib.resolve(journal, retry=True)
        print(json.dumps({"status": out["status"], "error": out.get("error")}))
        return 0 if out["status"] == "resolved" else 1
    with open(os.path.join(jobdir, "job.json")) as f:
        job = json.load(f)
    cfg = config(job)
    if os.path.exists(journal):
        out = lib.resume(PROGRAM, cfg, journal)
    else:
        out = lib.run(PROGRAM, cfg, journal)
    out.pop("log", None)
    print(json.dumps(out))
    return {"done": 0, "stopped": 3}.get(out["status"], 1)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
