import type { Tool } from './index.js';
/** staal Tools as an in-process MCP server config for the Claude Agent SDK's `mcpServers`. */
export function mcpServer(tools: Tool<any>[], options?: { name?: string; context?: unknown }): { type: 'sdk'; name: string; instance: any };
/** The names the Agent SDK gives `tools` served under `name`, for `allowedTools`. */
export function allowedTools(name: string, tools: Tool<any>[]): string[];
