// staal Tools as an in-process MCP server for the Claude Agent SDK
// (@anthropic-ai/claude-agent-sdk, with @modelcontextprotocol/sdk 1.x, which the app
// installs; @staal/node depends on neither). The Agent SDK is published as ES modules
// only, so this adapter is one too.
//
//   import { query } from '@anthropic-ai/claude-agent-sdk';
//   import { allowedTools, mcpServer } from '@staal/node/claude-agent-sdk';
//   for await (const message of query({ prompt, options: {
//     mcpServers: { staal: mcpServer([refund], { context: customer }) },   // one per query
//     allowedTools: allowedTools('staal', [refund]),
//     tools: [],
//   } })) { ... }
//
// The Agent SDK gives a tool only its arguments, so the app's context for the session
// (the signed-in customer, the tenant) is bound when the server is made: make one per
// query, from the app's own records, never from anything the model wrote. Each call
// runs the Staal program with the grants the app's policy gives for that context.
//
// The server is a plain MCP server (the MCP SDK's low-level Server), so the program's
// argument schema reaches the model as JSON Schema, unconverted, and Staal's own check
// of the arguments is the one that decides. A refusal, a failure or an unknown tool is
// a tool result with isError, with Staal's message, so the model can correct itself;
// nothing is retried. The model sees each tool as `mcp__<name>__<tool name>`.

import { Server } from '@modelcontextprotocol/sdk/server/index.js';
import { CallToolRequestSchema, ListToolsRequestSchema } from '@modelcontextprotocol/sdk/types.js';

const HINTS = ['title', 'readOnlyHint', 'destructiveHint', 'idempotentHint', 'openWorldHint'];

function checked(tools) {
  if (!Array.isArray(tools) || tools.length === 0) throw new TypeError('staal: mcpServer takes a list of Tools from Tool.open');
  const byName = new Map();
  for (const t of tools) {
    if (!t || typeof t.call !== 'function' || typeof t.name !== 'string') throw new TypeError('staal: mcpServer takes a list of Tools from Tool.open');
    if (byName.has(t.name)) throw new TypeError(`staal: mcpServer: two tools are named '${t.name}'`);
    byName.set(t.name, t);
  }
  return byName;
}

/**
 * `tools` as an in-process MCP server for the Agent SDK's `mcpServers` option.
 * `options.context` is what each tool's grants (and scope) function receives, for
 * every call made through this server; `options.name` (default "staal") must be the
 * key the server is given in `mcpServers`.
 */
export function mcpServer(tools, options = {}) {
  const byName = checked(tools);
  const name = options.name ?? 'staal';
  if (typeof name !== 'string' || !/^[A-Za-z0-9_-]+$/.test(name)) throw new TypeError('staal: mcpServer: name is letters, digits, _ and -');
  const context = options.context;
  const server = new Server({ name, version: '1.0.0' }, { capabilities: { tools: {} } });
  const listed = [...byName.values()].map((t) => {
    const annotations = {};
    for (const k of HINTS) if (t.annotations && k in t.annotations) annotations[k] = t.annotations[k];
    return { name: t.name, description: t.description, inputSchema: t.schema, annotations };
  });
  server.setRequestHandler(ListToolsRequestSchema, async () => ({ tools: listed }));
  server.setRequestHandler(CallToolRequestSchema, async (request) => {
    const t = byName.get(request.params.name);
    if (!t) return { content: [{ type: 'text', text: `there is no tool '${request.params.name}'` }], isError: true };
    try {
      const result = await t.call(request.params.arguments ?? {}, { context });
      return { content: [{ type: 'text', text: result.text }], isError: result.isError };
    } catch (e) {
      // The call did not reach an answer (the host is closed, the grants function
      // threw): the model is told so, as an MCP tool error, not a protocol error.
      return { content: [{ type: 'text', text: `the tool could not be called: ${e && e.message ? e.message : String(e)}` }], isError: true };
    }
  });
  return { type: 'sdk', name, instance: server };
}

/** The names the Agent SDK gives `tools` served under `name`, for `allowedTools`. */
export function allowedTools(name, tools) {
  return [...checked(tools).keys()].map((n) => `mcp__${name}__${n}`);
}
