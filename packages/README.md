# Packages

Six Staal packages (fifteen capabilities), and a template to start a new one from. To
write one, follow [docs/PACKAGES.md](../docs/PACKAGES.md), "Write your first Staal
package".

| Package | Capability | Service | Authority |
| --- | --- | --- | --- |
| `sms` | `sms.termii.send(to @target, text)` | Termii | POST `https://api.ng.termii.com/api/sms/send` |
| `payments` | `payments.stripe.refund(payment_intent @target, amount @amount)` | Stripe | GET `https://api.stripe.com/v1/payment_intents/*`, POST `https://api.stripe.com/v1/refunds` |
| `payments` | `payments.paystack.refund(reference @target, amount @amount)` | Paystack | GET `https://api.paystack.co/transaction/verify/*`, POST `https://api.paystack.co/refund` |
| `payments` | `payments.flutterwave.refund(transaction @target, amount @amount)` | Flutterwave (v3) | GET `https://api.flutterwave.com/v3/transactions/*/verify`, POST `https://api.flutterwave.com/v3/transactions/*/refund` |
| `email` | `email.resend.send(to @target, subject, text)` | Resend | POST `https://api.resend.com/emails` |
| `email` | `email.postmark.send(to @target, subject, text)` | Postmark | POST `https://api.postmarkapp.com/email` |
| `email` | `email.sendgrid.send(to @target, subject, text)` | Twilio SendGrid (v3) | POST `https://api.sendgrid.com/v3/mail/send` |
| `chat` | `chat.slack.post(channel @target, text)` | Slack | POST `https://slack.com/api/chat.postMessage` |
| `chat` | `chat.discord.post(channel @target, text)` | Discord (API v10) | POST `https://discord.com/api/v10/channels/*/messages` |
| `code` | `code.github.comment(issue @target, body)`, `comment_pull(pull @target, body)`, `label(issue @target, label)`, `create_issue(repo @target, title, body)` | GitHub | GET and POST `https://api.github.com/repos/*` |
| `messaging` | `messaging.twilio.sms(to @target, text)`, `whatsapp(to @target, text)` | Twilio | POST `https://api.twilio.com/2010-04-01/Accounts/*` |

`template` is not a package to depend on: it is the folder to copy (a manifest, a
library with one action, and its tests, passing from the start).

