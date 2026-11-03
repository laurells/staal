"""staal.Tools as an in-process MCP server for the Claude Agent SDK
(pip install "staal[claude-agent-sdk]").

    from claude_agent_sdk import ClaudeAgentOptions, query
    from staal.claude_agent_sdk import allowed_tools, mcp_server

    server = mcp_server([refund], context=customer)       # one server per session
    options = ClaudeAgentOptions(mcp_servers={"staal": server},
                                 allowed_tools=allowed_tools("staal", [refund]))
    async for message in query(prompt=..., options=options): ...

The Agent SDK gives a tool only its arguments, so the app's context for the session
(the signed-in customer, the tenant) is bound when the server is made: make one per
session, from the app's own records, never from anything the model wrote. Each call
runs the Staal program with the grants the app's policy gives for that context.

The model sees each tool as `mcp__<server name>__<tool name>`, with the tool's
description, argument schema and annotations. The SDK checks the arguments against
the schema before the call, and Staal checks them again: the runtime's check is the
one that decides. A refusal or a failure is returned to the model as an error result
with Staal's message, so the model can correct itself; nothing is retried.
"""

import asyncio

from claude_agent_sdk import SdkMcpTool, ToolAnnotations, create_sdk_mcp_server

_HINTS = ("title", "readOnlyHint", "destructiveHint", "idempotentHint", "openWorldHint")


def _sdk_tool(tool, context):
    schema = dict(tool.schema)
    # The SDK takes a dict with "type" and "properties" as a JSON Schema and anything
    # else as a map of parameter names to Python types, so a function with no arguments
    # must still say it has none.
    schema.setdefault("properties", {})

    async def handler(arguments):
        # A run waits on services and models: off the event loop.
        result = await asyncio.to_thread(tool, arguments, context=context)
        return {"content": [{"type": "text", "text": result.text}], "is_error": result.is_error}

    hints = {k: tool.annotations[k] for k in _HINTS if k in tool.annotations}
    return SdkMcpTool(name=tool.name, description=tool.description, input_schema=schema, handler=handler,
                      annotations=ToolAnnotations(**hints))


def mcp_server(tools, *, name="staal", context=None):
    """`tools` (staal.Tool) as an in-process MCP server for ClaudeAgentOptions.mcp_servers.
    `context` is what each tool's grants (and scope) function receives, for every call
    made through this server."""
    if not isinstance(tools, (list, tuple)) or not tools:
        raise TypeError("mcp_server takes a list of staal.Tool")
    names = set()
    for t in tools:
        if not callable(t) or not isinstance(getattr(t, "name", None), str):
            raise TypeError("mcp_server takes a list of staal.Tool")
        if t.name in names:
            raise TypeError(f"two tools are named {t.name!r}")
        names.add(t.name)
    return create_sdk_mcp_server(name, tools=[_sdk_tool(t, context) for t in tools])


def allowed_tools(name, tools):
    """The names the Agent SDK gives `tools` served under `name`, for allowed_tools."""
    return [f"mcp__{name}__{t.name}" for t in tools]
