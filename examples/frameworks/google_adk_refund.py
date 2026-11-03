"""A support agent on Google's Agent Development Kit that refunds through Staal: the
app's policy sets each customer's limit, and Staal enforces it, whatever the model asks."""
import asyncio
import os

import staal
from google.adk.agents import Agent
from google.adk.runners import InMemoryRunner
from google.genai import types
from staal.google_adk import adk_tool

HERE = os.path.dirname(os.path.abspath(__file__))
LIMITS = {"gold": "500.00", "standard": "50.00"}  # USD per refund, decided by the app

refund = staal.Tool(os.path.join(HERE, "refund.staal"), name="refund_order",
                    description="Refund part of an order to the customer's original payment.",
                    grants=lambda customer: {"pay": {"cap": "payments.refund", "uses": 1,
                                                     "each": {"amount": LIMITS[customer["tier"]], "currency": "USD"}}},
                    journals=os.environ.get("STAAL_JOURNALS", os.path.join(HERE, "runs")))


def agent(model):
    return Agent(name="support", model=model, instruction="Refund what the customer is owed, within the tool's limits.",
                 tools=[adk_tool(refund)])


async def support(model, prompt, customer):
    """The agent's final answer. The customer comes from the app's own records, and
    reaches the tool through the session's state."""
    runner = InMemoryRunner(agent=agent(model), app_name="support")
    session = await runner.session_service.create_session(app_name="support", user_id="customer",
                                                          state={"staal_context": customer})
    answer = None
    async for event in runner.run_async(user_id="customer", session_id=session.id,
                                        new_message=types.Content(role="user", parts=[types.Part(text=prompt)])):
        if event.is_final_response() and event.content and event.content.parts:
            answer = "".join(p.text or "" for p in event.content.parts)
    return answer


if __name__ == "__main__":
    # A Gemini model, with GOOGLE_API_KEY set; the model's name in STAAL_EXAMPLE_MODEL.
    print(asyncio.run(support(os.environ["STAAL_EXAMPLE_MODEL"], "Order 4417 arrived broken. Please refund 35 USD.",
                              {"tier": "standard"})))
    refund.close()
