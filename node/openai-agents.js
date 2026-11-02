'use strict';
// A staal Tool as a tool of the OpenAI Agents SDK for JavaScript (@openai/agents, which
// the app installs; @staal/node does not depend on it).
//
//   const { Agent } = require('@openai/agents');
//   const { functionTool } = require('@staal/node/openai-agents');
//   const agent = new Agent({ name: 'Support', tools: [functionTool(refund)] });
//
// Each call runs the Staal program with the grants the app's policy gives for the run's
// context (the `context` passed to run(), unless `context` says otherwise). A refusal
// or a failure is given to the model as the tool's output, the broker's message
// included, so the model can correct itself.

const { tool } = require('@openai/agents');

/** `staalTool` (from Tool.open) as an @openai/agents function tool. */
function functionTool(staalTool, options = {}) {
  const pick = options.context || ((runContext) => (runContext ? runContext.context : undefined));
  const parameters = { ...staalTool.schema };
  delete parameters.$schema;
  return tool({
    name: staalTool.name,
    description: staalTool.description,
    parameters,
    // Staal's own check of the arguments is the one that decides; the schema is given
    // as it is rather than rewritten into the API's strict subset.
    strict: false,
    execute: async (input, runContext) => {
      const result = await staalTool.call(input || {}, { context: pick(runContext) });
      return result.text;
    },
  });
}

module.exports = { functionTool };
