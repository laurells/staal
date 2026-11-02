import type { Tool } from './index';
/** A staal Tool as an @openai/agents function tool; `context` maps the run context to the tool's context. */
export function functionTool(staalTool: Tool<any>, options?: { context?: (runContext: any) => unknown }): any;
