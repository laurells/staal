import type { Tool } from './index.js';
/** A staal Tool as a Vercel AI SDK tool; `context` maps the tool's `toolsContext` entry to the staal Tool's context. */
export function aiTool(staalTool: Tool<any>, options?: { context?: (toolContext: any) => unknown }): any;
/** Several staal Tools as an AI SDK tool set, keyed by each tool's name. */
export function aiTools(staalTools: Tool<any>[], options?: { context?: (toolContext: any) => unknown }): Record<string, any>;
