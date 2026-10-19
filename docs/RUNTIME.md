# Staal runtime semantics (v0.1)

This document defines what a Staal program does when it runs. The reference
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
  "secrets": { "key": { "env": "API_KEY" } },
  "adapters": { "simulated": { "fail": [ ... ] } },
  "net": { "timeout_ms": 30000 },
  "limits": { "max_steps": 1000000, "max_depth": 256, "default_max_tokens": 1024 }
}
```

- `entry` names the function to run, in the program file. A `pub` function of
  an imported module can be named with its module path, e.g. `"sms.termii.send"`.
- Every non-capability parameter of the entry function must appear in
  `inputs`, and its JSON must match the declared type exactly (section 3).
  Unknown keys are an error.
- Every `Secret` parameter must appear in `secrets`, which names the environment
  variable to read it from; `net` and `fs` configure the runtime's own
  capabilities (both in section 12).
- `model` names the model provider: `"anthropic"` for Claude, or `"script"`
  for fixed replies in tests (section 5).
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

**Constants.** `const NAME: T = value` declares a constant at the top of a file
(the type may be left out). Its value may use literals, other constants, and
arithmetic, comparison, `and`/`or`/`not`, records, lists, field reads, indexing
and the built-ins other than `header` and `fill_secret`; anything else is E101,
as is a cycle between constants. The compiler computes every constant with the
same operations the engines use, and a failure (overflow, division by zero, an
index out of range) is E101. Before checking, every use of a constant is
replaced by its value's expression, so at run time a constant is exactly the
literal it stands for: trusted, revealing nothing, and the same on both engines.
`pub const` exports it to other modules, which use it as `module.NAME` or import
it by name (E107 without `pub`). A local variable, parameter, loop variable or
capability need cannot have the name of a constant in scope (E101).


| Type | Runtime value |
| --- | --- |
| `Text` | a UTF-8 string |
| `Int` | a signed 64-bit integer |
| `Bool` | true or false |
| `Money` | an exact amount in minor units (e.g. cents) and a currency code |
| declared record type | its fields, in declaration order |
| `List<T, N>` | at most N values of type T, in order |
| `Proposed<T>` | raw model output (JSON), not yet checked against T |
| `Receipt` | proof of one completed action: id, capability, method |
| `Outcome` | `ok`, `error`, and (hidden) what succeeded |
| `Done` | a list of evidence values |
| `cap ...` | a handle to a capability held by the broker (section 6) |
| `Unit` | the result of a function that returns nothing |

**Text literals** are written in double quotes on one line. A backslash starts
an escape: `\"` a quote, `\\` a backslash, `\n` a new line, `\t` a tab, `\r` a
carriage return, and `\u{...}` the Unicode character with that hexadecimal code
(1 to 6 digits, e.g. `\u{e9}` for é; surrogates are refused). Any other escape
is an error (E900), never a backslash kept as text. Braces are ordinary
characters. `len` counts characters, not bytes.

**Multi-line text** is written between `"""` quotes. The opening quotes end
their line; the closing quotes begin a line, after only spaces or tabs, and that
indentation is removed from every line of the text (a non-blank line
indented less is an error). Line breaks between the lines are kept, the one
before the closing quotes is not, and `\r\n` line endings count as `\n`. Code
may follow the closing quotes on their line. Escapes and interpolation work as
in one-line text.

**Interpolation.** `\(expr)` inside text is replaced by the value of `expr`,
which must be `Text`, `Int`, `Bool` or `Money` (`12.50 NGN`); the expression ends
at the matching `)` on the same line. `"a \(x) b"` is exactly
`"a " + to_text(x) + " b"`, so the result is trusted only if every part is, and
blinded data stays blinded (E006, E007). `to_text(x)` is available directly. A
`Secret` can never be interpolated, and a record or list needs `json(x)`.

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
- Money literals in source (`12.50 USD`, `0 NGN`, `1000 JPY`) are checked by
  the compiler with the same rules as money from outside: a supported
  currency, at most its number of decimal places, and an amount that fits.
  Their value is exact.
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
- A `List<T, N>` must be a JSON array of at most N items, each valid as T. A
  model's JSON schema for it carries `maxItems: N`, but the bound is enforced
  here whatever the model returns.
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
- `set x = e` evaluates `e` and replaces the value of `x`: the innermost
  variable named `x` that a `let` declared. An inner `let x` declares a
  different variable, which disappears at the end of its block. Parameters,
  loop variables, capabilities, `Receipt`, `Outcome` and `Done` values cannot
  be reassigned (compiler, E101). If `e` fails, `x` keeps its old value.
- `if` evaluates its condition, then exactly one branch.
- `repeat N { }` runs its body N times unless `break` or `return` leaves it.
- `loop { }` runs its body until `break` or `return`.
- `for x in xs { }` runs its body once per element of the list `xs`, in order,
  with `x` bound to that element. The list is evaluated once, before the first
  iteration. Because every list has a declared maximum length, a `for` loop is
  bounded, and the compiler costs it as at most N iterations.
- `break` leaves the innermost loop; `continue` goes on to its next iteration
  (for `repeat` and `for`, the next count or element). Each iteration costs one
  step either way. `return` leaves the function.
- A function call evaluates its arguments left to right, then runs the callee
  with a fresh scope containing only its parameters.
- Field access reads a field of a record or of an `Outcome` (`ok`, `error`).
- Comparisons on `Text`, `Int`, `Bool` compare values; `Text` ordering is not
  allowed by the compiler. Money is described above. `==` on lists and records
  compares every element or field.
