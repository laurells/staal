// Runs Staal programs through the staal package, as a Node app would.
//
//   STAAL_ROOT=<repo root> [STAAL_HAS_NATIVE=1] [STAAL_CRASH_ADDON=<addon built with
//   the failpoints feature>] [STAAL_PACKAGE=<an installed staal package>]
//   node --test node/test/staal.test.mjs
//
// Without STAAL_PACKAGE it tests this folder (with STAAL_NODE_ADDON naming the
// addon); with it, the package as npm installed it.
//
// The cases are the repository's own (tests/run), with their written expectations.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const staal = require(process.env.STAAL_PACKAGE || '..');
const here = path.dirname(fileURLToPath(import.meta.url));
const root = process.env.STAAL_ROOT || path.resolve(here, '..', '..');
const runDir = path.join(root, 'tests', 'run');
const clinic = path.join(runDir, 'domain_clinic.staal');
const engines = [undefined, 'interpreter', ...(process.env.STAAL_HAS_NATIVE === '1' ? ['native'] : [])];
const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'staal-node-'));

function kase(name) {
  const cfg = JSON.parse(fs.readFileSync(path.join(runDir, `${name}.json`), 'utf8'));
  const want = cfg.expect;
  delete cfg.expect;
  return { cfg, want };
}

test('the version is staalc\'s', () => {
  const cargo = fs.readFileSync(path.join(root, 'Cargo.toml'), 'utf8');
  assert.equal(staal.version, cargo.match(/^version = "([^"]+)"/m)[1]);
});

test('check: valid, invalid with located errors, missing file', async () => {
  assert.deepEqual(await staal.check(clinic), { status: 'ok' });
  const bad = path.join(scratch, 'bad.staal');
  fs.writeFileSync(bad, 'fn f() -> Int {\n    return true\n}\n');
  const r = await staal.check(bad);
  assert.equal(r.status, 'invalid');
  assert.equal(r.errors[0].code, 'E101');
  assert.equal(r.errors[0].line, 2);
  assert.equal((await staal.check(path.join(scratch, 'missing.staal'))).status, 'error');
});

test('every clinic case, on every engine, as its expectation says', async () => {
  const names = fs.readdirSync(runDir).filter((f) => f.startsWith('domain_clinic__')).map((f) => f.slice(0, -5)).sort();
  assert.ok(names.length >= 6, 'the clinic cases');
  for (const engine of engines) {
    for (const name of names) {
      const { cfg, want } = kase(name);
      const out = await staal.run(clinic, cfg, { engine });
      const what = `${name} (${engine})`;
      if ('config_error_contains' in want) {
        assert.equal(out.status, 'error', what);
        assert.ok(out.error.includes(want.config_error_contains), what);
        continue;
      }
      if ('result' in want) {
        assert.equal(out.status, 'done', `${what}: ${JSON.stringify(out.failure)}`);
        assert.deepEqual(out.result, want.result, what);
      } else {
        assert.equal(out.status, 'failed', what);
        assert.equal(out.failure.kind, want.failure, what);
        if ('message_contains' in want) assert.ok(out.failure.message.includes(want.message_contains), what);
      }
      assert.equal(out.log[0].event, 'run_start', what);
    }
  }
});

test('runs do not block the event loop, and run side by side', async () => {
  const { cfg } = kase('domain_clinic__booked');
  let ticks = 0;
  const timer = setInterval(() => ticks++, 0);
  const outs = await Promise.all(Array.from({ length: 8 }, () => staal.run(clinic, cfg)));
  clearInterval(timer);
  for (const out of outs) assert.equal(out.status, 'done');
  assert.ok(ticks > 0, 'the event loop ran while staalc worked');
});

