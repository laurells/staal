# Capabilities

A program acts on the world only through capabilities: sending a message, moving
money, making an HTTP request, writing a file. A capability is a parameter of type
`cap <path>`. The program cannot create one; whoever starts the run grants it in the
run config. The compiler checks every use of a capability, and at run time a broker
checks every action against the capability's limits before the action is performed.

## Capability types

```text
cap payments.refund
cap payments.refund(each: 100 USD, max: 150 USD)
cap email.send(only: ["*@example.com"], uses: 1)
cap net.http.post(only: ["https://api.example.com/v1/*"])
```

The path names the capability and, after its last `.`, the action: `pay.refund(...)`
performs the `refund` action of a `cap payments.refund`. An action returns a `Receipt`,
proof that it was performed.

| Limit | Meaning |
| --- | --- |
| `max` | The most money spent in total over the run. |
| `each` | The most money one action may spend. |
| `uses` | How many actions may be attempted. A failed attempt counts. |
| `only` | The targets that may be acted on: exact texts, or patterns with one `*`, such as `"*@example.com"` or `"/reports/*"`. |

An action's amount and target are what the limits are checked against. For a simulated
capability, they are the first `Money` and the first `Text` among the action's
arguments; for a library capability, the parameters marked `@amount` and `@target`.

## Limits are checked before the action

```staal file=refunds.staal
type Refund { order: Text, amount: Money }

fn refund_all(refunds: List<Refund, 20>, pay: cap payments.refund(each: 100 USD, max: 150 USD)) -> Int {
    let n = 0
    for r in refunds {
        let receipt = pay.refund(r.order, r.amount)
        set n = n + 1
    }
    return n
}

fn notify(to: Text, mail: cap email.send(only: ["*@example.com"], uses: 1)) -> Receipt {
    return mail.send(to, "Your refund was issued.")
}
```

`payments.refund` and `email.send` reach no real service here, so the runtime performs
them with its simulated service, which returns receipts `sim-1`, `sim-2`, and so on.

```json file=over-each.json
{
  "entry": "refund_all",
  "inputs": { "refunds": [{ "order": "A1", "amount": { "amount": "120.00", "currency": "USD" } }] },
  "grants": { "pay": { "cap": "payments.refund" } }
}
```

```console
$ staalc run refunds.staal --config over-each.json
staalc: run failed: capability: 'payments.refund' allows at most 100.00 USD per action; this action needs 120.00 USD
```

```json file=over-max.json
{
  "entry": "refund_all",
  "inputs": { "refunds": [
    { "order": "A1", "amount": { "amount": "80.00", "currency": "USD" } },
    { "order": "A2", "amount": { "amount": "80.00", "currency": "USD" } }
  ] },
  "grants": { "pay": { "cap": "payments.refund" } }
}
```

```console
$ staalc run refunds.staal --config over-max.json
staalc: run failed: capability: 'payments.refund' has 70.00 USD left of its 150.00 USD limit; this action needs 80.00 USD
```

The first refund was performed and charged; the second was refused. A refused action
fails the run with kind `capability` and never reaches the service. Money limits are
never converted between currencies: an action in another currency is refused.

```json file=other-domain.json
{
  "entry": "notify",
  "inputs": { "to": "ada@other.org" },
  "grants": { "mail": { "cap": "email.send" } }
}
```

```console
$ staalc run refunds.staal --config other-domain.json
staalc: run failed: capability: 'email.send' may only act on ["*@example.com"]; this action targets "ada@other.org"
```

## Narrowing, never widening

A capability can be passed to a function, assigned or returned only where its type
allows no more than it holds. Narrowing is allowed: fewer `uses`, a lower `max` or
`each`, a tighter `only`, or a limit added. Widening is a check error (E003): a higher
or missing `max`, `each` or `uses`, or an `only` pattern outside the original.

```staal
fn refund_one(order: Text, amount: Money, pay: cap payments.refund(each: 50 USD, uses: 1)) -> Receipt {
    return pay.refund(order, amount)
}

fn refund_many(order: Text, amount: Money, pay: cap payments.refund(each: 100 USD, uses: 5)) -> Receipt {
    return refund_one(order, amount, pay)
}
```

Here `refund_many` passes its capability to `refund_one`, narrowed to `each: 50 USD`
and `uses: 1`. Every action through a narrowed capability also counts against every
capability it was narrowed from.

## The grant and the code's limits

The limits in a parameter's type are the most the code ever accepts. The grant in the
run config can be narrower; the run uses the stricter of the two in each dimension. So
policy that depends on what only the application knows, such as a customer's tier, is
decided by the application and enforced by Staal:

```json file=standard-tier.json
{
  "entry": "refund_all",
  "inputs": { "refunds": [{ "order": "A1", "amount": { "amount": "60.00", "currency": "USD" } }] },
  "grants": { "pay": { "cap": "payments.refund", "each": { "amount": "50.00", "currency": "USD" } } }
}
```

