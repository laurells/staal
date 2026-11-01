// The official MCP TypeScript SDK (v2, @modelcontextprotocol/client) against
// `staalc mcp` (docs/MCP.md section 9).
//
//   node ts_client.mjs <staalc> <manifest.json> <mode>
//
// mode is "legacy" (the 2025-11-25 initialize handshake), "auto" (the SDK probes with
// server/discover) or a version to pin, such as "2026-07-28". Exits 0 and prints "ok"
// when every check passes.

import { Client } from '@modelcontextprotocol/client';
import { StdioClientTransport } from '@modelcontextprotocol/client/stdio';

const [staalc, manifest, mode] = process.argv.slice(2);

function check(cond, what) {
  if (!cond) {
    console.error(`FAILED: ${what}`);
    process.exit(1);
  }
}

const negotiation = mode === 'legacy' || mode === 'auto' ? mode : { pin: mode };
const client = new Client({ name: 'staal-conformance', version: '1.0.0' }, { versionNegotiation: { mode: negotiation } });
const transport = new StdioClientTransport({ command: staalc, args: ['mcp', manifest], stderr: 'inherit' });
await client.connect(transport);

const tools = await client.listTools();
const names = tools.tools.map((t) => t.name);
check(JSON.stringify(names) === JSON.stringify(['refund_order', 'staal_run_status']), `tool names ${names}`);
check(JSON.stringify(tools.tools[0].inputSchema.required) === JSON.stringify(['order', 'amount']), 'the input schema');
check(tools.tools[0].annotations.readOnlyHint === false, 'annotations');

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
