# What Staal guarantees, what it assumes, and where it stops

This document is for engineers deciding whether to put Staal between a model and
something that matters. It states each guarantee, what that guarantee depends on, how
it is tested, and what it does not cover. Nothing here goes beyond what the tests in
this repository check; where a guarantee rests on an assumption Staal cannot check, it
says so.

The running example is a financial operations agent (`examples/finops/`). It reads the
signed-in customer's record and orders, refunds part of an order, books the refund,
texts the customer, and works within daily limits. `examples/finops/attacks.py`
attacks it on purpose. Each attack is judged by what the stand-in bank and SMS
service record in their own ledgers, not by what Staal reports. CI runs it on both
engines (`tests/capi_test.rs`, `the_finance_agent_holds_under_attack_on_each_engine`):

    python3 examples/finops/attacks.py /tmp/finops [interpreter|native]

## 1. The model is the adversary

The design assumes the model can be wrong, or turned: by an instruction planted in
data it reads, by a customer's message, or by its own mistake. Nothing Staal
guarantees depends on the model behaving well. So the attack suite sends no prompt to
any model. It sends the tool calls a model would send if it did exactly what the
planted instruction said, which is the worst a model can do, and more repeatable than
any real model.

What a model can do is choose which tool to call and with which arguments. Everything
else comes from the app:

