# The Staal language guide

What Staal checks, how its programs are written, and how they run. The exact
runtime semantics are in [RUNTIME.md](RUNTIME.md), and the complete syntax in
[GRAMMAR.md](GRAMMAR.md).

## What it enforces today

| Code | Meaning |
| ---- | ------- |
| E001 | A model's `Proposed<T>` reaches an action or a non-Proposed return without `validate` |
| E002 | An action is performed without a matching capability in scope |
| E003 | A capability is widened: passed, assigned or returned with more authority than it holds |
| E004 | A function's worst-case model use can exceed its declared `budget(...)` |
| E005 | A model call (direct or through a called function) is inside an unbounded `loop` |
| E006 | An untrusted value is used as a model instruction, or flows into an `@trusted` slot |
| E007 | Blinded data can reach a model: through a call's inputs, through the decision to make the call, or through a function that does not promise to keep it blinded |
| E008 | `done(evidence: [...])` cites something that is not proof of this function's own work |
| E100 | Undefined name or function |
| E101 | Type mismatch or wrong number of arguments |
| E102 | Unknown type |
| E103 | `break` or `continue` outside `repeat`, `loop` or `for` |
| E104 | Unknown field, or a field read on a value that has no fields |
| E105 | A function with a return type can finish without returning |
| E106 | An import problem: a missing module, an import cycle, a name that clashes with an import |
| E107 | A declaration used from another module without being `pub` |
| E108 | A package uses a capability its `staal.json` does not declare, or reaches beyond the targets it declares |
| E109 | A `match` does not handle every variant of its enum (or both `some` and `none`), and has no `_` arm |
| E900 | Lexer error |
| E901 | Syntax error |

All eight safety checks (E001-E008) are implemented, and tests show each one
rejecting the programs it should. 49 valid programs compile,
including the two benchmark workflows (`workflow_*.staal`), and 586 invalid ones
are each rejected with the expected code (and, where one code covers many
rules, the expected message).

Nothing in the compiler or runtime is specific to payments, email or any other
domain. Capability paths such as `payments.refund`, `building.lock.open` or
`records.read` are names chosen by whoever deploys the program; the rules
treat them all alike.

## Language basics

    type Item { sku: Text, qty: Int, price: Money }
    type Line { sku: Text, total: Money }

    fn line_total(item: Item) -> Money {
        return item.price * item.qty
    }

    fn ship(items: List<Item, 50>, out: cap warehouse.dispatch) -> Done {
        for item in items {
            if item.qty > 0 and not starts_with(item.sku, "TEST-") {
                let r = out.dispatch(Line { sku: item.sku, total: line_total(item) })
                ...
            }
        }
        ...
    }

- **Arithmetic:** `+ - * /` on Int; `+ -` on Money of one currency; Money `*`
  Int; `+` joins Text; unary `-`. Overflow, division by zero, mixing
  currencies, negative money, and an index out of range are failures, never
  wrong answers.
- **Logic:** `and`, `or` (the right side runs only when needed), `not`.
- **Parentheses** group anything: `(validate draft).amount`.
- **Records:** `Line { sku: s, total: t }` must give every field exactly once.
- **Lists:** `List<T, N>` always declares its maximum length N, which is
  enforced on inputs and on model output. List literals `[a, b]`, indexing
  `xs[0]`, and `for x in xs { }`, which is bounded by N, so the compiler can
  cost model calls inside it.
- **Text:** `"..."` on one line, with the escapes `\"`, `\\`, `\n`, `\t`, `\r`
  and `\u{e9}`. An unknown escape is an error. Braces need no escaping.
  `"Total: \(price * qty)"` interpolates a Text, Int, Bool or Money, and is the
  same as joining with `+`: interpolating untrusted text into an instruction is
  still E006. Several lines go between `"""` quotes; the closing quotes'
  indentation is removed from every line:

      let note = """
          Order \(order.id)
          Total \(order.total)
          """

- **Built-ins:** `len`, `contains`, `starts_with`, `ends_with`.

The exact rules are in [RUNTIME.md](RUNTIME.md) section 4.

### Constants

    const REFUND_LIMIT: Money = 50.00 USD
    pub const POLICY = """
        Refund only damaged or missing items, never more than \(REFUND_LIMIT).
        """

A constant is declared at the top of a file, with or without a type, and is
computed only from literals, other constants and pure operations on them
(arithmetic, comparisons, records, lists, field reads, indexing, the pure
built-ins). The compiler computes it, so `const SHARE = 100 / 0` is an error, and
replaces every use with its value. A constant is therefore trusted like a literal
and may be a model instruction. A variable or parameter cannot take a
constant's name. `pub const` exports it: `import shop.policy { POLICY }` or
`policy.POLICY`.

### Enums and `match`

    enum Decision {
        Refund { amount: Money },
        Escalate { team: Text, note: Text },
        Close,
    }

    let d: Decision = validate propose(Decision, instruct: POLICY, data: [ticket], tokens: 300)
    match d {
        Refund { amount } => { ... }
        Escalate { team, .. } => { ... }
        Close => { ... }
    }

An enum is a value that is exactly one of its variants, each with its own
fields. Build one with `Decision.Close` or `Decision.Refund { amount: a }`. Its
fields are reached only through `match`, which must handle every variant or end
with `_ =>` (E109); `..` leaves the remaining fields unbound. A model is asked for
an enum as JSON with a `kind`, `{"kind": "Refund", "amount": ...}`, and
`validate` accepts only a declared variant with exactly its fields, so a model
cannot invent an outcome the program does not handle.

Matching follows the trust and blinding rules. The fields bound from untrusted
data are untrusted (E006). Choosing an arm reveals only which variant the value
is, so with `order: Order @blind(payment.reference)` an arm may still ask a model
about the amount, but binding `reference` there and passing it on is E007, and
with the whole `payment` blinded, deciding a model call by the variant is E007
too.

### Generic functions

    fn first<T>(xs: List<T, 100>) -> Optional<T> {
        for x in xs { return some(x) }
        return none
    }

    fn pair<A, B>(a: A, b: B) -> (A, B) { return (a, b) }

    let top = first(scores)        // T is Int
    let who = first(names)         // T is Text