- `and` and `or` evaluate their right side only when the left side does not
  decide the answer. `not` negates a Bool.
- A record literal `T { field: value, ... }` must give every field of T exactly
  once, in any order (checked by the compiler). Fields are evaluated in the
  order written.
- A list literal `[a, b, c]` evaluates its elements left to right. Its maximum
  is its length, so it fits any `List<T, N>` with N at least that.
- `xs[i]` is the element at position i, counting from 0.
- Built-in functions: `len(text)` (characters, not bytes), `len(list)`,
  `contains(text, part)`, `contains(list, value)`, `starts_with(text, prefix)`,
  `ends_with(text, suffix)`. Their names are reserved.

### Arithmetic

| Operands | Operators | Result |
| --- | --- | --- |
| Int, Int | `+ - * /` | Int; `/` rounds toward zero |
| Money, Money (same currency) | `+ -` | Money |
| Money, Int (either order) | `*` | Money |
| Text, Text | `+` | Text (joined) |
| Int | unary `-` | Int |

Anything else (Money / Int, Money + Int, ...) is rejected by the compiler.
Arithmetic never wraps, rounds money, or converts currencies. These are
failures of kind `value`, which `try` around a call can catch:

- an Int result outside the signed 64-bit range, including `-(minimum Int)`;
- division by zero;
- Money in two different currencies added or subtracted;
- a negative Money result (`5 USD - 10 USD`, or Money times a negative Int);
- Text longer than 16 MiB;
- an index that is negative or not less than the list's length.

### Limits that keep a run finite

Two deployment limits apply to every run; exceeding either is a failure
(`step_limit`, `depth_limit`):

- `max_steps` (default 1,000,000) counts statements executed and loop iterations.
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
5. **Result.** The reply's raw output becomes a `Proposed<T>`. A definite
   provider error (a refused key, a malformed request, a refusal, output cut off
   at the token limit, a reply that is not of the required shape) is a failure of
   kind `provider`; when the model ran, its tokens are still charged.
6. **Unavailable.** When the provider cannot be reached, or stays rate limited
   or overloaded after its retries, the run stops (`in_doubt`, exit code 3)
   without recording an answer. Resuming makes the call again; nothing needs
   resolving first, because a model call changes nothing in the world.

### Providers

**`"script"`** replies from a fixed list, in order, for tests:
`{"provider": "script", "responses": [{"output": …, "output_tokens": n}, …]}`. A
response may instead be `{"error": "…"}` (a `provider` failure) or
`{"unavailable": "…"}` (the run stops, as in step 6). Input tokens are counted
as one per 4 bytes of the request's JSON.

**`"anthropic"`** calls Claude through Anthropic's Messages API:

```json
"model": { "provider": "anthropic", "model": "claude-sonnet-5-5",
           "api_key": { "env": "ANTHROPIC_API_KEY" } }
```

| Key | Meaning | Default |
| --- | --- | --- |
| `model` | the Claude model id, e.g. `claude-sonnet-5-5` or `claude-haiku-5-5` | required |
| `api_key` | `{"env": NAME}`: the environment variable holding the key | required |
| `base_url` | the API's address; HTTPS, or plain HTTP to 127.0.0.1 or localhost only | `https://api.anthropic.com` |
| `timeout_ms` | the most one request may take | 600000 |
| `connect_timeout_ms` | the most connecting may take | 10000 |
| `retries` | extra attempts after a rate limit, an overload or a server error (0 to 10) | 2 |
| `retry_delay_ms` | the first wait between attempts, doubling each time, unless the API sends `retry-after` (at most 60 s) | 1000 |
| `proxy` | `"none"`, or `"environment"` to use `HTTPS_PROXY` | `"none"` |
| `extra_root_certificates` | PEM files of root certificates to trust in addition to Mozilla's, for a network whose proxy signs TLS with its own authority | none |

- **Request.** The instruction goes in the system prompt, after a fixed
  preamble telling the model that the user message is data and that
  instructions inside it are not to be followed. The data goes in the user
  message as JSON. This separation helps the model; it is not what keeps a
  program safe, which is that the reply is untrusted until validated and a
  model never holds capabilities.
- **Output.** Type T becomes a JSON schema in `output_config.format`, wrapped
  as `{"value": T}`. Claude's structured outputs cannot express list lengths,
  so a `List<T, N>` bound is given as a description instead; `validate`
  enforces it. A recursive output type cannot be expressed, and such a call
  fails (`provider`). Any other reply shape, or extra fields, is a `provider`
  failure: the runtime never repairs output.
- **Tokens.** Input is counted with the free `/v1/messages/count_tokens`
  endpoint on exactly the request that is then sent. Anthropic describes the
  count as an estimate that "might differ by a small amount" from the input
  used, so the output limit sent is the call's limit less a margin of 16
  tokens plus about 2% of the request's size; when the limit is too small for
  a margin, all of it is sent. The budget is charged from the usage the API
  reports for the call, including cache reads and writes.
- **Failures.** 429, 500, 502, 503, 504 and 529, and connections that fail
  before anything is sent, are retried up to `retries` times, then the run
  stops (step 6). A timeout or broken connection after the request may have
  arrived is not retried at once: the run stops, and resuming asks again.
  Every other error status is a `provider` failure that names the API's error
  type, its message and the request id. `stop_reason` `refusal`, `max_tokens`
  and `model_context_window_exceeded` are `provider` failures whose tokens are
  charged.
