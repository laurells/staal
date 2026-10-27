# @staal/node: Staal for Node.js

Run [Staal](https://github.com/laurells/staal) programs from Node.js and TypeScript.
Staal is a programming language for software where AI acts: its compiler refuses a
program that could act without permission, use a model's answer unchecked, or
overspend, and its runtime never repeats an action after a crash.

Install it with `npm install @staal/node`. This package holds no Staal logic of its own. It calls staalc's C interface
in-process, so a Node app gets exactly the results `staalc` and the Python package
give. Runs happen off the event loop and return Promises.

```js
const staal = require('@staal/node');

// Your app decides the policy; Staal enforces it.
const limit = customer.tier === 'gold' ? 500 : 50;
const out = await staal.run('refund.staal', {
  entry: 'main',
  inputs: { complaint },
  grants: { pay: { cap: 'payments.refund', each: { amount: String(limit), currency: 'USD' } } },
  model: { provider: 'anthropic', model: 'claude-sonnet-5-5', api_key: { env: 'ANTHROPIC_API_KEY' } },
}, { journal: 'refund.journal' });

if (out.status === 'done') console.log(out.result);
else if (out.status === 'failed') console.log('refused:', out.failure.message);
```

`run`, `resume`, `resolve` and `check` resolve to the JSON objects described in
[include/staal.h](https://github.com/laurells/staal/blob/main/include/staal.h), typed
in `index.d.ts`: `done` with a `result`, `failed` with a `failure`, `stopped` (an
action in doubt: `resolve` it, then `resume`), `invalid` with located check errors,
or `error`. Pass `{ engine: 'native' }` to run pure code as machine code.

**Platforms.** Node 18 or newer, on Linux (x86_64 and arm64, glibc 2.28 or newer) and
macOS (Apple Silicon, macOS 11 or newer; Intel, macOS 10.12 or newer). Elsewhere,
build `node/` from the repository.

**Licence.** Apache-2.0 WITH LLVM-exception. The addon contains staalc and its Rust
dependencies, whose licences are in `LICENSE-THIRD-PARTY.txt`.
