# The official MCP Python SDK against `staalc mcp` (docs/MCP.md section 9).
#
#   python3 python_client.py <staalc> <manifest.json> <mode>
#
# mode is "legacy" (the 2025-11-25 initialize handshake), "auto" (the SDK probes with
# server/discover) or a modern version such as "2026-07-28". Exits 0 and prints "ok"
# when every check passes.

import asyncio
import sys

from mcp import Client, StdioServerParameters


def check(cond, what):
    if not cond:
        raise SystemExit(f"FAILED: {what}")


async def main(staalc, manifest, mode):
    server = StdioServerParameters(command=staalc, args=["mcp", manifest])
    async with Client(server, mode=mode) as client:
        tools = await client.list_tools()
        names = [t.name for t in tools.tools]
        check(names == ["refund_order", "staal_run_status"], f"tool names {names}")
        refund = tools.tools[0]
        check(refund.input_schema["required"] == ["order", "amount"], "the input schema")
        check(refund.annotations.read_only_hint is False, "annotations")

        ok = await client.call_tool("refund_order", {"order": "4417", "amount": {"amount": "35.00", "currency": "USD"}})
        check(ok.is_error is False, f"a refund within the limit: {ok}")
        check(ok.structured_content == {"result": "Refunded 35.00 USD for order 4417"}, f"structured {ok.structured_content}")

        refused = await client.call_tool("refund_order", {"order": "4418", "amount": {"amount": "300.00", "currency": "USD"}})
        check(refused.is_error is True, "a refund over the limit is refused")
        text = refused.content[0].text
        check("allows at most 50.00 USD per action" in text, f"the broker's message: {text}")

        unknown = await client.call_tool("staal_run_status", {"run": "0" * 32})
        check(unknown.structured_content["status"] == "unknown", f"status {unknown.structured_content}")
    print("ok")


if __name__ == "__main__":
    asyncio.run(main(*sys.argv[1:4]))
