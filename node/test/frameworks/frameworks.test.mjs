// The JavaScript framework adapters (openai-agents.js, langgraph.js, vercel-ai.mjs,
// claude-agent-sdk.mjs), driven by each
// framework's own agent loop with a scripted model: the model asks for a refund over
// the customer's limit and is refused with the broker's message, then asks within it
// and the refund is made. The examples in examples/frameworks/*.mjs are run as
// written (copied next to this test, beside its node_modules); only their model is
// replaced.
//
//   cd node/test/frameworks && npm ci && STAAL_NODE_ADDON=<addon> node --test frameworks.test.mjs

import { test, after } from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const frameworks = path.dirname(fileURLToPath(import.meta.url));
const nodeDir = path.resolve(frameworks, '..', '..');
const examples = path.resolve(nodeDir, '..', 'examples', 'frameworks');
const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'staal-frameworks-'));
process.env.STAAL_JOURNALS = path.join(scratch, 'runs');
after(() => fs.rmSync(scratch, { recursive: true, force: true }));

// The examples import @staal/node: this folder's node_modules gets a copy of the
// package's files, as an install would put them, so that every module of it (CommonJS
// and ES modules alike) finds the frameworks beside it. The addon is STAAL_NODE_ADDON.
const installed = path.join(frameworks, 'node_modules', '@staal', 'node');
fs.rmSync(installed, { recursive: true, force: true });
fs.mkdirSync(installed, { recursive: true });
for (const f of JSON.parse(fs.readFileSync(path.join(nodeDir, 'package.json'), 'utf8')).files.concat('package.json')) {
  const from = path.join(nodeDir, f);
  if (!f.endsWith('/') && fs.existsSync(from)) fs.copyFileSync(from, path.join(installed, f));
}
for (const f of ['refund.staal', 'openai_agents_refund.mjs', 'langgraph_refund.mjs', 'vercel_ai_refund.mjs', 'claude_agent_sdk_refund.mjs']) {
  fs.copyFileSync(path.join(examples, f), path.join(frameworks, f));
}
const load = (f) => import(pathToFileURL(path.join(frameworks, f)).href);

const REFUSED = "'payments.refund' allows at most 50.00 USD per action; this action needs 80.00 USD";
const DONE = '"Refunded 35.00 USD for order 4417"';
const ASK = [
  { order: '4417', amount: { amount: '80.00', currency: 'USD' } },
  { order: '4417', amount: { amount: '35.00', currency: 'USD' } },
];

function outputText(o) {
  if (typeof o === 'string') return o;
  if (o && typeof o.text === 'string') return o.text;
  return JSON.stringify(o);
}

test('the OpenAI Agents SDK runs the tool through its own loop', async () => {
  const { setTracingDisabled, Usage, run } = await import('@openai/agents');
  setTracingDisabled(true);
  const ex = await load('openai_agents_refund.mjs');
  const seen = { tools: null, outputs: [] };
  const model = {
    async getResponse(request) {
      seen.tools = request.tools;
      const items = Array.isArray(request.input) ? request.input : [];
      seen.outputs = items.filter((i) => i.type === 'function_call_result').map((i) => outputText(i.output));
      const step = seen.outputs.length;
      const usage = new Usage();
      if (step < ASK.length) {
        return { usage, output: [{ type: 'function_call', callId: `call_${step}`, name: 'refund_order', arguments: JSON.stringify(ASK[step]), status: 'completed' }] };
      }
      return { usage, output: [{ type: 'message', role: 'assistant', status: 'completed', content: [{ type: 'output_text', text: seen.outputs.join(' | ') }] }] };
    },
    async *getStreamedResponse() {
      throw new Error('the test does not stream');
    },
  };
  try {
    const result = await run(ex.agent(model), 'Order 4417 arrived broken. Please refund it.', { context: { tier: 'standard' } });
    assert.equal(seen.tools[0].name, 'refund_order');
    assert.ok(!('pay' in seen.tools[0].parameters.properties), 'a grant is never an argument');
    assert.deepEqual(seen.outputs, [REFUSED, DONE]);
    assert.equal(result.finalOutput, `${REFUSED} | ${DONE}`);
  } finally {
    await ex.refund.close();
  }
});