```console
$ staalc run refunds.staal --config standard-tier.json
staalc: run failed: capability: 'payments.refund' allows at most 50.00 USD per action; this action needs 60.00 USD
```

A grant wider than the type is narrowed to the type: a grant of `each: 500 USD` to this
parameter allows at most 100 USD per refund. Inside a program, limits are fixed values;
limits that depend on the request are computed by the application for each run (see
[Calling Staal from an app](apps.md)).

## The runtime's own capabilities

| Capability | Actions |
| --- | --- |
| `net.http.get`, `post`, `put`, `patch`, `delete` | An HTTP request, returning a `Response { status, headers, body, complete }`. |
| `fs.read`, `fs.write` | Read or write a file. |
| `db.sqlite.*`, `db.postgres.*` | SQL statements (`db.sqlite` on Linux only). |
| `net.tcp.exchange`, `net.tls.exchange` | Send bytes and read a reply. |
| `net.tcp.accept`, `net.tls.accept`, `net.tcp.reply`, `net.tls.reply` | Serve requests. |
| `ffi.<library>.<function>` | Call a C function. |

```staal
type Reply { message_id: Text }

fn status(id: Text, key: Secret, api: cap net.http.get(only: ["https://api.example.com/v1/messages/*"], uses: 1)) -> Text {
    let r = api.get("https://api.example.com/v1/messages/" + url_segment(id), [header("Authorization", fill_secret("Bearer {secret}", key))])
    if r.status != 200 {
        fail("the service answered " + json(r.status))
    }
    let reply: Reply = validate parse(Reply, r.body)
    return reply.message_id
}
```

Rules that apply to every network capability:

- **Canonical targets.** A URL is parsed by the WHATWG URL Standard and put into one
  canonical form before `only` is checked, so `/api/../admin` cannot match
  `https://x.org/api/*`. The scheme must be `https`, or `http` for `localhost`,
  `127.0.0.0/8` and `::1`. A `*` cannot be in the host.
- **Public addresses only, unless granted.** The address is checked after the host
  name is resolved. Loopback, private networks and other non-public addresses are
  reached only if the grant lists them in `private_addresses`; cloud metadata
  addresses only with `"cloud_metadata": true`.
- **No redirects followed.** A redirect is returned as a response. An error status is
  a response, not a failure.
- **Responses are untrusted.** Data from a service is never a model instruction (E006),
  and `parse` gives a `Proposed<T>` that must be validated (E001).
- **Secrets** reach a service only inside a request: as a header, or in a body through
  `fill_secret`.

Files are opened one directory at a time without following symbolic links; writes are
atomic. The full rules for each capability are in [RUNTIME.md](../RUNTIME.md),
section 12.

## Library capabilities

A library can declare a capability of its own, written in Staal on top of the
runtime's capabilities. The program that uses it never holds the key, the URL or the
HTTP capability: the operator grants those to the library under `needs`.

```staal
type Sms { to: Text, sms: Text, api_key: Text }
type SmsReply { message_id: Text }

capability sms.example {
    needs http: cap net.http.post
    needs key: Secret
    needs base: Text

    canonical(phone: Text) -> Text {
        return replace(trim(phone), " ", "")
    }

    action send(to: Text @target, text: Text) -> Text {
        let body = fill_secret(json(Sms { to: to, sms: text, api_key: "{secret}" }), key)
        let r = http.post(base + "/sms/send", body, [header("Content-Type", "application/json")])
        if r.status != 200 {
            fail("the SMS service answered " + json(r.status))
        }
        let reply: SmsReply = validate parse(SmsReply, r.body)
        return reply.message_id
    }
}

fn remind(phone: Text, texts: cap sms.example.send(only: ["+2348*"], uses: 1)) -> Receipt {
    return texts.send(phone, "Your appointment is tomorrow.")
}
```

- `needs` lists what the operator supplies: capabilities, secrets and texts.
- `canonical` gives a target's canonical form; `only` is checked against it, so
  `+234 801 000 0001` matches `"+2348*"`.
- `@target` and `@amount` mark the parameters the program's limits are checked
  against, before any of the library's code runs.
- An action must make a request through its needs. It cannot call a model or contain an
  unbounded loop. If it returns without having made a request, it fails.

The grant supplies each need:

```json
{
  "texts": {
    "cap": "sms.example.send",
    "needs": {
      "http": { "cap": "net.http.post", "only": ["https://sms.example.com/sms/send"], "uses": 1 },
      "key": { "env": "SMS_API_KEY" },
      "base": "https://sms.example.com"
    }
  }
}
```

The program's limits apply to the library's actions; the operator's limits on each
need apply to the library's requests. The runtime cannot check that a library does
what its name says: read a library before granting it keys. Complete libraries for
Termii and Stripe are in [examples/capabilities](../../examples/capabilities/), and the
reference packages in [packages](../../packages/README.md).
