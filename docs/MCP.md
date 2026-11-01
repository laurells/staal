# Staal as an MCP server: design

Status: design, agreed before implementation (phase 0 of the ecosystem plan), revised
after two outside reviews. The manifest format in section 2 is frozen for version 1.
The server itself is not built yet. What it relies on from the runtime is built and
tested: `staalc schema` (section 4), `limits.deadline_ms` (section 3), each run's
`usage` (section 6) and the `reads_console` check (section 2). When the implementation
and this note disagree, the note is updated in the same change.

## 0. Invariants

Everything below serves these, and a change that breaks one is wrong whatever else it
does:

1. **The model asks; Staal decides.** Nothing from the client becomes authority: not a
   grant, a limit, a secret, a model or a deadline.
2. **MCP is an entry point, not a second engine.** A tool call is an ordinary Staal run,
   with the same checks, broker, journal and recovery as `staalc run`. The server adds
   bounds (quotas, concurrency, deadlines); it never removes one.
3. **Schemas inform; the runtime decides.** A JSON Schema accepting arguments implies
   nothing: Staal's own check of inputs is the one that decides.
4. **Nothing is done twice by accident.** An action is performed once per run (the
   journal), and a call the operator marks as one logical operation is one run, however
   often it is sent (section 5, `once`).
5. **Nothing uncertain is guessed.** An action that may or may not have happened waits
   for a person.
6. **What the server knows survives it.** Every state that matters is on disk before
   anything depends on it, and every crash point has a recovery rule (section 7).

## 1. What it is for

`staalc mcp` serves Staal programs as tools to any client that speaks the Model
Context Protocol: coding agents, desktop assistants and agent frameworks. A tool call
starts one Staal run. The run's grants, broker checks, budgets and journal apply exactly
as they do under `staalc run` or `lib.run`; the server adds nothing a run could use to
do more, and one thing that bounds it further: a quota across calls (section 6).

The client, and the model behind it, is untrusted. It chooses which tool to call and
with what arguments. It never chooses a grant, a secret, a limit or a model.

Not in version 1: a remote server (Streamable HTTP and OAuth), resources, prompts,
sampling, the tasks extension, and tools whose list changes while the server runs.

## 2. The manifest

The operator writes one JSON file. It is read once at start; any problem in it stops
the server before it answers anything, with the same kind of message a run config
problem gives. Unknown keys are refused at every level.

```json
{
  "server": { "name": "shop-actions", "journals": "/var/lib/staal/mcp", "keep": 90,
              "max_concurrent_runs": 8, "notify": ["/usr/local/bin/notify-operator"], "notify_env": ["SLACK_WEBHOOK_URL"] },
  "model": { "provider": "anthropic", "model": "claude-sonnet-5-5", "api_key": { "env": "ANTHROPIC_API_KEY" },
             "timeout_ms": 60000, "retries": 2 },
  "tools": [
    {
      "name": "refund_order",
      "title": "Refund an order",
      "description": "Refund part of an order to the customer's original payment, within the shop's limits.",
      "program": "refund.staal",
      "entry": "main",
      "grants": {
        "pay": { "cap": "payments.stripe.refund", "each": { "amount": "50.00", "currency": "USD" }, "uses": 1,
                 "needs": { "get": { "cap": "net.http.get", "only": ["https://api.stripe.com/v1/payment_intents/*"] },
                            "post": { "cap": "net.http.post", "only": ["https://api.stripe.com/v1/refunds"], "idempotency_key": true },
                            "key": { "env": "STRIPE_SECRET_KEY" } } }
      },
      "net": { "timeout_ms": 10000 },
      "deadline": "120s",
      "max_concurrent_runs": 2,
      "once": { "by": ["order"], "keep": "30d" },
      "quota": { "window": "24h", "calls": 100, "money": { "amount": "500.00", "currency": "USD" } }
    }
  ]
}
```

- `server.name` is reported to clients as the server's name.
- `server.journals` is the directory that holds every run's journal and the ledger.
  It is created with mode 0700 if missing. If it exists, it must be owned by the user
  the server runs as and must not be writable by group or others; otherwise the server
  refuses to start.
- `server.keep` is how many days a finished run's journal and log are kept, at least 1,
  or `"forever"`; the default is 90 (section 7).
- `server.max_concurrent_runs` is the most runs in progress at once across all tools,
  1 to 256, default 8 (section 5).
