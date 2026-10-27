'use strict';
// Run Staal programs from Node.js, through Staal's C interface (libstaalc).
//
// This package holds no Staal logic: it passes strings to staalc and parses the JSON
// it returns, exactly as the C interface (include/staal.h) and the Python package do.
//
//   const staal = require('staal');
//   const out = await staal.run('refund.staal', config, { journal: 'run.journal' });
//   if (out.status === 'done') console.log(out.result);

const fs = require('fs');
const path = require('path');

function platformDir() {
  if (process.platform === 'linux') {
    // The build links against glibc; musl systems (Alpine) cannot load it.
    const report = typeof process.report?.getReport === 'function' ? process.report.getReport() : null;
    const glibc = report && report.header && report.header.glibcVersionRuntime;
    return glibc ? `linux-${process.arch}-gnu` : `linux-${process.arch}-musl`;
  }
  return `${process.platform}-${process.arch}`;
}

function load() {
  const override = process.env.STAAL_NODE_ADDON;
  const file = override || path.join(__dirname, 'bin', platformDir(), 'staal.node');
  if (!fs.existsSync(file)) {
    throw new Error(
      `staal: this package has no build for ${platformDir()} yet (looked for ${file}). ` +
        'Builds exist for linux-x64-gnu; elsewhere, build node/ from https://github.com/laurells/staal',
    );
  }
  return require(file);
}

const addon = load();

function parse(text) {
  return JSON.parse(text);
}

function str(value, name) {
  if (typeof value !== 'string') throw new TypeError(`staal: ${name} must be a string`);
  return value;
}

function engineOf(options) {
  const engine = options && options.engine;
  return engine === undefined || engine === null ? undefined : str(engine, 'engine');
}

/** staalc's version, for example "0.23.0". */
const version = addon.version();

/** Checks a program without running it. */
async function check(program) {
  return parse(await addon.check(str(program, 'program')));
}

/** Runs a program. `config` is the run config, as an object. */
async function run(program, config, options = {}) {
  const journal = options.journal === undefined || options.journal === null ? undefined : str(options.journal, 'journal');
  return parse(await addon.run(str(program, 'program'), JSON.stringify(config), journal, engineOf(options)));
}

/** Resumes a run from its journal: finished actions are replayed, not made again. */
async function resume(program, config, journal, options = {}) {
  return parse(await addon.resume(str(program, 'program'), JSON.stringify(config), str(journal, 'journal'), engineOf(options)));
}

/**
 * Resolves the action in doubt in a stopped run's journal, then call `resume`.
 * `resolution` is one of { performed: "<receipt>" }, { status: 200, body: "..." },
 * { failed: "<reason>" } or { retry: true }.
 */
async function resolve(journal, resolution) {
  return parse(await addon.resolve(str(journal, 'journal'), JSON.stringify(resolution)));
}

module.exports = { version, check, run, resume, resolve };