| Decided by | What |
| --- | --- |
| The app, per call | who the customer is (an app input), the grants (each refund's limit, the payments it may touch, the one phone number a message may go to), the call's scope |
| The app, per tool | the program, the daily limits (`quota`), what counts as the same operation (`once`), timeouts, the deadline |
| The program | its own ceilings (`each: 500 USD`), what it checks before acting, and every SQL statement it runs |
| The model | which tool, and the arguments the tool's schema offers: here, an order id and an amount, or a message's text |

## 2. Trust: what is trusted, and what is not

**Not trusted:** the model, every argument it sends, and everything it read, including
the customer's record (whose name, in the demo, carries a planted instruction).

**Trusted, and not checked by Staal:**

- **Your app's code:** the functions that compute grants and app inputs, and the data
  they read. If the grants function gives a standard customer 500 USD, Staal enforces
  500 USD. Two things limit the damage. The program's own ceiling holds whatever the
  app grants (attack 9: a 5000 USD limit typed by mistake is still held to the
  program's 500). And the program's own checks still run.
- **Library capabilities** (`bank.staal`, `sms.staal`): code the operator chooses to
  give keys and network access to. The runtime checks the limits it is given against
  the values the library defines. It cannot check that `canonical` maps two different
  targets to two different forms, or that the target is the request's real recipient
  (RUNTIME.md section 13, "What the runtime does not check"). A library used as a
  package is pinned by hash, so it cannot change after review.
- **The operator's word about services.** `"idempotency_key": true` on a grant states
  that the service deduplicates by `Idempotency-Key`. Staal acts on that statement. If
  it is false, a lost answer can lead to the action happening twice. The attack suite
  shows this rather than hiding it (the LIMITATION attack: two refunds).
- **The machine:** the disk honours `fsync`, and one process uses a journal directory
  at a time (enforced by an OS lock).
- **The clock, across restarts.** A clock set back cannot give back quota. A clock set
  forward could free a quota or expire a `once` key early. While a host has its ledger
  open, it cannot: the ledger reads the clock as at most one minute ahead of the time
  since boot. Across a restart there is no such reference, so a jump forward is warned
  about, not refused (docs/MCP.md section 6). For a tool that moves money, keep `once`
  keys for ever (the host's `keep="forever"`), which no clock can expire, and keep the clock with
  NTP. The demo keeps them for the default 90 days.
- **C code called through `cap ffi`,** which the broker cannot check: a tool granted
  it gets a warning at start, and the run is marked unchecked.

## 3. The guarantees

Each guarantee is listed with what it depends on, and the tests that check it.

### G1. Authority comes only from the app

A model's arguments are inputs to the program, never authority. A grant is never an
argument. An app input (`"app_inputs"`) is not in the tool's schema, and a call whose
arguments name one is refused before anything is reserved. In attack 1, the model,
following the planted instruction, names Bola as the customer, and is refused:
`inputs.customer: given by the app, never by the caller`. The same happens when it
names another database (attack 3).

*Depends on:* the app supplying the customer from its own session, not from anything
the model said. *Tests:* attacks 1 and 3; `python/tests/test_tool.py`;
`node/test/staal.test.mjs` ("app inputs ..."), both engines.

### G2. Every action is checked before the service is called

Each action is checked against every limit of its capability and of every capability
it was narrowed from: money per action (`each`) and in total (`max`), the number of
attempts (`uses`), the targets (`only`), and the currency. A refused action never
reaches the service. Limits fail closed: an action whose amount or target cannot be
determined is refused. A program cannot widen a capability (E003 at compile time),
and the runtime checks the whole chain again.

In attack 2, 80 USD on a 50 USD limit is refused by the broker (`'bank.payments.refund'
allows at most 50.00 USD per action`), before the bank is called. The program's own
checks refuse the rest before it calls anything: more than is left of the order,
0.00, and an amount in EUR. The bank's ledger stays empty. (Without the program's
currency check, the broker would refuse the EUR refund too, as limited in USD.)

*Depends on:* the library reporting the action's real amount and target (`@amount`,
`@target`). *Tests:* attacks 2 and 9; the `caps__*` cases in `tests/run/`, run on both
engines with identical results (`tests/engines_test.rs`).

### G3. Inputs are checked before anything runs

Every input is checked against its declared type before the run starts. Attack 3
shows the refusals: a negative amount, `1e3`, a number instead of a decimal string,
an amount with more decimals than its currency has, an unknown currency, and a field
the program does not take. Each comes back as `not_run`, with no run, nothing
reserved, and nothing sent.

*Tests:* attack 3; `python/tests/test_tool.py`.

### G4. Model text is never an instruction, and never SQL

Text from outside can never be placed where a model call's instruction goes, or
where SQL goes (E006, at compile time): it is passed as data, or as a bound SQL
parameter. In attack 3, an order id of `o1001' or '1'='1` is only an id that no
order has.

*Does not cover:* a model obeying an instruction hidden in data. The instruction in
Ada's record reaches the model (attack 1 checks that it does). What Staal guarantees
is that obeying it cannot exceed the authority the app gave the call.

### G5. Limits across calls hold under concurrency and across restarts

A tool's `quota` (calls, and money, in a window) is kept in a durable ledger. Each call
reserves its worst case (here, the customer's per-refund limit) before it starts, and
holds it until it ends. So calls running at the same moment cannot overspend together.

In attack 5, thirty 10 USD refunds start at once against 20 refunds and 1000 USD a day.
The bank's ledger shows exactly 20 refunds, 200 USD. In attack 6, a model looping
through refunds is stopped at 20, and its texts at 5 a day. The ledger survives a
restart, so a new process sees what an earlier one spent.

*Depends on:* one process per journal directory. A quota that must hold across worker
processes needs their calls sent through one process. *Costs:* reserving the worst
case means fewer calls can run at once than the money would allow. *Tests:* attacks 5
and 6; `python/tests/test_tool.py`; `tests/mcp_test.rs`.

### G6. A repeated operation is one operation

With `once` (here keyed by customer and order), a call that repeats an earlier
operation gets the earlier run's answer, and nothing is done again. One that repeats
it with different arguments is refused. In attack 4, twenty identical refunds sent at
once, and one more later, made one run. The bank shows one refund and the SMS service
one message. A different amount for the same order is refused.

*Depends on:* the `once` key naming what makes two calls the same operation. Keyed by
order alone, a corrected request (80 refused, then 35) would be refused as the same
operation; that is the app's choice. Keys are kept as long as the run's files
(`keep`). *Tests:* attack 4; `python/tests/test_tool.py`; `tests/mcp_test.rs`.

### G7. A crash never repeats an action blind

Every operation is journaled before it is performed, and its result after. A run a
crash interrupts is resumed when its journal directory is next opened. Completed
operations are replayed from the journal without contacting anything. An action
whose intent was recorded but whose result was not is *in doubt*: it may or may not
have happened. Staal never guesses:

- When the service deduplicates by key (the grant's `"idempotency_key": true`), the
  action is sent again with the same key, `<run id>-<operation>` (in a parallel task,
  `<run id>-<task>-<operation>`), and the service answers with its first answer.
- Otherwise the run stops and waits for an operator, who checks the service's own
  records and records what happened (`staalc resolve`).

In attack 7, the process is killed with SIGKILL after the bank made the refund and
before its answer was recorded. After a restart, the bank shows one refund, the SMS
service one message, and the order 35.00 booked once.

*Depends on:* the services' deduplication (section 2) and the disk. *Tests:* attack 7;
`tests/crash_test.rs` and the crash cases at every crash point, on both engines.

### G8. A lost answer is retried, with the same key, and never as a new request

When an action safe to send again (a request with its idempotency key) gets no answer
(a dropped connection, a timeout), the run stops in doubt. The tool's host resumes it
within the same call, after 0.25 s, 1 s and 2 s, sending the request again with the
same key. It does so only when every operation in doubt is such an action: a run of
parallel tasks with one action in doubt that has no key waits for the operator, and a
model call in doubt is not repeated within the call. In attack 8, the bank's answer is
lost twice; the call finishes, and the bank shows one refund. When the answer is lost
every time, the call stops after four sends of one request, all with one key. The bank
refunds once, and the run finishes at the next start once the bank answers again.

*Depends on:* as G7. A stop that may need a person's answer is never retried.
*Tests:* attack 8; the host's unit test of which stops are retried (`src/host/mod.rs`).

### G9. What a refusal shows cannot be turned against its reader

Refusals reach models, logs and terminals. Text from outside, shown inside the
runtime's own messages, has control characters, line separators, bidirectional
formatting characters and backslashes escaped, and is cut to 100 characters with its
length. In attack 3, a field whose 1 MB name starts with a terminal escape code is
refused, and the refusal shows it as `\u{1b}[2Jxxx… (1000004 characters)`. Values
themselves reach programs, journals and services unchanged.

*Does not cover:* a program's own `fail(...)` text, which is the program's: a program
that writes a caller's text into it shows it as written. (`refund.staal` never does:
an unknown 1 MB order id is refused with its fixed `this customer has no such order`.)
*Tests:* attack 3; `src/clip.rs`; `python/tests/test_tool.py`.

## 4. Where it stops

These are not bugs to be fixed later. They are the edges of what a runtime between a
model and the world can promise.

- **What the model decides within its authority.** A turned model can still refund a
  standard customer 50 USD on one of their own orders, for no good reason, up to the
  daily limits. Staal makes the worst case bounded and known. It does not make the
  model's judgement good.
- **What the model writes where it may write.** `message_customer` sends the model's
  text, to the customer's own number only, at most 5 a day. A turned model can still
  send that customer a misleading message. If that is unacceptable, give the model no
  free-text channel, as `refund_order` does: its message is the program's own text.
- **Data inside the call's authority.** The model can read only the signed-in
  customer's record, and can text only that customer. Anything it read can still end
  up in a message to that same customer.
- **Business sense.** The runtime checks limits, not whether an action makes sense. A
  0.00 refund would pass the broker. The program refuses it (`a refund must be more
  than 0.00 USD`), and that check is the program's job.
- **The app's own policy.** A grants function that is wrong within the program's
  ceiling is enforced as written.
- **Amounts in two currencies are never ordered.** In Staal, `a <= b` fails (kind
  `value`) when `a` and `b` are in different currencies (RUNTIME.md section 2), rather
  than answering false, which would let a check such as `amount > left` pass unnoticed.
  The run then stops before acting, saying why; a program that wants a clearer refusal
  checks the currency first, as `refund.staal` does. The broker's limits hold either
  way: an action in a currency other than its limit's is refused.
- **Assumptions about services and the machine** (section 2): deduplication that is
  claimed but not real, a disk that lies about `fsync`, and C code.
- **Scope of quotas.** Quotas are per tool, across customers. A per-customer limit
  belongs in the grants the app computes. (The MCP server over HTTP, not yet in a
  release build, adds a per-principal share of a tool's quota: MCP.md section 12.)
- **Platforms.** Tools (the host behind `staal.Tool`, `Tool.open` and `staalc mcp`) run
  on Linux and macOS. SQLite actions are Linux only so far.
- **Tested, not proven.** These guarantees are checked by tests, including crash
  injection at every crash point on both engines. They are not formally verified.

## 5. Compared with ordinary application code

Everything above can be built in plain Python or TypeScript, and careful teams do build
it. A refund endpoint that meets the same bar needs:

- an idempotency key per operation, made and stored durably before the request is sent;
- a durable record of each step's intent and result, and code at start-up that finds
  interrupted work and decides, step by step, whether to replay, retry with the same
  key, or stop for a person;
- a reservation table for daily limits, updated in a transaction, holding each call's
  worst case until it ends, and kept across restarts;
- a check of every limit before every side effect, on every code path, including the
  ones added next year;
- queries scoped to the signed-in customer, with every value bound as a parameter;
- input validation that refuses a float for money, a currency the payment was not in,
  and fields nobody asked for;
- care about what error messages echo back.

None of that is unusual. The difference is where it lives. In application code, each
of these is a convention that every tool, and every developer who touches one, must
keep: a new tool that forgets the reservation, or one code path that calls the bank
before the limit check, breaks the guarantee for that path, and nothing says so. In
Staal they are properties of the runtime: one broker checks every action of every
tool, every operation is journaled the same way, and the compiler refuses a program
that widens a capability, uses model text as SQL or as an instruction, or calls a
model in an unbounded loop.

The costs are real and should be weighed:

- a language your team does not know yet;
- another runtime in your stack;
- a project before 1.0, whose language and file formats may change between minor
  versions;
- far fewer libraries than Python or TypeScript;
- the platform limits above.

For one simple tool written by one careful person, plain code can be as safe, and
simpler to run. Staal pays off when there are many tools, many people writing them,
and money or data behind them.
