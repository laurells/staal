'use strict';
// Runs one journaled run and prints its result; the crash test runs this in a child
// process with STAALC_FAILPOINT set, so the process dies partway through.
const staal = require(process.env.STAAL_PACKAGE || '..');
const [program, configJson, journal, engine] = process.argv.slice(2);
staal
  .run(program, JSON.parse(configJson), { journal, engine: engine === '-' ? undefined : engine })
  .then((out) => process.stdout.write(JSON.stringify(out)));