test('LangGraph runs the tool through its ToolNode', async () => {
  const { BaseChatModel } = await import('@langchain/core/language_models/chat_models');
  const { AIMessage, ToolMessage } = await import('@langchain/core/messages');
  class Scripted extends BaseChatModel {
    _llmType() {
      return 'scripted';
    }
    bindTools() {
      return this;
    }
    async _generate(messages) {
      const outputs = messages.filter((m) => ToolMessage.isInstance ? ToolMessage.isInstance(m) : m instanceof ToolMessage).map((m) => m.content);
      const step = outputs.length;
      const message =
        step < ASK.length
          ? new AIMessage({ content: '', tool_calls: [{ name: 'refund_order', args: ASK[step], id: `call_${step}`, type: 'tool_call' }] })
          : new AIMessage({ content: outputs.join(' | ') });
      return { generations: [{ message, text: typeof message.content === 'string' ? message.content : '' }] };
    }
  }
  const ex = await load('langgraph_refund.mjs');
  try {
    const out = await ex.graph(new Scripted({})).invoke(
      { messages: [{ role: 'user', content: 'Order 4417 arrived broken. Please refund it.' }] },
      { configurable: { staal_context: { tier: 'standard' } } },
    );
    const toolOutputs = out.messages.filter((m) => m.getType && m.getType() === 'tool').map((m) => m.content);
    assert.deepEqual(toolOutputs, [REFUSED, DONE]);
    assert.equal(out.messages.at(-1).content, `${REFUSED} | ${DONE}`);
    // A gold customer's limit, from the same graph's config, lets both through.
    const gold = await ex.graph(new Scripted({})).invoke(
      { messages: [{ role: 'user', content: 'Refund it.' }] },
      { configurable: { staal_context: { tier: 'gold' } } },
    );
    const goldOutputs = gold.messages.filter((m) => m.getType && m.getType() === 'tool').map((m) => m.content);
    assert.deepEqual(goldOutputs, ['"Refunded 80.00 USD for order 4417"', DONE]);
  } finally {
    await ex.refund.close();
  }
});

test("the Vercel AI SDK runs the tool through generateText's own tool loop", async () => {
  const { MockLanguageModelV4 } = await import('ai/test');
  const ex = await load('vercel_ai_refund.mjs');
  const usage = {
    inputTokens: { total: 1, noCache: 1, cacheRead: undefined, cacheWrite: undefined },
    outputTokens: { total: 1, text: 1, reasoning: undefined },
  };
  // A model that asks for each refund in ASK in turn, then answers with what the tool
  // said, and records what the AI SDK showed it.
  function scripted() {
    const seen = { tools: null, outputs: [] };
    const model = new MockLanguageModelV4({
      doGenerate: async (options) => {
        seen.tools = options.tools;
        seen.outputs = options.prompt
          .filter((m) => m.role === 'tool')
          .flatMap((m) => m.content)
          .filter((p) => p.type === 'tool-result')
          .map((p) => p.output);
        const step = seen.outputs.length;
        const content =
          step < ASK.length
            ? [{ type: 'tool-call', toolCallId: `call_${step}`, toolName: 'refund_order', input: JSON.stringify(ASK[step]) }]
            : [{ type: 'text', text: seen.outputs.map((o) => o.value).join(' | ') }];
        return { content, finishReason: { unified: step < ASK.length ? 'tool-calls' : 'stop', raw: undefined }, usage, warnings: [] };
      },
    });
    return { model, seen };
  }
  try {
    const { model, seen } = scripted();
    const result = await ex.support(model, 'Order 4417 arrived broken. Please refund it.', { tier: 'standard' });
    assert.equal(seen.tools.length, 1);
    assert.equal(seen.tools[0].name, 'refund_order');
    assert.ok(!('pay' in seen.tools[0].inputSchema.properties), 'a grant is never an argument');
    assert.ok(!('$schema' in seen.tools[0].inputSchema));
    // The refusal reaches the model as an error with the broker's message; the refund
    // within the limit as text.
    assert.deepEqual(seen.outputs, [{ type: 'error-text', value: REFUSED }, { type: 'text', value: DONE }]);
    assert.equal(result.text, `${REFUSED} | ${DONE}`);
    // What the app sees of each call: the Staal answer, with its run.
    const results = result.steps.flatMap((s) => s.toolResults).map((r) => r.output);
    assert.deepEqual(results.map((r) => r.status), ['failed', 'done']);
    assert.equal(results[1].value, 'Refunded 35.00 USD for order 4417');
    assert.match(results[1].run, /^[0-9a-f]{32}$/);
    // A gold customer, from the same app, through the same tool context.
    const gold = scripted();
    await ex.support(gold.model, 'Refund it.', { tier: 'gold' });
    assert.deepEqual(gold.seen.outputs, [{ type: 'text', value: '"Refunded 80.00 USD for order 4417"' }, { type: 'text', value: DONE }]);
    // No context: the app's grants function gets none, and the call fails as an error the
    // model can read, never as a refund.
    const none = scripted();
    await ex.support(none.model, 'Refund it.', undefined);
    assert.equal(none.seen.outputs[0].type, 'error-text');
  } finally {
    await ex.refund.close();
  }
});

