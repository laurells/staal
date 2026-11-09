# The official MCP Python SDK against `staalc mcp` over Streamable HTTP with OAuth
# (docs/MCP.md section 12.7, the release gate).
#
#   python3 python_http_client.py <url> <ca.pem> <issuer> <mode>
#
# The SDK finds the authorization server from the server's 401 and its protected
# resource metadata, gets a token with the client_credentials grant, and lists and
# calls tools. mode is "auto" (the SDK probes with server/discover) or "2026-07-28".
# Exits 0 and prints "ok" when every check passes.

import asyncio
import ssl
import sys

import httpx2
from mcp import Client
from mcp.client.auth import TokenStorage
from mcp.client.auth.extensions.client_credentials import ClientCredentialsOAuthProvider
from mcp.client.streamable_http import streamable_http_client


def check(cond, what):
    if not cond:
        raise SystemExit(f"FAILED: {what}")


class Memory(TokenStorage):
    def __init__(self):
        self.tokens = None
        self.info = None

    async def get_tokens(self):
        return self.tokens

    async def set_tokens(self, tokens):
        self.tokens = tokens

    async def get_client_info(self):
        return self.info

    async def set_client_info(self, info):
        self.info = info


async def main(url, ca, issuer, mode):
    tls = ssl.create_default_context(cafile=ca)
    auth = ClientCredentialsOAuthProvider(
        server_url=url,
        storage=Memory(),
        client_id="sdk-client",
        client_secret="sdk-secret",
        scope="tools refunds:write",
        issuer=issuer,
    )
    async with httpx2.AsyncClient(verify=tls, auth=auth, timeout=30) as http:
        async with Client(streamable_http_client(url, http_client=http), mode=mode) as client:
            tools = await client.list_tools()
            names = [t.name for t in tools.tools]
            check("refund_order" in names and "staal_run_status" in names, f"tool names {names}")

            ok = await client.call_tool("refund_order", {"order": "4417", "amount": {"amount": "35.00", "currency": "USD"}})
            check(ok.is_error is False, f"a refund within the limit: {ok}")
            check(ok.structured_content == {"result": "Refunded 35.00 USD for order 4417"}, f"structured {ok.structured_content}")

            refused = await client.call_tool("refund_order", {"order": "4418", "amount": {"amount": "300.00", "currency": "USD"}})
            check(refused.is_error is True, "a refund over the limit is refused")
            check("allows at most 50.00 USD per action" in refused.content[0].text, f"the broker's message: {refused.content[0].text}")

            unknown = await client.call_tool("staal_run_status", {"run": "0" * 32})
            check(unknown.structured_content["status"] == "unknown", f"status {unknown.structured_content}")
    print("ok")


if __name__ == "__main__":
    asyncio.run(main(*sys.argv[1:5]))