test('a journaled run resumes on the other engine and replays, making nothing again', async () => {
  const { cfg, want } = kase('domain_clinic__booked');
  const journal = path.join(scratch, 'resume.journal');
  const first = await staal.run(clinic, cfg, { journal });
  assert.equal(first.status, 'done');
  const again = await staal.resume(clinic, cfg, journal, { engine: engines.at(-1) });
  assert.equal(again.status, 'done');
  assert.deepEqual(again.result, want.result);
  const performed = (out) => out.log.filter((e) => e.event === 'action_performed').map((e) => e.replayed);
  assert.deepEqual(performed(first), [false, false]);
  assert.deepEqual(performed(again), [true, true]);
});

test('bad arguments are errors, never crashes', async () => {
  const { cfg } = kase('domain_clinic__booked');
  assert.equal((await staal.run(clinic, { entry: 'nope' })).status, 'error');
  assert.equal((await staal.run(clinic, cfg, { engine: 'fast' })).status, 'error');
  await assert.rejects(staal.run(42, cfg), TypeError);
  await assert.rejects(staal.run('a\u0000b', cfg), /NUL/);
  const journal = path.join(scratch, 'finished.journal');
  await staal.run(clinic, cfg, { journal });
  assert.equal((await staal.resolve(journal, { retry: true })).status, 'error', 'nothing to resolve');
  assert.equal((await staal.resolve(journal, { retry: true, failed: 'x' })).status, 'error', 'one resolution at a time');
  assert.equal((await staal.resume(clinic, cfg, path.join(scratch, 'none.journal'))).status, 'error');
});

// Real crashes: a child process dies at each journal write in turn, and this process
// resumes. Needs an addon built with the failpoints feature.
const crashAddon = process.env.STAAL_CRASH_ADDON;
test('a run killed at every journal write resumes through the package', { skip: !crashAddon && 'STAAL_CRASH_ADDON not set' }, async (t) => {
  for (const idempotent of [true, false]) {
    const { cfg, want } = kase('domain_clinic__booked');
    cfg.adapters = { simulated: { idempotent } };
    let crashed = 0;
    let stopped = 0;
    for (let n = 1; ; n++) {
      const journal = path.join(scratch, `crash-${idempotent}-${n}.journal`);
      const child = spawnSync(
        process.execPath,
        [path.join(here, 'crash_child.js'), clinic, JSON.stringify(cfg), journal, '-'],
        { env: { ...process.env, STAAL_NODE_ADDON: crashAddon, STAALC_FAILPOINT: `after_write:${n}` }, encoding: 'utf8' },
      );
      if (child.status === 0) {
        assert.equal(JSON.parse(child.stdout).status, 'done', `n=${n} ran to the end`);
        break;
      }
      assert.equal(child.signal, 'SIGABRT', `n=${n}: ${child.stderr}`);
      crashed++;
      for (const engine of engines) {
        const copy = `${journal}.${engine}`;
        fs.copyFileSync(journal, copy);
        let out = await staal.resume(clinic, cfg, copy, { engine });
        if (out.status === 'stopped') {
          // A non-idempotent action whose outcome the crash left unknown: the
          // operator says it happened, and the run continues without making it again.
          assert.equal(idempotent, false, `n=${n}: only a non-idempotent action is left in doubt`);
          assert.equal(out.failure.kind, 'in_doubt');
          stopped++;
          assert.equal((await staal.resolve(copy, { performed: 'sim-checked-by-operator' })).status, 'resolved');
          out = await staal.resume(clinic, cfg, copy, { engine });
          assert.equal(out.status, 'done', `n=${n} (${engine}) after resolving: ${JSON.stringify(out.failure)}`);
          continue;
        }
        assert.equal(out.status, 'done', `n=${n} (${engine}): ${JSON.stringify(out.failure)}`);
        assert.deepEqual(out.result, want.result, `n=${n} (${engine})`);
      }
    }
    t.diagnostic(`idempotent=${idempotent}: killed at ${crashed} journal writes, ${stopped} resumes stopped in doubt`);
    assert.ok(crashed >= 4, `crashed at ${crashed} points`);
    if (!idempotent) assert.ok(stopped > 0, 'some crash left an action in doubt');
  }
});
