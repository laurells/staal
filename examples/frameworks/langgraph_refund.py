"""A support agent on LangGraph that refunds through Staal: the app's policy sets each
customer's limit, and Staal enforces it, whatever the model asks."""
import os

import staal
from langgraph.graph import START, MessagesState, StateGraph
from langgraph.prebuilt import ToolNode, tools_condition
from staal.langgraph import structured_tool

HERE = os.path.dirname(os.path.abspath(__file__))
LIMITS = {"gold": "500.00", "standard": "50.00"}  # USD per refund, decided by the app

refund = staal.Tool(os.path.join(HERE, "refund.staal"), name="refund_order",
                    description="Refund part of an order to the customer's original payment.",
                    grants=lambda customer: {"pay": {"cap": "payments.refund", "uses": 1,
                                                     "each": {"amount": LIMITS[customer["tier"]], "currency": "USD"}}},
                    journals=os.environ.get("STAAL_JOURNALS", os.path.join(HERE, "runs")))
tools = [structured_tool(refund)]


def graph(model):
    """model: any LangChain chat model that can call tools."""
    llm = model.bind_tools(tools)
    g = StateGraph(MessagesState)
    g.add_node("model", lambda state: {"messages": [llm.invoke(state["messages"])]})
    g.add_node("tools", ToolNode(tools))
    g.add_edge(START, "model")
    g.add_conditional_edges("model", tools_condition)
    g.add_edge("tools", "model")
    return g.compile()


if __name__ == "__main__":
    from langchain_openai import ChatOpenAI  # or any chat model that can call tools

    out = graph(ChatOpenAI()).invoke({"messages": [("user", "Order 4417 arrived broken. Please refund 35 USD.")]},
                                     config={"configurable": {"staal_context": {"tier": "standard"}}})
    print(out["messages"][-1].content)
