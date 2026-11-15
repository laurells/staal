# Calling Staal from an app

An application runs Staal programs through `libstaalc`, Staal's C library, in-process.
The Python package (`staal`) and the Node.js package (`@staal/node`) each contain the
library and call it; neither contains Staal logic of its own, so a run from either gives
the same result as `staalc run`.

The usual division: the application decides policy from its own records (who the
customer is, what they may be refunded) and grants it to the run; the Staal program does
the part that acts on the world, within those grants.

## Python

```python
import staal

lib = staal.Library()
LIMIT = {"gold": 500, "standard": 50}  # USD per refund, decided by the app

def refund(tier, order, amount):
    return lib.run("refund.staal", {
        "entry": "main",
        "inputs": {"order": order, "amount": {"amount": amount, "currency": "USD"}},
        "grants": {"pay": {"cap": "payments.refund", "each": {"amount": str(LIMIT[tier]), "currency": "USD"}}},
    })

print(refund("standard", "4417", "35.00")["result"])
print(refund("standard", "4417", "300.00")["failure"]["message"])
```

with `refund.staal`:

```staal
fn main(order: Text, amount: Money, pay: cap payments.refund(each: 500 USD)) -> Text {
    let receipt = pay.refund(order, amount)
    return "Refunded " + to_text(amount) + " for order " + order
}
```

prints

```text
Refunded 35.00 USD for order 4417
'payments.refund' allows at most 50.00 USD per action; this action needs 300.00 USD
```

The tier comes from the application's records, never from the model. A grant above the
code's own `each: 500 USD` is narrowed to 500 USD, so an error in the application cannot
exceed what the code allows. This program and app are in
[examples/quickstart](../../examples/quickstart/), and the test suite checks this output.

`staal.Library(path=None)` loads the library bundled with the package, the one at
`path`, or the one `STAAL_LIBRARY` names.

| Method | Calls |
| --- | --- |
| `lib.check(program)` | `staal_check` |
| `lib.run(program, config, journal=None, engine=None)` | `staal_run` |
| `lib.resume(program, config, journal, engine=None)` | `staal_resume` |
| `lib.resolve(journal, performed=...)`, `(status=..., body=...)`, `(failed=...)`, `(retry=True)` | `staal_resolve` |
| `lib.version` | `staal_version` |

`config` is a dict in the [run config](../reference/run-config.md) format. Each method
returns the result object below, as a dict.

## Node.js

```js
const staal = require('@staal/node');

const limit = customer.tier === 'gold' ? 500 : 50;
const out = await staal.run('refund.staal', {
  entry: 'main',
  inputs: { order: '4417', amount: { amount: '35.00', currency: 'USD' } },
  grants: { pay: { cap: 'payments.refund', each: { amount: String(limit), currency: 'USD' } } },
}, { journal: 'refund-4417.journal' });

if (out.status === 'done') console.log(out.result);
else if (out.status === 'failed') console.log('refused:', out.failure.message);
```

`check(program)`, `run(program, config, { journal, engine })`,
`resume(program, config, journal, { engine })` and `resolve(journal, resolution)` return
Promises; runs happen off the event loop. Types are in `index.d.ts`. Node.js 18 or later.

## Results

| `status` | Meaning | Fields |
| --- | --- | --- |
| `done` | The run finished. | `result`, `log` |
| `failed` | The run failed. | `failure` (`kind`, `message`), `log` |
| `stopped` | The run stopped and can continue: resolve any action in doubt, then resume. | `failure` (kind `in_doubt` or `journal`), `log` |
| `invalid` | The program has check errors. Nothing ran. | `errors` (`code`, `file`, `line`, `col`, `message`) |
| `error` | Nothing ran: an unreadable file, invalid JSON, a bad config or journal. | `error` |

`log` is the run's events. A run's result also reports `usage`: for each capability
parameter of the entry function, the actions attempted and the money charged. These
correspond to the command line's exit codes 0, 1, 3, 1 and 2.

Runs share no state and can be made from several threads. A journal is used by one run
at a time.

## Crash-safe runs from an app

Give `journal`, and the run can be resumed after the process dies:

```python
out = lib.run("refund.staal", config, journal="refund-4417.journal")
# after a crash, with the same program and config:
out = lib.resume("refund.staal", config, "refund-4417.journal")
# if out["status"] == "stopped" with an action in doubt, after checking the service:
lib.resolve("refund-4417.journal", performed="re_3QxY")
out = lib.resume("refund.staal", config, "refund-4417.journal")
```