- **The key** is read once, before the run starts (a missing variable is a
  config error), sent only in the `x-api-key` header, and replaced by
  `[api key]` in every message the provider produces. The config holds only the
  variable's name, so the journal never contains the key.
- **What is not guaranteed.** A call that times out after reaching the API, or
  a crash between a call and its recorded answer, can make the model run twice;
  the budget is charged only for the answer the program uses, so Anthropic's
  bill can be higher than the budget shows by that one call. Model output is
  not deterministic: a call made again can answer differently.

## 6. Capabilities and actions

The **capability broker** holds every capability. A program only ever holds a
handle. A capability has a path (`building.lock.open`), a set of limits, what
has been used of them so far, and optionally a parent.

### Limits

Every limit is optional, and all limits declared on a capability apply
together. They are written on the type, `cap path(key: value, ...)`, or on the
grant in the run config.

| Limit | Meaning | Example |
|---|---|---|
| `max` | the most money spent through the capability in total over the run, charged when an action succeeds | `cap payments.refund(max: 500 USD)` |
| `each` | the most money one action may spend | `cap payments.refund(each: 50 USD)` |
| `uses` | how many actions may be attempted in total | `cap building.lock.open(uses: 2)` |
| `only` | the targets actions may act on, as patterns | `cap twilio.message.send(only: ["+234*"])` |

- **Patterns.** A pattern is either exact text, or text with exactly one `*`,
  which matches any run of characters, including none. Examples:
  `"north-door"`, `"*@teamozy.org"`, `"/reports/*"`, `"*"`. An empty pattern,
  or one with more than one `*`, is an error (E901 in source, a config error
  in a grant).
- **Money limits.** `max` and `each` on one capability must be in the same
  currency (E101).
- **`uses` counts attempts, not successes.** A use is counted when the broker
  approves the action, before the service is called. A failed attempt can
  still cost money or reach someone, so it uses up the allowance like any
  other.
- **Limits fail closed.** A limit that needs to know something about an action
  refuses the action when the adapter cannot say. A money limit needs the
  amount; `only` needs the target.
- **Targets are matched as text** (adapter requirement). `only` compares the
  target exactly as the adapter reports it, with no normalisation:
  `"/contracts/../payroll"` matches `"/contracts/*"`. An adapter whose
  targets have several spellings (file paths, URLs, phone numbers with or
  without spaces) must report one canonical form, and refuse a target it
  cannot put in that form.

### Grants, narrowing and checking

- **Grants** create root capabilities. Grant keys are `cap` plus any of the
  limits above, for example
  `{"cap": "twilio.message.send", "uses": 1, "only": ["+234*"]}`.
  Money is written `{"amount": "10.00", "currency": "USD"}`. An unknown key is
  a config error. If the entry parameter declares limits, the granted
  capability is then narrowed to them.
- **Narrowing** happens whenever a capability flows into a slot that declares
  limits: a call parameter, a typed `let`, or a function's return. The broker
  creates a child capability with those limits. Everything the child does also
  counts against every ancestor, so a child can never do more than its
  ancestors allow, whatever its own limits say.
- **The compiler forbids widening (E003).** Flowing a capability into a type
  with a looser limit is an error. Looser means:
  - a higher `max`, `each` or `uses`;
  - dropping a limit;
  - an `only` list containing a pattern that is not inside some pattern of the
    original. Pattern `a*b` is inside `c*d` exactly when `c` is a prefix of `a`
    and `d` is a suffix of `b`. The check is conservative: a pattern covered
    only by several patterns together is refused.

  The runtime check through the ancestor chain is the second line of defence.
- **Performing an action.** For an action `c.m(args)`, the adapter reports
  the action's amount and target. For the simulated adapter these are the
  first Money and the first Text in the arguments, searched depth first. The
  broker then checks every limit of the capability and of every ancestor:
  - the amount must be in the limit's currency;
  - it must fit within `each`;
  - it must fit within what is left of `max`;
  - a use must remain;
  - the target must match some `only` pattern.

  If any check fails, the action is refused (`capability`) and the adapter is
  never called.
- **After approval,** the use is counted and the adapter performs the action.
  Success yields a `Receipt`, and the amount is charged to `max` through the
  chain only on success. An adapter failure is a failure of kind `action`.

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
| `capability` | an action was refused by the broker (a limit, the currency, or an amount or target it cannot determine) |
| `value` | arithmetic or indexing could not produce a value (section 4) |
| `validation` | `validate` rejected model output |
| `budget` | a model call would exceed a token limit or a budget |
| `provider` | the model provider failed or misbehaved |
| `step_limit`, `depth_limit` | a deployment limit was reached |
| `internal` | a state the compiler should have ruled out (a bug in staalc), reported instead of crashing |
| `journal`, `in_doubt` | durability failures (section 11) |

A failure stops the current function and every caller up to the nearest
enclosing `try`. `try e` evaluates `e`: on success it yields
`Outcome { ok: true, error: "" }` holding the result; on failure it yields
`Outcome { ok: false, error: "<kind>: <message>" }`. If no `try` catches it,
the run ends with that failure.

`step_limit`, `depth_limit`, `internal`, `journal` and `in_doubt` are **fatal**: `try` never catches
them, and they always end the run. The deployment limits are safeguards, not
part of the program's meaning, so a program must not be able to observe them
(a runaway loop or recursion must not end in a "successful" run). An
`internal` failure is a bug in staalc, which no program may hide.

## 9. Evidence

