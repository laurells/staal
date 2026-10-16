# intentc — the Intent compiler (v0.1, front end only)

This is phase 1 of the Intent build plan: a lexer, parser and type checker,
written in Rust with no external dependencies. It does not run programs yet.
It checks them and rejects the ones that break Intent's rules.

## What it enforces today

| Code | Meaning |
| ---- | ------- |
| E001 | A model's `Proposed<T>` reaches an action or a non-Proposed return without `validate` |
| E002 | An action is performed without a matching capability in scope |
| E003 | A capability is widened: passed, assigned or returned with more authority than it holds |
| E004 | A function's worst-case model use can exceed its declared `budget(...)` |
| E005 | A model call (direct or through a called function) is inside an unbounded `loop` |
| E006 | An untrusted value is used as a model instruction, or flows into an `@trusted` slot |
| E007 | A model call's input can reveal a path listed in its `blind_to` |
| E100 | Undefined name or function |
| E101 | Type mismatch or wrong number of arguments |
| E102 | Unknown type |
| E103 | `break` outside `repeat` or `loop` |
| E104 | Unknown field, or a field read on a value that has no fields |
| E900 | Lexer error |
| E901 | Syntax error |

E008 from the spec (evidence) is not implemented yet.
The language has no record literals (building a value of a declared type in
code), arithmetic, or parentheses yet.

## Capability limits

    fn small_refund(pay: cap payments.refund(max: 50 USD), r: Refund) { ... }

A capability may be narrowed (500 USD -> 50 USD, unlimited -> 50 USD) and never
widened (50 USD -> 500 USD, 50 USD -> unlimited). Limits in different units are
never converted. Only `max` is supported, and limits are whole numbers.

The checker controls how much authority code is handed. It does not yet stop a
single action from exceeding the limit at runtime (e.g. refunding 600 through a
500 USD capability); that check belongs to the runtime capability broker.

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

## Fields

    type Customer { name: Text, email: Text }
    type Order { id: Text, total: Money, customer: Customer }

    if refund.amount <= order.total { mail.send(order.customer.email) }

Fields are read with `.`, to any depth. Reading a field of an unvalidated
`Proposed<T>` is E001: validate first, then read (`let r = validate draft`,
then `r.amount`). A field is exactly as trusted as the value it came from.
Type fields cannot be capabilities, so authority cannot travel inside data
sent to a model. `a.b.c(...)` is a syntax error: actions are called directly
on a capability variable. Because there are no parentheses yet,
`(validate draft).amount` cannot be written; bind it with `let` first.

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

Two leaks it does not catch, and why:

- **Other functions.** `blind_to` protects one model call. If you pass `order`
  to a helper that makes its own model call, that call has its own inputs and
  its own `blind_to`. A function-level guarantee (a parameter marked as blind
  for everything downstream) is not built yet.
- **Branches.** In `if order.customer.email == x { let a = "yes" }`, sending
  `a` reveals a fact about the email through which branch ran. The checker
  tracks values, not control flow, so it does not see this. Sending the
  comparison result itself *is* caught.

## Requirements

Rust, installed from https://rustup.rs. Developed with cargo 1.97.0.

## Use

    cargo build
    ./target/debug/intentc check tests/fixtures/pass/refund.int
    cargo test

`intentc check` exits 0 when the program is valid and 1 when it has errors.

## Tests

`tests/fixtures/pass/` holds programs that must compile.
`tests/fixtures/fail/` holds programs that must be rejected. Each starts with
`// expect: CODE`, and the test fails unless the compiler reports exactly that code.
To add a rule, write the failing program first, then make the checker reject it.

## Layout

    src/lexer.rs    source text -> tokens
    src/parser.rs   tokens -> AST (grammar is in the file header)
    src/ast.rs      AST and types
    src/checker.rs  type and safety rules
    src/main.rs     command-line interface
