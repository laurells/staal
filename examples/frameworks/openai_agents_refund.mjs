// A support agent on the OpenAI Agents SDK for JavaScript that refunds through Staal:
// the app's policy sets each customer's limit, and Staal enforces it, whatever the
// model asks.
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { Agent, run } from '@openai/agents';
import staal from '@staal/node';
import { functionTool } from '@staal/node/openai-agents';

const here = path.dirname(fileURLToPath(import.meta.url));
const LIMITS = { gold: '500.00', standard: '50.00' }; // USD per refund, decided by the app

export const refund = await staal.Tool.open(path.join(here, 'refund.staal'), {
  name: 'refund_order',
  description: "Refund part of an order to the customer's original payment.",
  grants: (customer) => ({ pay: { cap: 'payments.refund', uses: 1, each: { amount: LIMITS[customer.tier], currency: 'USD' } } }),
  journals: process.env.STAAL_JOURNALS || path.join(here, 'runs'),
});

export function agent(model) {
  return new Agent({ name: 'Support', instructions: "Refund what the customer is owed, within the tool's limits.", tools: [functionTool(refund)], model });
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const result = await run(agent(), 'Order 4417 arrived broken. Please refund 35 USD.', { context: { tier: 'standard' } });
  console.log(result.finalOutput);
  await refund.close();
}