`done(evidence: [...])` yields a `Done` holding its evidence values. Each item
must lead to at least one receipt (directly, through a successful outcome, or
through a nested `Done`); the compiler guarantees this (E008) and the runtime
checks it again, failing with `internal` if it ever does not hold. When the
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

## 11. Durability: the journal, crashes and resume

A run started with a **journal** survives being killed at any moment (power
loss, out-of-memory kill, `kill -9`). Resuming it finishes the run so that:

- no action is performed twice, and no performed action is lost;
- no model call whose reply was recorded is sent again;
- the result, the log and the world end up as if the run had never stopped.

### How

Everything a run does is deterministic except what the outside world tells
it: token counts, model replies and action results. Each of these is an
**external operation**, numbered 1, 2, 3, ... in the order the run reaches
them. For each one the runtime appends, and flushes to disk (`fsync`) before
continuing:

1. an **intent** record: what is about to be asked of the world;
2. after the world answers, a **result** record.

Resuming re-executes the program from the start. At each external operation:

- if the journal has its result, that result is used and the world is not
  contacted;
- the intent the program produces now must be identical to the recorded one;
  if not, the program, config or runtime has changed and resume stops
  (`journal`);
- once the recorded operations are used up, the run continues live,
  appending to the same journal.

Resume requires the **identical** program text and run config: the journal's
first record holds the SHA-256 of each, and any difference is refused before
anything runs. For a program with modules, the program hash covers every file
it loads (each file's module path, length and text), so a change to any imported
module is refused too. File names are not part of it: the same program resumed
from another folder, or after its folder was moved, is the same program. A
single-file program's hash is the hash of its text. (Journals of multi-file
programs written before v0.17 recorded file names, and cannot be resumed by
v0.17.)

### The crash window, and idempotency

A crash after an intent but before its result leaves that operation **in
doubt**: it may or may not have happened.

- A token count or model call in doubt is simply performed again. This may
  cost one extra call at the provider; the program never saw the lost reply.
- An action in doubt must not be guessed. Every action is performed with an
  **idempotency key**, `<run id>-<operation number>`, unique to this run and
  this operation. If the adapter declares the action idempotent (the service
  deduplicates by key, as payment providers do), resume performs it again
  with the same key and receives the original receipt. Otherwise resume stops
  with a failure of kind `in_doubt`, naming the action, and does nothing
  until an operator checks the real world and records the answer with
  `staalc resolve`:
  - `--performed <receipt>`: it happened; the program continues with that receipt;
  - `--failed <reason>`: the program sees it as a failed action;
  - `--retry`: it did not happen; resume performs it. This authorizes **one**
    attempt, recorded before it is made: if that attempt is itself interrupted,
    the action is in doubt again and must be resolved again.

The run id is 128 bits from the operating system's random source, so keys
never collide with another run's. A predictable or reused key could make a
service return a *different* run's receipt.

### Failure kinds

| Kind | Cause |
| --- | --- |
| `journal` | the journal could not be written, or replay diverged from it |
| `in_doubt` | an action that is not idempotent may or may not have happened; or a model call or token count did not complete (resuming makes it again) |

Both are fatal, like `step_limit`: `try` never catches them.

### The journal file

JSON lines, append-only. Records:

```
{"j":"start","version":1,"run_id":"…","program_sha256":"…","config_sha256":"…"}
{"j":"intent","n":1,"op":"count","key":"…","body":{…}}
{"j":"result","n":1,"ok":…}            or  {"j":"result","n":1,"err":"…"}
{"j":"retry","n":3}                    an operator authorized one more attempt at action 3
{"j":"attempt","n":3}                  that attempt is about to be made
{"j":"end","ok":true,"result":…}       or  {"j":"end","ok":false,"failure":"…","message":"…"}
```

`op` is `count`, `model` or `action`. After an intent come only its result, or,
for an action, `retry`/`attempt` pairs and then its result. A crash can tear only the last line; a
torn last line is discarded on resume (it was never acknowledged). Any other
malformed line is corruption, and resume refuses. Resuming a journal that
already has an `end` record replays it without contacting the world and
reports the same result. Only one process may use a journal at a time: an
operating-system lock, released automatically if the process dies, enforces
this.

The journal holds the data sent to models and to actions, and so may contain
sensitive data. Like the log, it must never be given to a model as context.

### Contracts for providers and adapters

- An adapter's `amount` must be a pure function of the action's arguments.
- An adapter that declares an action idempotent must, given a key it has seen,
  not perform the action again and must return the original receipt.

## 12. The runtime's own capabilities: `net.http` and `fs`

Most capabilities are performed by an adapter and return a `Receipt`. Two families
belong to the runtime itself, which performs them, checks them, and records them in
the journal like any other action. Capability paths starting with `net.` or `fs.` are
reserved for them; any other path under those names is a compile error (E101).

| Capability | Action | Result |
| --- | --- | --- |
| `net.http.get`, `net.http.delete` | `c.get(url)`, `c.get(url, headers)` | `Response` |
| `net.http.post`, `net.http.put`, `net.http.patch` | `c.post(url, body)`, `c.post(url, body, headers)` | `Response` |
| `fs.read` | `c.read(path)` | `Text` |
| `fs.write` | `c.write(path, content)` | `Receipt` |

`url`, `path` and `content` are `Text`. A `body` is `Text` or a `Secret` (below).
`headers` is a `List<Header, N>`. `uses` and `only` apply as to any capability;
`max` and `each` do not (no money the runtime can measure is spent), and are
refused (E101, or a config error on a grant).

### Built-in types and functions