A generic function works for any types its callers give it. Inside it, a type
parameter is a type of which nothing is known, so code that compiles there works for
every type. Each set of types it is used with becomes a copy, compiled and checked
like a hand-written function: a value passed through `first` keeps its trust and its
blinding, and a copy's model calls count against its caller's budget.

### Interfaces

    interface Priced {
        fn price(self) -> Money
    }

    fn total<T: Priced>(xs: List<T, 100>) -> Money {
        let sum = 0.00 NGN
        for x in xs { set sum = sum + x.price() }
        return sum
    }

An interface lists methods; a bound `T: Priced` lets a generic function call them,
and a caller may give it any type that has them. Nothing is declared on the type:
having the methods is enough. Each copy calls the type's own method directly, so a
method that asks a model still counts against the caller's budget, and blinding and
trust follow its real parameters. Bounds combine: `<T: Priced + Named>`.

### Methods

    type Line { price: Money, qty: Int }

    fn Line.total(self) -> Money {
        return self.price * self.qty
    }

    let sum = cart.lines[0].total() + line.total()

A method is a function declared beside its type, with `self` as its first
parameter. It is called on a value of that type, and is checked exactly as the
function it is: its model calls count against its caller's budget, `@blind` and
`@trusted` on its parameters (`self` included) apply, and a capability's actions
are never methods.

### Function values

    fn apply(f: fn(Int) -> Int, x: Int) -> Int { return f(x) }

    fn adder(k: Int) -> fn(Int) -> Int {
        return fn(x: Int) -> Int { return x + k }
    }

    let a = apply(double, 3)       // a named function as a value
    let add3 = adder(3)            // a function value that captured k = 3
    let b = add3(4) + adder(1)(2)
    let cost = total_by(lines, fn(l: Line) -> Money { return l.price * l.qty + fee })

A function value is a function to call later: a named function, or one written in
place with `fn(...) { ... }`, which may read the variables around it. What it reads
is copied when the value is made. A function value only computes: its function, and
everything that calls, makes no action, model call or `done`, and does not print,
read the clock or draw a random number. So a function value
can never carry a capability (reading one is E003, so authority cannot leave a
function inside one and be used later), a secret or proof, and nothing about budgets
or blinding depends on which function it holds. It is never kept inside data (a
list, record, map, tuple or optional value), compared, encoded or sent anywhere.
What one returns is untrusted, and reveals everything it captured and was given.
Generic functions take function values like any other type (`fn twice<T>(f: fn(T)
-> T, x: T) -> T`). Both engines run them.

### The standard library

    let parts = split("a,b,c", ",", 10)          // ["a", "b", "c"], at most 10 parts
    let shout = upper(join(reverse(parts), "-")) // "C-B-A"
    let middle = slice("hello", 1, 4)            // "ell"
    let n = to_int("42")                          // some(42)
    let xs: List<Int, 10> = [3, 1]
    set xs = append(sort(xs), 7)                  // [1, 3, 7]
    let total = sum(xs) + abs(-2) + max(1, rem(9, 4))

    let totals = map(lines, fn(l: Line) -> Money { return l.price * l.qty })
    let stocked = filter(lines, fn(l: Line) -> Bool { return l.qty > 0 })
    let sum_all = fold(totals, 0.00 NGN, fn(a: Money, m: Money) -> Money { return a + m })

Text, number and list functions are built in, on both engines: `upper`, `lower`,
`trim`, `slice`, `index_of`, `split`, `join`, `replace`, `to_int`, `to_text`; `abs`,
`min`, `max`, `rem`; `append`, `concat`, `reverse`, `sort`, `sum`, `len`,
`contains`; and with a function value, `map`, `filter`, `count`, `any`, `all` and
`fold`, which call it on each item in order; and for hashing and encoding,
`sha256`, `base64_encode`, `base64_decode`, `hex_encode` and `hex_decode` (the
decoders give an optional value, since not every text decodes); and for dates and
times, on a time written as an Int of milliseconds since 1970 in UTC, `format_time`,
`parse_time`, `make_time`, `date_parts`, `time_parts` and `weekday`. `print`,
`now` and `random_int` differ between runs, so each is journaled: a run resumed
after a crash uses the time and the numbers the first run recorded, and prints
nothing twice. Every list keeps its maximum: `append` to a full list fails, `split`
takes the most parts it may give, and `concat` gives room for both lists. Positions
count characters, so text in any language slices the same way. A result is as
trusted as its arguments and reveals what they reveal; a function that can fail
(`slice` out of range, `append` to a full list) makes what follows depend on its
arguments, as any failing operation does.

### Default values and named arguments

    fn notify(to: Text, body: Text = "Your order shipped.", urgent: Bool = false) { ... }

    notify(customer.phone)
    notify(customer.phone, urgent: true)
    notify(urgent: true, to: customer.phone, body: note)

A parameter with `= value` may be left out; the value is a constant, fixed when the
program is checked, so leaving a parameter out can never fail or reveal anything.
Parameters with defaults come last. A named argument goes to the parameter it
names, wherever it is written, and every rule of that parameter applies to it:
an untrusted value named for an `@trusted` parameter is still E006. Arguments are
evaluated in the order written. A run can leave out an entry parameter that has a
default.

### Tuples

    fn min_max(xs: List<Int, 100>) -> (Int, Int) { ... return (lo, hi) }

    let (lo, hi) = min_max(readings)
    let width = hi - lo
    let first = pair.0

A tuple groups a fixed number of values without naming them: `(Int, Int)`, read by
position (`.0`, `.1`) or unpacked with `let (a, b) = ...`. In JSON it is an array of
exactly that length. For anything a model produces, or anything whose parts deserve
names, use a record: a model is never asked for a tuple.

### Maps and sets

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

    for (word, n) in entries(tally(words)) { ... }
    let tags = add(to_set(names), "new")

Keys and set items are Text or Int, kept in order. Every map and set has a maximum
size, like a list, so loops over them are bounded and their cost is known; `put` and
`add` fail if they would go over it. Operations return new values and never change
the one they are given. In JSON a map is an object and a set an array of distinct
items. A model is never asked for a map or a set: ask for a list of records instead,
which a model's output schema can describe exactly.

