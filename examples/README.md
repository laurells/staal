# Examples that reach real services

Two complete programs that send a real message through a real API. They were
checked and run against a local test server, but not against the real services:
the machine they were built on cannot reach the internet. Running them is the
remaining step of roadmap phase 2, and needs your own account and key.

Build intentc first:

    cargo build --release

## SMS through Termii

`sms_termii/send.int` sends one SMS. The API key is read from the environment and
only ever placed inside the JSON body Termii expects; the capability can reach one
URL, once.

1. In your Termii dashboard, find your API key and an approved sender ID. Check
   which base URL your dashboard shows. The program uses
   `https://api.ng.termii.com`; if yours is different, change the URL in both
   places in `send.int` (the `only` pattern and the request).
2. Edit `sms_termii/run.json`: `to` (international format without `+`, e.g.
   `2348012345678`), `text`, `sender`, and `channel` (`generic`, or `dnd` if your
   account is set up for it).
3. Run:

       export TERMII_API_KEY=your-key
       ./target/release/intentc run examples/sms_termii/send.int \
           --config examples/sms_termii/run.json --journal sms.journal

It prints `"sent, message id ..."`, or Termii's status and answer if it refused.

Termii does not deduplicate requests, so `run.json` does not set
`idempotency_key`. If the connection breaks after the request was sent, the run
stops `in_doubt` rather than risk a second SMS. Check your Termii message log; if
the SMS was sent, record it and finish:

    ./target/release/intentc resolve --journal sms.journal --status 200 --body '{"message_id":"...","code":"ok"}'
    ./target/release/intentc resume examples/sms_termii/send.int --config examples/sms_termii/run.json --journal sms.journal

If it was not sent, use `--retry` instead of `--status`.

## Email through Resend

`email_resend/send.int` sends one email. The API key goes only into the
`Authorization` header.

1. Create an API key in Resend. Until you verify your own domain, Resend lets you
   send from `onboarding@resend.dev` to the address of your own account only.
2. Edit `email_resend/run.json`: `to`, `subject`, `text`, and `sender`.
3. Run:

       export RESEND_API_KEY=re_your_key
       ./target/release/intentc run examples/email_resend/send.int \
           --config examples/email_resend/run.json --journal email.journal

Resend deduplicates by `Idempotency-Key`, so `run.json` sets
`"idempotency_key": true`, and the runtime sends one with the request. If the
connection breaks, resuming sends the request again with the same key, and Resend
returns the first answer instead of sending a second email. Resend keeps keys for
24 hours: resume within that time.

## What to check

- The message arrives once.
- Neither the journal (`sms.journal`, `email.journal`) nor a log written with
  `--log` contains your key: `grep -c your-key sms.journal` should print 0.
- Changing the URL in the request to anything outside the `only` pattern makes the
  run fail with `capability` before anything is sent.

If you are behind a proxy, add `"net": { "proxy": "environment" }` to `run.json`.
