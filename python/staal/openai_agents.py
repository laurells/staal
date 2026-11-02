"""A staal.Tool as a tool of the OpenAI Agents SDK (pip install "staal[openai-agents]").

    from agents import Agent
    from staal.openai_agents import function_tool

    agent = Agent(name="support", tools=[function_tool(refund)])

The model sees the tool's name, description and argument schema. Each call runs the
Staal program with the grants the app's policy gives for the run's context (the
`context` passed to Runner.run, unless `context=` says otherwise). A refusal or a
failure is returned to the model as the tool's output, the broker's message
included, so the model can correct itself; nothing is retried by the adapter.
"""

import asyncio
import json

from agents import FunctionTool


def _schema(tool):
    # The schema as Staal describes it, without its "$schema" (the API takes the
    # 2020-12 dialect as its default).
    schema = {k: v for k, v in tool.schema.items() if k != "$schema"}
    return schema


def function_tool(tool, *, context=None):
    """`tool` as an agents.FunctionTool. `context` maps the run's context wrapper to
    the context the tool's grants function receives (default: its `.context`)."""
    pick = context or (lambda wrapper: wrapper.context)

    async def invoke(ctx, arguments):
        try:
            args = json.loads(arguments) if arguments else {}
        except ValueError as e:
            return f"the arguments are not JSON: {e}"
        if not isinstance(args, dict):
            return "the arguments must be a JSON object"
        # A run waits on services and models: off the event loop.
        result = await asyncio.to_thread(tool, args, context=pick(ctx))
        return result.text

    return FunctionTool(
        name=tool.name,
        description=tool.description,
        params_json_schema=_schema(tool),
        on_invoke_tool=invoke,
        # Staal's own check of the arguments is the one that decides; the schema is
        # given as it is rather than rewritten into the API's strict subset.
        strict_json_schema=False,
    )
