# Staal as an MCP server: design

Status: version 1 is built (`staalc mcp`, `src/mcp/`) and tested
(`tests/mcp_test.rs`, `tests/mcp_crash_test.rs`, `tests/mcp_sdk_test.rs`). The design
was agreed before implementation (phase 0 of the ecosystem plan) and revised after two
outside reviews; the manifest format in section 2 is frozen for version 1. Where
building it settled a detail the design had left open, this note says what was built.
When the implementation and this note disagree, the note is updated in the same change.

Running it: `staalc mcp <manifest.json>` serves on standard input and output, the way
an MCP client starts a local server; `staalc mcp status <manifest.json>` lists, beside a
running server, the runs that are unfinished and each tool's quota use.

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
  refuses to start. Every folder the server makes in it is 0700 and every file it
  writes (ledger, run files, journals, logs, waiting records, and the temporary files
  of compaction and of atomic writes) is 0600, since run files hold a call's arguments
  and journals what it sent and received. Each file is flushed (`fsync`) before
  anything depends on it, and a file created or renamed into place has its folder
  flushed too, so a power loss leaves it there whole or not at all.
- `server.keep` is how many days a finished run's journal and log are kept, at least 1,
  or `"forever"`; the default is 90 (section 7).
- `server.max_concurrent_runs` is the most runs in progress at once across all tools,
  1 to 256, default 8 (section 5).
- `server.notify`, optional, is a command (an array: the program, absolute or relative
  to the manifest and resolved at start, and its fixed arguments) run when a run stops
  for an operator; `server.notify_env` lists the further environment variables it may
  see (section 7).
- A tool's `deadline`, optional, bounds how long its runs may keep starting actions
  (section 3). Its `max_concurrent_runs`, optional, is the most of its runs in
  progress at once, at most `server.max_concurrent_runs`. Its `once`, optional, makes
  a call one logical operation (section 5). Its `engine`, optional, is `"interpreter"`
  (the default) or `"native"`, as `staalc run --engine`.