- `server.notify`, optional, is a command (an array: the program and its fixed
  arguments) run when a run stops for an operator; `server.notify_env` lists the
  environment variables it may see (section 7).
- A tool's `deadline`, optional, bounds how long its runs may keep starting actions
  (section 3). Its `max_concurrent_runs`, optional, is the most of its runs in
  progress at once. Its `once`, optional, makes a call one logical operation
  (section 5).
- `model` is the run config's `model`, used by every tool unless a tool gives its own.
- Each tool has `name` (1 to 128 characters of `A-Z a-z 0-9 _ - .`, unique in the
  manifest), an optional `title`, a `description` (required: a model chooses tools by
  it), `program` (a path relative to the manifest) and `entry` (default `main`).
  Paths are resolved once, at start, to absolute paths with every symbolic link
  followed; the program is read and hashed then, so a file changed or replaced while
  the server runs is never picked up (a restart picks it up, and journals of unfinished
  runs then refuse to resume, as section 7 says).
- A tool's `grants`, `secrets`, `model`, `net`, `fs`, `adapters` and `limits` are
  exactly the run config's keys of the same names (RUNTIME.md section 1), with the same
  rules. `entry` and `inputs` are not allowed: the entry is the tool's, and inputs come
  from each call.
- `quota` is optional and described in section 6.

At start, every program is checked (`staalc check`), and every tool's config is checked
by building a run config from it with placeholder inputs (as `staalc config` makes
them) and validating it as `staalc run` would, without running anything. A program
with check errors, or a config that would be refused, stops the server.

Two things a program could do under `staalc run` are refused in a tool, because they
would reach the protocol stream or wait without end:

- **Console input.** A program that can reach `read_line()` is refused: the server's
  standard input is the MCP stream, so a read would consume the client's messages.
  `staalc schema` reports it as `reads_console`, following calls and the library
  capabilities the function is given.
- **Listening for connections.** A grant of `net.tcp.accept` is refused: an accept
  waits until a client connects, with no time limit today (DEFERRED.md), so a call
  could hold its quota reservation for ever.

`print` stays allowed. In a tool, printed lines never go to standard output; they are
recorded in the run's log, as always, and nothing else (section 3).

The manifest holds environment variable names, never secret values, so it can be kept
in version control.

## 3. Protocol

Version 1 speaks stdio: newline-delimited JSON-RPC 2.0 messages, one per line, with no
embedded newlines. The server writes only MCP messages to stdout and its own log lines
to stderr, and it exits when stdin closes.

The stream is protected at the level of file descriptors, not by care alone. Before
the server reads its manifest, it duplicates descriptors 0 and 1 to private ones used
only by the protocol reader and writer, then points descriptor 1 at standard error and
descriptor 0 at `/dev/null`. Anything else in the process that writes to standard
output (a Staal `print` that somehow reached it, a panic message, a C library called
through `cap ffi`) lands on standard error, and nothing else can read the client's
messages. The protocol writer serializes each message, takes the output lock, writes
the whole line (retrying short writes and interrupted calls until every byte is
written or a write fails for good), flushes, and only then releases the lock, so
concurrent answers never interleave. A write that fails for good means the client is
gone: the server stops reading, lets running runs finish or stop as usual, and exits.
A test runs a tool whose program prints and whose C library writes to standard output,
and checks every line the client receives is a valid MCP message.

Limits on what crosses the stream: an incoming line longer than 4 MiB, or nested
deeper than 64 levels, is refused with `-32700` (or `-32600`) without being parsed
further. A tool result whose JSON is longer than 1 MiB is not sent: the call answers
`isError: true` saying the result was too large and naming the run id, and the result
stays in the journal, where `staal_run_status` and the operator can reach it.

A line that holds a JSON array is a JSON-RPC batch, which MCP has not allowed since
2025-06-18; it is refused with `-32600` and nothing in it is run.

It supports two protocol versions:

