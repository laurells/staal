# Intent runtime semantics (v0.1)

This document defines what an Intent program does when it runs. The reference
interpreter (`src/interp/`) implements it directly; any future backend
(native code via Cranelift) must produce the same results, model requests,
actions and log for every program. Where this document and an implementation
disagree, the implementation is wrong.

A program is only run after it passes every static check (E001-E008, E1xx).
The runtime relies on those checks and also re-checks the properties that
matter for safety, so a bug in one layer is caught by the other.

## 1. Running a program

A run is described by a **run config** (JSON):

```json
{
  "entry": "handle_refund",
  "inputs": { "message": "I was charged twice", "order": { ... } },
  "grants": { "pay": { "cap": "payments.refund", "max": { "amount": "500.00", "currency": "USD" } } },
  "model":  { "provider": "script", "responses": [ ... ] },
  "adapters": { "simulated": { "fail": [ ... ] } },
  "limits": { "max_steps": 1000000, "max_depth": 256, "default_max_tokens": 1024 }
}
```

- `entry` names the function to run.
- Every non-capability parameter of the entry function must appear in
  `inputs`, and its JSON must match the declared type exactly (section 3).
  Unknown keys are an error.
- Every capability parameter must appear in `grants`. Grants are how the
  person deploying the program hands out authority; nothing else can create a
  capability.
- An entry parameter may not have type `Proposed<T>`, `Receipt`, `Outcome` or
  `Done`: those can only be produced by the runtime itself.
- Any problem with the config is reported before execution starts, and nothing
  runs.

The result is either the entry function's return value or a failure
(section 8), plus the execution log (section 10).

## 2. Values

| Type | Runtime value |
| --- | --- |
| `Text` | a UTF-8 string |
| `Int` | a signed 64-bit integer |
| `Bool` | true or false |
| `Money` | an exact amount in minor units (e.g. cents) and a currency code |
| declared record type | its fields, in declaration order |
| `Proposed<T>` | raw model output (JSON), not yet checked against T |
| `Receipt` | proof of one completed action: id, capability, method |
| `Outcome` | `ok`, `error`, and (hidden) what succeeded |
| `Done` | a list of evidence values |
| `cap ...` | a handle to a capability held by the broker (section 6) |
| `Unit` | the result of a function that returns nothing |

No value can be created out of thin air: `Receipt` only by an action, `Done`
only by `done(...)`, `Outcome` only by `try`, capabilities only by grants.

### Money

Money is exact; there is no floating point anywhere in the runtime.

- In JSON, Money is `{ "amount": "12.50", "currency": "USD" }`. The amount is a
  decimal **string** with at most as many decimal places as the currency
  allows. JSON numbers are rejected for money, to avoid float rounding.
- Supported currencies and their minor-unit exponents: USD 2, EUR 2, GBP 2,
  NGN 2, GHS 2, KES 2, ZAR 2, CAD 2, AUD 2, JPY 0. Any other code is an error.
- Negative amounts are rejected.
- Comparisons between Money values in different currencies never fail and are
  never true for ordering: `<`, `<=`, `>`, `>=` are all false, `==` is false
  and `!=` is true. So `refund.amount <= order.total` is false when the
  currencies differ, and the refund does not happen.
- Capability limits written in source (`max: 500 USD`) are in **major** units:
  `500 USD` is 50000 minor units. A limit with no currency cannot be enforced
  against money and is rejected when the program is run.

### Int literals

Integer literals must fit in a signed 64-bit integer; larger literals are
rejected by the compiler (E101).

## 3. Inputs and validation

Values arriving from outside (run inputs, model output) are checked against a
declared type by the same strict rules:

- `Text` must be a JSON string; `Int` a JSON integer in range; `Bool` a JSON
  boolean; `Money` the object form above.
- A record must be a JSON object with **exactly** its declared fields: a
  missing field or an unknown extra field is an error.
- A field of type `Proposed<T>` keeps the raw JSON without checking it.
- `Receipt`, `Outcome`, `Done` and capabilities are never accepted from
  outside, at any depth. In particular a model cannot forge a receipt: a
  `validate` whose type contains one always fails (and the compiler rejects
  such types for `propose`, E101).

`validate x` applies these rules to the raw output in `x`. On success it
yields a value of type T. On failure it is a failure of kind `validation`
(section 8). Validation checks shape only; the result is still untrusted.

## 4. Statements and expressions

Evaluation is strictly left to right and statement by statement.

- `let` binds a name for the rest of its block. A block's names disappear at
  its end.
- `if` evaluates its condition, then exactly one branch.
- `repeat N { }` runs its body N times unless `break` or `return` leaves it.
- `loop { }` runs its body until `break` or `return`.
- `break` leaves the innermost loop. `return` leaves the function.
- A function call evaluates its arguments left to right, then runs the callee
  with a fresh scope containing only its parameters.
