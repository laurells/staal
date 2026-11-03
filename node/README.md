# @staal/node: Staal for Node.js

Run [Staal](https://github.com/laurells/staal) programs from Node.js and TypeScript.
Staal is the programming language for agents: its compiler refuses a
program that could act without permission, use a model's answer unchecked, or
overspend, and its runtime never repeats an action after a crash.

Install it with `npm install @staal/node`. This package holds no Staal logic of its own. It calls staalc's C interface
in-process, so a Node app gets exactly the results `staalc` and the Python package
give. Runs happen off the event loop and return Promises.

```js
const staal = require('@staal/node');

// Your app decides the policy; Staal enforces it.
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

`run`, `resume`, `resolve` and `check` resolve to the JSON objects described in
[include/staal.h](https://github.com/laurells/staal/blob/main/include/staal.h), typed
in `index.d.ts`: `done` with a `result`, `failed` with a `failure`, `stopped` (an
action in doubt: `resolve` it, then `resume`), `invalid` with located check errors,
or `error`. Pass `{ engine: 'native' }` to run pure code as machine code.

**As a tool.** `staal.Tool.open` makes a program a tool an agent can call many times,
with quotas across calls, `once` (the same operation called again is the same run),
and recovery of a run a crash interrupted when the journal directory is next opened.
Grants are fixed, or computed by your app for each call from a context it passes:

```js
const refund = await staal.Tool.open('refund.staal', {
  name: 'refund_order',
  description: 'Refund part of an order.',
  grants: (customer) => ({ pay: { cap: 'payments.refund', uses: 1, each: { amount: LIMITS[customer.tier], currency: 'USD' } } }),
  journals: 'runs',
  quota: { window: '1h', money: { amount: '1000.00', currency: 'USD' } },
});
const r = await refund.call({ order: '4417', amount: { amount: '35.00', currency: 'USD' } }, { context: customer });
// r.status: 'done', 'failed', 'stopped', 'in_progress' or 'not_run'; r.text, r.value, r.run
```

A grant is never an argument, and the model never sees one. An app serving several
tenants or users from one directory also gives `scope: (customer) => customer.tenantId`,
from its own records: `once` keys are then held per scope, and a run's status is given
only in its scope (`refund.status(run, { context: customer })`). One process uses a
journal directory at a time, so give each worker its own directory (whose quotas are
then its own), or send the calls through one process.

**Agent frameworks.** `functionTool` from `@staal/node/openai-agents` gives an OpenAI
Agents SDK tool (grants from the run's `context`), and `structuredTool` from
`@staal/node/langgraph` a LangChain tool for LangGraph (context from
`config.configurable.staal_context`). `aiTools` from `@staal/node/vercel-ai` gives
Vercel AI SDK tools, an ES module for `ai` 7 (Node 22 or newer), whose context is each
tool's entry in `toolsContext`; a refusal reaches the model as an error with Staal's
message. `mcpServer` from `@staal/node/claude-agent-sdk` serves tools to the Claude Agent SDK as
an in-process MCP server, with the session's context bound when the server is made
(one per query), and `allowedTools` gives their names for `allowedTools`. The
frameworks are optional peer dependencies. Each is tested through the framework's own
agent loop (@openai/agents 0.20.0, @langchain/langgraph 1.5.1, ai 7.0.145,
@anthropic-ai/claude-agent-sdk 0.3.304, whose loop runs in the CLI it ships, against a
local stand-in for the Messages API); the examples are in
[examples/frameworks](https://github.com/laurells/staal/tree/main/examples/frameworks).

**Platforms.** Node 18 or newer, on Linux (x86_64 and arm64, glibc 2.28 or newer) and
macOS (Apple Silicon, macOS 11 or newer; Intel, macOS 10.12 or newer). Elsewhere,
build `node/` from the repository.

**Licence.** Apache-2.0 WITH LLVM-exception. The addon contains staalc and its Rust
dependencies, whose licences are in `LICENSE-THIRD-PARTY.txt`.
