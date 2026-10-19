# Reference packages

Four Intent packages, written as references for how a service becomes a library:

| Package | Capability | Service | Authority |
| --- | --- | --- | --- |
| `sms` | `sms.termii.send(to @target, text)` | Termii | POST `https://api.ng.termii.com/api/sms/send` |
| `payments` | `payments.stripe.refund(payment_intent @target, amount @amount)` | Stripe | GET `https://api.stripe.com/v1/payment_intents/*`, POST `https://api.stripe.com/v1/refunds` |
| `email` | `email.resend.send(to @target, subject, text)` | Resend | POST `https://api.resend.com/emails` |
| `chat` | `chat.slack.post(channel @target, text)` | Slack | POST `https://slack.com/api/chat.postMessage` |

Each folder is one package: publish it as its own git repository (the folder's
contents at the repository's root), then depend on it by URL and commit:

    { "dependencies": { "sms": { "git": "https://github.com/you/intent-sms", "commit": "<full commit id>" } } }

and run `intentc fetch`. The grant for each capability supplies its `needs`
(`docs/RUNTIME.md` section 13): the HTTP capability, the key from an environment
variable, and for `sms` and `email` the sender.

Each package's code has been run against a local server standing in for its
service (`tests/packages_test.rs`, with the base URL swapped), but not against the
real services: the machine they were built on could not reach them.
