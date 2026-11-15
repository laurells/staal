# Models

A program asks an AI model for a value of a declared type with `propose`, and uses the
answer only after `validate` has checked it against that type. The compiler keeps
untrusted text out of the model's instructions, keeps marked data away from the model,
and bounds the number of model calls and tokens.

## Asking for a value

```staal file=tags.staal
type Tag { name: Text, score: Int }

fn tag(ticket: Text) -> List<Tag, 3> budget(calls: 1, tokens: 800) {
    let tags: List<Tag, 3> = validate propose(List<Tag, 3>,
        instruct: "Give up to three tags for this support ticket, each with a score from 1 to 10.",
        data: [ticket],
        tokens: 800)
    return tags
}
```

`propose(T, ...)` sends one request and gives the model's raw answer as a
`Proposed<T>`. A `Proposed<T>` cannot be used as a `T`: its fields cannot be read, it
cannot be compared or passed to an action, until `validate` checks it (E001).

| Option | Contents |
| --- | --- |
| `instruct:` | The instruction. Only trusted values: literals, constants, and parameters or results marked `@trusted` (E006). |
| `data:` | A list of values the model reads, encoded as JSON and placed in a section of the request marked as data. Any value except a capability or a secret. |
| `tokens:` | The most this call may use, input and output together. Without it, the output limit is the run's `default_max_tokens` and the compiler counts the call's tokens as unbounded. |
| `blind_to:` | Paths of data that no input of this call may reveal. |

Every call is a separate request with no conversation history.

## What a model may be asked for

The request describes `T` as a JSON schema. `T` may be `Text`, `Int`, `Bool`, `Money`,
a record, an enum, a list, an `Optional<T>`, or a combination of these. A model is never
asked for a map, a set or a tuple (E101): ask for a list of records. A type that contains
`Receipt`, `Outcome`, `Done`, a capability or a secret cannot be proposed.

An enum is asked for as an object whose `kind` names the variant:
`{"kind": "Refund", "cents": 3999, "reason": "..."}`. The model can only choose one of
the declared variants.

## Validation

`validate` checks the answer strictly: a record must have exactly its declared fields,
a list at most its maximum length, money the form `{"amount": "12.50", "currency":
"USD"}`, an enum a declared variant with exactly its fields, and an optional field must
be present (as `null` for `none`). A value that does not fit fails the run with kind
`validation`.

```json file=extra.json
{
  "entry": "tag",
  "inputs": { "ticket": "My card was charged twice." },
  "model": { "provider": "script", "responses": [{ "output": [{ "name": "billing", "score": 9, "urgent": true }] }] }
}
```

```console
$ staalc run tags.staal --config extra.json
staalc: run failed: validation: model output[0]: Tag has no field 'urgent'
```

```json file=many.json
{
  "entry": "tag",
  "inputs": { "ticket": "My card was charged twice." },
  "model": { "provider": "script", "responses": [{ "output": [
    { "name": "a", "score": 1 }, { "name": "b", "score": 1 }, { "name": "c", "score": 1 }, { "name": "d", "score": 1 }
  ] }] }
}
```

```console
$ staalc run tags.staal --config many.json
staalc: run failed: validation: model output: at most 3 items allowed, found 4
```

Validation checks shape only. A validated value is still untrusted: it cannot be an
instruction to another model call, and the program decides what to do with it.

`try` applies to a function call or an action, not to `validate` itself. To continue
after an answer that does not fit, validate in a function and call it with `try`:

```staal file=fallback.staal
type R { text: Text }

fn checked(p: Proposed<R>) -> R {
    return validate p
}

fn answer(q: Text) -> Text budget(calls: 1, tokens: 100) {
    let p = propose(R, instruct: "Answer the question.", data: [q], tokens: 100)
    let r = try checked(p)
    if r.ok {
        return "valid"
    }
    return "invalid: " + r.error
}
```

```json file=fallback.json
{
  "entry": "answer",
  "inputs": { "q": "What is the refund policy?" },
  "model": { "provider": "script", "responses": [{ "output": { "nope": 1 } }] }
}
```

```console
$ staalc run fallback.staal --config fallback.json
"invalid: validation: model output: R has no field 'nope'"
```

## Trust

`instruct:` takes only trusted values. Literals and constants are trusted. Parameters,
function results, model output, responses from services and console input are
untrusted. A parameter marked `name: Text @trusted` is trusted, and every caller must
pass a trusted value to it; a function marked `-> Text @trusted` must return only
trusted values. Interpolation and `+` give a trusted text only if every part is
trusted. Model output is never trusted, even after `validate`.

```staal
const POLICY = "Reply politely to the customer's message in the data, in one paragraph."

type Reply { text: Text }

fn reply(message: Text) -> Text budget(calls: 1, tokens: 1000) {
    let r: Reply = validate propose(Reply, instruct: POLICY, data: [message], tokens: 1000)
    return r.text
}
```

Placing `message` in `instruct:` is E006. What this guarantees: untrusted text never
occupies the instruction position. What it does not guarantee: that a model ignores
instructions written inside `data`. That is why the program validates the answer and
applies its own rules to it.