### Type aliases

    type Amount = Money
    type Names = List<Text, 20>
    type Email = Optional<Text>
    type State = Status        // an enum: State.Open builds Status.Open

A type alias is another name for a type, and nothing more: `Amount` and `Money`
are the same type everywhere, so a function taking `Money` accepts an `Amount` and
blinding, trust and budgets treat them alike. An alias of a record builds it with
`Alias { ... }`, and an alias of an enum builds its variants. A `pub type` alias is
imported like any type.

### Optional values

    type Contact { phone: Text, email: Optional<Text> }

    fn reach(c: Contact) -> Text {
        match c.email {
            some(e) => { return e }
            none => { return c.phone }
        }
    }

    fn find(words: List<Text, 20>, start: Text) -> Optional<Text> {
        for w in words {
            if starts_with(w, start) { return some(w) }
        }
        return none
    }

`Optional<T>` is a T or nothing. `some(x)` holds x and `none` holds nothing; a
plain T never becomes optional by itself, and what an optional value holds is
reached only through `match`, which must handle both `some(...)` and `none`, or
use `_` (E109). `let x = none` needs a type, as in `let x: Optional<Int> = none`.
In JSON, `none` is `null`, and a field of an optional type must still be present.
A model asked for a `Contact` may answer `"email": null`, and `validate` accepts
it.

Blinding works through optional values. With `order: Order @blind(contact.email)`
where `contact` is optional, an arm chosen by whether there is a contact may ask a
model about the phone, but passing the email bound from it is E007. With all of
`contact` blinded, deciding a model call by whether it is there is E007 too.

### Reassignment

    let total = 0
    for item in items {
        if item.qty > 0 { set total = total + item.qty }
    }

`set x = e` changes a variable declared with `let`. Parameters, loop variables,
capabilities and proof values (`Receipt`, `Outcome`, `Done`) cannot be
reassigned, so what a value proves or allows never changes under it.

For blinding and trust, the compiler follows the code in order. After
`set x = e`, `x` reveals what `e` reveals, plus whatever decided that the `set`
ran: setting `vip = true` inside `if ends_with(order.customer.email,
"@bigcorp.com")` makes `vip` reveal the email (E007), and setting an
instruction to an untrusted value makes it untrusted (E006). Where paths meet,
after an `if`, at the top of a loop and after it, a variable reveals what it
reveals on any path that arrives there. So a value read before a blinded
`set`, or overwritten before it is read, is safe and accepted; in a loop, the
next iteration sees what this one set.

What stays conservative: anything that can stop the code, such as `n + 1`
overflowing or an action failing, still counts as revealing its inputs to
everything after it. That is the price of never missing a leak.

Money literals are written with their currency: `0 USD`, `12.50 NGN`,
`1000 JPY`. They follow the rules for money from outside: a supported
currency, no more decimals than it has, and no amount too large to hold
exactly. A decimal number without a currency is an error, since Staal has no
floating point. The number and its currency must be on the same line.

## Modules

    // sms/termii.staal
    pub type Message { to: Text, body: Text }
    pub fn send(m: Message, http: cap net.http.post) -> Receipt { ... }
    fn endpoint() -> Text { ... }          // private to this file

    // clinic.staal
    import sms.termii                      // as `termii`
    import util.text as txt { clean }      // as `txt`, and `clean` unqualified

    let m = termii.Message { to: phone, body: text }
    let r = termii.send(m, http)

A file is a module. `import a.b` reads `a/b.staal` from the folder of the
program file and names it `b` (or the name after `as`); `{ ... }` also brings
those names in unqualified. Only `pub` declarations can be used from another
file (E107). Imports come first in a file. A name is defined once per file: a
local name, parameter or variable that clashes with an import, a module
imported twice, and import cycles are errors (E106).

Names never leak between files. A library that calls `helper()` gets its own
`helper` or an error, never the program's function of that name.

How the rules cross files: the compiler resolves every module into one program
in which each name is qualified by its module (`send` in `sms/termii.staal` is
`sms.termii.send`), then checks it exactly as it checks a single file. Blinding
promises, budgets, trust, evidence and partial operations therefore hold
across modules with no separate cross-module analysis. This is tested by
splitting every test program in two (`tests/split_test.rs`): types and called
functions move to a module (capability declarations too), and every one of 352
splittable fixtures and 355 runtime cases must give identical results. Mutations that hide one module's
model calls, failures or costs from another are each caught.

The program's hash, which a journal records, covers each module's path and
text but not file names, so a run can be resumed from another folder.

Code from outside the program's folder comes as packages (next section).

Speed. Every build checks the whole program, which keeps the cross-file rules
simple and exact. Measured on generated programs (`bench/gen_program.py`:
functions with loops, branches, reassignment, calls across 20 modules, and a
model call in about one in ten), release build, one core of a 2-core cloud
machine, best of three runs:

| Functions | Lines | Check time |
|---|---|---|
| 100 | 1,653 | 0.021 s |
| 1,000 | 16,002 | 0.19 s |
| 5,000 | 79,461 | 1.06 s |
| 20,000 | 317,876 | 4.85 s |

Time grows slightly faster than size (4 times the functions, about 4.6 times
the time). Per-module checking with stored summaries is therefore not needed
yet; it should be built only if real programs approach these sizes. These are
generated programs, so programs with deeply nested loops full of `set` may cost
more per line.

## Capability limits

    fn open_doors(doors: List<Text, 5>,
                  lock: cap building.lock.open(uses: 2, only: ["north-door", "loading-bay"])) { ... }
    fn notify(number: Text, sms: cap twilio.message.send(only: ["+234*"])) -> Receipt { ... }
    fn refund(r: Refund, pay: cap payments.refund(max: 500 USD, each: 50 USD)) -> Receipt { ... }

| Limit | Meaning |
|---|---|
| `max` | most money spent in total over the run |
| `each` | most money one action may spend |
| `uses` | how many actions may be attempted (a failed attempt counts) |
| `only` | which targets may be acted on: exact text, or one `*` wildcard (`"*@teamozy.org"`, `"/reports/*"`) |

