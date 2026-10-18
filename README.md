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
| E007 | Blinded data can reach a model: through a call's inputs, through the decision to make the call, or through a function that does not promise to keep it blinded |
| E008 | `done(evidence: [...])` cites something that is not proof of this function's own work |
| E100 | Undefined name or function |
| E101 | Type mismatch or wrong number of arguments |
| E102 | Unknown type |
| E103 | `break` outside `repeat` or `loop` |
| E104 | Unknown field, or a field read on a value that has no fields |
| E105 | A function with a return type can finish without returning |
| E900 | Lexer error |
| E901 | Syntax error |

All eight safety checks from the spec (E001-E008) are implemented. Phase 1's
gate is met: every one is rejected by tests, and 20 valid programs compile,
including the two benchmark workflows (`workflow_*.int`).

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
data; it has no clock, and Intent gives programs none. The **provider** (or
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
