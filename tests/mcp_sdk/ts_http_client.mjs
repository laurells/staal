// The official MCP TypeScript SDK (v2, @modelcontextprotocol/client) against
// `staalc mcp` over Streamable HTTP with OAuth (docs/MCP.md section 12.7, the release
// gate).
//
//   NODE_EXTRA_CA_CERTS=<ca.pem> node ts_http_client.mjs <url> <issuer> <mode>
//
// The SDK finds the authorization server from the server's 401 and its protected
// resource metadata, gets a token with the client_credentials grant, and lists and
// calls tools. mode is "auto" (the SDK probes with server/discover) or a version to
// pin, such as "2026-07-28". Exits 0 and prints "ok" when every check passes.

import { Client, ClientCredentialsProvider, StreamableHTTPClientTransport } from '@modelcontextprotocol/client';

const [url, issuer, mode] = process.argv.slice(2);

function check(cond, what) {
  if (!cond) {
    console.error(`FAILED: ${what}`);
    process.exit(1);
  }
}

const authProvider = new ClientCredentialsProvider({
  clientId: 'sdk-client',
  clientSecret: 'sdk-secret',
  scope: 'tools refunds:write',
  expectedIssuer: issuer,
});
const negotiation = mode === 'auto' ? mode : { pin: mode };
const client = new Client({ name: 'staal-conformance', version: '1.0.0' }, { versionNegotiation: { mode: negotiation } });
await client.connect(new StreamableHTTPClientTransport(new URL(url), { authProvider }));

const tools = await client.listTools();
const names = tools.tools.map((t) => t.name);
check(names.includes('refund_order') && names.includes('staal_run_status'), `tool names ${names}`);

const ok = await client.callTool({ name: 'refund_order', arguments: { order: '4417', amount: { amount: '35.00', currency: 'USD' } } });
check(ok.isError === false, `a refund within the limit: ${JSON.stringify(ok)}`);
check(JSON.stringify(ok.structuredContent) === JSON.stringify({ result: 'Refunded 35.00 USD for order 4417' }), 'structured content');

const refused = await client.callTool({ name: 'refund_order', arguments: { order: '4418', amount: { amount: '300.00', currency: 'USD' } } });
check(refused.isError === true, 'a refund over the limit is refused');
check(refused.content[0].text.includes('allows at most 50.00 USD per action'), `the broker's message: ${refused.content[0].text}`);

const unknown = await client.callTool({ name: 'staal_run_status', arguments: { run: '0'.repeat(32) } });
check(unknown.structuredContent.status === 'unknown', 'status');

await client.close();
console.log('ok');
