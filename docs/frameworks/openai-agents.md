# Staal with the OpenAI Agents SDK

This page builds a support agent on the [OpenAI Agents
SDK](https://openai.github.io/openai-agents-python/) that can refund an order, and can
never refund more than your app allows for the customer in front of it. The model
decides what to ask for. Staal decides what happens: a refund over the customer's
limit is refused before any payment service is called, whatever the prompt said, and
the model is told why.

It takes about fifteen minutes, in Python, with a TypeScript version at the end. You
need Python 3.10 or newer and an [OpenAI API key](https://platform.openai.com/api-keys).
No payment account is needed: with none configured, Staal uses a simulated payment
service. Every file and output on this page is checked by the test suite
(`python/tests/test_tutorials.py`), so what you see is what you will get.

This page is for `staal` 0.25.0 or newer.

## 1. Install

```sh
python3 -m venv .venv
. .venv/bin/activate
pip install "staal[openai-agents]"
```

That installs Staal (its compiler and runtime included, no Rust needed) and the OpenAI
Agents SDK.

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
interrupts is finished when the folder is next opened, and a refund it may already
have sent is never sent again blind (the end of this page says how).

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
from agents import Agent, Runner, ToolCallOutputItem
from staal.openai_agents import function_tool

from tools import refund

agent = Agent(
    name="Support",
    instructions="You are a support agent. Refund what the customer is owed, within the tool's limits.",
    tools=[function_tool(refund)],
)

# The customer comes from the app's own records, and reaches the tool as the run's context.
result = Runner.run_sync(agent, "Order 4417 arrived broken. It cost 80 USD. Please refund it.",
                         context={"tier": "standard"})
for item in result.new_items:
    if isinstance(item, ToolCallOutputItem):
        print("tool:", item.output)
print("Agent:", result.final_output)
refund.close()
```

`function_tool` gives the model the tool's name, description and argument schema. The
run's `context` is what the tool's grants function receives; it is never sent to the
model, so the model cannot claim to be a gold customer.

Run it with your key:

```sh
OPENAI_API_KEY=sk-... python agent.py
```

Which refunds the model asks for is up to the model; what Staal answers is not. If the
model first asks for the full 80 USD and then for 35 USD, the app prints Staal's two
answers, as the model was given them, then the model's reply:

```text output=agent.py
tool: 'payments.refund' allows at most 50.00 USD per action; this action needs 80.00 USD
tool: "Refunded 35.00 USD for order 4417"
```

Whatever the model asks for, a standard customer is never refunded more than 50 USD at
a time. Change `{"tier": "standard"}` to `{"tier": "gold"}` and the 80 USD refund is made.

## 6. In TypeScript

The same agent with `@openai/agents`, for Node.js 18 or newer:

```sh
npm install @staal/node @openai/agents zod
```

Save `refund.staal` as in step 2, and this as `agent.mjs`:

```js file=agent.mjs
import { Agent, run } from '@openai/agents';
import staal from '@staal/node';
import { functionTool } from '@staal/node/openai-agents';

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

const agent = new Agent({
  name: 'Support',
  instructions: "You are a support agent. Refund what the customer is owed, within the tool's limits.",
  tools: [functionTool(refund)],
});

// The customer comes from the app's own records, and reaches the tool as the run's context.
const result = await run(agent, 'Order 4417 arrived broken. It cost 80 USD. Please refund it.', {
  context: { tier: 'standard' },
});
for (const item of result.newItems) {
  if (item.type === 'tool_call_output_item') console.log('tool:', item.output);
}
console.log('Agent:', result.finalOutput);
await refund.close();
```

and run it with `OPENAI_API_KEY=sk-... node agent.mjs`. It prints the same answers from
Staal as the Python agent does.

## What the model cannot change

The tool is not a set of instructions the model is asked to follow. The model chooses
when to call it and with which arguments; whatever it chooses, and whatever the prompt
or a customer's message says, it cannot change:

- who the customer is, or their tier: those come from your app's records;
- the grants: what may be refunded, how much at a time (`each`), and how often (`uses`);
- the program's ceiling (`each: 500 USD`) and the services it may reach;
- the limits across calls (`once`, `quota`).

The model can misread the policy, argue with it, or try to exceed it, and the refund is
still refused before any payment service is called. The agent's other tools are yours
to choose: this one has only the refund.

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

What it doesn't: which refunds the model decides to ask for, or what it says to the
customer. That is the model's part, and you shape it with the agent's instructions.
Sections 5 and 6 of [docs/MCP.md](../MCP.md) give `once` and `quota` in full; they apply
to tools in your app as they do to `staalc mcp`.
