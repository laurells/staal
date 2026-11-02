'use strict';
// A staal Tool as a LangChain tool, for LangGraph's ToolNode and agents (@langchain/core
// and @langchain/langgraph, which the app installs; @staal/node does not depend on them).
//
//   const { ToolNode } = require('@langchain/langgraph/prebuilt');
//   const { structuredTool } = require('@staal/node/langgraph');
//   const tools = new ToolNode([structuredTool(refund)]);
//   await graph.invoke(state, { configurable: { staal_context: customer } });
//
// Each call runs the Staal program with the grants the app's policy gives for the call's
// context: by default `config.configurable.staal_context`, or whatever `context` returns
// for the call's config. A refusal or a failure is given to the model as the tool's
// output, the broker's message included, so the model can correct itself.

const { tool } = require('@langchain/core/tools');

/** `staalTool` (from Tool.open) as a @langchain/core tool. */
function structuredTool(staalTool, options = {}) {
  const pick = options.context || ((config) => (config && config.configurable ? config.configurable.staal_context : undefined));
  const schema = { ...staalTool.schema };
  delete schema.$schema;
  return tool(
    async (input, config) => {
      const result = await staalTool.call(input || {}, { context: pick(config) });
      return result.text;
    },
    { name: staalTool.name, description: staalTool.description, schema },
  );
}

module.exports = { structuredTool };
