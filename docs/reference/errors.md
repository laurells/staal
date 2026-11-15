# Error codes

`staalc check` prints each error as

```text
<file>:<line>:<column>: error[<code>]: <message>
```

then the number of errors, and exits 1. E001 to E008 are Staal's safety rules; the others are the rules of the language. Each example below is checked by the test suite.

## E001: Unvalidated model output

A `Proposed<T>` from a model reaches an action, or a return that is not `Proposed`, without `validate`.

```staal file=e001.staal
type Reply { text: Text }

fn answer(question: Text, mail: cap email.send) -> Receipt budget(calls: 1, tokens: 500) {
    let r = propose(Reply, instruct: "Answer the question.", data: [question], tokens: 500)
    return mail.send("support@example.com", r)
}
```

```console
$ staalc check e001.staal
e001.staal:5:45: error[E001]: unvalidated Proposed<Reply> passed to action 'mail.send'; use `validate` first
1 error(s)
```

## E002: Action without a capability

An action is called on a capability that does not permit it, or with no capability in scope.

```staal file=e002.staal
fn notify(phone: Text, mail: cap email.send) -> Receipt {
    return mail.charge(phone)
}
```

```console
$ staalc check e002.staal
e002.staal:2:12: error[E002]: capability 'mail' is `cap email.send` and does not permit 'charge'
1 error(s)
```

## E003: Widened capability

A capability is passed, assigned or returned where its type has more authority: a higher or missing `max`, `each` or `uses`, or an `only` pattern outside the original.

```staal file=e003.staal
fn refund(order: Text, amount: Money, pay: cap payments.refund) -> Receipt {
    return pay.refund(order, amount)
}

fn refund_small(order: Text, amount: Money, pay: cap payments.refund(each: 50 USD)) -> Receipt {
    return refund(order, amount, pay)
}
```

```console
$ staalc check e003.staal
e003.staal:6:34: error[E003]: in call to 'refund': capability `each: 50 USD` cannot be widened to an unlimited `each` on `cap payments.refund`
1 error(s)
```

## E004: Budget exceeded

A function's worst-case model use, through everything it calls, can exceed its declared `budget(calls: ..., tokens: ...)`.

```staal file=e004.staal
type Reply { text: Text }

fn twice(q: Text) -> Text budget(calls: 1, tokens: 500) {
    let a: Reply = validate propose(Reply, instruct: "Answer.", data: [q], tokens: 400)
    let b: Reply = validate propose(Reply, instruct: "Answer again.", data: [q], tokens: 400)
    return a.text + b.text
}
```

```console
$ staalc check e004.staal
e004.staal:3:27: error[E004]: function 'twice' declares budget calls: 1 but can use up to 2 calls
e004.staal:3:27: error[E004]: function 'twice' declares budget tokens: 500 but can use up to 800 tokens
2 error(s)
```

## E005: Model call in an unbounded loop

A model call, made directly or through a called function, is inside `loop`. Use `repeat N`.

```staal file=e005.staal
type Reply { text: Text }

fn forever(q: Text) -> Text {
    let last = ""
    loop {
        let r: Reply = validate propose(Reply, instruct: "Answer.", data: [q], tokens: 400)
        set last = r.text
    }
    return last
}
```

```console
$ staalc check e005.staal
e005.staal:5:5: error[E005]: model call inside an unbounded `loop`; use `repeat N { ... }` to bound it
1 error(s)
```

## E006: Untrusted value in a trusted position

An untrusted value is used as a model instruction, as SQL, or in an `@trusted` slot. Parameters, function results and model output are untrusted; a parameter or a function's result can be marked `@trusted`, model output never.

```staal file=e006.staal
type D { ok: Bool }
fn f(message: Text) -> Bool budget(calls: 1, tokens: 100) {
    let d: D = validate propose(D, instruct: message, data: [], tokens: 100)
    return d.ok
}
```

```console
$ staalc check e006.staal
e006.staal:3:46: error[E006]: untrusted value used as a model instruction; pass it in `data: [...]` instead, or mark its source @trusted
1 error(s)
```

## E007: Blinded data can reach a model

Data marked `@blind` can reach a model through a call's inputs, through the decision to make the call, or through a function that does not promise to keep it blinded.

```staal file=e007.staal
type Order { id: Text, card: Text }
type Reply { text: Text }

fn summarise(order: Order @blind(card)) -> Text budget(calls: 1, tokens: 500) {
    let r: Reply = validate propose(Reply, instruct: "Summarise the order.", data: [order], tokens: 500)
    return r.text
}
```