A capability may be narrowed (fewer uses, a lower limit, a tighter `only`
list, adding a limit) and never widened (E003): `uses: 3` cannot become
`uses: 10` or unlimited, and `only: ["/reports/*"]` cannot become `["/*"]`.
Money limits are never converted between currencies.

At runtime the broker checks every action against the limits of its
capability and of every capability it was narrowed from, before the service
is called. A limit that needs an action's amount or target refuses the action
when it cannot be determined. Grants in the run config can carry the same
limits. Details: [RUNTIME.md](RUNTIME.md) section 6.

## Budgets and loops

    fn draft() -> Note budget(calls: 3, tokens: 6000) {
        let n = validate propose(Note, tokens: 2000)
        repeat 2 { let retry = propose(Note, tokens: 2000) }
        return n
    }

`propose(T, tokens: N)` declares the most one model call may use; without
`tokens:` its token use is unbounded. `budget(calls: N, tokens: N)` declares a
function's worst case; either part may be omitted, and functions without a
budget are not checked against one.

The compiler sums worst-case costs: `repeat N` multiplies by N, nested loops
multiply together, and calls are counted through other functions. A called
function with a budget is costed by its declaration; one without is costed by
walking its body. Recursion that makes model calls is unbounded. `loop { }` is
unbounded and may not contain model calls (E005), even with a `break` inside,
because the compiler cannot prove the `break` is ever reached. An `if` costs
its condition plus its more expensive branch.

Budgets count calls and tokens only. Money and time depend on provider prices
and clocks, so they belong to the runtime budget meter. Token limits are only
as true as the runtime enforces: the runtime must cap each call at its
declared `tokens:`.

## Control flow

    if n < 1 { ... } else if n == 1 { ... } else { ... }
    repeat 3 { ... if ok { break } }
    return            // bare return, in functions that return nothing

Conditions must be `Bool`. Comparisons are `== != < <= > >=`, cannot be
chained, and `< <= > >=` need `Int` or `Money`. A model's unvalidated output
cannot be used as a condition or compared (E001).

## Errors: `try`

    let sent = try mail.send(order.customer.email)
    if sent.ok { ... } else { ... }      // sent.error: Text

An action can fail. Without `try`, a failure stops the function and passes up
to its caller, which stops too unless it called with `try`. With `try`, the
failure becomes an `Outcome { ok: Bool, error: Text }` value and the function
continues either way. `try` applies to an action or a function call. The
outcome comes from the outside world, so it is untrusted, and it reveals what
the operation was given (an error message can quote the address it failed on).

`try` catches failure, not a computation that never ends: a call to a function
containing `loop` or recursion can still stall, under `try` or not.

### Typed errors

    enum ChargeError {
        Declined { reason: Text },
        Limit { over: Money },
    }

    fn charge(c: Charge) -> Money fails ChargeError {
        if c.amount > LIMIT { fail(ChargeError.Limit { over: c.amount - LIMIT }) }
        ...
    }

    let r = try charge(c)
    match r.failure {
        some(e) => { match e { Declined { reason } => { ... } Limit { over } => { ... } } }
        none => { ... }    // succeeded, or failed some other way: r.error says how
    }

A function can declare what it fails with: a record or enum, so callers can decide
by kind instead of parsing a message. `fail(...)` in it must give that type. A
failure that is not caught leaves every caller as it is, so a function that calls
`charge` without `try` must declare `fails ChargeError` too; the compiler checks
this, and `.failure` is therefore always of the declared type.

## Claiming completion: `Done` and evidence

    fn refund(...) -> Done {
        let paid = pay.refund(r)
        let sent = try mail.send(order.customer.email)
        if sent.ok { return done(evidence: [paid, sent]) }
        return done(evidence: [paid])
    }

`Done` is built in, and `done(evidence: [...])` is the only way to make one.
Each item must be proof of work this function did: an action's `Receipt`,
another function's `Done` or `Receipt`, or a `try` outcome that holds one. An
outcome counts only if `try` in this function wrapped an action or a call
returning `Receipt` or `Done`, and only on a path where `.ok` is known to be
true (inside `if x.ok`, or after `if x.ok == false { return ... }`).
Rejected as evidence (E008): an empty list, model output, literals, an
unchecked outcome, an outcome of a call that returns anything else (it holds
no receipt), and anything passed in by the caller. A function returning
`Receipt` or `Done` may not return one it was given. At run time, every
evidence item is checked again to lead to at least one real receipt. A function that
returns a value must return on every path (E105), so a `Done` function cannot
fall off the end without proof.

What it guarantees: a function can only claim completion by citing receipts
of actions it actually performed. What it does not guarantee: that the action
achieved what the business intended (a receipt shows the refund was issued,
not that it was the right amount). The runtime stores the evidence with the
execution log.

## Fields

    type Customer { name: Text, email: Text }
    type Order { id: Text, total: Money, customer: Customer }

    if refund.amount <= order.total { mail.send(order.customer.email) }

Fields are read with `.`, to any depth. Reading a field of an unvalidated
`Proposed<T>` is E001: validate first, then read (`let r = validate draft`,
then `r.amount`). A field is exactly as trusted as the value it came from.
Type fields cannot be capabilities, so authority cannot travel inside data
sent to a model. `a.b.c(...)` is a syntax error: actions are called directly
on a capability variable. `(validate draft).amount` reads a field of a
validated value directly.

## Trust and prompt injection

    let reply = propose(Reply,
        instruct: "Reply politely to the customer message in the data.",
        data: [ticket],
        tokens: 1000)

`instruct:` is the instruction position of a model call. Only trusted values
may go there; anything may go in `data: [...]`. Literals are trusted.
Parameters are untrusted unless marked `name: Text @trusted`, and callers must
pass trusted values to them. Function results are untrusted unless the return
is marked `-> Text @trusted`, and then every returned value must be trusted.
Model output stays untrusted even after `validate`, because validation checks
shape, not intent. Capabilities cannot be sent to a model as data.

