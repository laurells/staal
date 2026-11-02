"""A support agent on the OpenAI Agents SDK that refunds through Staal: the app's
policy sets each customer's limit, and Staal enforces it, whatever the model asks."""
import os

import staal
from agents import Agent, Runner
from staal.openai_agents import function_tool

HERE = os.path.dirname(os.path.abspath(__file__))
LIMITS = {"gold": "500.00", "standard": "50.00"}  # USD per refund, decided by the app

refund = staal.Tool(os.path.join(HERE, "refund.staal"), name="refund_order",
                    description="Refund part of an order to the customer's original payment.",
                    grants=lambda customer: {"pay": {"cap": "payments.refund", "uses": 1,
                                                     "each": {"amount": LIMITS[customer["tier"]], "currency": "USD"}}},
                    journals=os.environ.get("STAAL_JOURNALS", os.path.join(HERE, "runs")))


def agent(model=None):
    return Agent(name="Support", instructions="Refund what the customer is owed, within the tool's limits.",
                 tools=[function_tool(refund)], model=model)


if __name__ == "__main__":
    result = Runner.run_sync(agent(), "Order 4417 arrived broken. Please refund 35 USD.", context={"tier": "standard"})
    print(result.final_output)
