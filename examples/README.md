# Examples

Build `staalc` first (`cargo build --release`), or install it. Every example here is
checked, formatted and documented by the test suite; the last column says which are
also run.

| Folder | What it shows | Run by the tests |
| --- | --- | --- |
| `invoice/` | A first program: totals in exact money. No account needed. | No |
| `quickstart/` | The README's refund example, from Python and Node.js | Yes |
| `policy/` | An app granting each run a refund limit from its own records | Yes (`python/tests/test_policy.py`) |
| `claude/` | Claude decides on a refund; the complaint stays data | No: needs an Anthropic key |
| `support_agent/` | A complete, publishable agent | Yes (`tests/support_agent_test.rs`) |
| `finops/` | A finance agent attacked on purpose | Yes (`tests/capi_test.rs`) |
| `frameworks/` | One refund tool on five agent frameworks | Yes (`tests/capi_test.rs`, `node/test/frameworks`) |
| `sms_termii/`, `email_resend/` | One SMS and one email sent directly over `net.http` | No: need real accounts |
| `capabilities/` | Libraries for Termii and Stripe that declare their own capabilities | Yes, against local stand-ins (`tests/library_test.rs`) |

## `invoice/`

```sh
staalc run examples/invoice/invoice.staal --config examples/invoice/run.json
```

prints `{"amount":"187501.50","currency":"NGN"}`.

## `policy/`

`app.py` grants each run of `refund.staal` a refund limit from the customer's tier and
the order's cost. Staal refuses any refund above that limit, and never one above the
500 USD the code allows. See [Calling Staal from an app](../docs/guides/apps.md).

```sh
PYTHONPATH=python STAAL_LIBRARY=target/release/libstaalc.so ANTHROPIC_API_KEY=... python3 examples/policy/app.py
```

With the `staal` package installed, `PYTHONPATH` and `STAAL_LIBRARY` are not needed.

## `claude/`

`triage.staal` asks Claude whether a complaint deserves a refund and validates the
answer before using it. The complaint tries to instruct the model; it is passed as
data.

```sh
export ANTHROPIC_API_KEY=...
staalc run examples/claude/triage.staal --config examples/claude/run.json --log triage.jsonl
```

The log records the request, the tokens counted before the call, and the tokens the
API reported. `"model"` in `run.json` selects the Claude model.

## `support_agent/`

Claude chooses to refund, decline or escalate; the agent's rules decide; it refunds
through the business's own payment service, at most once and at most 100 USD. The
folder holds `agent.json` and `agent.described.json`:

```sh
staalc agent check examples/support_agent
```

Its [README](support_agent/README.md) shows how to run it with a Claude key and a
payment service.

## `finops/`

An agent that reads customer records, refunds, books, texts and keeps daily limits.
`attacks.py` attacks it with planted instructions, unauthorized refunds, malicious
arguments, replays, concurrent calls, a process killed mid-refund, lost answers and a
mistake in the app's policy. Each attack is judged by what the stand-in bank and SMS
service record. Linux only (it uses SQLite); no account needed:

```sh
PYTHONPATH=python STAAL_LIBRARY=target/release/libstaalc.so python3 examples/finops/attacks.py /tmp/finops
```

[GUARANTEES.md](../docs/GUARANTEES.md) explains what it shows and where the guarantees
stop.

## `sms_termii/`

`send.staal` sends one SMS. The API key is read from the environment and placed only
in the request body; the capability reaches one URL, once.

1. In the Termii dashboard, find the API key, an approved sender ID and the base URL.
   The program uses `https://api.ng.termii.com`; for another base URL, change both the
   `only` pattern and the request in `send.staal`.
2. In `run.json`, set `to` (international format without `+`, such as
   `2348012345678`), `text`, `sender` and `channel` (`generic`, or `dnd` if the account
   allows it).
3. Run:

   ```sh
   export TERMII_API_KEY=...
   staalc run examples/sms_termii/send.staal --config examples/sms_termii/run.json --journal sms.journal
   ```

It prints `"sent, message id ..."`, or Termii's status and answer.

Termii does not deduplicate requests, so the grant has no `idempotency_key`. If the
connection breaks after the request may have been sent, the run stops in doubt rather
than risk a second SMS. Check the Termii message log, then record what happened and
resume:

```sh
staalc resolve --journal sms.journal --status 200 --body '{"message_id":"...","code":"ok"}'   # it was sent
staalc resolve --journal sms.journal --retry                                                 # it was not
staalc resume examples/sms_termii/send.staal --config examples/sms_termii/run.json --journal sms.journal
```

## `email_resend/`

`send.staal` sends one email. The API key is placed only in the `Authorization`
header.

1. Create an API key in Resend. Until a domain is verified, Resend sends only from
   `onboarding@resend.dev` to the account's own address.
2. In `run.json`, set `to`, `subject`, `text` and `sender`.
3. Run:

   ```sh
   export RESEND_API_KEY=...
   staalc run examples/email_resend/send.staal --config examples/email_resend/run.json --journal email.journal
   ```

Resend deduplicates by `Idempotency-Key`, so the grant sets `"idempotency_key": true`.
After a broken connection, resuming sends the request again with the same key and
Resend returns its first answer. Resend keeps keys for 24 hours; resume within that
time.

## `capabilities/`

`termii.staal` and `stripe.staal` are libraries that declare `termii.sms.send` and
`stripe.refunds.create`. `remind.staal` and `refund.staal` use them and never hold a
key or URL; `remind.json` and `refund.json` supply the libraries' `needs`
([RUNTIME.md](../docs/RUNTIME.md), section 13). `support.staal` combines both with a
model. After setting the phone number, sender ID, payment intent and amount:

```sh
export TERMII_API_KEY=...
staalc run examples/capabilities/remind.staal --config examples/capabilities/remind.json --journal remind.journal

export STRIPE_SECRET_KEY=sk_test_...
staalc run examples/capabilities/refund.staal --config examples/capabilities/refund.json --journal refund.journal
```

The phone number must match `only` in canonical form: international, without `+` or
spaces. `refund.json` limits the run to 5.00 USD and grants `post` with
`"idempotency_key": true`, since Stripe deduplicates by key. Use a test-mode key and
payment first.

## Checking a run against a real service

- The message arrives once.
- Neither the journal nor a log written with `--log` contains the key:
  `grep -c "$TERMII_API_KEY" sms.journal` prints `0`.
- A request URL outside the `only` pattern fails with `capability` before anything is
  sent.

Behind a proxy, add `"net": { "proxy": "environment" }` to the run config.
