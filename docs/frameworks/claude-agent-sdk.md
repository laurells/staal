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

`each: 500 USD` is the most this program will ever ask for in one refund. Your app can
grant less, per customer, and the runtime holds every refund to the smaller of the two.

## 3. Make it a tool

Save this as `tools.py`. It turns the program into a tool, with your app's policy as
its grants: a gold customer may be refunded up to 500 USD at a time, anyone else up to
50.

```python file=tools.py
import staal

LIMITS = {"gold": "500.00", "standard": "50.00"}  # USD per refund, decided by the app


def grants(customer):
    return {"pay": {"cap": "payments.refund", "uses": 1,
                    "each": {"amount": LIMITS[customer["tier"]], "currency": "USD"}}}


refund = staal.Tool("refund.staal", name="refund_order",
                    description="Refund part of an order to the customer's original payment.",
                    grants=grants, journals="runs")
```

`grants` is called for each call with the customer, which only your app supplies; it is
never an argument the model can fill in. `uses: 1` allows one refund per call.
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

## 4. The agent

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

## 5. In TypeScript

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
  grants: (customer) => ({ pay: { cap: 'payments.refund', uses: 1, each: { amount: LIMITS[customer.tier], currency: 'USD' } } }),
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

## What is and isn't covered

What Staal guarantees here: no refund over the customer's limit is made, whatever the
model asks or the prompt says; the customer's limit comes only from your app; and every
call is journaled, so a crash never makes a refund twice. What it doesn't: which
refunds Claude decides to ask for, or what it says to the customer. That is the model's
part, and you shape it with the system prompt.

A call made twice is two refunds unless you say otherwise: give the tool
`once={"by": ["order"]}` and a repeated call for the same order is the same run, with the
same answer. Quotas across calls (calls and money per hour or day) are the tool's
`quota=`. Both are in [docs/MCP.md](../MCP.md), sections 5 and 6, which apply to tools in
your app as they do to `staalc mcp`.
