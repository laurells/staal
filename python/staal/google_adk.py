"""A staal.Tool as a tool of Google's Agent Development Kit (pip install "staal[google-adk]").

    from google.adk.agents import Agent
    from staal.google_adk import adk_tool

    agent = Agent(name="support", model="gemini-...", tools=[adk_tool(refund)])
    session = await runner.session_service.create_session(
        app_name=..., user_id=..., state={"staal_context": customer})

The model sees the tool's name, description and argument schema (as JSON Schema,
without its "$schema"). Each call runs the Staal program with the grants the app's
policy gives for the call's context: by default the session's state under
"staal_context", which the app sets when it creates the session, from its own records;
or whatever `context=` returns for the call's ToolContext. Session state is the app's:
the model cannot write it, but the app's other tools can, so nothing but the app
should write that key.

The answer goes back to the model as ADK's tools do: {"result": <text>} for a run
that was done, and {"error": <text>} for a refusal or a failure, with Staal's message,
so the model can correct itself. Nothing is retried, and nothing is raised into the
agent loop.
"""

import asyncio

from google.adk.tools.base_tool import BaseTool
from google.genai import types


def _from_state(tool_context):
    return tool_context.state.get("staal_context")


class StaalTool(BaseTool):
    """An ADK tool that runs a staal.Tool."""

    def __init__(self, tool, *, context=None):
        if not callable(tool) or not isinstance(getattr(tool, "name", None), str):
            raise TypeError("adk_tool takes a staal.Tool")
        if context is not None and not callable(context):
            raise TypeError("adk_tool: context is a function of the ToolContext")
        super().__init__(name=tool.name, description=tool.description)
        self._tool = tool
        self._context = context or _from_state
        self._schema = {k: v for k, v in tool.schema.items() if k != "$schema"}

    def _get_declaration(self):
        return types.FunctionDeclaration(name=self.name, description=self.description,
                                         parameters_json_schema=self._schema)

    async def run_async(self, *, args, tool_context):
        try:
            context = self._context(tool_context)
            # A run waits on services and models: off the event loop.
            result = await asyncio.to_thread(self._tool, args or {}, context=context)
        except Exception as e:  # the call reached no answer: told to the model, as ADK's MCP tools do
            return {"error": f"the tool could not be called: {e}"}
        return {"error": result.text} if result.is_error else {"result": result.text}


def adk_tool(tool, *, context=None):
    """`tool` (a staal.Tool) as an ADK tool. `context` maps the call's ToolContext to
    the context the tool's grants (and scope) function receives."""
    return StaalTool(tool, context=context)