- `model` is the run config's `model`, used by every tool unless a tool gives its own.
- The manifest's own keys are `server`, `model` and `tools` (at least one tool).
- Each tool has `name` (1 to 128 characters of `A-Z a-z 0-9 _ - .`, not only dots,
  since it names the tool's folder of journals, and unique in the manifest), an
  optional `title`, a `description` (required: a model chooses tools by
  it), `program` (a path relative to the manifest) and `entry` (default `main`).
  Paths are resolved once, at start, to absolute paths with every symbolic link
  followed; the program is read and hashed then, so a file changed or replaced while
  the server runs is never picked up (a restart picks it up, and journals of unfinished
  runs then refuse to resume, as section 7 says). What is hashed is the whole program:
  the text of every module it imports, a package's modules included, by module path,
  and each package's repository and commit; a C library is granted by its SHA-256 in
  the config, which the journal binds too.
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

A third, C code (`cap ffi`), is allowed with a warning at start: it cannot be
interrupted either (section 3, "How long a call can take").

`print` stays allowed. In a tool, printed lines never reach the protocol stream: they
go to the server's standard error (section 3) and are journaled, as every print is.

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
stays in the journal, where the operator can read it. `staal_run_status` reports such a
run as done, with the result's size and not the result (section 7).

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
  `UnsupportedProtocolVersionError` (`-32022`) whose `data` is
  `{"supported": ["2026-07-28"], "requested": ...}`: the version it serves per
  request (2025-11-25 is reached through `initialize`, so listing it there would send
  a client round in a circle). The server serves `server/discover` (supported
  versions, capabilities and server identity), `tools/list` and `tools/call`; `ping`,
  which this version removed, gets `-32601`. Every result carries `resultType:
  "complete"` and `_meta["io.modelcontextprotocol/serverInfo"]`. `server/discover` and
  `tools/list` also carry the `ttlMs` and `cacheScope` this version requires on them:
  `cacheScope: "private"`, and `ttlMs` of one hour, since nothing they describe
  changes while the server runs. The server never sends `notifications/message`, which this version allows
  only for requests that ask for a log level.

Each version has its own request handler; they share the code that lists tools and
runs calls, not the code that reads and writes messages, so a field one version
requires never leaks into the other.

Which version a request is served under follows from the request, as the 2026-07-28
specification has a dual-era server do: one carrying
`_meta["io.modelcontextprotocol/protocolVersion"]` is served as 2026-07-28; once a
client has sent `initialize`, one without it is served as 2025-11-25; before that, one
without it is refused with `-32602`. A client uses one lifecycle: a 2026-07-28 client
starts with `server/discover` (or its first request) and sends the version in each
request's `_meta`; a 2025-11-25 client starts with `initialize` and uses the version it
negotiated. The server does not hold a connection to one lifecycle, because each request
says which it is; that is deliberate, so a client probing with one and settling on the
other works, and the official Python and TypeScript SDKs connect in either mode and
with their own probe (section 9). Clients should not mix the two otherwise.

The server holds in memory only what active requests need: whether a client has sent
`initialize`, the calls in progress (for cancellation), and the runs in progress with
their concurrency slots. Everything about an operation that must outlive the process
(which runs exist, what they reserve, quota use, `once` keys, what each run did) is
in the ledger and the journals, on disk.

Errors are kept apart by what went wrong. A line that is not JSON is `-32700`; a
message that is not a valid JSON-RPC request is `-32600`; an unknown method is
`-32601`; invalid parameters for a known method (an unknown tool name, a missing
`_meta` field) are `-32602`. Everything about running a tool, including arguments the
program's types refuse, is a tool result with `isError: true` (section 5), so the
model sees it. Notifications the server does not use are ignored.

**Cancellation.** The reservation (step 5 of section 5) is the point after which a
call goes ahead. Cancellation is checked under the same lock that makes the
reservation, immediately before it: a call cancelled before then reserves nothing and
runs nothing; once the reservation is written, the call is treated as started, whatever
arrives afterwards, and its run is never released on account of a cancellation. Once
its run has started, cancellation cannot stop it:
stopping a run between operations is possible, but the operation in progress would be
cut in half and left in doubt. So a cancelled call's run carries on to its end, and the
server, as both protocol versions require, sends nothing more for that request. The
client is never told the call was cancelled when it may have acted: the run's outcome
is in its journal and in `staal_run_status`, under the run id that a
`notifications/progress` message, if the client asked for progress, gave at the start,
or in the server's log.

### How long a call can take

Every external operation for which the runtime has an enforceable timeout is bounded
by it (the table). Outside that guarantee: C code called through `cap ffi`, which no
timeout can stop (below the table), and stalls of the machine itself, such as a disk
that does not complete a journal write or `fsync`; a run waits for those like any
program does.


| Operation | Bounded by |
| --- | --- |
| HTTP request | `net.connect_timeout_ms` and `net.timeout_ms` |
| Model call | the model's `timeout_ms`, times `retries` + 1 attempts, plus the waits between them (at most 60 s each) |
| TCP and TLS exchange | `net.connect_timeout_ms` and `net.timeout_ms` |
| PostgreSQL | `net.connect_timeout_ms` and `net.timeout_ms`, per connection and per statement |
| SQLite | at most 5 s waiting for a locked database, and `net.timeout_ms` per statement (a statement past it is stopped and changes nothing) |
| Computation | `max_steps` |
| Listening | refused in a tool (section 2) |
| A C function (`cap ffi`) | nothing: C code cannot be interrupted |

A tool granted a C function is therefore the one kind of tool whose call can hold its
concurrency slot and its reservation until the process is restarted, if the function
never returns. The server says so in a warning at start for each such tool, and a tool
should be given only C functions the operator knows return.

The defaults suit a script, not a tool call, so in a tool they are required: the
manifest must give `net.connect_timeout_ms` and `net.timeout_ms` and, for a tool that
can reach a model, the model's `timeout_ms` and `retries`, or the server refuses to
start. A provider that never answers is a timeout like any other.

On top of those, a tool may set `deadline` (for example `"deadline": "120s"`), counted
from when the runtime begins executing the run, not from when the request arrived. It
bounds when a new action or model call may start, not when the run ends: past the
deadline, the run fails as `deadline` instead of starting the next one, so a deadline
never cuts an action in half and never leaves one in doubt. Because every operation is
bounded (the table above; C functions apart), a run takes at most its deadline plus its
longest single operation; the run does not end exactly at the deadline, and the public documentation
says so. A resumed run makes the same decision:
an operation already in the journal is replayed, and a run the journal records as
ended at its deadline ends there again. The tool's `deadline` sets the run's
`limits.deadline_ms`, which is built and tested on both engines (RUNTIME.md section 4,
`tests/deadline_test.rs`).

A client that sent a `progressToken` gets one notification when the run starts, after
its reservation:

```json
{"jsonrpc": "2.0", "method": "notifications/progress",
 "params": {"progressToken": <the client's token>, "progress": 0, "message": "run <run id> started"}}
```

It says only that the run began, and gives its run id for `staal_run_status`; it has no
`total`, and promises nothing about when the run ends. Version 1 sends nothing per
operation: the runtime has no hook for it yet (DEFERRED.md, "Progress per operation").

## 4. Listing tools

`tools/list` returns every tool in the manifest's order, which is deterministic,
followed by the server's own read-only `staal_run_status` (section 7). A manifest tool
may not use that name.

- `inputSchema` is the `input` of `staalc schema <program> --entry <entry>`: a JSON
  Schema 2020-12 object with one property per parameter the caller gives, `required`
  listing those without a default, `additionalProperties: false`, and `$defs` for
  declared records and enums. Capability and `Secret` parameters are not in it; they
  come from the manifest. Money is described with its currency's decimal places.
- `outputSchema` is `{"type": "object", "properties": {"result": <the output of the
  same command>}, "required": ["result"]}`, with its `$defs` at the top: MCP requires
  an object for both schemas and for `structuredContent`, and a Staal function may
  return text, a number or a list.
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
| `done` | `structuredContent` = `{"result": <the result>}`; `content` = one text item holding the result's JSON; `isError: false` |
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
one. Arguments are compared as the runtime reads them, defaults filled in, so
`{"amount": "35", "currency": "USD"}` and `"35.00"` are the same. A key is remembered
for `keep` (a whole number and `m`, `h` or `d`; at most `server.keep`, which the
manifest check enforces; `server.keep` when left out), counted from the run's end;
after that, the same arguments start a new run. A call that never started (refused
as busy or over its quota, or released at start) holds no key.
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
connections. Runs resumed at start are resumed one after another, before anything is
served. Calls waiting to be checked are bounded too: with more than four times
`server.max_concurrent_runs` plus 16 in progress, a call is answered busy at once.

Annotations, derived from the tool's grants:

- `readOnlyHint: true` only when every grant is a read: `net.http.get`, `fs.read`,
  `db.sqlite.query` or `db.postgres.query`, and the tool has no library capability.
  The broker enforces the last three (a read-only file, a read-only SQLite connection,
  a READ ONLY PostgreSQL transaction). `net.http.get` is counted as a read because HTTP
  defines GET as safe (RFC 9110); a service whose GET changes something breaks that,
  and Staal cannot tell.
- `destructiveHint: false` only when `readOnlyHint` is true.
- `idempotentHint: true` only when `readOnlyHint` is true or the tool has `once` with
  its key kept for ever (`server.keep` `"forever"`). A key kept for a time lets the same
  call run again once it has gone, so a tool whose key expires is not idempotent.
  An idempotency key does not make a tool idempotent: the keys are the run's own
  (section 7), so a second call is a second run with new keys and acts again. Only
  `once` turns the same arguments into the same run. (The design first said
  `idempotency_key` was enough; building it showed that would claim more than the
  broker enforces.)
- `openWorldHint: true` whenever the tool can reach a model, any grant (or any of a
  library capability's needs) reaches the network or loads C code, or a grant goes to
  an adapter's capability, which may be a service.

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
journal is named by the run id that event records (the runtime creates the journal
with that id, so the idempotency keys a service sees carry it too). Only one process
may use a journal directory: it holds an OS lock on `<journals>/server.lock`, freed if
the process dies, and a second process waits up to five seconds for it (an SDK's
probe, or a restart, may still be letting go) and then refuses to start. This holds
for an app's tools too (section 11), which use the same code: a directory is served by
one `staalc mcp` server or one app process, never both. A torn last line is discarded; any
other damage stops the server, as a damaged journal does.

Events: `reserve` (run id, tool, time, calls and money held, the tool's quota window
in milliseconds, and the `once` key, arguments hash and how long the key is kept, if
any), `settle` (run id, money actually charged, the outcome kind),
`release` (run id; a reservation whose run never started). Each run has exactly one
`reserve` and at most one `settle` or `release`; recovery checks this, and writing a
second one for a run is a bug the server refuses to commit. Compaction, at start and at each daily sweep, keeps
every event of a run that is unsettled, whose files are still kept (section 7), that
was reserved within its tool's quota window, or that holds a `once` key not yet
expired (both read from the `reserve` event itself, so compaction does not depend on
the tools as they are now), and drops released runs, which never started and hold nothing; it writes the new ledger to a temporary file, flushes it,
and renames it over the old one, so a crash during compaction leaves one or the other.

**Time.** Each event records the wall-clock time, in UTC milliseconds since 1970, so
time zones and daylight saving never enter into it. The ledger's time never goes
backwards: an event is stamped with the later of the clock and the last event's time,
and a clock found behind the ledger is logged as a warning. So a clock set back cannot
make a window shorter or reorder events. When compaction drops the event that held the
latest time, a `clock` event (`{"e":"clock","t":MS}`) keeps it, so this holds across a
compaction and a restart too.

A clock set forward is the dangerous direction: it would free a quota early and, worse,
expire a `once` key early, so a repeated payment could start a new run. While a host
has its ledger open, this cannot happen: the ledger reads the clock as at most one
minute ahead of the time since boot (`CLOCK_BOOTTIME` on Linux, `CLOCK_MONOTONIC` on
macOS, both counting sleep and neither set by anyone) counted from when it opened, so a
jump moves nothing on, and the first one is logged as a warning. Across a restart there
is no such reference: the host warns when it finds the clock more than a day ahead of
the ledger's last event, and cannot tell a jump from a long shutdown, so it warns
rather than refuses. For a tool that moves money, give `once` a key kept for ever
(`server.keep` `"forever"`), which no clock can expire, and run the host where the
clock is kept by NTP.

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
| 2. keep the run file | `<run id>.run.json` beside the journal: the program's path, the engine and the whole run config (arguments, grants, model; secrets only as the names of their variables) | still no journal: as step 1 (the file is removed) |
| 3. create the journal | the journal's first line | the journal is there and empty of operations: the run is resumed, which runs it from the start |
| 4. run | the journal's records | the run is resumed, exactly as `staalc resume` would, from its journal and its kept arguments; it finishes, fails, or stops in doubt |
| 5. waiting record (a stopped run) | `waiting/<run id>.json` | the run is resumed again and stops again; the record stays as first written |
| 6. settle | `settle` in the ledger | done: the run's journal records its end, and the ledger its settlement; the run file is removed |
| 7. answer | none | nothing to recover; a repeated call is answered from the ledger and journal |

The run file is kept only while a run may need resuming: a settled run's file is
removed at once, and at every start whatever is left of one. Because it holds the
config the run started with, a run is resumed without its tool's definition: what the
manifest says now does not matter to it. A run's log goes beside
its journal as `<run id>.log.jsonl`.

At start, before it answers anything, the server goes through the ledger and the
journal directory together:

- **A `reserve` with no journal** gets a `release`: the run never started.
- **A `reserve` whose journal records no end** is resumed. Before any run is resumed,
  every unfinished run is checked: its run file is there, its program is checked again
  from the file the run file names, and its journal was started with the identical
  program, config and runtime version.
  Any failure stops the server, naming each run (see Upgrades below). If it ends, it is settled
  from the `usage` its result reports, which counts the replayed actions too; if it
  stops in doubt, it keeps its reservation and its waiting record is rewritten.
- **A `reserve` whose journal records an end, with no `settle`** (the server died
  between steps 3 and 5): the journal is resumed, which replays it without contacting
  anything and reports the same `usage`, and the run is settled from that. Settling
  is therefore idempotent: it always reads the same usage from the same journal.
- **A journal with no `reserve`** cannot come from the server, which writes the
  `reserve` first. The server refuses to start and names it, for an operator to look
  at, rather than guess whether it ran. The same holds for a run file.
- **A released run's files** (a journal with no records, a run file) are
  removed, so that once compaction forgets the run nothing is left that no reservation
  explains.
- **A run the runtime could not finish** (it refused at the start, or its thread
  crashed): if its journal holds nothing past its start record, nothing happened and
  the run is released; otherwise it is kept as stopped, kind `internal`, and resumed
  at the next start.
- **A `settle` for a run whose journal is gone** is ordinary once the journal is past
  `server.keep`: the run is reported as expired (below).

Settling never releases a reservation that should stay: a run that stopped in doubt is
not settled at all until it is resolved and resumed to an end. A restart frees the
concurrency slots, which live in memory, never a reservation, which lives in the
ledger.

**A crash inside a C function.** A call to C is an action like any other (RUNTIME.md
section 19): journaled before it is made, its value recorded after. A process killed
while the function runs leaves the call in doubt. On restart the run is resumed, and
the call is made again only if its grant says `"repeatable": true`; otherwise the run
stops, keeps its reservation, and waits for an operator, who can only record it as
failed or have it retried, since nothing outside the process saw what it returned
(`tests/ffi_test.rs`).

**Deletions are flushed too.** Removing a file (a finished run's journal, log or run
file, a released run's leftovers, a resolved run's waiting record) flushes its folder
before anything else happens, so the ledger never forgets a run whose file could come
back after a power loss: such a file would be one no reservation explains, which stops
the next start.

`staalc mcp status <manifest>` lists unfinished runs: those waiting for an operator,
with what is in doubt and the `staalc resolve --journal ...` path to resolve each,
and any other (in progress, or interrupted and resumed at the next start). It also
gives each tool's quota use. It reads the ledger without locking or changing it, so it
can run beside the server. An operator resolves a stopped run with `staalc resolve`,
unchanged, and the next start of the server resumes it.

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
  `failure` when there is one. A result larger than a call's answer may be (1 MiB, section
  3) is not sent this way either: the snapshot gives `result_bytes` and a `reason`
  instead, and the operator reads the result from the journal. The status is `running` only for a run in progress in
  this server process right now, which the server knows from memory; a run that was in
  progress when an earlier process died is resumed at start, before any status is
  answered, so `running` never describes a dead process. Otherwise the status comes
  from the ledger (`waiting_for_operator` for an unsettled run that stopped, with the
  `failure` its waiting record holds; `done` or `failed` once settled, with the result
  or failure read from the journal's end record, an internal failure's message left
  out as in section 5; `not_started` for a call released before its run started), and
  a run past retention is `expired`. A run id the ledger has never seen is `unknown`. It
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
  `server.notify` command, the server runs it with the record's path as its last
  argument, without a shell. How it ended is recorded beside the record, in
  `waiting/<run id>.notify` (`ok`, or `failed:` and why); a command that fails or cannot
  start is run again at each later start of the server, until it succeeds. A run that
  stops again in the same way when it is resumed keeps its first record, and its
  operator is not told again. The record is removed once the run is resolved and resumed.
  The command runs with a cleared environment: only `PATH`, `HOME`, `LANG` and `TZ`
  (those that are set), plus the variables `server.notify_env` names (which may not
  repeat those four, and need a `server.notify`). It never inherits a model key, a
  payment key or any other secret the server holds, and the manifest is refused if
  `notify_env` names a variable that a grant, a secret or the model uses. Its standard
  input is `/dev/null` and its output goes to the server's standard error; it is given
  at most 30 seconds, then killed and counted as failed. The waiting record holds no
  secret and no tool arguments, only the tool, the run id, what is in doubt (the
  runtime's message, which never holds a secret) and the time, so it is safe to hand to
  the command.

Delivery is at least once, not exactly once: if the command succeeds and the process
dies before `ok` is recorded, the command runs again at the next start. The waiting
record's run id is stable, so a command that must not alert twice deduplicates by it.
A failed command is run again only at a later start of the server (or the next time an
app opens the directory), not while it runs.

So an operator learns of every stopped run even if the notify command is broken: the
record is on disk, `staalc mcp status` lists it, and stderr has it. The notify command
only makes the news faster.

**Upgrades.** A journal is tied to its program, config and runtime version. The config
comes from the run file, so changing a tool in the manifest (its grants, quota, even
removing it) does not stop its unfinished runs from resuming as they started. Editing
a program's file, or upgrading `staalc`, does: those runs refuse to resume. The policy
is explicit: before an upgrade or a change to a program, the operator runs `staalc mcp
status` and lets every unfinished run end (resolving any that wait).
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

- Arguments are inputs and nothing more (section 5, step 6).
- A secret's variable is checked when the server starts (a missing one stops it), and
  its value is read for each run and only ever given to the run that needs it; secret
  values are scrubbed from every message
  the runtime produces, and tests check the server's stdout, stderr, journals and
  ledger for each secret. C code called through `cap ffi` is outside that: it writes
  what it likes to standard error, so a C function must not be given a secret it may
  not disclose.
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
- The clock. A forward step while a host runs moves nothing on; across a restart it
  can free a quota or expire a `once` key early, which the host warns about at start
  when the clock is more than a day ahead of the ledger (section 6).

## 9. How it is tested

- Conformance: the official Python SDK (`mcp` 2.3.0) and TypeScript SDK
  (`@modelcontextprotocol/client` 2.3.1) clients start the server, list its tools and
  call them, under 2025-11-25, under 2026-07-28 and with each SDK's own probe
  (`tests/mcp_sdk_test.rs`), in CI on Linux x86_64, Linux arm64, Apple Silicon and
  Intel macOS.
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
  busy before reserving anything, and a fourth succeeds once one finishes. Sixteen
  identical `once` calls released together make one reservation and one run; sixteen
  calls racing for a money quota that fits one worst case make exactly one run, and
  the others reserve nothing (python/tests/test_tool.py). Splitting the lock between
  the `once` check and the reservation fails the first of these.
- Scope: two tenants making the same `once` call get two runs and see only their
  own; a run's status asked from another scope, or none, is `unknown`; a conflict is
  told without anything of the other tenant; a malformed scope is refused (Python and
  Node.js tests). Dropping the scope from the key, or from the status check, fails them.
- The clock: the latest time survives a compaction that drops its event and a
  restart, and a clock set forward while the ledger is open stamps nothing ahead of
  the time since boot (`src/host/ledger.rs`).
- Changed code: a module a program imports is changed while a run is unfinished; the
  directory refuses to open, naming the run, and opens and finishes it once the module
  is restored (python/tests/test_tool.py, with crash injection).
- The protocol: a batch is refused; `ping` works under 2025-11-25 and is unknown under
  2026-07-28; `tools/list` under 2026-07-28 carries `ttlMs` and `cacheScope`; a 5 MiB
  line, a 100-level nesting and a result over 1 MiB are each handled as section 3 says;
  a call cancelled after its run started gets no answer and its run still ends.
- Notify: the command sees none of the server's secrets (it prints its environment in
  the test), and only the variables `notify_env` names, and on Linux it holds no
  descriptor beyond its standard ones (the protocol's private ones are close-on-exec;
  making them inheritable fails the test).
- Time limits: an endless SQLite statement is stopped at `net.timeout_ms`, on both
  engines, and a stopped `execute` changes nothing (`tests/db_test.rs`, `src/interp/db.rs`).
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
a durable record, a notify command run again at each later start until it succeeds,
and `staalc mcp status`.
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

## 11. Tools in an app: the C interface, Python and Node.js

The same host serves tools inside an app (`src/host/`): `staalc mcp` and the C
interface's tool functions (`staal_host_open`, `staal_tool_open`, `staal_tool_call`,
`staal_run_status`; include/staal.h) share one implementation of sections 5 to 7, so
quotas, `once`, the ledger, run files, recovery, the waiting records and the notify
command behave identically. Python's `staal.Tool` and `staal.Host`, and Node's
`Tool.open` and `Host.open`, are thin layers over those functions.