- `Response { status: Int, body: Text, complete: Bool }`, made only by the runtime.
  Any status is a response, not a failure: a 500 or a 404 is data for the program.
  `complete` is false when the body was larger than `net.max_response_bytes`, was
  cut off, or was not UTF-8; `body` is then empty. The request still happened.
- `Secret`: a value from the environment (below). It cannot become `Text`, be
  compared, be stored in a record, be sent to a model, be encoded by `json`, or be
  passed to any action other than as part of an HTTP request (E101). Its value is
  never logged or journaled.
- `Header`, made by `header(name: Text, value: Text or Secret)`. A header can hold a
  secret, so the rules for secrets apply to headers too.
- `fill_secret(template: Text, key: Secret) -> Secret`: the template with its
  `{secret}` placeholder replaced by the key, as a new Secret, for APIs that want
  `Bearer <key>` or the key inside a JSON body. The template must contain
  `{secret}` exactly once: the program writes its own placeholder, and a second one
  can only have come in with data (a message text saying `{secret}`), which would
  copy the key into the message. Such a call fails (`value`) and nothing is sent.
- `json(x) -> Text`: x encoded as JSON (records as objects, money as
  `{"amount", "currency"}`). Secrets, headers and capabilities cannot be encoded.
- `parse(T, text) -> Proposed<T>`: JSON text from outside, unchecked until
  `validate` (so E001 applies). Text that is not JSON fails `validation`. When
  validated, fields the type does not declare are dropped, since services add
  fields over time; every declared field is still required and checked exactly.
  (Model output stays strict: an unknown field fails validation.)
- Record field names may be keywords (`type`, `in`, `for`), as JSON APIs need.

### Secrets

An entry parameter of type `Secret` is supplied only from the environment:

```json
"secrets": { "key": { "env": "TERMII_API_KEY" } }
```

Giving it in `inputs` is a config error: secrets are never written into a run
config. The variable must be set, at least 8 characters long, and printable ASCII
without spaces, quotes or backslashes (so filling it into a JSON string or a
header can never change their structure). The journal and the log record a secret
by its variable name only. Every secret value is also removed from every response
body, file read and log event (replaced by `[secret removed]`), so a service that
echoes a key back cannot leak it into the program, the log or the journal.

### Canonical targets

`only` compares text, so every target is first put into one canonical form, and a
target that cannot be is refused (`capability`) before anything is sent.

- **URLs** are parsed by the WHATWG URL Standard, as browsers do: scheme and host
  lowercased, international hosts in ASCII, the default port dropped, and `.` and
  `..` segments resolved, including percent-encoded ones. Then: the scheme must be
  `https`, or `http` for `localhost`, `127.0.0.0/8` or `::1` only; user names and
  passwords in URLs are refused; an encoded `/` or `\` in the path is refused,
  since servers disagree on what it means; escapes of unreserved characters are
  decoded and other escapes uppercased; the fragment is dropped.
- **Paths** must be absolute; empty and `.` segments are removed and `..` resolved
  as text, never above `/`.
- **Patterns** in `only` for these capabilities must themselves be canonical
  (E101 in source, a config error in a grant), and in a URL pattern the `*` must
  come after `scheme://host/`, so it can never match another server.

### Files

Every path is opened one directory at a time, each relative to the one before, and
no symbolic link is ever followed, in any directory or as the file itself. A
directory swapped for a link while a program runs is therefore either refused or
irrelevant. Reads accept only regular files, opened without blocking (a FIFO
cannot hang the run), refuse a file with more than one hard link, and stop at
`fs.max_read_bytes`; the text must be UTF-8. Writes go to a new temporary file in
the same directory, are flushed, and replace the target with one atomic rename, then
the directory is flushed: a crash never leaves half a file, an existing file keeps
its permissions, and another hard link to it keeps its own contents. A write
returns a `Receipt` whose id is derived from the content's SHA-256.

Linux and macOS only; on other systems every file action fails.

### HTTP requests

No redirect is followed (a redirect could lead outside `only`; the program gets the
3xx response). The runtime owns these headers and refuses them from programs:
`Host`, `Content-Length`, `Transfer-Encoding`, `Connection`, `Upgrade`, `TE`,
`Trailer`, `Keep-Alive`, `Idempotency-Key` and any `Proxy-*`; header names must be
HTTP tokens and values contain no control characters. No proxy is used unless the
run config asks for one.

```json
"net": { "timeout_ms": 30000, "connect_timeout_ms": 10000,
         "max_response_bytes": 4194304, "proxy": "none" },
"fs":  { "max_read_bytes": 16777216 }
```

(These are the defaults. `proxy` is `"none"` or `"environment"`, which reads the
usual `HTTPS_PROXY` and `NO_PROXY` variables.)

Server certificates are verified against Mozilla's root certificates, the list
browsers use. An organisation whose services are signed by its own authority adds
it with `"net": { "extra_root_certificates": ["/etc/ourco/ca.pem"] }`: the
certificates in those PEM files are trusted in addition to Mozilla's, never
instead. A file with no certificate, or with a private key in it, is a config error.
There is no way to turn verification off. A certificate that is expired, not yet
valid, for another name, self-signed or from an authority not trusted is refused
during the handshake, before any request data is sent.

### What is known about a request, and crashes

| What happened | Example | Result |
| --- | --- | --- |
| The server answered | any status | a `Response` |
| Failed before anything was sent | host not found, connection refused, connect timeout, a certificate refused, no common TLS version | failure `action`, which `try` can catch |
| Failed after it may have reached the server | timeout, broken connection, or any other TLS error while sending or waiting | see below |