The first four were written as references for how a service becomes a library.
Each of them is also published as its own repository
([staal-sms](https://github.com/laurells/staal-sms),
[staal-payments](https://github.com/laurells/staal-payments),
[staal-email](https://github.com/laurells/staal-email),
[staal-chat](https://github.com/laurells/staal-chat)), with Resend and Slack only:
Postmark, SendGrid and Discord are in this repository and not yet in those. Depend on
one by URL and commit:

    { "dependencies": { "sms": { "git": "https://github.com/laurells/staal-sms", "commit": "<full commit id>" } } }

and run `staalc fetch`. The grant for each capability supplies its `needs`
(`docs/RUNTIME.md` section 13): the HTTP capability, the key from an environment
variable, and for `sms` and `email` the sender.

Each refund is checked against its payment first: it must exist, have succeeded, and
be in the refund's currency. Paystack takes amounts in kobo and Flutterwave v3 in
naira; the Flutterwave refund takes whole units only and refuses anything else rather
than round it. Neither documents an idempotency key, so grant their `post` without
`idempotency_key`: a refund a crash leaves unanswered stops the run in doubt, for an
operator to check in the dashboard, instead of being sent twice.

Each package's code has been run against a local server standing in for its
service (`tests/packages_test.rs` for the first four, `tests/*.json` in each folder
for the rest, on both engines), and the first four have been fetched from their GitHub repositories
with `staalc fetch`. Runs against the live services need your own keys.

### `code`: GitHub

Targets are `owner/repo#number` for an issue or pull request and `owner/repo` for a
repository, in lower case. `comment` posts on an issue or a pull request (GitHub
treats both as issues); `comment_pull` first checks that the number is a pull request
and refuses otherwise; `label` adds one label. Grant:

    "gh": { "cap": "code.github.comment", "only": ["teamozy/app#*"], "uses": 5,
            "needs": { "get":  { "cap": "net.http.get",  "only": ["https://api.github.com/repos/teamozy/app/*"] },
                       "post": { "cap": "net.http.post", "only": ["https://api.github.com/repos/teamozy/app/*"] },
                       "token": { "env": "GITHUB_TOKEN" } } }

GitHub's REST API does not deduplicate repeated requests, so `post` is granted
without `idempotency_key`. Status: tested against a local server only.

### `messaging`: Twilio

SMS and WhatsApp through Twilio's Messages resource. Numbers are E.164, written
`+2348012345678` in `only` patterns. `key` is Twilio's HTTP Basic credentials
(an API key's SID and secret, or the Account SID and Auth Token) as `SID:SECRET`,
base64-encoded, so that the value Twilio receives is the secret itself and is removed
from anything echoed back: `printf '%s' "$SID:$SECRET" | base64 | tr -d '\n'`. Grant:

    "texts": { "cap": "messaging.twilio.sms", "only": ["+234*"], "uses": 20,
               "needs": { "http": { "cap": "net.http.post", "only": ["https://api.twilio.com/2010-04-01/Accounts/AC.../Messages.json"] },
                          "key": { "env": "TWILIO_BASIC_CREDENTIALS" },
                          "account": "AC...", "from": "+14155550100" } }

Twilio's Messages documentation describes no idempotency key, so `http` is granted
without `idempotency_key`. A WhatsApp message outside a conversation the customer
started needs an approved template, which this package does not send yet. Status:
tested against a local server only.

### `email`: Postmark

The package written in [docs/PACKAGES.md](../docs/PACKAGES.md), finished. Addresses are
written in lower case; one with capitals is refused, never lowercased. Grant:

    "mail": { "cap": "email.postmark.send", "only": ["*@clinic.org"], "uses": 20,
              "needs": { "http": { "cap": "net.http.post", "only": ["https://api.postmarkapp.com/email"] },
                         "token": { "env": "POSTMARK_SERVER_TOKEN" },
                         "sender": "results@clinic.org" } }

Postmark documents no idempotency key, so `http` is granted without
`idempotency_key`. An answer of 200 whose `ErrorCode` is not 0 is a refusal. Status:
tested against a local server only.

### `email`: SendGrid

Email through Twilio SendGrid's v3 Mail Send, plain text, to one address. Grant:

    "mail": { "cap": "email.sendgrid.send", "only": ["*@clinic.org"], "uses": 20,
              "needs": { "http": { "cap": "net.http.post", "only": ["https://api.sendgrid.com/v3/mail/send"] },
                         "key": { "env": "SENDGRID_API_KEY" },
                         "sender": "results@clinic.org" } }

SendGrid answers 202 with no body, and gives the message's id only in its
`X-Message-Id` response header, which a Staal `Response` does not carry: the receipt is
`accepted for <address>`. It documents no idempotency key, so `http` is granted
without `idempotency_key`. Only the global endpoint is reached; EU regional subusers
(`api.eu.sendgrid.com`) are not supported. An empty text is refused before any request.
Status: tested against a local server only.

### `chat`: Discord

Messages to a channel, as a bot. Targets are channel ids (digits). The bot needs the
SEND_MESSAGES permission in the channel. Grant:

    "discord": { "cap": "chat.discord.post", "only": ["1234567890123456789"], "uses": 5,
                 "needs": { "http": { "cap": "net.http.post", "only": ["https://discord.com/api/v10/channels/1234567890123456789/messages"] },
                            "token": { "env": "DISCORD_BOT_TOKEN" } } }

Mentions are never parsed (`allowed_mentions` with an empty `parse`), so text from an
agent cannot ping `@everyone`, a role or a person. A text over Discord's 2000
characters is refused before any request. Discord's `enforce_nonce` deduplicates only
"in the past few minutes", which a resumed run is not held to, so it is not used and
`http` is granted without `idempotency_key`. The receipt is the message's id; an
answer for another channel is a failure. Status: tested against a local server only.

## Testing a package

A package's tests are files in its `tests/` folder, each a program run against a
fake service described as data: its routes and answers, as the service's
documentation shows them. The fields are explained in docs/PACKAGES.md (step 5) and
in `tests/common/package_case.rs`. Run them with

    STAAL_PACKAGE=<folder> cargo test --test package_cases_test

Each test runs on both engines, and checks the result, every request the service received, and that the key
appears in neither the log nor the journal. With `--features failpoints`, each is also
killed at every journal write and between every request and its recorded answer,
resumed, and must end as the uninterrupted run did, with the same effects on the
service.

## What every package must meet

A new package is reviewed against this list, in addition to the rules in
[CONTRIBUTING.md](../CONTRIBUTING.md):

- A `staal.json` manifest declaring exactly the URLs the package reaches, so the
  compiler holds it to them (E108).
- A `canonical` form for every target, so `only` patterns mean what they say: email
  addresses trimmed and lower-cased, phone numbers in one international form,
  identifiers such as `owner/repo#12` in one spelling.
- A comment at the top of each library saying whether the service deduplicates
  repeated requests, and by which header or key, so an operator knows whether to grant
  `idempotency_key`.
- Tests in `tests/*.json` against a fake service answering as the service's
  documentation shows, with at least one action refused by `only` before any request
  is made, passing with and without `--features failpoints`.
- A README section with a grant example.
- An honest status: tested against a live test-mode account, or tested against a
  local server only.
