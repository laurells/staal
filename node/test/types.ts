// Type-checked, never run: what a TypeScript app writes against the staal package.
// `npm run types` fails if index.d.ts stops describing the API this way.
import * as staal from '..';
import type { RunConfig, RunResult } from '..';

const limit = 50;
const config: RunConfig = {
  entry: 'main',
  inputs: { complaint: 'My blender arrived broken. Order 4417.' },
  grants: { pay: { cap: 'payments.refund', each: { amount: String(limit), currency: 'USD' } } },
  model: { provider: 'anthropic', model: 'claude-sonnet-5-5', api_key: { env: 'ANTHROPIC_API_KEY' } },
};

async function handle(): Promise<string> {
  const out: RunResult = await staal.run('refund.staal', config, { journal: 'run.journal', engine: 'native' });
  switch (out.status) {
    case 'done': {
      const charged: string[] = Object.values(out.usage).flatMap((u) => u.charged.map((m) => `${m.amount} ${m.currency}`));
      return `${String(out.result)} (${charged.join(', ')})`;
    }
    case 'failed':
      return `refused (${out.failure.kind}): ${out.failure.message}`;
    case 'stopped': {
      const kind: 'in_doubt' | 'journal' = out.failure.kind;
      await staal.resolve('run.journal', { performed: 'receipt-from-the-provider' });
      const again = await staal.resume('refund.staal', config, 'run.journal');
      return `${kind}, then ${again.status}`;
    }
    case 'invalid':
      return out.errors.map((e) => `${e.file}:${e.line}:${e.col}: ${e.code} ${e.message}`).join('\n');
    case 'error':
      return out.error;
  }
}

const v: string = staal.version;
void handle;
void v;

async function tool(): Promise<string> {
  const refund = await staal.Tool.open<{ limit: string }>('refund.staal', {
    name: 'refund_order',
    description: 'Refund part of an order.',
    grants: (ctx) => ({ pay: { cap: 'payments.refund', each: { amount: ctx.limit, currency: 'USD' } } }),
    journals: '/var/lib/staal/runs',
    quota: { window: '1h', calls: 10 },
  });
  const r: staal.ToolResult = await refund.call({ order: '4417' }, { context: { limit: '50.00' } });
  const st: staal.RunStatus = await refund.status(r.run ?? '');
  await refund.close();
  return `${r.status} ${st.status}`;
}
void tool;
