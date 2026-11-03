// A support agent on the Vercel AI SDK that refunds through Staal: the app's policy
// sets each customer's limit, and Staal enforces it, whatever the model asks.
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { generateText, isStepCount } from 'ai';
import staal from '@staal/node';
import { aiTools } from '@staal/node/vercel-ai';

const here = path.dirname(fileURLToPath(import.meta.url));
const LIMITS = { gold: '500.00', standard: '50.00' }; // USD per refund, decided by the app

export const refund = await staal.Tool.open(path.join(here, 'refund.staal'), {
  name: 'refund_order',
  description: "Refund part of an order to the customer's original payment.",
  grants: (customer) => ({ pay: { cap: 'payments.refund', uses: 1, each: { amount: LIMITS[customer.tier], currency: 'USD' } } }),
  journals: process.env.STAAL_JOURNALS || path.join(here, 'runs'),
});

/** The customer comes from the app's own records, and reaches the tool as its context. */
export function support(model, prompt, customer) {
  return generateText({
    model,
    system: "Refund what the customer is owed, within the tool's limits.",
    prompt,
    tools: aiTools([refund]),
    toolsContext: { [refund.name]: customer },
    stopWhen: isStepCount(5),
  });
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  // A model through the AI Gateway, named in STAAL_EXAMPLE_MODEL, with its key in AI_GATEWAY_API_KEY.
  if (!process.env.STAAL_EXAMPLE_MODEL) throw new Error('set STAAL_EXAMPLE_MODEL to an AI Gateway model id');
  const result = await support(process.env.STAAL_EXAMPLE_MODEL, 'Order 4417 arrived broken. Please refund 35 USD.', { tier: 'standard' });
  console.log(result.text);
  await refund.close();
}
