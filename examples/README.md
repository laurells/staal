# Examples that reach real services

Complete programs that act through real APIs: an SMS and an email sent directly
over `net.http`, and an SMS and a Stripe refund sent through libraries that
declare their own capabilities. They are checked and run in the test suite against a
local server that answers like each service. Running them against the real
services needs your own accounts and keys, as described below.

For a first program that needs no account, see `invoice/`:

    staalc run examples/invoice/invoice.staal --config examples/invoice/run.json

Build staalc first:

    cargo build --release

## SMS through Termii

`sms_termii/send.staal` sends one SMS. The API key is read from the environment and
only ever placed inside the JSON body Termii expects; the capability can reach one
URL, once.

1. In your Termii dashboard, find your API key and an approved sender ID. Check
   which base URL your dashboard shows. The program uses
   `https://api.ng.termii.com`; if yours is different, change the URL in both
   places in `send.staal` (the `only` pattern and the request).
2. Edit `sms_termii/run.json`: `to` (international format without `+`, e.g.
   `2348012345678`), `text`, `sender`, and `channel` (`generic`, or `dnd` if your
   account is set up for it).
3. Run:

       export TERMII_API_KEY=your-key
       ./target/release/staalc run examples/sms_termii/send.staal \
           --config examples/sms_termii/run.json --journal sms.journal

It prints `"sent, message id ..."`, or Termii's status and answer if it refused.

Termii does not deduplicate requests, so `run.json` does not set
`idempotency_key`. If the connection breaks after the request was sent, the run
stops `in_doubt` rather than risk a second SMS. Check your Termii message log; if
the SMS was sent, record it and finish:

    ./target/release/staalc resolve --journal sms.journal --status 200 --body '{"message_id":"...","code":"ok"}'
    ./target/release/staalc resume examples/sms_termii/send.staal --config examples/sms_termii/run.json --journal sms.journal

If it was not sent, use `--retry` instead of `--status`.

## Email through Resend

`email_resend/send.staal` sends one email. The API key goes only into the
`Authorization` header.

1. Create an API key in Resend. Until you verify your own domain, Resend lets you
   send from `onboarding@resend.dev` to the address of your own account only.
2. Edit `email_resend/run.json`: `to`, `subject`, `text`, and `sender`.
3. Run:

       export RESEND_API_KEY=re_your_key
       ./target/release/staalc run examples/email_resend/send.staal \
           --config examples/email_resend/run.json --journal email.journal

Resend deduplicates by `Idempotency-Key`, so `run.json` sets
`"idempotency_key": true`, and the runtime sends one with the request. If the
connection breaks, resuming sends the request again with the same key, and Resend
returns the first answer instead of sending a second email. Resend keeps keys for
24 hours: resume within that time.

## Through a library: `capabilities/`

`capabilities/termii.staal` and `capabilities/stripe.staal` are libraries that declare
capabilities (`termii.sms.send`, `stripe.refunds.create`); `remind.staal` and
`refund.staal` are programs that use them and never see a key or URL. They are
tested against a local server (`tests/library_test.rs`), not the real services.
`remind.json` and `refund.json` are run configs whose grants supply the
libraries' `needs` (`docs/RUNTIME.md` section 13). Edit the phone number, sender
ID, payment intent id and amount, then:

    export TERMII_API_KEY=your-key
    ./target/release/staalc run examples/capabilities/remind.staal \
        --config examples/capabilities/remind.json --journal remind.journal

    export STRIPE_SECRET_KEY=sk_test_your_key
    ./target/release/staalc run examples/capabilities/refund.staal \
        --config examples/capabilities/refund.json --journal refund.journal

The phone number must match `only` in canonical form (international, no `+` or
spaces). For Stripe, use a test-mode key and a test payment first. `refund.json`
grants `post` with `"idempotency_key": true`, since Stripe deduplicates by
Idempotency-Key, and limits the run to 5.00 USD. The checks listed below apply
to these runs too.

## What to check

- The message arrives once.
- Neither the journal (`sms.journal`, `email.journal`) nor a log written with
  `--log` contains your key: `grep -c your-key sms.journal` should print 0.
- Changing the URL in the request to anything outside the `only` pattern makes the
  run fail with `capability` before anything is sent.

If you are behind a proxy, add `"net": { "proxy": "environment" }` to `run.json`.
