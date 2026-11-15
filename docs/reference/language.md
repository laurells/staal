# Language reference

This page defines the language. The complete syntax is in [GRAMMAR.md](../GRAMMAR.md),
and the exact runtime behaviour of each construct in [RUNTIME.md](../RUNTIME.md).
Capabilities and model calls have their own guides:
[Capabilities](../guides/capabilities.md) and [Models](../guides/models.md).

## Files and declarations

A program is a file whose name ends in `.staal`. Each file is a module. A file holds
`import` lines first, then declarations in any order:

| Declaration | Form |
| --- | --- |
| record type | `type Name { field: Type, ... }` |
| enum | `enum Name { Variant, Variant { field: Type, ... }, ... }` |
| type alias | `type Name = Type` |
| constant | `const NAME: Type = value` |
| function | `fn name(param: Type, ...) -> Type { ... }` |
| method | `fn Type.name(self, ...) -> Type { ... }` |
| interface | `interface Name { fn name(self, ...) -> Type }` |
| capability | `capability path { ... }` ([Capabilities](../guides/capabilities.md)) |

`pub` before a declaration makes it usable from other modules. Comments start with
`//` and run to the end of the line.

## Types

| Type | Values |
| --- | --- |
| `Int` | Signed 64-bit integers. |
| `Bool` | `true`, `false`. |
| `Text` | UTF-8 text, at most 16 MiB. `len` counts characters. |
| `Money` | An exact amount and a currency. |
| `List<T, N>` | At most `N` values of type `T`, in order. |
| `Map<K, V, N>` | At most `N` values of type `V` under distinct keys of type `K`, in key order. `K` is `Text` or `Int`. |
| `Set<T, N>` | At most `N` distinct values of type `T`, in order. `T` is `Text` or `Int`. |
| `Optional<T>` | A `T` (`some(x)`) or nothing (`none`). |
| `(A, B, ...)` | A tuple of two or more values. |
| record | Named fields, declared with `type`. |
| enum | One of a set of variants, each with its own fields. |
| `fn(A, B) -> R` | A function value. |
| `Proposed<T>` | A model's answer, not yet checked against `T`. |
| `Receipt`, `Outcome`, `Done` | Proof produced by the runtime: of an action, of a `try`, of completed work. |
| `Secret` | A secret from the environment. |
| `cap path(...)` | A capability, with optional limits. |

Every list, map and set declares its maximum size. Staal has no floating point.

## Literals

```staal
type Point { x: Int, y: Int }

fn literals() -> Text {
    let n = 42
    let negative = -7
    let price = 12.50 USD
    let yen = 1000 JPY
    let ok = true
    let p = Point { x: 1, y: 2 }
    let xs: List<Int, 10> = [1, 2, 3]
    let m: Map<Text, Int, 10> = ["a": 1, "b": 2]
    let empty: Map<Text, Int, 10> = [:]
    let tags: Set<Text, 10> = []
    let pair = (n, "forty-two")
    let quoted = "Say \"hi\"\n\ttabbed \u{e9}"
    let total = "Total: \(price * 3)"
    return total
}
```

| Literal | Form |
| --- | --- |
| integer | Decimal digits, within the signed 64-bit range. A negative number is written with unary `-`. |
| money | A decimal number and a currency code on the same line: `12.50 USD`, `0 NGN`, `1000 JPY`. At most the currency's decimal places. |
| text | Double quotes, on one line. Escapes: `\"`, `\\`, `\n`, `\t`, `\r`, `\u{...}` (1 to 6 hexadecimal digits). Any other escape is an error. |
| interpolation | `\(expr)` inside text, for a `Text`, `Int`, `Bool` or `Money`. |
| list | `[a, b]`; `[]` is an empty list or set. |
| map | `[key: value, ...]`; `[:]` is an empty map. |
| record | `Name { field: value, ... }`, every field exactly once, in any order. |
| tuple | `(a, b, ...)`. |

Text over several lines goes between `"""` quotes. The indentation of the closing
quotes is removed from every line:

```staal
fn note(id: Text, total: Money) -> Text {
    return """
        Order \(id)
        Total \(total)
        """
}
```

Currencies: USD, EUR, GBP, NGN, GHS, KES, ZAR, CAD and AUD with two decimal places,
and JPY with none.

## Variables

```staal
fn count_positive(xs: List<Int, 100>) -> Int {
    let n = 0
    for x in xs {
        if x > 0 {
            set n = n + 1
        }
    }
    return n
}
```

