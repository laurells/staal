"""A staal.Tool as a LangChain structured tool, for LangGraph's ToolNode and agents
(pip install "staal[langgraph]").

    from langgraph.prebuilt import ToolNode
    from staal.langgraph import structured_tool

    tools = ToolNode([structured_tool(refund)])
    graph.invoke(state, config={"configurable": {"staal_context": customer}})

The model sees the tool's name, description and argument schema. Each call runs the
Staal program with the grants the app's policy gives for the call's context: by
default `config["configurable"]["staal_context"]`, or whatever `context=` returns for
the call's RunnableConfig. A refusal or a failure is returned to the model as the
tool's output, the broker's message included, so the model can correct itself.
"""

import asyncio

from langchain_core.runnables import RunnableConfig
from langchain_core.tools import StructuredTool


def structured_tool(tool, *, context=None):
    """`tool` as a langchain_core StructuredTool. `context` maps the call's
    RunnableConfig to the context the tool's grants function receives."""
    pick = context or (lambda config: (config or {}).get("configurable", {}).get("staal_context"))
    schema = {k: v for k, v in tool.schema.items() if k != "$schema"}
    schema.setdefault("title", tool.name)
    schema.setdefault("description", tool.description)

    def call(config: RunnableConfig, **arguments):
        return tool(arguments, context=pick(config)).text

    async def acall(config: RunnableConfig, **arguments):
        # A run waits on services and models: off the event loop.
        result = await asyncio.to_thread(tool, arguments, context=pick(config))
        return result.text

    return StructuredTool(
        name=tool.name,
        description=tool.description,
        args_schema=schema,
        func=call,
        coroutine=acall,
    )