What this guarantees: customer text, model output and anything not explicitly
vouched for can never be placed in an instruction slot. What it does not
guarantee: that a model ignores instructions hidden inside `data`. Keeping data
in a separate slot lets the runtime delimit it, but the type system cannot make
a model obey that boundary. Whoever marks a parameter `@trusted` is responsible
for that claim.

## Keeping data from a model: `blind_to`

    let s = propose(Text,
        instruct: "Summarise this order.",
        data: [order.id, order.total, order.customer.name],
        blind_to: [order.customer.email])

No input of the call, instruction or data, may reveal a path in `blind_to`.
Paths overlap when one contains the other, so sending `order` or
`order.customer` reveals `order.customer.email`, and blinding `order.customer`
blinds every field inside it. Sibling fields are fine.

The check follows data through `let` aliases (`let c = order.customer`), and
treats anything computed as able to reveal all of its inputs: function
results, action results, comparisons, and model output (a model that saw the
email may repeat it). `blind_to` entries must name data directly; a computed
value cannot be blinded, so name what it came from.

### Across functions: `@blind` parameters

    fn greet(o: Order @blind(customer.email)) { ... }
    fn top(order: Order @blind(customer.email)) { greet(order) }

`@blind(path, ...)` on a parameter promises that those fields never reach a
model inside this function or anything it calls; `@blind` alone blinds the
whole value. Every model call in the function is checked against it, as if
listed in its `blind_to`. Passing blinded data to another function requires
that function's parameter to make the same promise (blinding a parent such as
`customer` covers `customer.email`), so the guarantee holds at every depth.
Functions that cannot reach a model, directly or through calls, need no
promise; their results still count as revealing their arguments.

### Through branches

Whether code runs reveals the conditions that led to it. So inside
`if order.customer.email == x { ... }`, every value bound counts as revealing
the email, and so does the decision to call a model or a model-calling
function there. The same applies to code after an `if` whose branch can
`return` or `break`, and to a whole loop body when a `break` depends on
blinded data (later iterations run only if it was not taken).

### Through operations that might not finish

An action can fail (a bad address, a declined payment), and a function with a
`loop` or recursion might never return. Either way, code after it only runs
for some argument values. The compiler tracks two properties per function:
it **may fail** if it contains an action, or a call to a failing function, not
under `try`; it **may not finish** if it contains `loop` or recursion, or calls
anything that may not finish (`try` does not help). An operation is *partial*
if it is an action not under `try`, a call to a failing function not under
`try`, or any call to a function that may not finish. Everything after a
partial operation counts as depending on its arguments, and everything after
a branch containing one counts as depending on the branch condition. Pure
helpers (no actions, `loop` or recursion; `repeat N` is fine) are unaffected.

In practice, with `@blind(customer.email)`:

    mail.send(order.customer.email)              // stops here if it fails...
    let s = propose(...)                         // ...so this call reveals success: E007

    let sent = try mail.send(order.customer.email)
    let s = propose(...)                         // runs either way: allowed
    if sent.ok { let t = propose(...) }          // depends on success: E007

### Side channels: what is closed and what is not

Two observers matter. The **model** sees only each request's instruction and
data; it has no clock, and Staal gives programs none. The **provider** (or
anyone on the network path) also sees when requests arrive, how many there
are, and their sizes.

Closed by the rules above, for both observers: whether a model call happens,
how many happen, their order, their inputs, and therefore their token use and
budget consumption. No model call may depend on blinded data, so the whole
sequence of model calls is fixed by non-blinded data.

Not closed, and not closable by a type system: **wall-clock timing**. Slow
work done only for some blinded values shifts when an identical model call
starts. A provider with precise timestamps could infer which branch ran, at
most one bit per blinded decision per observation, never the value itself.
The model cannot observe it. The mitigation belongs to the runtime (releasing
model calls on a fixed schedule, at a latency cost).

Other limits:

- **Actions.** `@blind` and `blind_to` keep data from *models*. Sending
  `order.customer.email` through an email capability is allowed; that is what
  capabilities are for.
- **Stateless calls (runtime requirement).** All of the above assumes every
  model call starts from a fresh context. A runtime that reused a conversation
  across calls would show call two the inputs of call one, which were checked
  against call one's `blind_to`, not call two's.
- **Over-caution.** The analysis is conservative and rejects some safe
  programs: a loop with a blinded `break` taints its whole body, and an action
  with blinded arguments that is not under `try` taints everything after it
  (use `try` when code must continue past it). A branch that always returns
  is recognised as unable to affect later code, which removes the most common
  false alarm (send-then-return-either-way inside a retry loop).

## Running programs, and surviving crashes

    staalc run     prog.staal --config run.json --journal run.journal [--log run.jsonl]
    staalc resume  prog.staal --config run.json --journal run.journal
    staalc resolve --journal run.journal (--performed <receipt> | --failed <reason> | --retry)

The reference interpreter runs checked programs. Model calls go to Claude
(`"provider": "anthropic"`) or, in tests, to a scripted model; actions go through
the runtime's own `net.http` and `fs` capabilities, libraries built on them, or
simulated adapters. With
`--journal`, a run survives being killed at any moment: `resume` finishes it
without performing any action twice and without losing any action that
happened. The full protocol is in [RUNTIME.md](RUNTIME.md) section 11.

- Calling Claude takes a model id and the environment variable holding the key:

      "model": { "provider": "anthropic", "model": "claude-sonnet-5-5",
                 "api_key": { "env": "ANTHROPIC_API_KEY" } }

  Tokens are counted before each call and charged from what the API reports.
  Rate limits and overloads are retried; if the API stays unavailable, the run
  stops and `resume` makes the call again. Refusals and output cut off at the
  token limit are `provider` failures a program can `try`. The full behaviour
  is in [RUNTIME.md](RUNTIME.md) section 5.
- Each token count, model call and action is recorded as an intent before it
  and a result after it, each flushed to disk (`fsync`) before the run goes on.
  Resume re-executes the program and takes recorded results from the journal
  instead of contacting the world; if the program, config or runtime changed,
  it refuses rather than diverge.