A request that may have reached the server is treated exactly like a crash in the
middle of an action. For `get`, which HTTP defines as safe, it is a failure
`action`, and after a crash it is retried. For `post`, `put`, `patch` and `delete`
the run stops `in_doubt` (the program cannot catch it, so it cannot retry and send
twice), unless the grant says the service deduplicates:

```json
"grants": { "pay": { "cap": "net.http.post", "idempotency_key": true } }
```

Then the runtime sends `Idempotency-Key: <run id>-<operation>` with the request,
the same key on every attempt, and such a request is retried after a crash or a
lost answer. Only the operator can say this, in the grant; a program cannot. If the
service does not really deduplicate, or a retry comes after it has forgotten the key
(Resend keeps keys for 24 hours), a request can be performed twice: this is the
operator's statement to make.

An HTTP request left in doubt is resolved with the response the server gave, which
the operator finds in the service's own records:

    staalc resolve --journal run.journal --status 200 --body '{"id": "..."}'

(`--failed` and `--retry` work as for other actions; `--performed` is refused for
HTTP requests, whose result is a response, not a receipt.) File reads and writes are
always safe to retry: reading changes nothing, and writing the same content again
leaves the same file. The journal records a write's content by its SHA-256, which a
resumed run must reproduce.

Journal files are created readable and writable by their owner only (0600), since
they hold what the run sent and received.

## 13. Capabilities declared by libraries

A library can declare a capability of its own, built on the runtime's: an SMS
service, a payment provider, a ticketing system. A program then holds a capability
for what it wants done (`cap termii.sms.send(only: [...], uses: 10)`), and never the
HTTP capability, the API key or the URL behind it.

    capability stripe.refunds {
        needs get: cap net.http.get
        needs post: cap net.http.post
        needs key: Secret
        needs base: Text

        canonical(id: Text) -> Text {
            return trim(id)
        }

        action create(payment_intent: Text @target, amount: Money @amount) -> Text {
            ...                       // requests through `get` and `post`
            return refunded.id        // the service's id, for the receipt
        }
    }

    fn refund(p: Text, a: Money, refunds: cap stripe.refunds.create(max: 200 USD)) -> Receipt {
        return refunds.create(p, a)
    }

### Declaring

`capability PATH { ... }` is a top-level declaration, in any file of the program.
Its body holds, in this order:

- `needs NAME: TYPE`, any number: what the operator must supply when granting it.
  A need is a runtime capability (`cap net.http.*`, `cap fs.*`, with limits if the
  library wants to declare them), a `Secret`, or a `Text` setting. Inside the
  capability, each need is a variable of that name.
- `canonical(t: Text) -> Text`, required when an action has a `@target`: puts a
  target into the one form `only` patterns are written in (a phone number without
  `+` or spaces, an id without surrounding space). It may not call models, `loop`
  or recurse. If it fails, the action is refused (`capability`).
- `action NAME(params) -> Text { ... }`, one or more. A program calls it as
  `c.NAME(args)` on a `cap PATH.NAME`, and gets a `Receipt`. The action returns the
  service's own id for it, which becomes the receipt's id.

Paths are global, not qualified by module: two declarations of one path are an
error, and paths under `net` and `fs` belong to the runtime (E101).

An action's own parameters are data only (no capabilities or secrets: those come
through `needs`). At most one is marked `@target` (Text) and at most one `@amount`
(Money). The compiler requires, for each action (E101):

- it makes at least one request through a capability it needs;
- the `@target` value, and the `@amount` value, reach some request (its URL,
  body or headers), so a program's `only` and money limits are checked against
  values the action really uses;
- it calls no model, and has no `loop` or recursion, so it finishes.

