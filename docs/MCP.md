# Staal as an MCP server: design

Status: design, agreed before implementation (phase 0 of the ecosystem plan); the
manifest format in section 2 is frozen for version 1. Nothing here is built yet except
`staalc schema` (section 4). When the implementation and this note disagree, the note
is updated in the same change.

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
  "server": { "name": "shop-actions", "journals": "/var/lib/staal/mcp" },
  "model": { "provider": "anthropic", "model": "claude-sonnet-5-5", "api_key": { "env": "ANTHROPIC_API_KEY" } },
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
      "quota": { "window": "24h", "calls": 100, "money": { "amount": "500.00", "currency": "USD" } }
    }
  ]
}
```

- `server.name` is reported to clients as the server's name. `server.journals` is the
  directory that holds every run's journal and the quota ledger; it is created with
  mode 0700 if missing. `server.notify`, optional, is a command (an array: the program
  and its fixed arguments) run when a run stops for an operator (section 7).
- `model` is the run config's `model`, used by every tool unless a tool gives its own.
- Each tool has `name` (1 to 128 characters of `A-Z a-z 0-9 _ - .`, unique in the
  manifest), an optional `title`, a `description` (required: a model chooses tools by
  it), `program` (a path relative to the manifest) and `entry` (default `main`).
- A tool's `grants`, `secrets`, `model`, `net`, `fs`, `adapters` and `limits` are
  exactly the run config's keys of the same names (RUNTIME.md section 1), with the same
  rules. `entry` and `inputs` are not allowed: the entry is the tool's, and inputs come
  from each call.
- `quota` is optional and described in section 6.

At start, every program is checked (`staalc check`), and every tool's config is checked
by building a run config from it with placeholder inputs (as `staalc config` makes
them) and validating it as `staalc run` would, without running anything. A program
with check errors, or a config that would be refused, stops the server.

The manifest holds environment variable names, never secret values, so it can be kept
in version control.

## 3. Protocol

Version 1 speaks stdio: newline-delimited JSON-RPC 2.0 messages, one per line, with no
embedded newlines. The server writes only MCP messages to stdout and its own log lines
to stderr, and it exits when stdin closes.

It supports two protocol versions:

- **2025-11-25**, which begins with `initialize`. The server answers with its name,
  its version (`staalc`'s) and `capabilities: { "tools": { "listChanged": false } }`, and
  serves the requests that version defines for tools (`tools/list`, `tools/call`,
  `ping`).
- **2026-07-28**, which has no handshake. Every request carries
  `_meta["io.modelcontextprotocol/protocolVersion"]` and
  `_meta["io.modelcontextprotocol/clientCapabilities"]`; a request missing either is
  refused with `-32602`. A request naming a version the server does not support gets
  `UnsupportedProtocolVersionError` (`-32022`) listing the two it does. `server/discover`
  answers with the supported versions, capabilities and server identity. Results carry
  `resultType: "complete"`.

A connection that begins with `initialize` is treated as 2025-11-25 throughout; any
other first request is treated per request, as 2026-07-28 requires. The server keeps no
state between requests except the quota ledger and the journals, which are on disk.

Unknown methods get `-32601`. Notifications the server does not use are ignored.
`notifications/cancelled` for a call in progress is honoured only before its run
starts; once a run has started it finishes or stops, because stopping it halfway would
leave its last action in doubt (section 7).

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
2. **Check the quota** (section 6). A call over the quota returns `isError: true` with a
   message naming the quota and when it frees up. Nothing runs.
3. **Build the run config**: the tool's config from the manifest, `entry`, and
   `inputs` = `arguments`. The arguments are only ever inputs: they cannot add or change
   a grant, a secret or a limit, and a value that would be a receipt, an outcome or a
   capability is refused by the input rules, as for any run.
4. **Run it** with a new journal at `<journals>/<tool name>/<run id>.journal`, where the
   run id is 32 random hexadecimal characters. The journal is created before the run
   starts, so a crash at any point after this leaves a journal to resume (section 7).
5. **Answer** from the run's outcome:

| Outcome | MCP result |
| --- | --- |
| `done` | `structuredContent` = the result; `content` = one text item holding the same JSON; `isError: false` |
| `failed`, kind `capability` | `isError: true`; the text is the broker's message, e.g. "'payments.stripe.refund' allows at most 50.00 USD per action; this action needs 300.00 USD" |
| `failed`, kind `validation`, `action`, `value`, `provider` or `budget` | `isError: true`; the text is the failure's kind and message |
| `failed`, kind `internal` | `isError: true`; the text says an internal error happened and names the run id; the details go to stderr |
| `stopped` | `isError: true`; the text says the action may already have happened, that the run is waiting for an operator, its run id, and not to call the tool again for it; `structuredContent` = `{"status": "waiting_for_operator", "run": "<run id>"}` (section 7) |
| a config or input problem (`error`) | `isError: true` with the problem, so the model can correct its arguments |

A failure is a tool result, not a protocol error, so the model sees it and can try
again with different arguments. Messages are the runtime's own, which never contain a
secret's value (RUNTIME.md section 12). The run's log is not sent to the client; it
stays in the journal directory beside the journal.

Annotations, derived from the tool's grants:

- `readOnlyHint: true` only when every grant is a read: `net.http.get`, `fs.read`,
  `db.sqlite.query` or `db.postgres.query`, and the tool has no library capability.
- `destructiveHint: false` only when `readOnlyHint` is true.
- `idempotentHint: true` only when every grant is read-only or granted
  `idempotency_key`.
- `openWorldHint: true` whenever any grant reaches the network.

Clients treat annotations as hints from an untrusted server; they change nothing about
what the broker allows.

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

**The ledger** is `<journals>/quota.ledger`, one JSON line per event (`reserve`,
`settle`), each written and flushed with `fsync` before the call it belongs to goes on.
Only one server process may hold it (an OS lock, freed if the process dies). A torn last
line is discarded; any other damage stops the server, as a damaged journal does. Events
older than the longest window are compacted away at start.

Windows use the system clock. A clock moved backwards makes a window last longer, never
shorter; moved forwards, it can free a quota early. That is stated, not hidden.

**What the runtime must add for this:** a run's result reports, for each root grant,
the uses counted and the money charged. Today the C interface does not, so this is part
of phase 1, for the interpreter and for native runs alike.

## 7. Crashes and restarts

If the server dies during a call, the client sees the connection close, and a client
that restarts it may send the call again; the quota bounds what a resent call can do.

When the server starts, before it answers anything, it looks for journals of runs that
did not finish and resumes each one, exactly as `staalc resume` would. Each finishes
(its reservation settled), fails (settled) or stops in doubt (still reserved). The
results of runs whose callers are gone are kept in the journal directory.

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
  `staal_run_status`, which takes a run id and returns the run's state (`running`,
  `waiting_for_operator`, `done` with its result, `failed` with its failure). It reads
  the journal and changes nothing. It answers only about runs this server started, and
  a run id is 32 random hexadecimal characters, so one caller cannot find another's
  runs by guessing. A model can then tell its user what finally happened, after the
  operator has acted.
- **The operator** is told outside MCP, durably. When a run stops, the server writes a
  record to `<journals>/waiting/<run id>.json` (tool, run id, what is in doubt, when),
  flushed before the caller is answered, and logs it to stderr. If the manifest names a
  `server.notify` command, the server runs it with the record's path as its only
  argument, without a shell. The command's exit status is recorded beside the record;
  a command that fails or cannot start is run again at each later start of the server,
  until it succeeds. The record is removed once the run is resolved and resumed.

So an operator learns of every stopped run even if the notify command is broken: the
record is on disk, `staalc mcp status` lists it, and stderr has it. The notify command
only makes the news faster.

A journal is tied to its program, config and runtime version, so upgrading `staalc` or
changing a tool with runs still stopped makes those runs refuse to resume. `staalc mcp
status` warns about them, and the release notes of each version say so.

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

## 9. How it is tested

- Conformance: the official Python and TypeScript MCP SDK clients start the server, list
  its tools and call them, under both protocol versions, in CI on Linux x86_64, Linux
  arm64, Apple Silicon and Intel macOS.
- Outcomes: each row of the table in section 5 has a test, against the local fake
  services in `bench/agent`, checking that a refused action never reached the service.
- Quotas: calls, money, concurrent calls, a run in doubt holding its reservation, and
  all of it across a server killed and restarted.
- Crashes: the server is killed at every journal write and every ledger write during a
  call; after restart, the world and the ledger match an uninterrupted call.
- Secrets: as in section 8.
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