`let name = value` declares a variable; `let name: Type = value` gives its type.
`set name = value` assigns a new value to a variable declared with `let`. Parameters,
loop variables, capabilities, and values of type `Receipt`, `Outcome` and `Done`
cannot be reassigned. A `let` of a name already in scope declares a new variable that
hides the earlier one until the end of the block it is in; `set` assigns the nearest.

## Operators

From the loosest binding to the tightest:

| Operators | Operands |
| --- | --- |
| `or` | `Bool`. The right side is evaluated only if the left is false. |
| `and` | `Bool`. The right side is evaluated only if the left is true. |
| `not` | `Bool`. |
| `==` `!=` `<` `<=` `>` `>=` | Values of one type; the ordering operators need `Int` or `Money`. Comparisons cannot be chained. |
| `+` `-` | `Int`; `Money` of one currency; `+` also joins `Text`. |
| `*` `/` | `Int`; `Money * Int` and `Int * Money`. `/` rounds toward zero. |
| unary `-` | `Int`. |
| `.field`, `.0`, `[i]`, `(args)` | Field, tuple position, list index, call. |

`not a == b` is `not (a == b)`; `a + b * c` is `a + (b * c)`.

These fail the run instead of giving a wrong answer: an `Int` result outside the
signed 64-bit range, division by zero, money in two currencies, money below zero, an
index out of range, and text longer than 16 MiB. The failure's kind is `value`.

## Control flow

```staal
fn classify(n: Int) -> Text {
    if n < 0 {
        return "negative"
    } else if n == 0 {
        return "zero"
    } else {
        return "positive"
    }
}

fn first_over(xs: List<Int, 100>, limit: Int) -> Int {
    let found = -1
    for x in xs {
        if x > limit {
            set found = x
            break
        }
    }
    return found
}

fn countdown() -> Int {
    let n = 3
    let steps = 0
    loop {
        if n == 0 {
            break
        }
        set n = n - 1
        set steps = steps + 1
    }
    repeat 2 {
        set steps = steps + 1
    }
    return steps
}
```

| Statement | Meaning |
| --- | --- |
| `if c { } else if c { } else { }` | `c` must be `Bool`. |
| `for x in xs { }` | Each item of a list, in order. `for (k, v) in entries(m)` unpacks tuples. |
| `repeat N { }` | `N` times; `N` is an integer literal. |
| `loop { }` | Until `break`. A `loop` cannot contain a model call (E005). |
| `break`, `continue` | Inside `for`, `repeat` or `loop`. |
| `return value`, `return` | Leaves the function; a bare `return` in a function with no result. |

A function with a result must return on every path (E105).

## Functions

```staal
fn notify_text(name: Text, body: Text = "Your order shipped.", urgent: Bool = false) -> Text {
    if urgent {
        return "URGENT " + name + ": " + body
    }
    return name + ": " + body
}

fn examples() -> Text {
    let a = notify_text("Ada")
    let b = notify_text("Ada", urgent: true)
    let c = notify_text(urgent: false, name: "Ada", body: "Delivered.")
    return a + "\n" + b + "\n" + c
}
```

A parameter with `= value` may be left out; the default is a constant. Parameters with
defaults come last. A named argument goes to the parameter it names; arguments are
evaluated in the order written.

### Generic functions

```staal
fn first<T>(xs: List<T, 100>) -> Optional<T> {
    for x in xs {
        return some(x)
    }
    return none
}

fn pair<A, B>(a: A, b: B) -> (A, B) {
    return (a, b)
}
```

A type parameter stands for any type the caller gives. Each set of types a generic
function is used with is compiled and checked as a separate function.

### Methods and interfaces

```staal
type Line { price: Money, qty: Int }

fn Line.total(self) -> Money {
    return self.price * self.qty
}

interface Priced {
    fn total(self) -> Money
}

fn total_of<T: Priced>(xs: List<T, 100>, start: Money) -> Money {
    let s = start
    for x in xs {
        set s = s + x.total()
    }
    return s
}

fn order_total(lines: List<Line, 100>) -> Money {
    return total_of(lines, 0.00 USD)
}
```

A method is declared as `fn Type.name(self, ...)` and called as `value.name(...)`. An
interface lists methods; a bound `T: Priced` lets a generic function call them, and any
type with those methods satisfies it. Bounds combine with `+`.

### Function values

```staal
fn double(x: Int) -> Int {
    return x * 2
}

fn apply(f: fn(Int) -> Int, x: Int) -> Int {
    return f(x)
}

fn adder(k: Int) -> fn(Int) -> Int {
    return fn(x: Int) -> Int {
        return x + k
    }
}

fn use_values() -> Int {
    let add3 = adder(3)
    return apply(double, 3) + add3(4)
}
```