`fail(message)` ends an action (or any function) with an `action` failure carrying
the message, which a caller's `try` can catch. Library code also has `replace(t,
from, to)`, `lower(t)`, `trim(t)`, `form(record)` (a record of Text, Int and Bool
fields, form-encoded), `minor_units(money)` (an Int, cents for USD), `currency(money)`
(the code, `"USD"`) and `url_segment(t)` (the text percent-encoded as one URL path
segment; fails for `""`, `"."` and `".."`, which a URL treats as no segment or as
moving up a level).

### Granting

The grant names the capability as usual and supplies each need under `needs`:

    "refunds": { "cap": "stripe.refunds.create", "max": { "amount": "200.00", "currency": "USD" },
                 "needs": {
                     "get":  { "cap": "net.http.get",  "only": ["https://api.stripe.com/v1/payment_intents/*"] },
                     "post": { "cap": "net.http.post", "only": ["https://api.stripe.com/v1/refunds"],
                               "idempotency_key": true },
                     "key":  { "env": "STRIPE_SECRET_KEY" },
                     "base": "https://api.stripe.com" } }

A need that is a capability is a grant like any other (same keys and checks), and
the library's declared limits narrow it when the run starts. A `Secret` need is
`{"env": VARIABLE}`, with the rules of section 12. A `Text` need is a string. A
missing need, an unknown one, a value of the wrong kind, and `needs` on a capability
that is not a library's are config errors. Each granted inner capability is logged
(`grant`, with `"for"` naming the library capability).

A run cannot start inside a capability: an `entry` naming an action or `canonical`
is refused.

### Performing

`c.create(p, a)` on a library capability, in this order:

1. The `@target` argument is put in canonical form by the library's `canonical`.
2. The broker checks the amount against `max` and `each` and the canonical target
   against `only`, through the whole chain of narrowed capabilities, and counts a
   use. If any check fails the action is refused and the library's code never runs.
3. The action runs with its arguments and the needs. Its requests are checked,
   journaled, and may be in doubt, exactly as if the program had made them.
4. If it returns, it must have made at least one request that reached the world
   (was answered, or performed). Otherwise it fails with `action`: a receipt must
   stand for something that happened.
5. The amount is charged and the program gets `Receipt { id, capability, method }`.

If the action fails after a request that may have changed something (any request
but `net.http.get` or `fs.read`, unless it was certainly never sent), the amount
may have been spent, so it is charged anyway and the log's `action_failed` says
`"may_have_spent": true`. A failure can therefore never be used to spend past a
limit; the cost is that a refund refused by the service still counts against `max`.

`uses` on the library capability counts its actions; each inner capability has its
own `uses` and `only`, set by the operator and narrowed by the library.

### Crashes

The library action is not itself a journal entry; its requests are. A resumed run
runs the action's code again, and its requests replay from the journal in order, so
the action ends exactly as it first did. This depends on the action computing the
same requests from the same inputs and answers, which holds because library code has
no clock, randomness or model calls. A request that is in doubt stops the run as in
section 11, and is resolved the same way.

### What the runtime does not check

The runtime checks the limits it is given, against the values the library defines.
It cannot check that the library does what its name says. A library is code the
operator chooses to trust with keys and network access, as with any library in any
language:

- `canonical` decides what `only` compares. One that maps two different targets to
  the same form would let a program reach a target its `only` does not list.
- The `@target` check proves that the target influences a request, not that it is
  the request's recipient.
- The receipt proves that a request was made and that the library accepted the
  answer; whether the service did what was asked is the library's to check.

Read a library before granting it keys. A library used as a package (section 14)
is pinned to its exact text by hash, so it cannot change after it was reviewed.

## 14. Packages

A project is the folder holding the entry file. Without a `staal.json` there, a
program has no packages and imports only its own files (section 11 and the README).

### Manifests

The project's `staal.json` names its dependencies, each by git location and full
commit id:

    { "dependencies": {
        "sms": { "git": "https://github.com/laurells/staal-sms", "commit": "<40 lowercase hex digits>" } } }

A location is `https://`, `ssh://`, `user@host:path`, `file://` or an absolute path.
Plain `http://` and `git://` are refused. A branch or tag name is refused: they can
move, and a dependency must name exactly one tree of files.

A package is a git repository with a `staal.json` at its root:

    { "package": "sms", "version": "0.1.0",
      "authority": { "net.http.post": ["https://api.ng.termii.com/api/sms/send"] },
      "dependencies": { ... } }

`package` must be the name the depending project uses. `authority` is required
(`{}` for none). Each key is a capability path; the value is `"any"`, or a non-empty
list of `only` patterns, checked like a grant's (canonical, no `*` in a host).
Package names are lowercase letters, digits and `_`, starting with a letter; `net`
and `fs` are refused. Unknown keys are refused everywhere.

### Fetching and the lockfile

`staalc fetch [folder]` reads each dependency's commit from git's objects (a bare
clone, then the commit's tree and blobs; never a checkout, so line-ending settings,
attributes and filters cannot change a byte). It follows each package's own
dependencies. Then it refuses, before changing anything:

- a commit the repository does not have;
- a file that is a link or a submodule, a path with an empty, `.`, `..` or `.git`
  part, a backslash or a colon, two paths that differ only in letter case, or more
  than 10,000 files or 64 MiB;
- a package whose manifest gives another name, or is malformed;
- two different commits (or locations) of one package anywhere in the graph: one
  version of each package per project;
