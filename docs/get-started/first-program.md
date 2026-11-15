# Your first program

This page writes a program, checks it, runs it, gives it a capability, and tests it.
Create an empty folder and run the commands there.

## Write a program

```staal file=order.staal
// The total of an order: each line's price times its quantity, plus delivery.
pub type Line { item: Text, price: Money, qty: Int }

pub fn total(lines: List<Line, 20>, delivery: Money) -> Money {
    let sum = delivery
    for line in lines {
        set sum = sum + line.price * line.qty
    }
    return sum
}
```

`Money` is an exact decimal amount with a currency. `List<Line, 20>` is a list of at
most 20 lines: every list declares its maximum length. `set` reassigns a variable.
`pub` makes a declaration usable from other files, which the test below needs.

## Check it

```console
$ staalc check order.staal
order.staal: ok
```

`staalc check` parses the program and applies every compile-time rule. It exits 0 when
the program is accepted and 1 when it is not.

## Run it

A run is described by a run config: the entry function and its inputs.

```json file=run.json
{
  "entry": "total",
  "inputs": {
    "lines": [
      { "item": "rice", "price": { "amount": "12.50", "currency": "USD" }, "qty": 2 },
      { "item": "oil", "price": { "amount": "4.25", "currency": "USD" }, "qty": 1 }
    ],
    "delivery": { "amount": "3.00", "currency": "USD" }
  }
}
```

```console
$ staalc run order.staal --config run.json
{"amount":"32.25","currency":"USD"}
```

The result is printed as JSON. `staalc config order.staal --entry total` prints a
config with a placeholder for every input, to fill in.

A run that cannot give a correct answer fails instead. Money in two currencies is never
added:

```json file=run-eur.json
{
  "entry": "total",
  "inputs": {
    "lines": [{ "item": "rice", "price": { "amount": "12.50", "currency": "EUR" }, "qty": 2 }],
    "delivery": { "amount": "3.00", "currency": "USD" }
  }
}
```

```console
$ staalc run order.staal --config run-eur.json
staalc: run failed: value: cannot combine 3.00 USD and 25.00 EUR: different currencies
```

A failed run exits 1. `value` is the failure's kind; integer overflow, division by
zero and an index out of range fail the same way.

## Act through a capability

A program acts on the world only through a capability, a parameter of type `cap`. The
caller grants it; the program cannot create one. Its type can carry limits.

```staal file=notify.staal
fn notify(phone: Text, sms: cap twilio.message.send) -> Receipt {
    return sms.send(phone, "Your order has shipped.")
}

fn notify_all(phones: List<Text, 100>, sms: cap twilio.message.send(uses: 3)) -> Int {
    let sent = 0
    for p in phones {
        let r = notify(p, sms)
        set sent = sent + 1
    }
    return sent
}
```

```console
$ staalc check notify.staal
notify.staal:8:27: error[E003]: in call to 'notify': capability limited to 3 uses cannot be widened to unlimited uses
1 error(s)
```

`notify_all` may send three messages; `notify` declares no limit, so passing the
capability to it would remove the limit. A capability can be narrowed, never widened.
Declare the same limit in `notify`:

```staal file=notify.staal
fn notify(phone: Text, sms: cap twilio.message.send(uses: 3)) -> Receipt {
    return sms.send(phone, "Your order has shipped.")
}

fn notify_all(phones: List<Text, 100>, sms: cap twilio.message.send(uses: 3)) -> Int {
    let sent = 0
    for p in phones {
        let r = notify(p, sms)
        set sent = sent + 1
    }
    return sent
}
```

The run config grants the capability. With no SMS provider configured, the runtime
uses a simulated one, so this runs without an account:

```json file=notify.json
{
  "entry": "notify_all",
  "inputs": { "phones": ["+2348010000001", "+2348010000002", "+2348010000003", "+2348010000004"] },
  "grants": { "sms": { "cap": "twilio.message.send" } }
}
```

```console
$ staalc run notify.staal --config notify.json
staalc: run failed: capability: 'twilio.message.send' has used all 3 of its allowed uses
```

Three messages are sent; the fourth is refused before it reaches the provider, and the
run fails. `--log run.jsonl` writes the run's log, one JSON event per line: each
action requested, performed or refused, with its target.

## Test it

A test file holds functions named `test_...` with no parameters and no result. A test
passes if it returns, and fails if it fails; `fail(...)` is the assertion.

```staal file=order_test.staal
import order

fn line(price: Money, qty: Int) -> order.Line {
    return order.Line { item: "x", price: price, qty: qty }
}

fn test_total_adds_delivery() {
    let t = order.total([line(12.50 USD, 2), line(4.25 USD, 1)], 3.00 USD)
    if t != 32.25 USD {
        fail("expected 32.25 USD, got " + to_text(t))
    }
}

fn test_mixed_currencies_fail() {
    let r = try order.total([line(12.50 EUR, 2)], 3.00 USD)
    if r.ok {
        fail("a total in two currencies gave a value")
    }
}
```

```console
$ staalc test order_test.staal
ok   order_test.staal::test_total_adds_delivery
ok   order_test.staal::test_mixed_currencies_fail
2 passed, 0 failed
```

`import order` loads `order.staal` from the same folder. `try` runs a call and gives an
outcome whose `ok` says whether it failed, instead of ending the test. Each test runs
on both engines, the interpreter and native code (on a `staalc` built with native code,
as releases are), and passes only if both give the same outcome.

## Format it

```staal file=messy.staal
fn  double( x:Int )->Int{
return x*2
}
```

```console
$ staalc fmt --check messy.staal
messy.staal
$ staalc fmt messy.staal
$ staalc fmt --check messy.staal
```

`--check` lists the files that are not formatted and exits 1 if there are any;
without it, `staalc fmt` rewrites them.

## Next

[Your first agent](first-agent.md) adds an AI model's decision to a program that moves
money.
