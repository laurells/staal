// A support agent on the Claude Agent SDK for TypeScript that refunds through Staal:
// the app's policy sets each customer's limit, and Staal enforces it, whatever the
// model asks.
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { query } from '@anthropic-ai/claude-agent-sdk';
import staal from '@staal/node';
import { allowedTools, mcpServer } from '@staal/node/claude-agent-sdk';

const here = path.dirname(fileURLToPath(import.meta.url));
const LIMITS = { gold: '500.00', standard: '50.00' }; // USD per refund, decided by the app

export const refund = await staal.Tool.open(path.join(here, 'refund.staal'), {
  name: 'refund_order',
  description: "Refund part of an order to the customer's original payment.",
  grants: (customer) => ({ pay: { cap: 'payments.refund', uses: 1, each: { amount: LIMITS[customer.tier], currency: 'USD' } } }),
  journals: process.env.STAAL_JOURNALS || path.join(here, 'runs'),
});

/**
 * One session's options. The customer comes from the app's own records; `tools: []`
 * leaves the agent no built-in tool (no shell, no files), only the refund.
 */
export function options(customer, extra = {}) {
  return {
    systemPrompt: "Refund what the customer is owed, within the tool's limits.",
    mcpServers: { staal: mcpServer([refund], { context: customer }) },
    allowedTools: allowedTools('staal', [refund]),
    tools: [],
    ...extra,
  };
}

/** The agent's final answer. */
export async function support(prompt, customer, extra) {
  let answer;
  for await (const message of query({ prompt, options: options(customer, extra) })) {
    if (message.type === 'result') answer = message.result;
  }
  return answer;
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  console.log(await support('Order 4417 arrived broken. Please refund 35 USD.', { tier: 'standard' }));
  await refund.close();
}
