// The JavaScript framework adapters (openai-agents.js, langgraph.js), driven by each
// framework's own agent loop with a scripted model: the model asks for a refund over
// the customer's limit and is refused with the broker's message, then asks within it
// and the refund is made. The examples in examples/frameworks/*.mjs are run as
// written (copied next to this test, beside its node_modules); only their model is
// replaced.
//
//   cd node/test/frameworks && npm ci &&
//   STAAL_NODE_ADDON=<addon> NODE_PATH=$PWD/node_modules node --test frameworks.test.mjs

import { test, after } from 'node:test';
import assert from 'node:assert/strict';
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

// The examples import @staal/node: this folder's node_modules gets it, as an app's would.
const link = path.join(frameworks, 'node_modules', '@staal', 'node');
fs.mkdirSync(path.dirname(link), { recursive: true });
if (!fs.existsSync(link)) fs.symlinkSync(nodeDir, link, 'dir');
for (const f of ['refund.staal', 'openai_agents_refund.mjs', 'langgraph_refund.mjs']) {
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
