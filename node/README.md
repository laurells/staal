# @staal/node

Run [Staal](https://github.com/laurells/staal) programs from Node.js and TypeScript.
Staal is the programming language for agents: its compiler refuses a program that
could act without a capability, use a model's output unvalidated, or exceed its model
budget, and its runtime journals every action so a crashed run resumes without
repeating one that may have happened.

```sh
npm install @staal/node
```

The package calls Staal's C interface in-process and contains no Staal logic of its
own, so a run gives the same result as `staalc` and the Python package. It does not
install the `staalc` command; install that separately to check, test and format
programs.

## Running a program

```js
const staal = require('@staal/node');

// The application decides the policy; Staal enforces it.
const limit = customer.tier === 'gold' ? 500 : 50;
const out = await staal.run('refund.staal', {
  entry: 'main',
  inputs: { complaint },
  grants: { pay: { cap: 'payments.refund', each: { amount: String(limit), currency: 'USD' } } },
  model: { provider: 'anthropic', model: 'claude-sonnet-5-5', api_key: { env: 'ANTHROPIC_API_KEY' } },
}, { journal: 'refund.journal' });

if (out.status === 'done') console.log(out.result);
else if (out.status === 'failed') console.log('refused:', out.failure.message);
```

`run`, `resume`, `resolve` and `check` return Promises, and runs happen off the event
loop. Each resolves to the result object of the
[C interface](https://github.com/laurells/staal/blob/main/include/staal.h), typed in
`index.d.ts`:

| `status` | Meaning |
| --- | --- |
| `done` | Finished, with `result`. |
| `failed` | Failed, with `failure`. |
| `stopped` | An action is in doubt: `resolve` it, then `resume`. |
| `invalid` | Check errors, with file, line and column. Nothing ran. |
| `error` | Nothing ran: a file, config or journal problem. |

`{ engine: 'native' }` runs qualifying functions as machine code.

## Tools

`staal.Tool.open` makes a program's entry function a tool that is called many times,
with quotas across calls, `once` (the same operation called again is the same run),
and recovery of a run a crash interrupted when the journal directory is next opened.

```js
const refund = await staal.Tool.open('refund.staal', {
  name: 'refund_order',
  description: 'Refund part of an order.',
  grants: (customer) => ({ pay: { cap: 'payments.refund', uses: 1, each: { amount: LIMITS[customer.tier], currency: 'USD' } } }),
  journals: 'runs',
  quota: { window: '1h', money: { amount: '1000.00', currency: 'USD' } },
});
const r = await refund.call({ order: '4417', amount: { amount: '35.00', currency: 'USD' } }, { context: customer });
// r.status is 'done', 'failed', 'stopped', 'in_progress' or 'not_run'; also r.value, r.text, r.run
```

| Option | Purpose |
| --- | --- |
| `grants` | A grant object, or a function of the context. A grant is never a call argument. |
| `appInputs` | Inputs only the application gives, such as `{ customer: (c) => c.id }`. They are left out of the schema, and a call whose arguments name one is refused. |
| `scope` | A function of the context, such as `(c) => c.tenantId`. `once` keys and run statuses are kept per scope; `refund.status(run, { context })` answers only within it. |
| `quota`, `once`, `deadline`, `model`, `net`, `secrets`, `engine` | As for a tool in `staalc mcp`'s manifest ([MCP.md](https://github.com/laurells/staal/blob/main/docs/MCP.md)). |

One process uses a journal directory at a time. Give each worker its own directory,
whose quotas are then its own, or send all calls through one process.

## Agent frameworks

| Framework | Import | Context |
| --- | --- | --- |
| OpenAI Agents SDK | `functionTool` from `@staal/node/openai-agents` | The run's `context` |
| LangGraph | `structuredTool` from `@staal/node/langgraph` | `config.configurable.staal_context` |
| Vercel AI SDK | `aiTools` from `@staal/node/vercel-ai` (ES module; `ai` 7, Node.js 22 or later) | Each tool's entry in `toolsContext` |
| Claude Agent SDK | `mcpServer` from `@staal/node/claude-agent-sdk`, one per query; `allowedTools` for `allowedTools` | Bound when the server is made |

The frameworks are optional peer dependencies. A refusal reaches the model with the
broker's message. Each adapter is tested through the framework's own agent loop
(@openai/agents 0.20.0, @langchain/langgraph 1.5.1, ai 7.0.145,
@anthropic-ai/claude-agent-sdk 0.3.304 against a local stand-in for the Messages API),
and against each framework's latest release for every Staal release. Complete agents
are in [examples/frameworks](https://github.com/laurells/staal/tree/main/examples/frameworks).

## Platforms

Node.js 18 or later on Linux (x86_64 and arm64, glibc 2.28 or later) and macOS (Apple
Silicon, macOS 11 or later; Intel, macOS 10.12 or later). Elsewhere, build `node/` from
the repository. Calling C libraries from Node.js needs `STAALC_FFI_WORKER` set to a
`staalc` of the same version.

## Licence

Apache-2.0 WITH LLVM-exception. The addon contains `staalc` and its Rust dependencies,
whose licences are in `LICENSE-THIRD-PARTY.txt`.