- **2025-11-25**, which begins with `initialize`. The server answers with its name,
  its version (`staalc`'s) and `capabilities: { "tools": { "listChanged": false } }`, and
  serves `tools/list`, `tools/call` and `ping`.
- **2026-07-28**, which has no handshake. Every request carries
  `_meta["io.modelcontextprotocol/protocolVersion"]` and
  `_meta["io.modelcontextprotocol/clientCapabilities"]`; a request missing either is
  refused with `-32602`. A request naming a version the server does not support gets
  `UnsupportedProtocolVersionError` (`-32022`) listing the two it does. The server
  serves `server/discover` (supported versions, capabilities and server identity),
  `tools/list` and `tools/call`; `ping`, which this version removed, gets `-32601`.
  Every result carries `resultType: "complete"` and
  `_meta["io.modelcontextprotocol/serverInfo"]`. `tools/list` also carries the
  `ttlMs` and `cacheScope` this version requires on list results: `cacheScope:
  "private"`, and `ttlMs` of one hour, since the list never changes while the server
  runs. The server never sends `notifications/message`, which this version allows
  only for requests that ask for a log level.

Each version has its own request handler; they share the code that lists tools and
runs calls, not the code that reads and writes messages, so a field one version
requires never leaks into the other.

A connection that begins with `initialize` is treated as 2025-11-25 throughout; any
other first request is treated per request, as 2026-07-28 requires. The server keeps no
state between requests except the ledger and the journals, which are on disk.

Errors are kept apart by what went wrong. A line that is not JSON is `-32700`; a
message that is not a valid JSON-RPC request is `-32600`; an unknown method is
`-32601`; invalid parameters for a known method (an unknown tool name, a missing
`_meta` field) are `-32602`. Everything about running a tool, including arguments the
program's types refuse, is a tool result with `isError: true` (section 5), so the
model sees it. Notifications the server does not use are ignored.

**Cancellation.** `notifications/cancelled` for a call whose run has not started (it
is waiting for a concurrency slot, or its arguments are being checked) stops it before
anything is reserved or run. Once its run has started, cancellation cannot stop it:
stopping a run between operations is possible, but the operation in progress would be
cut in half and left in doubt. So a cancelled call's run carries on to its end, and the
server, as both protocol versions require, sends nothing more for that request. The
client is never told the call was cancelled when it may have acted: the run's outcome
is in its journal and in `staal_run_status`, under the run id that a
`notifications/progress` message, if the client asked for progress, gave at the start.

### How long a call can take

A run cannot hang for ever, but it can take long. Every operation that waits is bounded
by its own timeout:

| Operation | Bounded by |
| --- | --- |
| HTTP request | `net.connect_timeout_ms` and `net.timeout_ms` |
| Model call | the model's `timeout_ms`, times `retries` + 1 attempts, plus the waits between them (at most 60 s each) |
| TCP and TLS exchange | `net.connect_timeout_ms` and `net.timeout_ms` |
| PostgreSQL | `net.connect_timeout_ms` and `net.timeout_ms`, per connection and per statement |
| SQLite | at most 5 s waiting for a locked database; a statement itself has no time limit, so a tool's queries should be ones the operator knows are quick |
| Computation | `max_steps` |
| Listening | refused in a tool (section 2) |

The defaults suit a script, not a tool call, so in a tool they are required: the
manifest must give `net.connect_timeout_ms` and `net.timeout_ms` and, for a tool that
can reach a model, the model's `timeout_ms` and `retries`, or the server refuses to
start. A provider that never answers is a timeout like any other.

On top of those, a tool may set `deadline` (for example `"deadline": "120s"`), counted
from when the runtime begins executing the run, not from when the request arrived. It
bounds when a new action or model call may start, not when the run ends: past the
deadline, the run fails as `deadline` instead of starting the next one, so a deadline
never cuts an action in half and never leaves one in doubt. Because every operation is
bounded (the table above), a run takes at most its deadline plus its longest single
operation; the run does not end exactly at the deadline, and the public documentation
says so. A resumed run makes the same decision:
an operation already in the journal is replayed, and a run the journal records as
ended at its deadline ends there again. The tool's `deadline` sets the run's
`limits.deadline_ms`, which is built and tested on both engines (RUNTIME.md section 4,
`tests/deadline_test.rs`).

A client that sent a `progressToken` gets `notifications/progress` when the run starts,
carrying its run id, and once at the start of each external operation.

## 4. Listing tools

`tools/list` returns every tool in the manifest's order, which is deterministic,
followed by the server's own read-only `staal_run_status` (section 7). A manifest tool
may not use that name.

- `inputSchema` is the `input` of `staalc schema <program> --entry <entry>`: a JSON
  Schema 2020-12 object with one property per parameter the caller gives, `required`
  listing those without a default, `additionalProperties: false`, and `$defs` for
  declared records and enums. Capability and `Secret` parameters are not in it; they
  come from the manifest. Money is described with its currency's decimal places.
- `outputSchema` is the `output` of the same command.
- `annotations` are derived from the grants (section 5), never written by hand.

`staalc schema` is built and tested (`tests/interface_test.rs`). Its schemas agree with
Staal's own check of inputs on every case tested, with one exception the schema cannot
express: a money amount too large to hold exactly is refused by the runtime and
accepted by the schema. Staal's check of the arguments (section 5) is the one that
decides, so the gap can only refuse a call, never let a wrong one through.

## 5. Calling a tool

For `tools/call` with a tool `name` and `arguments`:

1. **Find the tool.** An unknown name is a protocol error (`-32602`).
2. **Check the arguments** by Staal's own input rules, without running anything. A
   refusal is a tool result with `isError: true`, so the model can correct them.
3. **Find an earlier call, for a tool with `once`.** If the ledger already holds this
   call's key, nothing new runs; the call is answered from the existing run (below).
4. **Take a concurrency slot.** If the server already has `server.max_concurrent_runs`
   runs in progress, or the tool its own `max_concurrent_runs`, the call answers
   `isError: true` saying the server is busy and to try again later. Nothing is
   reserved and nothing runs. Calls are not queued: a queue would be one more thing to
   make durable and recover.
5. **Reserve** (section 6). A call over the quota answers `isError: true`, naming the
   quota and when it frees up. Nothing runs.
6. **Run it** with a new journal at `<journals>/<tool>/<first two characters of the run
   id>/<run id>.journal`, where the run id is 32 random hexadecimal characters, and with
   `inputs` = the arguments. The arguments are only ever inputs: they cannot add or
   change a grant, a secret or a limit.
7. **Settle** (section 6), then **answer** from the run's outcome:

| Outcome | MCP result |
| --- | --- |
| `done` | `structuredContent` = the result; `content` = one text item holding the same JSON; `isError: false` |
| `failed`, kind `capability` | `isError: true`; the text is the broker's message, e.g. "'payments.stripe.refund' allows at most 50.00 USD per action; this action needs 300.00 USD" |
| `failed`, kind `validation`, `action`, `value`, `provider`, `budget` or `deadline` | `isError: true`; the text is the failure's kind and message |
| `failed`, kind `internal` | `isError: true`; the text says an internal error happened and names the run id; the details go to stderr |
| `stopped` | `isError: true`; the text says the action may already have happened, that the run is waiting for an operator, its run id, and not to call the tool again for it; `structuredContent` = `{"status": "waiting_for_operator", "run": "<run id>"}` (section 7) |
| a config or input problem (`error`) | `isError: true` with the problem, so the model can correct its arguments |

A failure is a tool result, not a protocol error, so the model sees it and can try
again with different arguments. Messages are the runtime's own, which never contain a
secret's value (RUNTIME.md section 12). The run's log is not sent to the client; it
stays in the journal directory beside the journal.

### One logical operation: `once`

Without `once`, every call is a new run. If a client sends the same call again (for
example after the server died before it answered), the second call is a new execution:
it can act again, and only the quota bounds how far. The quota limits total exposure;
it does not prevent duplicates, and nothing here says it does. The server logs a warning
at start for every tool that can spend money and has no `once`.

With `once`, the operator names which arguments identify one logical operation:

```json
"once": { "by": ["order"], "keep": "30d" }
```

The call's key is the SHA-256 of the tool's name and the named arguments, in Staal's
canonical JSON form (keys sorted, money as written by the runtime). `by` may name every
argument, and must name at least one. The key is written to the ledger in the same
`reserve` event that starts the run, and flushed before the run begins, so from that
moment a repeated call finds it. A call whose key is found never starts a run:

| The earlier run | The repeated call gets |
| --- | --- |
| still running | `isError: true`: already in progress, with its run id, to follow with `staal_run_status` |
| done | the earlier result, exactly as it was answered |
| failed | the earlier failure |
| waiting for an operator | the same `stopped` result |

A repeated key with different arguments (the same order, a different amount) is
refused with `isError: true`: it is a different operation pretending to be the same
one. A key is remembered for `keep` (at most `server.keep`, which the manifest check
enforces), counted from the run's end; after that, the same arguments start a new run.
Over stdio, one server serves one client, so keys are scoped to the tool; with a
remote transport they will be scoped to the authenticated client too.

A repeated call returns the same run, and the same run sends the same idempotency keys
to the service, so `once` and the service's own deduplication work together:
`once` stops a second run, and the service's key stops a second request within a run.
Neither replaces the other, and a payment grant keeps `idempotency_key` with or without
`once`.

The server runs at most `server.max_concurrent_runs` runs at once (and each tool at
most its own `max_concurrent_runs`), so a model sending a hundred calls at once gets a
handful of runs and many "busy" answers, not a hundred model requests and open
connections. Runs resumed at start count against the limits like any other.

Annotations, derived from the tool's grants:

- `readOnlyHint: true` only when every grant is a read: `net.http.get`, `fs.read`,
  `db.sqlite.query` or `db.postgres.query`, and the tool has no library capability.
- `destructiveHint: false` only when `readOnlyHint` is true.
- `idempotentHint: true` only when every grant is read-only or granted
  `idempotency_key`.
- `openWorldHint: true` whenever any grant reaches the network.

Clients treat annotations as hints from an untrusted server; they change nothing about
what the broker allows, and they never claim more than the broker enforces. Each rule
has a test: a tool with no grants, with only reads, with reads and writes, with a
narrowed capability, with `idempotency_key`, and with network reads only.

## 6. Quotas across calls

Every limit in Staal belongs to one run. Through MCP, each call is a new run with fresh
`uses` and `max`, so without more, `uses: 1` would mean one action per call, and a
model calling a tool in a loop would get as many as it asked for. A quota bounds a tool
across calls, over a sliding time window:

```json
"quota": { "window": "24h", "calls": 100, "money": { "amount": "500.00", "currency": "USD" } }
```

- `window` is a whole number of minutes (`m`), hours (`h`) or days (`d`), at least
  `1m` and at most `30d`.
- `calls` is the most calls that may start within any window.
- `money` is the most money the tool's runs may spend within any window, counted as the
  broker counts it: an action that succeeded, or one that failed after a request that
  may have spent money. It is only allowed when every money-limited grant of the tool
  is in that currency, and each grant has a `max`, or both `each` and `uses`, so a run's
  worst case is known before it starts. Otherwise the manifest is refused.

**Reserve, then settle.** Before a run starts, the server reserves one call and the
run's worst-case spend: the sum over its money-limited grants of `max`, or of `each`
times `uses` where there is no `max`. If the reservation would exceed the quota, the
call is refused and nothing runs. When the run ends, the reservation is settled to what
the run's broker actually spent, which the run's result reports per grant. Concurrent
calls therefore cannot overspend together: each holds its worst case until it settles.

A run that stops in doubt keeps its whole reservation until it is resolved and resumed,
because the action in doubt may have spent money.

**The ledger** is `<journals>/server.ledger`, one JSON line per event, each written and
flushed with `fsync` before anything that depends on it happens. It is the authority on
which runs exist and what they hold: a run exists from its `reserve` event, and its
journal is named by the run id that event records. Only one server process may hold
the ledger (an OS lock, freed if the process dies). A torn last line is discarded; any
other damage stops the server, as a damaged journal does.

Events: `reserve` (run id, tool, time, calls and money held, and the `once` key and
arguments hash if any), `settle` (run id, money actually charged, the outcome kind),
`release` (run id; a reservation whose run never started). Each run has exactly one
`reserve` and at most one `settle` or `release`; recovery checks this, and writing a
second one for a run is a bug the server refuses to commit. Compaction at start keeps
every event of a run that is unsettled, holds a `once` key not yet expired, or ended
within the longest window; it writes the new ledger to a temporary file, flushes it,
and renames it over the old one, so a crash during compaction leaves one or the other.

**Time.** Each event records the wall-clock time, in UTC milliseconds since 1970, so
time zones and daylight saving never enter into it. The ledger's time never goes
backwards: an event is stamped with the later of the clock and the last event's time,
and a clock found behind the ledger is logged as a warning. So a clock set back cannot
make a window shorter or reorder events. A clock set forwards can free a quota early,
and nothing inside one machine can detect that; an operator who needs it guarded
should run the server where the clock is kept by NTP and alarms on large steps.

A monotonic clock was considered and refused: it does not survive a restart, and a
quota has to, so the ledger would still depend on the wall clock at every start.

**What settling reads:** a run's result reports `usage`: for each capability parameter
of the entry function, the actions attempted and the money charged, per currency,
counting everything narrowed from it and a resumed run's replay (RUNTIME.md section 6).
It is built, the same on both engines, and tested (`tests/deadline_test.rs`,
`tests/capi_test.rs`).

## 7. Crashes and restarts

If the server dies during a call, the client sees the connection close. A client that
restarts it may send the call again: for a tool with `once`, it gets the original run;
without, it starts a new one (section 5).

A call moves through these states, each made durable before the next begins:

| Step | Durable record | If the server dies here, at the next start |
| --- | --- | --- |
| 1. reserve | `reserve` in the ledger | there is no journal: the run never started, so it gets a `release` |
| 2. create the journal | the journal's first line | the journal is there and empty of operations: the run is resumed, which runs it from the start |
| 3. run | the journal's records | the run is resumed, exactly as `staalc resume` would; it finishes, fails, or stops in doubt |
| 4. waiting record (a stopped run) | `waiting/<run id>.json` | the run is resumed again and stops again, rewriting the record |
| 5. settle | `settle` in the ledger | done: the run's journal records its end, and the ledger its settlement |
| 6. answer | none | nothing to recover; a repeated call is answered from the ledger and journal |

At start, before it answers anything, the server goes through the ledger and the
journal directory together:

- **A `reserve` with no journal** gets a `release`: the run never started.
- **A `reserve` whose journal records no end** is resumed. If it ends, it is settled
  from the `usage` its result reports, which counts the replayed actions too; if it
  stops in doubt, it keeps its reservation and its waiting record is rewritten.
- **A `reserve` whose journal records an end, with no `settle`** (the server died
  between steps 3 and 5): the journal is resumed, which replays it without contacting
  anything and reports the same `usage`, and the run is settled from that. Settling
  is therefore idempotent: it always reads the same usage from the same journal.
- **A journal with no `reserve`** cannot come from the server, which writes the
  `reserve` first. The server refuses to start and names it, for an operator to look
  at, rather than guess whether it ran.
- **A `settle` for a run whose journal is gone** is ordinary once the journal is past
  `server.keep`: the run is reported as expired (below).

Settling never releases a reservation that should stay: a run that stopped in doubt is
not settled at all until it is resolved and resumed to an end.

`staalc mcp status <manifest>` lists runs that are stopped, with what is in doubt, and
quota use per tool. An operator resolves a stopped run with `staalc resolve`, unchanged,
and the next start of the server resumes it.

### When a run waits for an operator

A run stops in doubt when an action may or may not have happened and cannot safely be
sent again. Only the operator can settle that, from the service's own records. The MCP
caller cannot: it is a model, or a user who has no access to those records, and an
answer from it would be a guess at the one point where Staal refuses to guess. So
resolution never travels over MCP, in either protocol version. In particular the
server never answers with `input_required`: that asks the client's user, who is the
wrong person, and 2025-11-25 clients do not support it.

What each side is told:

- **The caller** gets the `stopped` result in section 5: the action may already have
  happened, a person will settle it, and the tool must not be called again for it. That
  last part matters: a second call is a new run, which would act again. The result also
  carries the run id.
- **The caller can follow it** with a read-only tool the server adds to every manifest,
  `staal_run_status`, which takes a run id and returns a snapshot:
  `{"run": "<run id>", "status": ..., "observed_at": "<UTC time>"}`, with `result` or
  `failure` when there is one. The status is `running` only for a run in progress in
  this server process right now, which the server knows from memory; a run that was in
  progress when an earlier process died is resumed at start, before any status is
  answered, so `running` never describes a dead process. Otherwise the status comes
  from the ledger (`waiting_for_operator` for an unsettled run that stopped, `done` or
  `failed` once settled, with the result read from the journal's end record), and a run
  past retention is `expired`. A run id the ledger has never seen is `unknown`. It
  answers only about runs this server's ledger holds, and a run id is 32 random
  hexadecimal characters, so one caller cannot find another's runs by guessing.
  It opens nothing for writing and takes no lock: a journal is read as it stands, a
  torn last line is ignored as it would be on resume, and a damaged journal gives
  `unknown`-with-a-reason rather than any attempt to repair or resume it. The snapshot
  can be a moment old: a run an operator has just resolved stays `waiting_for_operator`
  until it is resumed and settled.
- **The operator** is told outside MCP, durably. When a run stops, the server writes a
  record to `<journals>/waiting/<run id>.json` (tool, run id, what is in doubt, when),
  flushed before the caller is answered, and logs it to stderr. If the manifest names a
  `server.notify` command, the server runs it with the record's path as its only
  argument, without a shell. The command's exit status is recorded beside the record;
  a command that fails or cannot start is run again at each later start of the server,
  until it succeeds. The record is removed once the run is resolved and resumed.
  The command runs with a cleared environment: only `PATH`, `HOME`, `LANG` and `TZ`,
  plus the variables `server.notify_env` names. It never inherits a model key, a
  payment key or any other secret the server holds, and the manifest is refused if
  `notify_env` names a variable that a grant, a secret or the model uses. Its standard
  input is `/dev/null` and its output goes to the server's standard error; it is given
  at most 30 seconds, then killed and counted as failed. The waiting record holds no
  secret and no tool arguments, only the tool, the run id, what is in doubt (the
  runtime's message, which never holds a secret) and the time, so it is safe to hand to
  the command.

So an operator learns of every stopped run even if the notify command is broken: the
record is on disk, `staalc mcp status` lists it, and stderr has it. The notify command
only makes the news faster.

**Upgrades.** A journal is tied to its program, config and runtime version, so
upgrading `staalc` or changing a tool while runs are unfinished makes those runs refuse
to resume. The policy is explicit: before an upgrade or a tool change, the operator
runs `staalc mcp status` and lets every unfinished run end (resolving any that wait).
If the server starts and finds an unfinished run it cannot resume for this reason, it
does not start: it names each such run, so nothing is silently abandoned with money
reserved. Each release's notes repeat this.

### Keeping files

Journals are kept as `<journals>/<tool>/<first two characters of the run id>/<run
id>.journal`, so no directory holds more than about a 256th of a tool's runs.
Journals are the authoritative record of what runs did. The server deletes them only
under the retention policy the operator sets: `server.keep` is how many days a settled
run's journal and log are kept (at least 1, default 90), or `"forever"`. At each start,
and once a day while running, the journals and logs of runs settled longer ago than
that are deleted, the log first and then the journal, so a failure part-way leaves a
journal without a log (harmless, retried at the next sweep) and never a log without
its journal. A run that is in progress, unsettled, or holds a `once` key not yet
expired is never deleted, whatever its age. `staal_run_status` answers `expired` for a
run whose journal is gone but whose ledger events are still kept, and `unknown` once
compaction has removed those too.

## 8. Security

What the server trusts: the manifest, the programs and libraries it names, and the
environment that holds the secrets. What it does not: everything that arrives on stdin.

- Arguments are inputs and nothing more (section 5, step 3).
- A secret is read from its environment variable when the server starts and is only
  ever given to the run that needs it; secret values are scrubbed from every message
  the runtime produces, and tests check the server's stdout, stderr, journals and
  ledger for each secret.
- The client sees tool names, descriptions, schemas, results and failure messages. It
  never sees grants, limits or the manifest's paths. A failure message can name a limit
  ("allows at most 50.00 USD per action"), which tells the model what it may do; that is
  intended.
- Tool results are data. A client that passes them to a model should treat them as
  untrusted, as it would any tool's output.
- Every call is logged to stderr: the tool, the run id, the outcome kind, and quota use.
  Arguments are not logged, because they may hold personal data.
- The notify command is a separate trusted program, run with a cleared environment
  and no secrets (section 7).
- The journal directory must belong to the server's user and not be writable by
  others (section 2): a journal decides what a resumed run replays, so whoever can
  write one can make a run "remember" things that never happened.
- The clock. A forward step can free a quota early (section 6). The server records the
  time at each start and logs a warning when it finds the clock more than a day ahead
  of the ledger's last event, so an operator watching the log sees large jumps; it
  cannot tell a jump from a long shutdown, so it warns rather than refuses.

## 9. How it is tested

- Conformance: the official Python and TypeScript MCP SDK clients start the server, list
  its tools and call them, under both protocol versions, in CI on Linux x86_64, Linux
  arm64, Apple Silicon and Intel macOS.
- Outcomes: each row of the table in section 5 has a test, against the local fake
  services in `bench/agent`, checking that a refused action never reached the service.
- Quotas: calls, money, concurrent calls, a run in doubt holding its reservation, and
  all of it across a server killed and restarted.
- Crashes: the server is killed at every journal write and every ledger write during a
  call; after restart, the world and the ledger match an uninterrupted call, and every
  row of the recovery table in section 7 is reached.
- Duplicates: the server is killed after a refund succeeded at the payment service and
  before the client got the answer; it is restarted and the client sends the same call
  again. For a tool with `once`, the service records exactly one refund and the client
  gets the original result; for one without, the test shows the second refund, as the
  documentation says it can happen. A repeated key with different arguments is refused.
- Concurrency: with `max_concurrent_runs: 2`, a third simultaneous call is refused as
  busy before reserving anything, and a fourth succeeds once one finishes.
- The protocol: a batch is refused; `ping` works under 2025-11-25 and is unknown under
  2026-07-28; `tools/list` under 2026-07-28 carries `ttlMs` and `cacheScope`; a 5 MiB
  line, a 100-level nesting and a result over 1 MiB are each handled as section 3 says;
  a call cancelled after its run started gets no answer and its run still ends.
- Notify: the command sees none of the server's secrets (it prints its environment in
  the test), and only the variables `notify_env` names.
- Secrets: as in section 8.
- The stream: a tool that prints, and one whose C library writes to standard output,
  never put anything but MCP messages on the client's stream; a program that can reach
  `read_line()`, and a grant of `net.tcp.accept`, are refused at start.
- Deadlines: a run past its deadline fails before its next action and never during
  one, on both engines, and a resumed run decides the same way.
- Keeping files: finished journals older than `server.keep` are deleted, and a running,
  stopped or unsettled run's never is.
- Operators: a stopped run leaves its `waiting` record before the caller is answered,
  including when the server is killed between the two; a failing notify command is run
  again at the next start until it succeeds; `staal_run_status` reports every state,
  refuses an unknown or malformed run id, and changes nothing.
- By hand before each release: the walkthroughs for Claude Code, Cursor and the OpenAI
  Agents SDK, run as written.

## 10. Decisions

**Grants never come from the call.** A tool's grants are fixed in the manifest, and
nothing in a call's arguments can widen, select or fill in a grant. Templating a grant
from arguments (for example `only` set to the order the model names, checked against
an allowlist in the manifest) was considered and refused, for two reasons:

- It adds no safety. A value the model picks from an allowlist can be any entry on
  that list, which is exactly what granting `only: <the allowlist>` already allows,
  with one fewer mechanism to get wrong.
- It moves authority into the least trusted input. The arguments are written by a
  model, possibly steered by an injection. The design keeps one rule with no
  exceptions: data never becomes authority.

Scoping a run to one order or one customer is still needed, and it has two right
places, both of which take the scope from trusted data rather than from the model:

1. **Inside the program, from a trusted lookup.** The program reads the order through
   a read capability the operator granted, and narrows its refund capability to that
   order's payment before using it. The narrowing is code, reviewed with the program,
   checked by the compiler and recorded in the journal. Today a program can narrow
   only to limits written in the code (GUIDE.md, "Policy from your app"); narrowing to
   a value the program computes is planned language work, and this is the use case
   that sets its priority.
2. **In the app, when Staal runs in-process.** With the framework adapters, the app
   computes each call's grants from what it knows about the request (the signed-in
   user, the order in front of them), as `lib.run` already allows.

When the server gains an authenticated remote transport, a third place opens: grants
chosen by the authenticated principal (a profile per OAuth client or scope), because
an authenticated identity is trusted input and a tool argument is not. That is
designed with the remote transport, not before.

**Operators are told outside MCP.** Section 7 ("When a run waits for an operator")
says how: the caller is told the truth and given a status tool, and the operator gets
a durable record, a notify command retried until it succeeds, and `staalc mcp status`.
Resolution stays with `staalc resolve`.

**Duplicates are stopped by `once`, not by the quota.** A quota bounds total exposure
over a window; it cannot tell a resent call from a new one. Deduplication by a key the
operator chooses from the arguments (section 5) was chosen over a key the client
sends, because MCP gives the client no field for one and a model would have to invent
it; the arguments that identify an operation (an order, a ticket) are already in the
call. The key is written to the ledger with the reservation, before the run begins, so
there is no moment when a run exists that a repeated call cannot find.

**The ledger is the authority; journals are the evidence.** The ledger says which runs
exist and what they hold; a run's journal says what it did. Every crash point has one
recovery rule (section 7), and the one case the server cannot explain (a journal with
no reservation) stops it rather than being guessed at.

**No queue.** When the server is at its concurrency limit, a call is refused at once.
A queue would be another state to make durable, recover and bound, for a client that
can simply try again.