test('aiTools refuses what is not a Staal tool, and two tools of one name', async () => {
  const { aiTool, aiTools } = await import('@staal/node/vercel-ai');
  assert.throws(() => aiTool({}), TypeError);
  assert.throws(() => aiTools('x'), TypeError);
  const fake = { name: 'x', description: 'd', schema: { type: 'object' }, call: async () => ({ isError: false, text: '1' }) };
  assert.throws(() => aiTools([fake, fake]), /two tools are named 'x'/);
  assert.deepEqual(Object.keys(aiTools([fake])), ['x']);
});

// The Claude Agent SDK's loop runs in the Claude Code CLI it ships: it is pointed at a
// local server answering as the Anthropic Messages API does (streamed), with a
// scripted model, and given an environment and a home of its own.
async function scriptedMessages(toolName = 'mcp__staal__refund_order') {
  const http = await import('node:http');
  const state = { requests: [] };
  const server = http.createServer((req, res) => {
    let data = '';
    req.on('data', (c) => (data += c));
    req.on('end', () => {
      // The Anthropic SDKs add /v1 to the base URL; the AI SDK's provider expects it in
      // the base URL (as https://api.anthropic.com/v1) and adds only /messages.
      const where = req.url.split('?')[0];
      if (where !== '/v1/messages' && where !== '/messages') {
        res.writeHead(200, { 'content-type': 'application/json' });
        res.end('{"input_tokens":1}');
        return;
      }
      const body = JSON.parse(data || '{}');
      const results = body.messages
        .filter((m) => m.role === 'user' && Array.isArray(m.content))
        .flatMap((m) => m.content)
        .filter((b) => b.type === 'tool_result');
      const outputs = results.map((r) => ({
        text: typeof r.content === 'string' ? r.content : r.content.filter((c) => c.type === 'text').map((c) => c.text).join(''),
        isError: r.is_error === true,
      }));
      state.requests.push({ tools: body.tools || [], outputs });
      const step = outputs.length;
      const block =
        step < ASK.length
          ? { type: 'tool_use', id: `toolu_${step}`, name: toolName, input: ASK[step] }
          : { type: 'text', text: outputs.map((o) => o.text).join(' | ') };
      const stop = block.type === 'tool_use' ? 'tool_use' : 'end_turn';
      const message = { id: `msg_${step}`, type: 'message', role: 'assistant', model: body.model, content: [], stop_reason: null, stop_sequence: null, usage: { input_tokens: 1, output_tokens: 1 } };
      if (!body.stream) {
        res.writeHead(200, { 'content-type': 'application/json' });
        res.end(JSON.stringify({ ...message, content: [block], stop_reason: stop }));
        return;
      }
      res.writeHead(200, { 'content-type': 'text/event-stream' });
      const ev = (type, d) => res.write(`event: ${type}\ndata: ${JSON.stringify(d)}\n\n`);
      ev('message_start', { type: 'message_start', message });
      if (block.type === 'text') {
        ev('content_block_start', { type: 'content_block_start', index: 0, content_block: { type: 'text', text: '' } });
        ev('content_block_delta', { type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text: block.text } });
      } else {
        ev('content_block_start', { type: 'content_block_start', index: 0, content_block: { ...block, input: {} } });
        ev('content_block_delta', { type: 'content_block_delta', index: 0, delta: { type: 'input_json_delta', partial_json: JSON.stringify(block.input) } });
      }
      ev('content_block_stop', { type: 'content_block_stop', index: 0 });
      ev('message_delta', { type: 'message_delta', delta: { stop_reason: stop, stop_sequence: null }, usage: { output_tokens: 1 } });
      ev('message_stop', { type: 'message_stop' });
      res.end();
    });
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  return { server, state, url: `http://127.0.0.1:${server.address().port}` };
}

test("the Claude Agent SDK runs the tool through the agent loop of the CLI it ships", async () => {
  const ex = await load('claude_agent_sdk_refund.mjs');
  const api = await scriptedMessages();
  const home = path.join(scratch, 'claude-home');
  fs.mkdirSync(home, { recursive: true });
  const extra = {
    cwd: home,
    maxTurns: 6,
    env: { PATH: process.env.PATH, HOME: home, ANTHROPIC_BASE_URL: api.url, ANTHROPIC_API_KEY: 'sk-test-not-a-key', CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC: '1' },
  };
  try {
    const answer = await ex.support('Order 4417 arrived broken. Please refund it.', { tier: 'standard' }, extra);
    const last = api.state.requests.at(-1);
    const names = last.tools.map((t) => t.name);
    assert.ok(names.includes('mcp__staal__refund_order'), `the model is shown the refund: ${names}`);
    for (const builtin of ['Bash', 'Read', 'Write', 'Edit', 'WebFetch']) assert.ok(!names.includes(builtin), `no built-in tool: ${names}`);
    const refund = last.tools.find((t) => t.name === 'mcp__staal__refund_order');
    assert.ok(!('pay' in refund.input_schema.properties), 'a grant is never an argument');
    assert.equal(refund.description, ex.refund.description);
    assert.deepEqual(last.outputs, [{ text: REFUSED, isError: true }, { text: DONE, isError: false }]);
    assert.equal(answer, `${REFUSED} | ${DONE}`);
    // A gold customer's session, through a server made for that customer.
    api.state.requests = [];
    await ex.support('Refund it.', { tier: 'gold' }, extra);
    assert.deepEqual(api.state.requests.at(-1).outputs, [{ text: '"Refunded 80.00 USD for order 4417"', isError: false }, { text: DONE, isError: false }]);
  } finally {
    api.server.close();
    await ex.refund.close();
  }
});

test('the Claude Agent SDK server answers as an MCP server, and refuses what is not a Staal tool', async () => {
  const { mcpServer, allowedTools } = await import('@staal/node/claude-agent-sdk');
  const { Client } = await import('@modelcontextprotocol/sdk/client/index.js');
  const { InMemoryTransport } = await import('@modelcontextprotocol/sdk/inMemory.js');
  const calls = [];
  const fake = {
    name: 'x', description: 'd', schema: { type: 'object', properties: { n: { type: 'integer' } } }, annotations: { readOnlyHint: true, other: 1 },
    call: async (args, options) => { calls.push([args, options.context]); if (args.n === 2) throw new Error('host closed'); return { isError: args.n === 1, text: `n=${args.n}` }; },
  };
  assert.throws(() => mcpServer([]), TypeError);
  assert.throws(() => mcpServer([{}]), TypeError);
  assert.throws(() => mcpServer([fake, fake]), /two tools are named 'x'/);
  assert.throws(() => mcpServer([fake], { name: 'a b' }), /name is letters/);
  assert.deepEqual(allowedTools('billing', [fake]), ['mcp__billing__x']);
  const config = mcpServer([fake], { context: { tenant: 't1' } });
  assert.equal(config.type, 'sdk');
  assert.equal(config.name, 'staal');
  const [a, b] = InMemoryTransport.createLinkedPair();
  const client = new Client({ name: 'test', version: '1.0.0' });
  await Promise.all([config.instance.connect(a), client.connect(b)]);
  const listed = await client.listTools();
  assert.deepEqual(listed.tools.map((t) => [t.name, t.annotations]), [['x', { readOnlyHint: true }]]);
  assert.deepEqual(await client.callTool({ name: 'x', arguments: { n: 0 } }), { content: [{ type: 'text', text: 'n=0' }], isError: false });
  assert.equal((await client.callTool({ name: 'x', arguments: { n: 1 } })).isError, true);
  const thrown = await client.callTool({ name: 'x', arguments: { n: 2 } });
  assert.equal(thrown.isError, true);
  assert.match(thrown.content[0].text, /could not be called: host closed/);
  const unknown = await client.callTool({ name: 'nope', arguments: {} });
  assert.equal(unknown.isError, true);
  assert.deepEqual(calls.map((c) => c[1]), [{ tenant: 't1' }, { tenant: 't1' }, { tenant: 't1' }], 'every call gets the server\'s context');
  await client.close();
});

// A local stand-in for the OpenAI Responses API (POST /v1/responses, not streamed),
// with the same script: a function_call for each refund in ASK, then a message.
async function scriptedResponses(toolName) {
  const http = await import('node:http');
  const state = { requests: [] };
  const server = http.createServer((req, res) => {
    let data = '';
    req.on('data', (c) => (data += c));
    req.on('end', () => {
      const body = JSON.parse(data || '{}');
      if (!req.url.replace(/\/$/, '').endsWith('/responses') || body.stream) {
        res.writeHead(404, { 'content-type': 'application/json' });
        res.end(JSON.stringify({ error: { message: `the stand-in does not answer ${req.url}`, type: 'invalid_request_error' } }));
        return;
      }
      const items = Array.isArray(body.input) ? body.input : [];
      const outputs = items.filter((i) => i && i.type === 'function_call_output').map((i) => (typeof i.output === 'string' ? i.output : JSON.stringify(i.output)));
      state.requests.push({ tools: body.tools || [], outputs });
      const step = outputs.length;
      const output =
        step < ASK.length
          ? [{ type: 'function_call', id: `fc_${step}`, call_id: `call_${step}`, name: toolName, arguments: JSON.stringify(ASK[step]), status: 'completed' }]
          : [{ type: 'message', id: `msg_${step}`, role: 'assistant', status: 'completed', content: [{ type: 'output_text', text: outputs.join(' | '), annotations: [] }] }];
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end(JSON.stringify({
        id: `resp_${step}`, object: 'response', created_at: Math.floor(Date.now() / 1000), model: body.model || 'scripted', status: 'completed',
        output, parallel_tool_calls: false, tool_choice: 'auto', tools: body.tools || [], error: null, incomplete_details: null,
        instructions: null, metadata: {}, temperature: null, top_p: null,
        usage: { input_tokens: 1, output_tokens: 1, total_tokens: 2, input_tokens_details: { cached_tokens: 0 }, output_tokens_details: { reasoning_tokens: 0 } },
      }));
    });
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  return { server, state, url: `http://127.0.0.1:${server.address().port}/v1` };
}

// The tutorials in docs/frameworks/ that have a TypeScript agent: its agent.mjs saved as
// written, beside this test's node_modules, and run against a stand-in for its model's
// API, must print what the page shows (the agent's own output block, or the Python
// agent's, which the page says it matches), then the model's reply.
const TUTORIALS = {
  'claude-agent-sdk.md': { api: 'messages', tool: 'mcp__staal__refund_order', reply: 'Claude: ' },
  'openai-agents.md': { api: 'responses', tool: 'refund_order', reply: 'Agent: ' },
  'langgraph.md': { api: 'messages', tool: 'refund_order', reply: 'Agent: ' },
  'vercel-ai.md': { api: 'messages', tool: 'refund_order', reply: 'Agent: ' },
};
const pagesDir = path.resolve(nodeDir, '..', 'docs', 'frameworks');

for (const [pageName, how] of Object.entries(TUTORIALS)) {
  test(`the ${pageName} tutorial works as written, in TypeScript`, async () => {
    const page = fs.readFileSync(path.join(pagesDir, pageName), 'utf8');
    const fence = /^```(\S*)((?: \S+)*)\n([\s\S]*?)^```$/gm;
    const files = {};
    const outputs = {};
    for (const m of page.matchAll(fence)) {
      for (const w of m[2].trim().split(/\s+/)) {
        if (w.startsWith('file=')) files[w.slice(5)] = m[3];
        if (w.startsWith('output=')) outputs[w.slice(7)] = m[3];
      }
    }
    const want = outputs['agent.mjs'] ?? outputs['agent.py'];
    assert.ok(files['agent.mjs'] && files['refund.staal'] && want, `${pageName} has agent.mjs, refund.staal and the agent's output`);
    const work = path.join(frameworks, 'tutorial-' + pageName.replace(/\.md$/, ''));
    fs.rmSync(work, { recursive: true, force: true });
    fs.mkdirSync(work);
    for (const [f, text] of Object.entries(files)) if (f.endsWith('.mjs') || f.endsWith('.staal')) fs.writeFileSync(path.join(work, f), text);
    const api = how.api === 'messages' ? await scriptedMessages(how.tool) : await scriptedResponses(how.tool);
    const home = path.join(scratch, 'home-' + pageName);
    fs.mkdirSync(home, { recursive: true });
    const env = { PATH: process.env.PATH, HOME: home, STAAL_NODE_ADDON: process.env.STAAL_NODE_ADDON, OPENAI_AGENTS_DISABLE_TRACING: '1' };
    if (how.api === 'messages') Object.assign(env, { ANTHROPIC_BASE_URL: api.url, ANTHROPIC_API_KEY: 'sk-test-not-a-key', CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC: '1' });
    else Object.assign(env, { OPENAI_BASE_URL: api.url, OPENAI_API_KEY: 'sk-test-not-a-key' });
    try {
      // Run without blocking this process, which serves the scripted model.
      const { spawn } = await import('node:child_process');
      const child = spawn(process.execPath, ['agent.mjs'], { cwd: work, env });
      let stdout = '';
      let stderr = '';
      child.stdout.on('data', (d) => (stdout += d));
      child.stderr.on('data', (d) => (stderr += d));
      const code = await new Promise((resolve) => child.on('close', resolve));
      assert.equal(code, 0, `${stdout}\n${stderr}`);
      const lines = stdout.split('\n').filter((l) => l !== '');
      assert.ok(lines.at(-1).startsWith(how.reply), stdout);
      assert.equal(lines.slice(0, -1).join('\n') + '\n', want);
      assert.equal(api.state.requests.length, ASK.length + 1);
      // Every other JavaScript program the page shows the output of, run as written,
      // in the page's order, with no model: `<time>` stands for a UTC time, as in
      // python/tests/test_tutorials.py.
      for (const [program, shown] of Object.entries(outputs)) {
        if (program === 'agent.mjs' || !program.endsWith('.mjs')) continue;
        assert.ok(files[program], `${pageName} shows the output of ${program} but not the program`);
        const run = spawnSync(process.execPath, [program], { cwd: work, env: { PATH: env.PATH, HOME: home, STAAL_NODE_ADDON: env.STAAL_NODE_ADDON }, encoding: 'utf8' });
        assert.equal(run.status, 0, `${program}: ${run.stdout}\n${run.stderr}`);
        const pattern = new RegExp('^' + shown.split('<time>').map((t) => t.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')).join('\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}(?:\\.\\d+)?Z') + '$');
        assert.match(run.stdout, pattern, `${program} printed:\n${run.stdout}`);
      }
    } finally {
      api.server.close();
      fs.rmSync(work, { recursive: true, force: true });
    }
  });
}