A function value is a named function, or `fn(...) { ... }` written in place, which
copies the variables it reads. A function value only computes: it makes no action,
model call or `done`, and does not print, read the clock or draw a random number. It
cannot capture a capability, a secret or proof, and it cannot be stored in a list,
record, map, tuple or optional value.

## Records, enums and `match`

```staal
type Contact { phone: Text, email: Optional<Text> }

enum Status {
    Open,
    Held { reason: Text },
    Closed { at: Text },
}

fn reach(c: Contact) -> Text {
    match c.email {
        some(e) => { return e }
        none => { return c.phone }
    }
}

fn describe(s: Status) -> Text {
    match s {
        Open => { return "open" }
        Held { reason } => { return "held: " + reason }
        _ => { return "closed" }
    }
}

fn build() -> Status {
    return Status.Held { reason: "awaiting payment" }
}
```

A record's fields are read with `.`. An enum value is built with `Enum.Variant` or
`Enum.Variant { field: value }`, and its fields are reached only through `match`. A
`match` handles every variant, or ends with `_ =>` (E109); `..` leaves the remaining
fields of a variant unbound. On an `Optional<T>`, `match` has `some(x)` and `none`
arms. `let x = none` needs a type: `let x: Optional<Int> = none`.

## Maps and sets

```staal
fn tally(words: List<Text, 500>) -> Map<Text, Int, 500> {
    let m: Map<Text, Int, 500> = [:]
    for w in words {
        match get(m, w) {
            some(n) => { set m = put(m, w, n + 1) }
            none => { set m = put(m, w, 1) }
        }
    }
    return m
}

fn total_count(words: List<Text, 500>) -> Int {
    let total = 0
    for (word, n) in entries(tally(words)) {
        set total = total + n
    }
    return total
}
```

Maps and sets keep their keys in order: every `Int` before every `Text`, `Int` keys by
value, `Text` keys by their bytes. `put` and `add` fail if the result would exceed the
declared size. Operations return a new value; the value they are given is unchanged.

## Failures and `try`

A failure stops the function and every caller, unless a caller uses `try`.

```staal
fn divide(a: Int, b: Int) -> Int {
    return a / b
}

fn safe_divide(a: Int, b: Int) -> Int {
    let r = try divide(a, b)
    if r.ok {
        return a / b
    }
    return 0
}
```

`try` applies to a function call or an action. It gives an `Outcome`: `ok` is `true`
if the call succeeded, and `error` holds the failure's message otherwise. `fail(message)`
ends a function with a failure of kind `action` and that message.

A function can declare the type it fails with, a record or enum, so callers decide by
kind:

```staal
enum ChargeError {
    Declined { reason: Text },
    Limit { over: Money },
}

const LIMIT: Money = 100.00 USD

fn charge(amount: Money) -> Money fails ChargeError {
    if amount > LIMIT {
        fail(ChargeError.Limit { over: amount - LIMIT })
    }
    return amount
}

fn explain(amount: Money) -> Text {
    let r = try charge(amount)
    match r.failure {
        some(e) => {
            match e {
                Declined { reason } => { return "declined: " + reason }
                Limit { over } => { return "over the limit by " + to_text(over) }
            }
        }
        none => { return "charged" }
    }
}
```

`fail(...)` in a function that declares `fails E` must give an `E`. A function that
calls such a function without `try` must declare `fails E` too.

## Constants and type aliases

```staal
const REFUND_LIMIT: Money = 50.00 USD
const POLICY = "Refund only damaged or missing items."

type Amount = Money
type Names = List<Text, 20>

fn within(a: Amount) -> Bool {
    return a <= REFUND_LIMIT
}
```

A constant is computed by the compiler from literals, other constants and pure
operations; an error in it, such as a division by zero, is a check error. A constant
is trusted like a literal, so it can be a model instruction. A type alias is another
name for the same type.

## Modules

```text
project/
  main.staal
  shop/policy.staal
```

`import shop.policy` in `main.staal` reads `shop/policy.staal` from the folder of the
program file and names it `policy`; `import shop.policy as p` names it `p`, and
`import shop.policy { REFUND_LIMIT }` also brings `REFUND_LIMIT` in unqualified. Only
`pub` declarations are visible from another module (E107). Names never leak between
modules. Code from outside the program's folder comes as
[packages](../guides/packages.md).