```console
$ staalc check e007.staal
e007.staal:5:85: error[E007]: model input 'order' can reveal 'order.card (blinded by parameter 'order')', which this call is blind to
1 error(s)
```

## E008: Invalid evidence

`done(evidence: [...])` cites something that is not proof of this function's own work: a value passed in by the caller, model output, a literal, or an unchecked outcome.

```staal file=e008.staal
fn finish(previous: Receipt) -> Done {
    return done(evidence: [previous])
}
```

```console
$ staalc check e008.staal
e008.staal:2:28: error[E008]: 'previous' was passed in by the caller, so it proves nothing about this function's work
1 error(s)
```

## E100: Undefined name

A variable or function is not defined.

```staal file=e100.staal
fn main() -> Int {
    return totl(3)
}
```

```console
$ staalc check e100.staal
e100.staal:2:12: error[E100]: undefined function 'totl'
1 error(s)
```

## E101: Type error

A type mismatch, a wrong number of arguments, and the other rules on types and values. The message names the rule.

```staal file=e101.staal
fn main() -> Int {
    return "three"
}
```

```console
$ staalc check e101.staal
e101.staal:2:5: error[E101]: in return from 'main': expected Int, found Text
1 error(s)
```

## E102: Unknown type

A type name is not defined.

```staal file=e102.staal
fn main(o: Ordr) -> Int {
    return 1
}
```

```console
$ staalc check e102.staal
e102.staal:1:1: error[E102]: unknown type 'Ordr'
1 error(s)
```

## E103: `break` or `continue` outside a loop

`break` or `continue` is outside `repeat`, `loop` or `for`.

```staal file=e103.staal
fn main() -> Int {
    break
    return 1
}
```

```console
$ staalc check e103.staal
e103.staal:2:5: error[E103]: `break` is only allowed inside `repeat`, `loop` or `for`
1 error(s)
```

## E104: Unknown field

A field does not exist on the type, or is read on a value that has no fields.

```staal file=e104.staal
type Order { id: Text }

fn main(o: Order) -> Text {
    return o.total
}
```

```console
$ staalc check e104.staal
e104.staal:4:14: error[E104]: type 'Order' has no field 'total'
1 error(s)
```

## E105: Missing return

A function with a return type can finish without returning.

```staal file=e105.staal
fn sign(n: Int) -> Int {
    if n > 0 {
        return 1
    }
}
```

```console
$ staalc check e105.staal
e105.staal:1:1: error[E105]: 'sign' can finish without returning a value of type Int
1 error(s)
```

## E106: Import problem

A module cannot be read, imports form a cycle, or a name clashes with an import.

```staal file=e106.staal
import missing

fn main() -> Int {
    return 1
}
```

```console
$ staalc check e106.staal
e106.staal:1:1: error[E106]: cannot import 'missing': cannot read ./missing.staal: No such file or directory (os error 2)
1 error(s)
```

## E107: Not public

A declaration of another module is used without being `pub` there.

```staal file=lib/util.staal
fn helper() -> Int {
    return 1
}
```

```staal file=e107.staal
import lib.util

fn main() -> Int {
    return util.helper()
}
```

```console
$ staalc check e107.staal
e107.staal:4:12: error[E107]: function 'util.helper' is not `pub` in its module
1 error(s)
```

## E108: Package authority

A package's code uses a capability its `staal.json` does not list in `authority`, or reaches beyond the targets listed there. See [Packages](../guides/packages.md).

## E109: Incomplete match

A `match` does not handle every variant of its enum (or both `some` and `none`) and has no `_` arm.

```staal file=e109.staal
enum Status {
    Open,
    Closed,
}

fn label(s: Status) -> Text {
    match s {
        Open => { return "open" }
    }
}
```

```console
$ staalc check e109.staal
e109.staal:7:5: error[E109]: this `match` does not handle Closed; add an arm for each, or `_ =>`
1 error(s)
```

## E900: Lexical error

The source contains a character or token that is not valid.

```staal file=e900.staal
fn main() -> Int {
    return 1 # 2
}
```

```console
$ staalc check e900.staal
e900.staal:2:14: error[E900]: unexpected character '#'
1 error(s)
```

## E901: Syntax error

The source does not parse.

```staal file=e901.staal
fn main() -> Int {
    return 1 +
}
```

```console
$ staalc check e901.staal
e901.staal:3:1: error[E901]: expected expression, found RBrace
1 error(s)
```
