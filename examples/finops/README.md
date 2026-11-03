# A financial operations agent, attacked on purpose

An agent that reads the signed-in customer's record and orders, refunds part of an
order, books the refund, texts the customer, and works within daily limits. Then
every way a turned model could try to misuse it, sent as that model would send it.

| File | What it is |
| --- | --- |
| `app.py` | The app: its data, its policy (each customer's limit by tier, the payments and the phone number a call may touch, the daily limits), and its three tools |
| `customer.staal`, `refund.staal`, `message.staal` | The programs behind the tools |
| `bank.staal`, `sms.staal` | Library capabilities: refunds and text messages over HTTP |
| `services.py` | Stand-ins for the bank and the SMS service, each with its own ledger, and ways to misbehave: delays, lost answers, forgetting idempotency keys |
| `attacks.py` | The attacks, each judged by the services' ledgers |

Run it (Linux: SQLite actions are Linux only so far), with the `staal` package
installed or `PYTHONPATH` pointing at this repository's `python/`:

    python3 attacks.py /tmp/finops            # or: python3 attacks.py /tmp/finops native

It prints each attack, what the runtime answered, and what the services recorded, and
exits non-zero if any guarantee did not hold. The tests run it on both engines.
[docs/GUARANTEES.md](../../docs/GUARANTEES.md) explains what each attack shows, what
the guarantees rest on, and where they stop.
