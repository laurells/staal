"""A support agent on the Claude Agent SDK that refunds through Staal: the app's policy
sets each customer's limit, and Staal enforces it, whatever the model asks."""
import asyncio
import os

import staal
from claude_agent_sdk import ClaudeAgentOptions, ResultMessage, query
from staal.claude_agent_sdk import allowed_tools, mcp_server

HERE = os.path.dirname(os.path.abspath(__file__))
LIMITS = {"gold": "500.00", "standard": "50.00"}  # USD per refund, decided by the app

refund = staal.Tool(os.path.join(HERE, "refund.staal"), name="refund_order",
                    description="Refund part of an order to the customer's original payment.",
                    grants=lambda customer: {"pay": {"cap": "payments.refund", "uses": 1,
                                                     "each": {"amount": LIMITS[customer["tier"]], "currency": "USD"}}},
                    journals=os.environ.get("STAAL_JOURNALS", os.path.join(HERE, "runs")))


def options(customer, **extra):
    """One session's options. The customer comes from the app's own records; `tools=[]`
    leaves the agent no built-in tool (no shell, no files), only the refund."""
    return ClaudeAgentOptions(
        system_prompt="Refund what the customer is owed, within the tool's limits.",
        mcp_servers={"staal": mcp_server([refund], context=customer)},
        allowed_tools=allowed_tools("staal", [refund]),
        tools=[],
        **extra,
    )


async def support(prompt, customer, **extra):
    """The agent's final answer."""
    answer = None
    async for message in query(prompt=prompt, options=options(customer, **extra)):
        if isinstance(message, ResultMessage):
            answer = message.result
    return answer


if __name__ == "__main__":
    print(asyncio.run(support("Order 4417 arrived broken. Please refund 35 USD.", {"tier": "standard"})))
    refund.close()