## Keeping data from a model

`blind_to:` on a call, and `@blind(...)` on a parameter, name data that must not reach
a model.

```staal
type Customer { name: Text, email: Text }
type Order { id: Text, total: Money, customer: Customer }
type Summary { text: Text }

fn summarise(order: Order @blind(customer.email)) -> Text budget(calls: 1, tokens: 500) {
    let s: Summary = validate propose(Summary,
        instruct: "Summarise this order in one sentence.",
        data: [order.id, order.total, order.customer.name],
        tokens: 500)
    return s.text
}
```

`@blind(customer.email)` promises that `order.customer.email` reaches no model in this
function or anything it calls. Passing `order` or `order.customer` as data would reveal
it (E007); sibling fields can be sent. Passing blinded data to another function requires
that function's parameter to make the same promise. `@blind` alone blinds the whole
value.

The rules (E007):

- **Paths.** Paths overlap when one contains the other: blinding `order.customer`
  blinds every field inside it, and sending `order` reveals `order.customer.email`.
- **Values.** The check follows `let` aliases. Anything computed reveals all of its
  inputs: function results, action results, comparisons, and model output (a model that
  saw a value may repeat it). `blind_to:` entries must name data directly, not computed
  values.
- **Branches.** Code inside `if order.customer.email == x { ... }` reveals the email:
  every value bound there, and the decision to call a model there. The same holds for
  code after an `if` whose branch can `return` or `break`, and for a whole loop body when
  a `break` depends on blinded data. A branch that always returns does not affect later
  code.
- **Partial operations.** An action or a call to a function that can fail, not under
  `try`, and any call to a function that may not finish (it contains `loop` or
  recursion), is partial: code after it runs only for some values of its arguments, so
  it reveals them. Pure helpers, with no actions, `loop` or recursion, are not partial;
  `repeat N` is allowed.

```text
mail.send(order.customer.email)            // stops here if it fails...
let s = propose(...)                       // ...so this call reveals success: E007

let sent = try mail.send(order.customer.email)
let s = propose(...)                       // runs either way: allowed
if sent.ok { let t = propose(...) }        // depends on success: E007
```

The analysis is conservative and refuses some safe programs. Use `try` when code must
continue past an action on blinded data.

What blinding covers and does not:

- Whether a model call happens, how many, their order and their inputs, and so their
  token use, are fixed by data that is not blinded.
- Wall-clock timing is not covered. Slow work done only for some blinded values shifts
  when a model call starts; a provider with precise timestamps could infer which branch
  ran. The model itself has no clock.
- Blinding keeps data from models only. Sending `order.customer.email` through an email
  capability is allowed.
- Every model call starts from a fresh context; no conversation is reused across calls.

## Budgets

`budget(calls: C, tokens: T)` on a function declares its worst-case model use: at most
`C` calls and `T` tokens, counting every call it makes directly or through the functions
it calls. Either part may be left out. The compiler computes the worst case and refuses
a function that could exceed it (E004):

- a call with `tokens: N` costs one call and `N` tokens; without `tokens:`, its tokens
  are unbounded;
- `repeat N` multiplies by `N`; nested loops multiply;
- an `if` costs its condition and its more expensive branch;
- a called function with a budget costs its budget; one without costs what its body
  can use;
- recursion that reaches a model call, and a model call inside `loop`, are unbounded
  (E005 for `loop`).

At run time, before each call, the runtime asks the provider to count the request's
input tokens. If the input alone reaches the call's `tokens:`, the call fails with kind
`budget` without being sent; otherwise the output is limited so input and output stay
within it. Each enclosing budget must have a call left, and enough tokens for the call's
worst case. After the reply, the tokens the provider reports are charged. A reply that
exceeds its limit fails with kind `provider`.

## Providers

The run config's `model` names the provider: `"anthropic"` (Claude) or `"script"`
(fixed replies, for tests and examples). Claude is the only model provider. See
[Run config](../reference/run-config.md) for the keys.

```json
{ "provider": "anthropic", "model": "claude-haiku-5-5", "api_key": { "env": "ANTHROPIC_API_KEY" } }
```

Without `model`, a model call fails before anything is sent:

```json file=nomodel.json
{ "entry": "tag", "inputs": { "ticket": "My card was charged twice." } }
```

```console
$ staalc run tags.staal --config nomodel.json
staalc: run failed: provider: this run names no model: give one under "model" in its run config (RUNTIME.md section 5)
```

| Failure kind | When |
| --- | --- |
| `validation` | The answer does not fit the type. |
| `budget` | A call would exceed its `tokens:` or an enclosing budget. Nothing is sent. |
| `provider` | A definite error: no model configured, a refused key, a malformed request, a refusal, output cut off at the token limit. |
| `in_doubt` | The provider stayed unavailable after its retries. The run stops (exit code 3); `staalc resume` makes the call again. |

With `--journal`, every model call is recorded: a resumed run takes each recorded answer
from the journal instead of asking again. See [Journals and resuming](durability.md).
