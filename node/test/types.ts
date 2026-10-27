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
    case 'done':
      return String(out.result);
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
