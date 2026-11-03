// A staal Tool as a tool of the Vercel AI SDK (`ai`, version 7, which the app installs;
// @staal/node does not depend on it). The AI SDK is published as ES modules only, so
// this adapter is one too.
//
//   import { generateText, stepCountIs } from 'ai';
//   import { aiTools } from '@staal/node/vercel-ai';
//   const result = await generateText({
//     model, prompt, stopWhen: stepCountIs(5),
//     tools: aiTools([refund]),
//     toolsContext: { refund_order: customer },   // the app's context for that tool
//   });
//
// Each call runs the Staal program with the grants the app's policy gives for the
// tool's context: the AI SDK hands each tool its own entry of `toolsContext`, keyed by
// the tool's name, and that entry is what the Staal tool's grants (and scope) function
// receives, unless `options.context` maps it to something else. Context never reaches
// the model.
//
// The model is shown the program's argument schema as it is, without validation by
// the AI SDK: Staal's own check of the arguments is the one that decides, and a call it
// refuses comes back to the model as an error with Staal's message, as a refusal by the
// broker does, so the model can correct itself. Nothing is retried. An abort of the
// generation does not stop a Staal run that has started: its outcome is in its journal
// and in the tool's status, as with any call.

import { tool, jsonSchema } from 'ai';

/** `staalTool` (from Tool.open) as an AI SDK tool. Give it to `tools` under `staalTool.name`. */
export function aiTool(staalTool, options = {}) {
  if (!staalTool || typeof staalTool.call !== 'function' || typeof staalTool.name !== 'string') {
    throw new TypeError('staal: aiTool takes a Tool from Tool.open');
  }
  const pick = options.context || ((context) => context);
  if (typeof pick !== 'function') throw new TypeError('staal: aiTool: options.context is a function of the tool context');
  const schema = { ...staalTool.schema };
  delete schema.$schema;
  return tool({
    description: staalTool.description,
    // No `validate`: the AI SDK passes the model's input on as it is, and Staal checks it.
    inputSchema: jsonSchema(schema),
    execute: async (input, { context }) => staalTool.call(input ?? {}, { context: await pick(context) }),
    // What the model reads: the answer's text, as an MCP client would get it, marked
    // as an error when Staal refused or the run failed.
    toModelOutput: ({ output }) => (output.isError ? { type: 'error-text', value: output.text } : { type: 'text', value: output.text }),
  });
}

/** Several staal Tools as an AI SDK tool set, each under its own name. */
export function aiTools(staalTools, options = {}) {
  if (!Array.isArray(staalTools)) throw new TypeError('staal: aiTools takes a list of Tools');
  const set = {};
  for (const t of staalTools) {
    const made = aiTool(t, options);
    if (Object.prototype.hasOwnProperty.call(set, t.name)) throw new TypeError(`staal: aiTools: two tools are named '${t.name}'`);
    set[t.name] = made;
  }
  return set;
}