- a package already in `staal.lock` at the same commit whose files differ from
  the lock (the lock was edited, or the repository's history rewritten).

Only then are packages written to `.staal/packages/<name>/` and `staal.lock`
replaced. The lock records, per package, its location, commit and the SHA-256 of
every file. `fetch` prints each package with its authority, marked `new`,
`unchanged` or `was <old commit>`, for review.

### Loading

Before a program is checked or run, the lock must match `staal.json`: every
dependency, and every package's own dependencies, at the commit wanted, and nothing
else. Every package folder must hold exactly the files the lock lists, each with its
hash, and only plain files (a link, even to identical content, is refused). Files
are read once, verified, and the verified text is what the compiler reads. Any
mismatch stops before anything is checked (exit 2), naming the file.

### Imports

In the program, `import sms.termii` means module `termii` of package `sms` when
`sms` is a dependency; the program must not also have a file or folder named `sms`.
The program cannot import a package it does not depend on itself, even one another
package uses. Inside a package, an import whose first name is one of that package's
dependencies means that package; any other import is the package's own module
(`import util` in `sms` is `sms/util.staal`, never the program's `util`). Qualified
names carry the package (`sms.termii.send`), so nothing in two packages, or in a
package and the program, can be confused.

### Authority (E108)

Every capability type written in a package's code (parameters, return types, `let`
annotations, and a declared capability's `needs`) must name a path the manifest
lists. If the manifest lists patterns for it, the type must carry an `only` whose
every pattern is within them. Since a capability passed into a slot with `only` is
narrowed to it (section 6), the package can never act beyond its manifest, whatever
the caller grants. A capability a package declares must start with the package's
name. The program's own code is not limited by a manifest: the operator's grants
limit it.

### Journals

The program hash a journal records (section 11) covers every package module's text
and each package's location and commit, so a run cannot resume on different
package code.

### What is not checked

`staalc check` and `run` trust `staal.lock`: whoever can edit both the lock and
`.staal/packages/` decides the code, as with any language's lockfile. `staalc
fetch` compares both with git. Git's commit ids are SHA-1; the SHA-256 file hashes
in the lock are what a later fetch is compared with. Manifests describe authority;
they do not prove a package does what its name says.

## 15. Engines and native code

A run uses one of two engines, chosen for the run (`--engine native` on the command
line, `engine` in the C interface), never in the run config.

- **interpreter** (the default): every function on the interpreter.
- **native**: functions that qualify, below, are compiled to machine code with
  Cranelift when the run starts, in memory; every other function runs on the
  interpreter.

A function qualifies when its parameters and result are `Int`, `Bool`, `Text`,
`Money`, records whose fields are all of these, or lists of these, and its body uses
only `let`, `set`, `if`, `repeat`, `loop`, `for`, `break`, `return` with a value,
`fail`, literals (an integer must fit an Int; a list must not be empty), arithmetic,
unary `-`, comparisons, `and`, `or`, `not`, field reads, record literals, indexing,
the built-in functions other than `header` and `fill_secret`, and calls to other
qualifying functions. A function that calls one that does not qualify does not
qualify. Native code is entered only from the interpreter, when it calls a compiled
function; model calls, actions, `try`, `parse`, `validate`, HTTP, files, budgets and
the journal therefore never run in native code.

Both engines give the same results, failures (kind and message), logs and
journals. Native code:

- counts one step per statement and one per `repeat` or `loop` iteration, on the
  run's own counter, against `max_steps`;
- checks `max_depth` before each call, counting the calls already active, exactly as
  the interpreter does;
- checks every `+`, `-`, `*`, `/` and unary `-` on Int: an overflow, a division by
  zero and `MIN / -1` fail as `value`, with the message the interpreter gives (the
  same function makes it);
- evaluates left to right, and `and`/`or` evaluate their right side only when it
  decides the answer;
- holds Text, Money, records and lists in an arena owned by the runtime, and does
  every operation on them with the interpreter's own functions (arithmetic,
  comparison, built-ins, indexing, record building);
- copies a value when a variable is read, as the interpreter does (a field or item
  read from a variable copies only that field or item), and drops a variable's value
  when it is reassigned or leaves scope, so memory use matches the interpreter's.

Because the engine is not in the config, a journal does not record it: a run that
crashed on one engine resumes on the other.

The interpreter stays the reference. The tests run every runtime case on both
engines and compare results, logs and journals, and compare generated programs on
both (README).

## 16. The C interface

`libstaalc` (built as a C dynamic library alongside the Rust library) exports the
functions declared in `include/staal.h`. Each takes UTF-8 strings, borrowed for
the call, and returns a JSON object as a string the caller frees with
`staal_free`:

| Function | Does | `status` |
| --- | --- | --- |
| `staal_check(program_path)` | checks a program with its modules and packages | `ok`, `invalid`, `error` |
| `staal_run(program_path, config_json, journal_path, engine)` | runs it, with a new journal if `journal_path` is not NULL | `done`, `failed`, `stopped`, `invalid`, `error` |
| `staal_resume(program_path, config_json, journal_path, engine)` | resumes a journaled run | as `staal_run` |
| `staal_resolve(journal_path, resolution_json)` | resolves an action in doubt: `{"performed": "<receipt>"}`, `{"status": 200, "body": "..."}`, `{"failed": "<reason>"}` or `{"retry": true}` | `resolved`, `error` |
| `staal_version()` | the version, a static string not to be freed | |

`done` carries `result`; `failed` and `stopped` carry `failure` (`kind`, `message`);
all three carry `log`. `stopped` means resolve (if an action is in doubt) and resume,
as exit code 3 does on the command line. `invalid` carries `errors`, each with `code`,
`file`, `line`, `col` and `message`. `error` means nothing ran: a NULL or non-UTF-8
argument, an unreadable file, invalid JSON, a bad config, journal or engine, or a
panic inside the library, which is caught and never unwinds into the caller. Runs
share no state and may be made from several threads. Never use one journal for two
runs at once: the journal's lock is tested between processes (section 11), not
between threads of one process.

## 17. Executables

`staalc build <file.staal> -o <executable> [--runtime <libstaalc.a>]` makes a
standalone executable (Linux only, for now; elsewhere it refuses):

1. The program is checked as by `staalc check`, packages and authority included. A
   program with errors is not built (exit 1).
2. Every function that qualifies for native code (section 15) is compiled into an
   object file. Code is generated for the baseline of the target processor family,
   not for features of the build machine, so the executable runs on any machine of
   that family.
3. The object file also holds the program's source files (the entry file, every
   module and every package module, with how each import was resolved), the SHA-256
   of the program's fingerprint (section 11), the names of the compiled functions,
   and the constants and record layouts their code uses.
4. It is linked with the runtime, `libstaalc.a` (found next to `staalc`, or given
   with `--runtime`), using `cc`.

The executable takes `run`, `resume` and `resolve` as `staalc` does, without the
program file, with the same options (except `--engine`) and exit codes. When it
starts, it loads and checks its program again from the sources it carries (package
authority is not checked again: it needs the manifests, and was checked when it was
built), and refuses to run (exit 2) unless the program's fingerprint matches the one
recorded and its native functions are exactly the compiled ones. Functions without
native code run on the interpreter.

Its journals are bound to the program's fingerprint, as `staalc`'s are, and not to
how it is run: a run started by the executable can be resumed by `staalc resume`
with the same program, and the other way round.
