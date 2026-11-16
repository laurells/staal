# staal

The Python package for [Staal](https://github.com/laurells/staal), the programming
language for agents. It installs the `staalc` command and a Python interface to
Staal's runtime. No Rust toolchain is needed.

Staal's compiler refuses a program that could act without a capability, use a model's
output unvalidated, put untrusted text in a model's instruction, or exceed its model
budget. Its runtime enforces the limits your application grants, and journals every
action so a crashed run resumes without repeating one that may have happened.

```sh
pip install staal
staalc check program.staal
```

## Running a program

The application decides the policy and grants it to the run; Staal enforces it.

```python
import staal

lib = staal.Library()  # the library installed with this package
limit = 500 if customer["tier"] == "gold" else 50
out = lib.run("refund.staal", {
    "entry": "main",
    "inputs": {"complaint": complaint},
    "grants": {"pay": {"cap": "payments.refund", "each": {"amount": str(limit), "currency": "USD"}}},
    "model": {"provider": "anthropic", "model": "claude-sonnet-5-5", "api_key": {"env": "ANTHROPIC_API_KEY"}},
}, journal="refund.journal")
if out["status"] == "done":
    print(out["result"])
```

`lib.check`, `lib.run`, `lib.resume` and `lib.resolve` return the result object of the
[C interface](https://github.com/laurells/staal/blob/main/include/staal.h) as a dict,
with `status` one of `done`, `failed`, `stopped`, `invalid` or `error`.

## Tools

`staal.Tool` makes a program's entry function a tool that is called many times. It
adds quotas across calls, `once` (the same operation called again is the same run),
and recovery: a run a crash interrupted is finished when the journal directory is next
opened. Grants are fixed, or computed for each call from a context the application
passes.

```python
refund = staal.Tool("refund.staal", name="refund_order",
                    description="Refund part of an order.",
                    grants=lambda customer: {"pay": {"cap": "payments.refund", "uses": 1,
                                                     "each": {"amount": LIMITS[customer["tier"]], "currency": "USD"}}},
                    journals="runs",
                    quota={"window": "1h", "money": {"amount": "1000.00", "currency": "USD"}})
r = refund({"order": "4417", "amount": {"amount": "35.00", "currency": "USD"}}, context=customer)
# r.status is "done", "failed", "stopped", "in_progress" or "not_run"; also r.value, r.text, r.run
```

| Argument | Purpose |
| --- | --- |
| `grants` | A grant dict, or a function of the context. A grant is never a call argument. |
| `app_inputs` | Inputs only the application gives, such as `{"customer": lambda c: c["id"]}`. They are left out of the schema, and a call whose arguments name one is refused. |
| `scope` | A function of the context, such as `lambda c: c["tenant_id"]`. `once` keys and run statuses are kept per scope; `refund.status(run, context=customer)` answers only within it. |
| `quota`, `once`, `deadline`, `model`, `net`, `secrets`, `engine` | As for a tool in `staalc mcp`'s manifest ([MCP.md](https://github.com/laurells/staal/blob/main/docs/MCP.md)). |

One process uses a journal directory at a time, enforced by an operating-system lock:
another process waits five seconds, then is refused. Give each worker process its own
directory, whose quotas are then its own, or send all calls through one process.

## Agent frameworks

| Framework | Adapter | Install | Context |
| --- | --- | --- | --- |
| OpenAI Agents SDK | `staal.openai_agents.function_tool(tool)` | `staal[openai-agents]` | The run's `context` |
| LangGraph | `staal.langgraph.structured_tool(tool)` | `staal[langgraph]` | `config["configurable"]["staal_context"]` |
| Claude Agent SDK | `staal.claude_agent_sdk.mcp_server(tools, context=...)`, one per session; `allowed_tools(name, tools)` for `allowed_tools` | `staal[claude-agent-sdk]` | Bound when the server is made |
| Google ADK | `staal.google_adk.adk_tool(tool)` | `staal[google-adk]` | The session state's `"staal_context"` |

A refusal goes back to the model as the tool's output, as an error where the framework
supports one, with the broker's message. Each adapter is tested through the
framework's own agent loop (openai-agents 0.24.0, langgraph 1.2.16, claude-agent-sdk
0.2.168 against a local stand-in for the Messages API, google-adk 2.12.0), and against
each framework's latest release for every Staal release. Complete agents are in
[examples/frameworks](https://github.com/laurells/staal/tree/main/examples/frameworks).

## Platforms

Python 3.8 or later on Linux (x86_64 and arm64, glibc 2.28 or later) and macOS (Apple
Silicon, macOS 11 or later; Intel, macOS 10.12 or later). Elsewhere, build from
source. `staalc build` also needs a C compiler (`cc`) and works on Linux only.

Staal is a preview, with no independent security review yet. Start with [Your first
program](https://github.com/laurells/staal/blob/main/docs/get-started/first-program.md);
[Calling Staal from an app](https://github.com/laurells/staal/blob/main/docs/guides/apps.md)
covers this package in full.

## Licence

Apache-2.0 WITH LLVM-exception. The package contains `staalc` and its Rust
dependencies, whose licences are in `LICENSE-THIRD-PARTY.txt`.