See [Journals and resuming](durability.md).

## Tools: many calls to one program

`staal.Tool` (Python) and `staal.Tool.open` (Node.js) make a program's entry function a
tool that an application or an agent calls repeatedly. A tool adds what a single run
cannot keep: quotas across calls, `once` (the same operation called again is the same
run), and recovery: a run a crash interrupted is finished when the journal directory is
next opened. Tools work on Linux and macOS.

```python
import staal

LIMITS = {"gold": "500.00", "standard": "50.00"}
refund = staal.Tool("refund.staal", name="refund_order",
                    description="Refund part of an order.",
                    grants=lambda customer: {"pay": {"cap": "payments.refund", "uses": 1,
                                                     "each": {"amount": LIMITS[customer["tier"]], "currency": "USD"}}},
                    journals="runs")

customer = {"id": "cus_1", "tier": "standard"}
r = refund({"order": "4417", "amount": {"amount": "35.00", "currency": "USD"}}, context=customer)
print(r.status, r.value)   # done Refunded 35.00 USD for order 4417
r = refund({"order": "4418", "amount": {"amount": "300.00", "currency": "USD"}}, context=customer)
print(r.status, r.text)    # failed 'payments.refund' allows at most 50.00 USD per action; ...
refund.close()
```

- The arguments are only inputs. A grant is never an argument: `grants` computes it from
  the `context` the application passes. `refund.schema` is the JSON Schema of the
  arguments.
- `r.status` is `done`, `failed`, `stopped`, `in_progress` or `not_run`; `r.value` is
  the result, `r.text` what to show a model, `r.run` the run's id.
- `app_inputs={"customer": lambda c: c["id"]}` supplies inputs only the application may
  give. They are left out of the schema, and a call whose arguments name one is refused.
- `scope=lambda c: c["tenant_id"]` keeps `once` keys and run statuses separate per
  tenant or user.
- `quota`, `once`, `deadline`, `model`, `net`, `secrets` and `engine` are the settings of
  a tool in `staalc mcp`'s manifest ([MCP.md](../MCP.md)).
- One process uses a journal directory at a time, enforced by an operating-system lock.
  Give each worker process its own directory.
- The tool host writes one line for each finished run to standard error, its log.

## Agent frameworks

A tool can be given to an agent framework as one of its tools. The framework's agent
loop decides when to call it; Staal enforces the grants on every call, and a refusal
goes back to the model as the tool's output.

| Framework | Python | Node.js |
| --- | --- | --- |
| OpenAI Agents SDK | `staal.openai_agents.function_tool(tool)` | `functionTool` from `@staal/node/openai-agents` |
| LangGraph | `staal.langgraph.structured_tool(tool)` | `structuredTool` from `@staal/node/langgraph` |
| Claude Agent SDK | `staal.claude_agent_sdk.mcp_server(tools, context=...)` | `mcpServer` from `@staal/node/claude-agent-sdk` |
| Vercel AI SDK | | `aiTools` from `@staal/node/vercel-ai` |
| Google ADK | `staal.google_adk.adk_tool(tool)` | |

Each framework has a guide with a complete agent: [OpenAI Agents
SDK](../frameworks/openai-agents.md), [LangGraph](../frameworks/langgraph.md), [Vercel AI
SDK](../frameworks/vercel-ai.md), [Claude Agent SDK](../frameworks/claude-agent-sdk.md),
[Google ADK](../frameworks/google-adk.md). The framework packages are optional
dependencies: `pip install "staal[openai-agents]"`, `"staal[langgraph]"`,
`"staal[claude-agent-sdk]"`, `"staal[google-adk]"`.

## C

`include/staal.h` declares the C interface. Each function takes UTF-8 strings, borrowed
for the call, and returns a JSON string the caller frees with `staal_free`:

```c
char *staal_check(const char *program_path);
char *staal_run(const char *program_path, const char *config_json, const char *journal_path, const char *engine);
char *staal_resume(const char *program_path, const char *config_json, const char *journal_path, const char *engine);
char *staal_resolve(const char *journal_path, const char *resolution_json);
const char *staal_version(void);
void staal_free(char *s);
```

`engine` (`"interpreter"` or `"native"`) may be NULL, and so may `staal_run`'s `journal_path` for a run without a journal. A panic inside the library is caught and
reported as `error`; it never unwinds into the caller. The header also declares the tool
host functions that the Python and Node.js tools use.
