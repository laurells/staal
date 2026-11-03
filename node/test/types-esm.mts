// The ES module adapters' types, checked with `npm run types`.
import type { Tool } from '../index.js';
import { aiTool, aiTools } from '../vercel-ai.mjs';

declare const refund: Tool<{ tier: string }>;
const one = aiTool(refund);
const set: Record<string, unknown> = aiTools([refund], { context: (c: { tier: string }) => c });
void one;
void set;

import { allowedTools, mcpServer } from '../claude-agent-sdk.mjs';
const server: { type: 'sdk'; name: string } = mcpServer([refund], { name: 'staal', context: { tier: 'gold' } });
const names: string[] = allowedTools('staal', [refund]);
void server;
void names;
