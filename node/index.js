'use strict';
// Run Staal programs from Node.js, through Staal's C interface (libstaalc).
//
// This package holds no Staal logic: it passes strings to staalc and parses the JSON
// it returns, exactly as the C interface (include/staal.h) and the Python package do.
//
//   const staal = require('@staal/node');
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
        'Builds exist for linux-x64-gnu, linux-arm64-gnu, darwin-arm64 and darwin-x64; elsewhere, build node/ from https://github.com/laurells/staal',
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

class StaalError extends Error {}

function ok(out) {
  if (out.status === 'error') throw new StaalError(out.error);
  return out;
}

/**
 * Where tools keep their runs: one journal directory, its ledger and its quotas.
 * Opening it resumes or settles whatever an earlier process left there. One process
 * uses a directory at a time; opening it again in this process gives the same host.
 */
class Host {
  constructor(handle, journals) {
    this._handle = handle;
    this.journals = journals;
  }

  static async open(journals, options = {}) {
    const settings = { journals: str(journals, 'journals'), keep: 90, max_concurrent_runs: 8, ...options };
    const out = ok(parse(await addon.hostOpen(JSON.stringify(settings))));
    return new Host(out.host, journals);
  }

  /**
   * What happened to a run, by its id. Changes nothing. A run made in a scope is
   * described only when `options.scope` is that scope; otherwise it is unknown.
   */
  async status(run, options = {}) {
    return ok(parse(await addon.runStatus(this._handle, str(run, 'run'), scopeOf(options.scope))));
  }

  async close() {
    if (this._handle !== null) {
      const h = this._handle;
      this._handle = null;
      ok(parse(await addon.hostClose(h)));
    }
  }
}

function scopeOf(scope) {
  if (scope === undefined || scope === null) return undefined;
  if (typeof scope !== 'string') throw new TypeError("staal: a scope must be a string (a tenant's or a user's id, from the app's own records)");
  return scope;
}

const SPEC_KEYS = ['title', 'entry', 'quota', 'once', 'deadline', 'max_concurrent_runs', 'engine',
  'model', 'secrets', 'net', 'fs', 'adapters', 'limits'];

/**
 * A program's entry function as a tool. `grants` is an object, the same for every
 * call, or a function of the call's context that returns that call's grants, so the
 * app's policy decides each call's limits. `scope`, if given, is a function of the
 * same context returning the call's trusted scope as a string (a tenant's or a user's
 * id, from the app's own records, never from the arguments): a `once` key is held
 * within its scope, and a run's status is given only in its scope. An app serving more
 * than one tenant or user from one journal directory gives a scope. `appInputs`, if
 * given, maps names of the entry function's inputs to functions of the same context
 * that give their values: inputs only the app supplies, such as the customer a call
 * is for. They are not in the schema a model sees, and a call whose arguments name
 * one is refused. Give `journals` (a directory) or `host`.
 */
class Tool {
  static async open(program, options) {
    const { name, description, grants, scope, appInputs, journals, host, ...spec } = options || {};
    if (scope !== undefined && typeof scope !== 'function') throw new TypeError("staal: Tool: scope is a function of the call's context");
    if (appInputs !== undefined) {
      const entries = appInputs !== null && typeof appInputs === 'object' && !Array.isArray(appInputs) ? Object.entries(appInputs) : [];
      if (entries.length === 0 || !entries.every(([, f]) => typeof f === 'function')) {
        throw new TypeError("staal: Tool: appInputs maps input names to functions of the call's context");
      }
    }
    for (const k of Object.keys(spec)) {
      if (!SPEC_KEYS.includes(k)) throw new TypeError(`staal: Tool: unknown option '${k}'`);
    }
    if ((journals === undefined) === (host === undefined)) {
      throw new TypeError('staal: Tool: give journals (a directory) or host, not both and not neither');
    }
    const ownHost = host === undefined;
    const h = ownHost ? await Host.open(journals) : host;
    const body = { name, description, program: str(program, 'program') };
    if (grants !== undefined && typeof grants !== 'function') body.grants = grants;
    if (appInputs !== undefined) body.app_inputs = Object.keys(appInputs);
    for (const [k, v] of Object.entries(spec)) if (v !== undefined && v !== null) body[k] = v;
    let out;
    try {
      out = ok(parse(await addon.toolOpen(h._handle, JSON.stringify(body))));
    } catch (e) {
      if (ownHost) await h.close();
      throw e;
    }
    const t = new Tool(out, h, ownHost, typeof grants === 'function' ? grants : null, scope || null, appInputs ? { ...appInputs } : null);
    if (t.perCallGrants && t._grants === null) {
      await t.close();
      throw new TypeError('staal: Tool: the program needs grants; give grants (an object or a function of the context)');
    }
    if (!t.perCallGrants && t._grants !== null) {
      await t.close();
      throw new TypeError("staal: Tool: the program's entry function takes no capabilities, so it takes no grants");
    }
    return t;
  }

  constructor(out, host, ownHost, grants, scope, appInputs) {
    this._handle = out.tool;
    this._scope = scope;
    this._appInputs = appInputs;
    this.host = host;
    this._ownHost = ownHost;
    this._grants = grants;
    this.name = out.name;
    this.title = out.title;
    this.description = out.description;
    this.schema = out.input_schema;
    this.outputSchema = out.output_schema;
    this.annotations = out.annotations;
    this.warnings = out.warnings;
    this.perCallGrants = out.grants === 'per_call';
  }

  /**
   * One call. Resolves to { status, isError, text, structured, run, value }: status is
   * "done", "failed", "stopped" (waiting for an operator), "in_progress" or "not_run";
   * a refusal is a result too, so a model can be told why.
   */
  async call(args = {}, options = {}) {
    if (args === null || typeof args !== 'object' || Array.isArray(args)) {
      throw new TypeError(`staal: the arguments are an object (a JSON object), not ${args === null ? 'null' : Array.isArray(args) ? 'an array' : typeof args}`);
    }
    let grants;
    if (this.perCallGrants) {
      const g = await this._grants(options.context);
      if (g === null || typeof g !== 'object' || Array.isArray(g)) throw new TypeError('staal: the grants function must return an object');
      grants = JSON.stringify(g);
    }
    const scope = this._scope ? scopeOf(await this._scope(options.context)) : undefined;
    let inputs;
    if (this._appInputs) {
      const values = {};
      for (const [k, f] of Object.entries(this._appInputs)) values[k] = await f(options.context);
      inputs = JSON.stringify(values);
    }
    const out = ok(parse(await addon.toolCall(this._handle, JSON.stringify(args), inputs, grants, scope)));
    return {
      status: out.status,
      isError: out.is_error,
      text: out.text,
      structured: out.structured,
      run: out.run,
      value: out.status === 'done' && out.structured ? out.structured.result : undefined,
    };
  }

  /** A run's status, in the scope the tool's scope function gives for `options.context`. */
  async status(run, options = {}) {
    const scope = this._scope ? scopeOf(await this._scope(options.context)) : undefined;
    return this.host.status(run, { scope });
  }

  async close() {
    if (this._handle !== null) {
      const t = this._handle;
      this._handle = null;
      ok(parse(await addon.toolClose(t)));
    }
    if (this._ownHost) await this.host.close();
  }
}

module.exports = { version, check, run, resume, resolve, Host, Tool, StaalError };
