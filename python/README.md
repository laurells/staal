# staal

[Staal](https://github.com/laurells/staal) is the programming language for agents,
built for what they do: sending messages, moving money, changing records. Its compiler
refuses a program that could act without permission, use a model's answer
unchecked, let untrusted text instruct a model, or overspend its budget, and its
runtime journals every action so a crashed run resumes without repeating one.

This package installs the `staalc` compiler and runtime, and a Python interface to
it. No Rust toolchain is needed.

    pip install staal
    staalc check program.staal
    staalc run program.staal

From Python, through Staal's C interface. Your app decides the policy (here, a
refund limit per customer tier) and Staal enforces it:

    import staal
    lib = staal.Library()          # the library installed with this package
    limit = 500 if customer["tier"] == "gold" else 50
    out = lib.run("refund.staal", {
        "entry": "main",
        "inputs": {"complaint": complaint},
        "grants": {"pay": {"cap": "payments.refund", "each": {"amount": str(limit), "currency": "USD"}}},
        "model": {"provider": "anthropic", "model": "claude-sonnet-5-5", "api_key": {"env": "ANTHROPIC_API_KEY"}},
    }, journal="refund.journal")
    if out["status"] == "done":
        print(out["result"])

Every call returns the JSON object described in
[include/staal.h](https://github.com/laurells/staal/blob/main/include/staal.h), as a
dict. `staalc build` also needs a C compiler (`cc`) to link executables.

**As a tool.** `staal.Tool` makes a program a tool an agent can call many times, with
what one run cannot keep alone: quotas across calls (calls and money per window, each
run holding its worst case until it settles), `once` (the same operation called again
is the same run), and recovery (a run a crash interrupted is finished when the
journal directory is next opened, by this process or another). Grants are fixed, or
computed by your app for each call from a context it passes:

    refund = staal.Tool("refund.staal", name="refund_order",
                        description="Refund part of an order.",
                        grants=lambda customer: {"pay": {"cap": "payments.refund", "uses": 1,
                                                         "each": {"amount": LIMITS[customer["tier"]], "currency": "USD"}}},
                        journals="runs", quota={"window": "1h", "money": {"amount": "1000.00", "currency": "USD"}})
    r = refund({"order": "4417", "amount": {"amount": "35.00", "currency": "USD"}}, context=customer)
    # r.status: "done", "failed", "stopped", "in_progress" or "not_run"; r.text, r.value, r.run

The arguments are only ever inputs: a grant is never an argument, and the model never
sees one. One process uses a journal directory at a time (an OS lock; another process
waits five seconds, then is refused), so give each worker process its own directory,
whose quotas are then its own, or send the calls through one process. The tool's
other settings (`quota`, `once`, `deadline`, `model`, `net`, `secrets`, `engine`)
are those of a tool in `staalc mcp`'s manifest
([docs/MCP.md](https://github.com/laurells/staal/blob/main/docs/MCP.md)).

**Agent frameworks.** `staal.openai_agents.function_tool(tool)` gives an OpenAI Agents
SDK tool (`pip install "staal[openai-agents]"`), whose grants come from the run's
`context`; `staal.langgraph.structured_tool(tool)` gives a LangChain tool for
LangGraph (`pip install "staal[langgraph]"`), with the context taken from
`config["configurable"]["staal_context"]`. A refusal goes back to the model as the
tool's output, the broker's message included. Both are tested through the framework's
own agent loop (openai-agents 0.24.0, langgraph 1.2.16); the examples are in
[examples/frameworks](https://github.com/laurells/staal/tree/main/examples/frameworks).

**Platforms.** Python 3.8 or newer, on Linux (x86_64 and arm64, glibc 2.28 or newer)
and macOS (Apple Silicon, macOS 11 or newer; Intel, macOS 10.12 or newer), from
0.24.0. Version 0.23.0 has Linux x86_64 and macOS 15 wheels only. Elsewhere, build
from source.

**Status.** A preview: tested on all four platforms above, with no independent
security review yet.
The [tutorial](https://github.com/laurells/staal/blob/main/docs/TUTORIAL.md) takes
about an hour.

**Licence.** Apache-2.0 WITH LLVM-exception. The package contains staalc and its Rust
dependencies, whose licences are in `LICENSE-THIRD-PARTY.txt`.