- Actions carry an idempotency key unique to the run and the operation. If an
  action was in flight when the process died, resume retries it only if the
  adapter declares the service idempotent. Otherwise it stops (exit 3,
  `in_doubt`) and does nothing until an operator records what really happened
  with `staalc resolve`.
- Only one process may use a journal at a time (an OS lock, freed if the
  process dies). A torn last line is discarded; any other damage is refused.

Exit codes: 0 success; 1 check errors or the run failed; 2 a usage, file,
config or journal problem (nothing ran); 3 the run stopped and can be
continued (resolve, then resume).

How this is tested: `cargo test --features failpoints` kills the process (with
`abort`) at every journal write and inside every in-doubt window of two
programs, resumes, and checks the result and the simulated world match an
uninterrupted run, with no action performed twice, against both an idempotent
and a non-deduplicating service. The programs are a refund workflow, and the
building-access program below: six model calls and ten actions under `uses`
and `only` limits, at 68 crash points per service. The second proves a
resumed run rebuilds its capability counts exactly. A runtime that forgot
replayed uses would open a fourth door, and this test catches that mistake
where the first one does not.
The crash hooks are compiled out of normal builds.

What this does not cover: `abort` kills the process but not the operating
system, so these tests do not exercise power loss; durability against power
loss relies on `fsync` being honoured by the OS and disk. An adapter that
declares itself idempotent must really be: the runtime trusts that contract.

## Programs from different fields

`tests/run/domain_*.staal` are complete programs, each run against a scripted
model and simulated services, with cases for success, for model mistakes, for
limits refusing, and for bad input:

| Program | Field | What keeps it safe |
|---|---|---|
| `domain_clinic` | healthcare scheduling | notes and phone blinded from the model; the slot the model picks is range-checked; one booking per run; SMS only to `+234*` |
| `domain_access` | building access | a model decision per visitor, but only three listed doors open, at most three times; every decision audited |
| `domain_documents` | contract review | ids that could escape the folder are refused; writes only under `/contracts/*`; mail only inside the company; owner address blinded |
| `domain_logistics` | carrier rebooking | `each: 500 USD` and `max: 2000 USD` across a loop; refused bookings reported; the model's action list bounded to 5 |
| `domain_support` | ticket triage | the ticket is data, never instruction; one reply; the pager reaches only `support-oncall`, once, even if the ticket talks the model into "urgent" |

Each also has a compile-fail twin in `tests/fixtures/fail/` showing a likely
mistake the compiler rejects: sending the whole patient record (E007),
handing the lock to a helper that accepts any door (E003), filing an
unvalidated review (E001), a model call per shipment over budget (E004), and
the ticket as the instruction (E006).

One thing these programs show: `only` matches the target text exactly as the
adapter reports it. `"/contracts/../payroll"` starts with `/contracts/`, so
an adapter for paths must report resolved paths, and the program refuses ids
containing `/` or `..` before building one.

## HTTP and files: `net.http` and `fs`

    type Sms { to: Text, sms: Text, type: Text, api_key: Text }

    fn send(to: Text, text: Text, key: Secret,
            termii: cap net.http.post(only: ["https://api.ng.termii.com/api/sms/send"], uses: 1)) -> Text {
        let body = fill_secret(json(Sms { to: to, sms: text, type: "plain", api_key: "{secret}" }), key)
        let r = termii.post("https://api.ng.termii.com/api/sms/send", body, [header("Content-Type", "application/json")])
        let reply: Reply = validate parse(Reply, r.body)
        ...
    }