- Field access reads a field of a record or of an `Outcome` (`ok`, `error`).
- Comparisons on `Text`, `Int`, `Bool` compare values; `Text` ordering is not
  allowed by the compiler. Money is described above.

### Limits that keep a run finite

Two deployment limits apply to every run; exceeding either is a failure
(`step_limit`, `depth_limit`):

- `max_steps` (default 1,000,000) counts statements executed.
- `max_depth` (default 256) bounds nested function calls.

These limits are safeguards, like a timeout; they are not part of a program's
meaning, and whether they trigger can depend on data (see the timing note in
the README).

## 5. Model calls

`propose(T, instruct: I, data: [D...], tokens: N)`:

1. **Stateless.** Every model call is a separate request with no conversation
   history, no shared session and nothing carried over from earlier calls.
   This is a hard requirement: the compiler checks each call's inputs against
   that call's rules only.
2. **Request.** The request contains exactly: the instruction text `I` (or a
   fixed default when there is none), the data values `D...` encoded as JSON,
   and a JSON description of type T. Data is placed in a delimited section
   marked as data, never in the instruction position. Capabilities are never
   encoded (the compiler forbids it; the runtime refuses if one appears).
3. **Token limit.** The runtime asks the provider to count the request's
   input tokens. With `tokens: N`, if input >= N the call is refused
   (`budget`) without being sent; otherwise the output limit is set to
   N - input, so input + output can never exceed N. Without `tokens:`, the
   output limit is `default_max_tokens`.
4. **Budget.** Before sending, every enclosing budget (section 7) must have at
   least one call left and, if it limits tokens, at least the call's
   worst-case tokens left; otherwise the call is refused (`budget`) without
   being sent. After the reply, the actual input + output tokens are charged.
   A provider reply that exceeds the output limit is a failure (`provider`),
   and its tokens are still charged.
5. **Result.** The reply's raw output becomes a `Proposed<T>`. Provider errors
   (network, refusal, malformed reply) are failures of kind `provider`.

## 6. Capabilities and actions

The **capability broker** holds every capability. A capability has a path
(`payments.refund`), an optional limit, an amount spent, and optionally a
parent.

- **Grants** create root capabilities. If the entry parameter declares a
  limit, the granted capability is narrowed to it.
- **Narrowing** happens whenever a capability flows into a slot that declares a
  smaller limit (a call parameter or a typed `let`): the broker creates a
  child capability with that limit. A child's spending also counts against
  every ancestor.
- **Limits are cumulative.** `max: 500 USD` means at most 500 USD in total
  through that capability (and its children) over the run, not 500 per action.
- An action `c.m(args)` asks the adapter for the action's amount. If the
  capability or any ancestor has a limit, the amount must be a Money value in
  the limit's currency and must fit within every limit in the chain;
  otherwise the action is refused (`capability`) and the adapter is never
  called. If no amount can be determined for a limited capability, the action
  is refused.
- The adapter then performs the action. Success yields a `Receipt`; the amount
  is charged to the chain only on success. Adapter failure is a failure of
  kind `action`.

## 7. Budgets

A function declaring `budget(calls: C, tokens: T)` opens a **budget meter**
for each invocation. A model call is charged to every meter open on the call
stack at that moment (the function's own and its callers'). Meters close when
the function returns or fails.

## 8. Failures

A failure has a kind and a message:

| Kind | Cause |
| --- | --- |
| `action` | an adapter reported that an action failed |
| `capability` | an action was refused by the broker (limit, currency, unknown amount) |
| `validation` | `validate` rejected model output |
| `budget` | a model call would exceed a token limit or a budget |
| `provider` | the model provider failed or misbehaved |
| `step_limit`, `depth_limit` | a deployment limit was reached |

A failure stops the current function and every caller up to the nearest
enclosing `try`. `try e` evaluates `e`: on success it yields
`Outcome { ok: true, error: "" }` holding the result; on failure it yields
`Outcome { ok: false, error: "<kind>: <message>" }`. If no `try` catches it,
the run ends with that failure.

## 9. Evidence

`done(evidence: [...])` yields a `Done` holding its evidence values. When the
entry function returns a `Done`, its evidence (receipts, and the results held
by successful outcomes, recursively through nested `Done` values) is written
to the log as the run's evidence.

## 10. Execution log

Every run produces an ordered log of events (JSON lines): run start, each
model request exactly as sent and its reply and token counts, each validation
result, each action requested, refused, performed or failed (with amounts),
each budget refusal, each failure caught by `try`, and the run's result and
evidence. The log contains the data sent to models and so may contain
sensitive data; it must never be given to a model as context.
