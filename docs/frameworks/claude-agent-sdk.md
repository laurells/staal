# Staal with the Claude Agent SDK

This page builds a support agent on the [Claude Agent
SDK](https://docs.claude.com/en/docs/agent-sdk/overview) that can refund an order, and
can never refund more than your app allows for the customer in front of it. Claude
decides what to ask for. Staal decides what happens: a refund over the customer's
limit is refused before any payment service is called, whatever the prompt said, and
Claude is told why.

It takes about fifteen minutes, in Python, with a TypeScript version at the end. You
need Python 3.10 or newer and an [Anthropic API key](https://console.anthropic.com).
No payment account is needed: with none configured, Staal uses a simulated payment
service. Every file and output on this page is checked by the test suite
(`python/tests/test_tutorials.py`), so what you see is what you will get.

This page is for `staal` 0.26.0 or newer, the first release with this adapter.

## 1. Install

```sh
python3 -m venv .venv
. .venv/bin/activate
pip install "staal[claude-agent-sdk]"
```

That installs Staal (its compiler and runtime included, no Rust needed) and the Claude
Agent SDK, which brings the Claude Code CLI it runs its agent loop in.

## 2. The part that moves money

Save this as `refund.staal`. It is the one place money moves: a function that takes an
order and an amount, and a capability to refund that the caller must grant.

```staal file=refund.staal
// Refunds an order, never by more than the app grants for the customer.
fn main(order: Text, amount: Money, pay: cap payments.refund(each: 500 USD)) -> Text {
    let receipt = pay.refund(order, amount)
    return "Refunded " + to_text(amount) + " for order " + order
}
```

`each: 500 USD` is the program's own ceiling: the most it will ever ask for in one
refund, whoever grants it. Your app grants each call its own limit, which can only be
lower, and the runtime holds every refund to the smaller of the two.

## 3. Make it a tool

Save this as `tools.py`. It turns the program into a tool, with your app's policy as
its grants: a gold customer may be refunded up to 500 USD at a time, anyone else up to
50.

```python file=tools.py
import staal

LIMITS = {"gold": "500.00", "standard": "50.00"}  # USD per refund, decided by the app


def grants(customer):
    # A tier the policy does not name gets no refund at all, and says why.
    limit = LIMITS.get(customer.get("tier")) if isinstance(customer, dict) else None
    if limit is None:
        raise PermissionError(f"no refund policy for this customer: {customer!r}")
    return {"pay": {"cap": "payments.refund", "uses": 1,
                    "each": {"amount": limit, "currency": "USD"}}}


refund = staal.Tool("refund.staal", name="refund_order",
                    description="Refund part of an order to the customer's original payment.",
                    grants=grants, journals="runs")
```

`grants` is called for each call with the customer, which only your app supplies; it is
never an argument the model can fill in. A customer whose tier the policy does not name
is refused outright, rather than given some default. `each` is the most one refund may
be, and `uses: 1` allows one refund per call.
`journals="runs"` is the folder where every call's journal is kept, so a run a crash
interrupts is finished when the folder is next opened, without refunding twice.

Before any model is involved, try it. Save this as `try_it.py`:

```python file=try_it.py
from tools import refund

for amount in ["80.00", "35.00"]:
    result = refund({"order": "4417", "amount": {"amount": amount, "currency": "USD"}},
                    context={"tier": "standard"})
    print(result.status, result.text)
refund.close()
```

and run it with `python try_it.py`:

```text output=try_it.py
failed 'payments.refund' allows at most 50.00 USD per action; this action needs 80.00 USD
done "Refunded 35.00 USD for order 4417"
```

The first refund was refused by Staal's broker, before the payment service was called;
the second was made. `result.text` is what a model is shown: the refusal's message, or
the result as JSON (hence the quotes).

## 4. Limits across calls

`each` and `uses` hold within one call. A model can call a tool again, so two refunds of
35 USD each are two calls, each within its limits. Two more settings hold across calls:

- `once` makes a repeated call one operation: the same run, with the same answer.
- `quota` bounds what all calls together may do within a window of time: how many
  calls, and how much money. Each call reserves its worst case (here its grant's
  `each`) until it ends, so calls running at the same moment cannot overspend together.

Save this as `limits.py`:

```python file=limits.py
import staal

from tools import grants

refund = staal.Tool("refund.staal", name="refund_order",
                    description="Refund part of an order to the customer's original payment.",
                    grants=grants, journals="runs-limits",
                    once={"by": ["order", "amount"]},
                    quota={"window": "24h", "money": {"amount": "100.00", "currency": "USD"}})

for amount in ["35.00", "35.00", "40.00", "20.00"]:
    result = refund({"order": "4417", "amount": {"amount": amount, "currency": "USD"}},
                    context={"tier": "standard"})
    print(result.status, result.text)
refund.close()
```

and run it with `python limits.py`:

```text output=limits.py
done "Refunded 35.00 USD for order 4417"
done "Refunded 35.00 USD for order 4417"
done "Refunded 40.00 USD for order 4417"
not_run the quota of 100.00 USD per 1d cannot hold this call, which can spend up to 50.00 USD; nothing was done; enough frees up at <time>
```

(`<time>` stands for the moment enough of the quota frees up, a day after the first
refund.) The second 35 USD call is the first one again: the same run, and no second
refund. The
40 USD refund is a new operation, and fits. The 20 USD one would too by its amount, but
it reserves its worst case, 50 USD, and 75 USD is already spent of the 100 USD quota, so
it is refused before anything runs.

Choose `once`'s key with care: it says which calls are the same operation. Keyed by the
order alone, a corrected request (80 USD refused, then 35 USD) would be refused as the
same operation with different arguments. A key the app gives each refund request, such
as a ticket id passed as an argument, is the most exact. The quota here is the tool's,
across every customer; a limit per customer belongs in the grants your app computes.

## 5. The agent

Save this as `agent.py`:

```python file=agent.py
import asyncio

from claude_agent_sdk import ClaudeAgentOptions, ResultMessage, ToolResultBlock, UserMessage, query
from staal.claude_agent_sdk import allowed_tools, mcp_server

from tools import refund


def text(block):
    content = block.content or []
    return content if isinstance(content, str) else "".join(c.get("text", "") for c in content)


async def support(customer, prompt):
    options = ClaudeAgentOptions(
        system_prompt="You are a support agent. Refund what the customer is owed, within the tool's limits.",
        # One server per session, made with this customer, from the app's own records.
        mcp_servers={"staal": mcp_server([refund], context=customer)},
        allowed_tools=allowed_tools("staal", [refund]),
        tools=[],  # no built-in tools: no shell, no files, only the refund
        max_turns=6,
    )
    async for message in query(prompt=prompt, options=options):
        if isinstance(message, UserMessage) and isinstance(message.content, list):
            for block in message.content:
                if isinstance(block, ToolResultBlock):
                    print("refused:" if block.is_error else "done:", text(block))
        elif isinstance(message, ResultMessage):
            print("Claude:", message.result)


asyncio.run(support({"tier": "standard"}, "Order 4417 arrived broken. It cost 80 USD. Please refund it."))
refund.close()
```

The Agent SDK gives a tool only its arguments, so the customer is bound when the
server is made: make one per session. `allowed_tools` lets Claude use the refund
without asking, and `tools=[]` leaves it nothing else.

Run it with your key:

```sh
ANTHROPIC_API_KEY=sk-ant-... python agent.py
```

Which refunds Claude asks for is up to Claude; what Staal answers is not. If Claude
first asks for the full 80 USD and then for 35 USD, the app prints Staal's two answers,
then Claude's reply:

```text output=agent.py
refused: 'payments.refund' allows at most 50.00 USD per action; this action needs 80.00 USD
done: "Refunded 35.00 USD for order 4417"
```

Whatever Claude asks for, a standard customer is never refunded more than 50 USD at a
time. Change `{"tier": "standard"}` to `{"tier": "gold"}` and the 80 USD refund is made.

## 6. In TypeScript

The same agent with `@anthropic-ai/claude-agent-sdk`, for Node.js 18 or newer:

```sh
npm install @staal/node @anthropic-ai/claude-agent-sdk @modelcontextprotocol/sdk zod
```

Save `refund.staal` as in step 2, and this as `agent.mjs`:

```js file=agent.mjs
import { query } from '@anthropic-ai/claude-agent-sdk';
import staal from '@staal/node';
import { allowedTools, mcpServer } from '@staal/node/claude-agent-sdk';

const LIMITS = { gold: '500.00', standard: '50.00' }; // USD per refund, decided by the app

const refund = await staal.Tool.open('refund.staal', {
  name: 'refund_order',
  description: "Refund part of an order to the customer's original payment.",
  grants: (customer) => {
    // A tier the policy does not name gets no refund at all, and says why.
    const limit = Object.hasOwn(LIMITS, customer?.tier) ? LIMITS[customer.tier] : undefined;
    if (limit === undefined) throw new Error(`no refund policy for this customer: ${JSON.stringify(customer)}`);
    return { pay: { cap: 'payments.refund', uses: 1, each: { amount: limit, currency: 'USD' } } };
  },
  journals: 'runs',
});

const text = (block) => (typeof block.content === 'string' ? block.content : (block.content || []).map((c) => c.text || '').join(''));

const customer = { tier: 'standard' }; // from the app's own records
for await (const message of query({
  prompt: 'Order 4417 arrived broken. It cost 80 USD. Please refund it.',
  options: {
    systemPrompt: "You are a support agent. Refund what the customer is owed, within the tool's limits.",
    mcpServers: { staal: mcpServer([refund], { context: customer }) }, // one per session
    allowedTools: allowedTools('staal', [refund]),
    tools: [], // no built-in tools: no shell, no files, only the refund
    maxTurns: 6,
  },
})) {
  if (message.type === 'user' && Array.isArray(message.message.content)) {
    for (const block of message.message.content) {
      if (block.type === 'tool_result') console.log(block.is_error ? 'refused:' : 'done:', text(block));
    }
  } else if (message.type === 'result') {
    console.log('Claude:', message.result);
  }
}
await refund.close();
```

and run it with `ANTHROPIC_API_KEY=sk-ant-... node agent.mjs`. It prints the same
answers from Staal as the Python agent does.

## What Claude cannot change

The tool is not a set of instructions Claude is asked to follow. Claude chooses when to
call it and with which arguments; whatever it chooses, and whatever the prompt or a
customer's message says, it cannot change:

- who the customer is, or their tier: those come from your app's records;
- the grants: what may be refunded, how much at a time (`each`), and how often (`uses`);
- the program's ceiling (`each: 500 USD`) and the services it may reach;
- the limits across calls (`once`, `quota`);
- the agent's other tools: with `tools=[]` it has none.

Claude can misread the policy, argue with it, or try to exceed it, and the refund is
still refused before any payment service is called.

## What is and isn't covered

What Staal guarantees here: no refund over the customer's limit is made, whatever the
model asks or the prompt says, and the customer's limit comes only from your app. Every
call is journaled before it acts. If a crash comes after a refund was sent and before
its answer was recorded, nobody can know from the app alone whether it happened, so
Staal never sends it again blind. Every refund carries an idempotency key unique to its
run and step: when the payment service deduplicates by it, as payment providers do, the
refund is sent again with the same key and the service answers with the original
result; otherwise the run stops for a person to check the service's own records and
record what happened (`staalc resolve`).

What it doesn't: which refunds Claude decides to ask for, or what it says to the
customer. That is the model's part, and you shape it with the system prompt. Sections 5
and 6 of [docs/MCP.md](../MCP.md) give `once` and `quota` in full; they apply to tools in
your app as they do to `staalc mcp`.