Two capability families belong to the runtime: `net.http.get/post/put/patch/delete`
returning a `Response { status, body, complete }`, and `fs.read` / `fs.write`.
Every existing limit applies to them, and every request and write goes through
the journal. Libraries for real services are written on top of these (see
[Capabilities from libraries](#capabilities-from-libraries)). The full rules are in [RUNTIME.md](RUNTIME.md) section 12. In short:

- **Secrets** come only from environment variables named in the run config. A
  `Secret` cannot become text, be compared, sit in a record, reach a model or go to
  any other action: it can only be part of an HTTP request, as a header or a body
  template (`fill_secret`). The journal and log record its variable name, never its
  value, and the value is removed from every response before the program sees it.
- **One spelling per target.** URLs are put into canonical form (WHATWG parsing,
  then stricter rules: https only except localhost, no credentials in URLs, no
  encoded slashes) and paths are normalised before `only` is checked, so
  `/api/../admin` and `/api/%2e%2e/admin` cannot slip past `"https://x.org/api/*"`.
  Patterns must be canonical, and a `*` cannot sit in the host.
- **No surprises from HTTP.** Redirects are returned, never followed. Error
  statuses are responses. The runtime owns `Host`, `Content-Length`,
  `Idempotency-Key` and similar headers, and no header can smuggle in another.
- **Files** are opened one directory at a time, never following a symbolic link,
  so a folder swapped for a link mid-run cannot redirect a read or write. Reads
  refuse special files and files with other hard links; writes are atomic.
- **A request that may have happened is in doubt.** If a POST's connection breaks
  after it was sent, the run stops `in_doubt` instead of failing, so the program
  cannot retry and send twice, and an operator records what the server answered
  (`staalc resolve --status 200 --body ...`). If the grant says the service
  deduplicates (`"idempotency_key": true`), the runtime sends an
  `Idempotency-Key` and retries safely instead.
- **Data from services** is untrusted (never an instruction, E006) and unchecked
  until validated (`parse` gives a `Proposed`, E001). Fields a type does not
  declare are dropped; declared ones are checked exactly. Keywords such as `type`
  can be field names, as JSON APIs need.

How this is tested: 16 runtime tests against a local HTTP server check what the
server received as well as what the program saw (secrets only in headers and
bodies, never logs or journals; traversal and refused targets never sent; a
`{secret}` smuggled into message text refused). `tests/http_crash_test.rs` kills a
run at all 14 points of a program with a GET, a deduplicated POST, a plain POST and
a file write, and checks each POST took effect exactly once. File access is tested
against links in folders and files, loops, hard links, FIFOs, and a folder swapped
for a link 11,000 times during 20,000 reads, with no leak. Seventeen mutations of the
guarantees are each caught: skipping URL normalisation, not removing secrets from
responses, recording a secret's value in the journal, allowing a second
`{secret}`, retrying every POST, not retrying a GET, dropping the
Idempotency-Key, reporting "may have happened" as "not sent", following
redirects, following a link in a folder, following a link as the file,
reading a file with other hard links, treating every TLS error as "not sent",
treating certificate errors as "may have been sent", replacing Mozilla's roots
with the extra ones, ignoring the extra ones, and accepting a private key in a
root file.

HTTPS is tested against a local rustls server with certificates made by the
tests: a trusted authority works; an unknown authority, a self-signed certificate,
a certificate for another name, an expired one and one not yet valid are each
refused before any request is sent, and reported as ordinary failures, not in
doubt. A TLS error after the request was sent (the server answers with bytes that
are not TLS) stays in doubt. An organisation's own authority can be added with
`net.extra_root_certificates`, on top of Mozilla's roots, never instead.

What these tests do not cover: real services. `examples/` holds Termii SMS and
Resend email programs with instructions to run them with your own keys.

## Capabilities from libraries

    // termii.staal: a library
    capability termii.sms {
        needs http: cap net.http.post
        needs key: Secret
        needs base: Text
        needs sender: Text

        canonical(phone: Text) -> Text {
            return number(phone)
        }

        action send(to: Text @target, text: Text) -> Text {
            ...                                   // a request through `http`
            return reply.message_id
        }
    }

    // remind.staal: a program
    import termii
    fn remind(patients: List<Patient, 50>, texts: cap termii.sms.send(uses: 50)) -> Int { ... }

A library declares a capability; a program asks for it and calls it like any
other, getting a `Receipt`. The program never holds the key, the URL or the HTTP
capability: the operator grants those to the library, under `needs` in the run
config. The full rules are in [RUNTIME.md](RUNTIME.md) section 13. In short:

- **The program's limits apply to the library's action.** `only` is checked
  against the target in the form the library's `canonical` gives (so `+234 801
  234 5678` matches `"2348012345678"`), and `max` and `each` against the
  `@amount`, before any of the library's code runs. `uses` counts actions.
- **The operator's limits apply to the library's requests.** Each need that is a
  capability is granted with its own `only` and `uses`, and narrowed further by
  what the library declared.
- **The compiler checks the library.** An action must make a request through its
  needs, the `@target` and `@amount` must reach a request, and actions cannot call
  models or run without end. Secrets stay secrets inside libraries too.
- **A receipt stands for a request.** An action that returns without having made
  one fails. An action that fails after a request that may have spent money is
  charged anyway, so failures cannot be used to spend past `max`.
- **Crashes** replay the library's requests from the journal, in order; a request
  in doubt stops the run as usual.

`examples/capabilities/` holds a Termii SMS library and a Stripe refunds library
(which checks the payment's currency, sends minor units form-encoded, and keeps
the payment id one URL segment with `url_segment`), with a program for each.

How this is tested: 13 runtime tests run the two libraries against a local server
and check what it received: the canonical number and the key in the body (never in
the log), refusal by `only` before any request, `uses`, `each` and `max` on the
library's amount, the operator's `only` on the library's requests, a library's
declared limits narrowing the grant, a receipt refused when no request was made,
a failed action charged after a POST but not after a GET, data kept to one URL
segment, and config errors in `needs`. `tests/library_crash_test.rs` kills a run
that refunds through Stripe and texts through Termii at all 11 points, resolves the
SMS from what the server received when it is in doubt, and checks the refund took
effect once (same Idempotency-Key on every attempt) and the SMS reached the server
once. Sixteen mutations are each caught: skipping `canonical`, skipping the
broker's check, not counting a use, not narrowing a need, giving a receipt without
a request, not charging on success, not charging after a failed POST, charging
after a failed GET, accepting `needs` on a plain capability, starting a run inside
a capability, turning off the compiler's target, amount and request checks,
hashing file names instead of module paths, and letting `url_segment` pass a `/`
or `..`.

What the runtime cannot check is that a library does what its name says: a
library is code the operator trusts with keys, as in any language. A `canonical`
that maps two targets to one form would weaken `only`. Read a library before
granting it keys. A library used as a package is pinned by hash, so it cannot
change after it was read (next section).

## Packages

    // staal.json, next to the program
    { "dependencies": {
        "sms": { "git": "https://github.com/laurells/staal-sms", "commit": "1bd0e7645a505227084bc83379affbcd71732560" } } }

    // main.staal
    import sms.termii
    fn remind(phone: Text, texts: cap sms.termii.send(uses: 1)) -> Receipt { ... }

    $ staalc fetch
    sms https://github.com/laurells/staal-sms at 1bd0e7645a50 (new)
        authority: net.http.post only https://api.ng.termii.com/api/sms/send

A package is a git repository with a `staal.json` naming it and listing its
`authority`: every capability its code may use, with the targets it may reach.
`staalc fetch` reads each dependency at its exact commit (a branch or tag is
refused: they move), writes it to `.staal/packages/`, records the SHA-256 of
every file in `staal.lock`, and prints each package's authority for review.
The full rules are in [RUNTIME.md](RUNTIME.md) section 14. In short:

- **Nothing unverified is read as code.** Before checking or running, every
  package folder is compared with `staal.lock`: a file changed, added, removed,
  or replaced by a link is refused. `fetch` reads files from git's objects, not a
  checkout, so no line-ending conversion or filter can change them, and it compares
  them with the lock every time, so an edited lock is caught too.
- **A package is held to its manifest (E108).** Its code may only mention
  capabilities its `authority` lists, and where the manifest gives targets, every
  such capability in the code must carry an `only` within them, which the runtime
  enforces when a capability is passed in. A package's own capabilities must be
  named after it (`sms.*`), so it cannot pose as another.
- **Imports stay inside the package.** `import util` in package `sms` means
  `sms`'s own `util`, never the program's; a package's dependencies are reached by
  their names. The program can import only packages it depends on itself, and one
  version of each package is used across the whole project.
- **Journals pin the code.** A run cannot resume after a package changed commit.

`packages/` holds four reference packages: `sms` (Termii), `payments` (Stripe
refunds), `email` (Resend) and `chat` (Slack). Each pins the exact endpoints it
calls.

How this is tested: 12 tests build git repositories and fetch from them. They
check the four reference packages fetch, check, and run against a local server
standing in for each service (including Slack's "ok": false refusal and an email
address `canonical` refuses); tampering by a changed, added
or removed file or a link is refused; an edited lock is caught by the next fetch;
branches, unknown commits, misnamed packages, plain http and links in a repository
are refused; imports resolve inside packages and across dependencies; two
versions of one package are refused; each authority rule; a program runs through a
fetched package against a local server; and a journal refuses to resume on a new
commit. Seventeen mutations are each caught: not comparing hashes, ignoring an
added or missing file, following a link, accepting a link from git, trusting the
lock without git, accepting a branch name, not prefixing a package's own imports,
letting the program import a package it does not depend on, allowing a dependency
and a folder of one name, each of the four authority rules, allowing two versions,
allowing plain http, and accepting a misnamed package.

Packages are fetched from any git host over HTTPS or SSH, GitHub included. There
is no package registry yet: a dependency is always a git URL and a commit. Whoever can edit both `staal.lock` and `.staal/packages/` controls the
code, as with any lockfile: `staalc check` trusts the lock, and `staalc fetch`
compares both with git. `git` must be installed to fetch.

## Native code and the C interface

    staalc run program.staal --config run.json --engine native

On the native engine, every pure function is compiled to machine code when the run
starts (Cranelift, in memory): its parameters and result are Int, Bool, Text, Money,
records, enums, optional values, tuples, maps, sets or lists of these, and its body uses `let`, `set`, `if`, `match`,
`repeat`, `loop`, `for`, `break`, `continue`, `return`, `fail`, arithmetic,
comparisons, field reads, record, variant, tuple, list and map literals, `some` and `none`, indexing, the pure built-in functions, and calls to other such
functions. Everything else runs on the interpreter, which stays the reference.
Model calls, actions, `try`, `parse`, `validate`, HTTP, files and the journal never
run in native code. The full rules are in [RUNTIME.md](RUNTIME.md) section 15. In short:

- **The same answer, byte for byte.** Native code counts steps and call depth
  against the same limits on the same counters, and computes Int and Bool in
  machine code with every operation checked. Text, money, records and lists live
  in an arena the runtime owns; every operation on them calls the interpreter's own
  function, so results, failure messages, logs and journals are identical. Values
  are dropped exactly when the interpreter would drop them, so a long loop holds no
  more memory.
- **Crash on one engine, resume on the other.** The engine is not part of the run
  config, so the journal does not depend on it.
- **Faster where it applies, and honest about where it does not.** Release build,
  best of seven runs: a recursive Int program, 0.016 s native against 1.42 s on the
  interpreter; a loop over records with money, 0.079 s against 0.115 s; a short
  text function, 3 ms slower native, as compiling at the start costs more than it
  saves. Work on text, money, records and lists still calls into the runtime for
  each operation.

`include/staal.h` declares the C interface: `staal_check`, `staal_run`,
`staal_resume`, `staal_resolve`, `staal_free` and `staal_version`, in
`libstaalc.so` (or `.dylib`). Each returns a JSON object whose `status` is `done`,
`failed`, `stopped`, `invalid` or `error`, mirroring the command line's exit codes,
with the run's log. `python/staal` wraps it with `ctypes`; Staal contains no
Python.

    import staal
    lib = staal.Library("target/release/libstaalc.so")
    out = lib.run("clinic.staal", config, journal="run.journal", engine="native")

How this is tested: all 419 runtime cases run on both engines with a journal, and
their results, logs and journals are identical (3,935 functions compile across them).
2,500 generated programs using Int, Bool, Text, money, records and lists, compiled
entirely to native code and run under tight step and depth limits with values at
their edges, end identically on both engines; across them, runs finish, fail with
`value` and with `fail`, and reach step and depth limits. A table of programs hits
every failure native code reports at its edge. A program mixing both engines is run
at every step limit from 1 to past its end. A 10,000-iteration loop never holds more
than 12 values at once. The crash tests run with every pairing of engines for the
crash and the resume. Twenty-seven mutations of the native compiler are each caught,
among them: a missing step per statement or per `loop`, `repeat` or `for` iteration;
limits off by one; unchecked overflow, `MIN / -1` or negation; no short-circuit; a
value not dropped on `set`, `break` or the end of a block; a variable read without
copying; record fields stored in the order written; a helper's failure ignored;
swapped operands; and a field read that moves the record out of its variable. A
Python script runs every clinic case through the shared library on both engines.

### Executables

    staalc build clinic.staal -o clinic
    ./clinic run --config run.json --journal run.journal

`staalc build` checks the program, compiles its native functions into an object
file with Cranelift for the baseline of this processor family (so the executable
runs on any x86-64 Linux machine, not only this one), and links it with the runtime
(`libstaalc.a`, built next to `staalc`) using `cc`. The executable carries the
program's source files, modules and packages included: it checks them again when it
starts, refuses to run if they are not exactly the program its code was made from,
and runs functions without native code on the interpreter. Its journals are the same
as `staalc`'s, so a run started by one can be resumed by the other. In a release
build an executable is about 23 MB (13 MB stripped) and needs only the system's C
library; the recursive Int program above runs in 0.015 s, against 0.018 s for
`staalc --engine native`, which compiles when the run starts.

How this is tested: every runtime case, run by its built executable, gives the same
exit code, output, log and journal as `staalc run`; a program with modules runs as
an executable; a run crashed at every journal point by the executable resumes under
`staalc`, and the other way round, with the same result and world; an executable
whose embedded program was changed by one character refuses to run; build errors are
reported. Five mutations are each caught: the program check skipped, the native code
table out of order, constants or record layouts lost, and running through the
in-memory compiler instead of the linked code.

What is not built: executables on macOS and Windows (`staalc build` refuses there
for now); an installable Python package; and calling C libraries from Staal, which is left
for last on purpose: the broker could not check what native code does.