A tool in an app is defined as a tool in the manifest is (section 2), with the host's
settings (`journals`, `keep`, `max_concurrent_runs`, `notify`, `notify_env`) given when
the host opens. The one difference is where grants come from:

- **Fixed**, in the definition, as in a manifest. A function that takes no capability
  is always this.
- **Per call**, given by the app with each call: the app computes them from what it
  knows (the signed-in user, the customer's tier), never from the call's arguments,
  which are inputs only, as in section 5. Because the grants are not known when the
  tool is defined, each call's grants are checked as a manifest's are when it loads:
  `net.tcp.accept` is refused, a tool that reaches the network must set both `net`
  timeouts, with a money quota every grant that can spend must be bounded (`max`, or
  `each` and `uses`) in the quota's currency, and no grant may name a variable that
  `notify_env` passes to the notify command. A call that fails a check is refused
  before anything is reserved. The tool's annotations claim nothing it cannot know:
  `readOnlyHint` false and `openWorldHint` true.

A call's answer is `{status, run, text, structured, is_error}`, with `status` one of
`done`, `failed`, `stopped` (waiting for an operator), `in_progress` (a repeated `once`
call of a run still going) and `not_run` (refused before any run: arguments the types
refuse, a quota, a busy host, grants a check refuses). `text` is what an MCP client
would be given: for `done`, the result as JSON, so a text result keeps its quotes.

**Scope: more than one tenant or user.** The MCP server serves one client. An app can
serve many from one journal directory, and two of them can make the same call: two
shops each refunding their order 123. So a call can carry a trusted *scope* (a
tenant's or a user's id, 1 to 256 characters, no control characters), given by the
app from its own records, never taken from the arguments (`scope=` in Python and
Node.js, a function of the call's context like `grants`). The scope is recorded with
the run's reservation, and:

- A `once` key is held within its scope: the same arguments in two scopes are two
  runs, and a conflict is found only against the same scope's runs.
- A run's status is given only to a question in its scope; asked from any other scope,
  or with none, a run made in a scope is `unknown`, as a run never made is, so even
  its existence is not revealed. A run id is an identifier, not an authorization.
- Quotas and the concurrency limit stay per tool, across scopes: they bound what the
  tool does in total. A per-tenant limit belongs in the grants the app computes.

**One process per journal directory.** The lock of section 6 holds across the MCP
server and apps alike. In one process, opening a directory again gives the same host
(with the same settings; other settings are refused), and it is closed when the last
holder closes it. An app with several worker processes gives each its own directory,
and each worker's quotas are then its own; a quota that must hold across workers needs
the calls sent through one process.

**Framework adapters.** `staal.openai_agents.function_tool` and
`staal.langgraph.structured_tool` in Python, and `@staal/node/openai-agents`,
`@staal/node/langgraph` and `@staal/node/vercel-ai` in Node.js, wrap a tool for the
OpenAI Agents SDK, LangGraph and the Vercel AI SDK. For the Claude Agent SDK,
`staal.claude_agent_sdk.mcp_server` and `@staal/node/claude-agent-sdk` serve tools as an
in-process MCP server, made per session with its context, since that SDK gives a tool
only its arguments. The model sees the tool's name, description and argument schema; the grants
come from the framework's run context through the app's function; the answer's `text`
goes back to the model as the tool's output, a refusal's message included (marked as an
error where the framework has a way to say so, as the AI SDK's `error-text` does), and
nothing is retried. They are tested through each framework's own agent loop with a scripted
model (python/tests/test_adapters.py, node/test/frameworks/), against the framework
versions pinned in python/tests/frameworks.txt and node/test/frameworks/package.json. The
Claude Agent SDK's loop runs in the Claude Code CLI it ships, pointed at a local server
answering as the Messages API does.

