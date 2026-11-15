# Journals and resuming

A run started with `--journal` survives the process being killed at any point.
`staalc resume` finishes it without performing any action twice and without losing an
action that happened. When the runtime cannot know whether an action happened, it
stops and an operator records the answer.

## How the journal works

These are the run's external operations: token counts, model calls, actions, prints,
clock reads, random numbers and console reads. The runtime numbers them in the order
the run reaches them. For each one it appends two records to the journal, each written
to disk (`fsync`) before the run continues:

1. an intent, before the operation: what is about to be asked of the world;
2. a result, after it: what the world answered.

`staalc resume` runs the program again from the start. At each external operation, a
recorded result is used instead of contacting the world; when the recorded operations
are used up, the run continues and appends to the same journal.

Resume requires the same program (every module, `staal.json`, `staal.lock` and every
package file), the same run config, and the same proxies. Each is recorded in the
journal's first record, and any difference is refused before anything runs. The engine
may differ. One process at a time may use a journal; the lock is released if the
process dies. A last line cut off by a crash is discarded; any other damage is refused.

```staal file=notify.staal
fn notify(phones: List<Text, 10>, sms: cap twilio.message.send(uses: 10)) -> Int {
    let sent = 0
    for p in phones {
        let r = sms.send(p, "Your order has shipped.")
        set sent = sent + 1
    }
    return sent
}
```

```json file=run.json
{
  "entry": "notify",
  "inputs": { "phones": ["+2348010000001", "+2348010000002"] },
  "grants": { "sms": { "cap": "twilio.message.send" } }
}
```

```console
$ staalc run notify.staal --config run.json --journal run.journal
2
$ staalc resume notify.staal --config run.json --journal run.journal
2
$ staalc run notify.staal --config run.json --journal run.journal
staalc: journal run.journal already exists; use `staalc resume` to continue that run
```

The resumed run takes both sends from the journal: it returns the same result and sends
nothing. A journal belongs to one run; `staalc run` refuses an existing one.

## What happens to an operation a crash interrupted

An operation has an intent but no result when the process died during it. Resume
handles each kind differently:

| Operation | On resume |
| --- | --- |
| Token count, model call | Made again. The program never saw the lost answer. A model call may cost one extra request. |
| Clock read, random number | Made again. |
| Print | Printed again: the line may appear twice. |
| Action that is always safe to repeat (`fs.read`, `fs.write`, database queries, socket `accept` and `reply`) | Made again. |
| Action whose service deduplicates by key | Sent again with the same idempotency key; the service returns the original result. |
| Any other action | The run stops with `in_doubt` (exit code 3) until an operator resolves it. |

Every action is sent with an idempotency key, `<run id>-<operation number>`. Whether
the service deduplicates by it is declared by the adapter, or for `net.http.*` by
`"idempotency_key": true` in the grant, which also sends the key as an
`Idempotency-Key` header.

A model provider that stays unavailable after its retries also stops the run with
`in_doubt`. Nothing needs resolving: resume makes the call again.

```staal file=ask.staal
type Reply { text: Text }

fn ask(q: Text) -> Text budget(calls: 1, tokens: 500) {
    let r: Reply = validate propose(Reply, instruct: "Answer in one sentence.", data: [q], tokens: 500)
    return r.text
}
```

```json file=down.json
{
  "entry": "ask",
  "inputs": { "q": "When do you open?" },
  "model": { "provider": "script", "responses": [{ "unavailable": "overloaded" }] }
}
```

```console
$ staalc run ask.staal --config down.json --journal ask.journal
staalc: run stopped: in_doubt: model call (operation 2) did not complete (overloaded). Resume the run to make it again
```

## Resolving an action in doubt

When a run stops with an action in doubt, check in the real service whether the action
happened, record the answer, then resume:

```sh
staalc resolve --journal run.journal --performed re_3QxY     # it happened, with this receipt
staalc resolve --journal run.journal --failed "declined"     # the program sees a failed action
staalc resolve --journal run.journal --retry                 # it did not happen: make it once more
staalc resume refund.staal --config run.json --journal run.journal
```

For an HTTP request, `--status <code> [--body <text>]` records the response instead of
`--performed`; for a TCP or TLS exchange, `--reply <text>`. `--retry` authorizes one
attempt, recorded before it is made: if that attempt is interrupted too, the action is
in doubt again. When several actions are in doubt, `--operation <id>` names one. The
operator interface from an application is in [Calling Staal from an app](apps.md).

## Proof of completion

A function returning `Done` claims it completed its work, and must cite proof:
`done(evidence: [...])`. Each item must be a `Receipt` of an action this function
performed, the `Done` or `Receipt` of a function it called, or a `try` outcome of one of
these on a path where its `ok` is known to be true.

```staal file=ship.staal
fn ship(order: Text, phone: Text, out: cap warehouse.dispatch(uses: 1), sms: cap twilio.message.send(uses: 1)) -> Done {
    let sent = out.dispatch(order)
    let told = try sms.send(phone, "Order " + order + " has shipped.")
    if told.ok {
        return done(evidence: [sent, told])
    }
    return done(evidence: [sent])
}
```

```json file=ship.json
{
  "entry": "ship",
  "inputs": { "order": "A1", "phone": "+2348010000001" },
  "grants": { "out": { "cap": "warehouse.dispatch" }, "sms": { "cap": "twilio.message.send" } }
}
```

```console
$ staalc run ship.staal --config ship.json
{"done":[{"capability":"warehouse.dispatch","method":"dispatch","receipt":"sim-1"},{"error":"","ok":true}]}
```

The compiler refuses evidence that is not proof of this function's work (E008): an
empty list, a literal, model output, an unchecked outcome, or a value passed in by the
caller. At run time, every evidence item is checked again to lead to a real receipt. A
`Done` shows that the cited actions happened, not that they achieved what the business
intended.

## Handling journals

A journal holds the data sent to models and to actions, so it can contain personal or
confidential data; secrets are recorded by their variable name only. Store journals as
you store that data, and never give a journal or a log to a model as context. The
journal format is specified in [RUNTIME.md](../RUNTIME.md), section 11.
