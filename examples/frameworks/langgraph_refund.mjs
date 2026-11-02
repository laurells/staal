// A support agent on LangGraph for JavaScript that refunds through Staal: the app's
// policy sets each customer's limit, and Staal enforces it, whatever the model asks.
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { StateGraph, MessagesAnnotation, START } from '@langchain/langgraph';
import { ToolNode, toolsCondition } from '@langchain/langgraph/prebuilt';
import staal from '@staal/node';
import { structuredTool } from '@staal/node/langgraph';

const here = path.dirname(fileURLToPath(import.meta.url));
const LIMITS = { gold: '500.00', standard: '50.00' }; // USD per refund, decided by the app

export const refund = await staal.Tool.open(path.join(here, 'refund.staal'), {
  name: 'refund_order',
  description: "Refund part of an order to the customer's original payment.",
  grants: (customer) => ({ pay: { cap: 'payments.refund', uses: 1, each: { amount: LIMITS[customer.tier], currency: 'USD' } } }),
  journals: process.env.STAAL_JOURNALS || path.join(here, 'runs'),
});
export const tools = [structuredTool(refund)];

/** model: any LangChain chat model that can call tools. */
export function graph(model) {
  const llm = model.bindTools(tools);
  return new StateGraph(MessagesAnnotation)
    .addNode('model', async (state) => ({ messages: [await llm.invoke(state.messages)] }))
    .addNode('tools', new ToolNode(tools))
    .addEdge(START, 'model')
    .addConditionalEdges('model', toolsCondition)
    .addEdge('tools', 'model')
    .compile();
}
