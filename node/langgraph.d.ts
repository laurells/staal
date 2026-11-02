import type { Tool } from './index';
/** A staal Tool as a @langchain/core tool; `context` maps the call's config to the tool's context. */
export function structuredTool(staalTool: Tool<any>, options?: { context?: (config: any) => unknown }): any;
