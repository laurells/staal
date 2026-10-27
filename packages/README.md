# Reference packages

Four Staal packages (six capabilities), written as references for how a service becomes a library:

| Package | Capability | Service | Authority |
| --- | --- | --- | --- |
| `sms` | `sms.termii.send(to @target, text)` | Termii | POST `https://api.ng.termii.com/api/sms/send` |
| `payments` | `payments.stripe.refund(payment_intent @target, amount @amount)` | Stripe | GET `https://api.stripe.com/v1/payment_intents/*`, POST `https://api.stripe.com/v1/refunds` |
| `payments` | `payments.paystack.refund(reference @target, amount @amount)` | Paystack | GET `https://api.paystack.co/transaction/verify/*`, POST `https://api.paystack.co/refund` |
| `payments` | `payments.flutterwave.refund(transaction @target, amount @amount)` | Flutterwave (v3) | GET `https://api.flutterwave.com/v3/transactions/*/verify`, POST `https://api.flutterwave.com/v3/transactions/*/refund` |
| `email` | `email.resend.send(to @target, subject, text)` | Resend | POST `https://api.resend.com/emails` |
| `chat` | `chat.slack.post(channel @target, text)` | Slack | POST `https://slack.com/api/chat.postMessage` |

Each folder is one package, also published as its own repository
([staal-sms](https://github.com/laurells/staal-sms),
[staal-payments](https://github.com/laurells/staal-payments),
[staal-email](https://github.com/laurells/staal-email),
[staal-chat](https://github.com/laurells/staal-chat)). Depend on one by URL and
commit:

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
service (`tests/packages_test.rs`, with the base URL swapped), and all four have
been fetched from their GitHub repositories with `staalc fetch`. Runs against the
live services need your own keys.
