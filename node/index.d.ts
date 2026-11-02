// Types for the staal package. Results are the JSON objects described in
// include/staal.h, as returned by staalc.

/** JSON as it crosses into and out of Staal. */
export type Json = null | boolean | number | string | Json[] | { [key: string]: Json };

/** One event of a run's log (RUNTIME.md section 10). */
export interface LogEvent {
  event: string;
  [field: string]: Json;
}

/** A check error, with where it is and the text staalc check prints. */
export interface Diagnostic {
  code: string;
  file: string;
  line: number;
  col: number;
  message: string;
  text: string;
}

/** What made a run fail (RUNTIME.md section 8). */
export type FailureKind =
  | 'action'
  | 'capability'
  | 'validation'
  | 'value'
  | 'budget'
  | 'provider'
  | 'step_limit'
  | 'depth_limit'
  | 'deadline'
  | 'internal'
  | 'journal'
  | 'in_doubt';

export interface Failure {
  kind: FailureKind;
  message: string;
}

/** What one granted capability used over a run (RUNTIME.md section 6). */
export interface GrantUsage {
  capability: string;
  uses: number;
  charged: { amount: string; currency: string }[];
}

interface Ran {
  log: LogEvent[];
  /** For each capability parameter of the entry function, by name. */
  usage: { [parameter: string]: GrantUsage };
  /** Present when the run loaded C code, which Staal cannot check (RUNTIME.md section 19). */
  unchecked?: Json[];
}

export type RunResult =
  | (Ran & { status: 'done'; result: Json })
  | (Ran & { status: 'failed'; failure: Failure })
  /** Stopped and can continue: resolve the action in doubt, then resume. */
  | (Ran & { status: 'stopped'; failure: Failure & { kind: 'in_doubt' | 'journal' } })
  | { status: 'invalid'; errors: Diagnostic[] }
  | { status: 'error'; error: string };

export type CheckResult = { status: 'ok' } | { status: 'invalid'; errors: Diagnostic[] } | { status: 'error'; error: string };

export type Resolution = { performed: string } | { status: number; body?: string } | { failed: string } | { retry: true };

export type ResolveResult = { status: 'resolved'; summary: Json } | { status: 'error'; error: string };

/** A run config (RUNTIME.md section 1): entry, inputs, grants, model and settings. */
export type RunConfig = { [key: string]: Json };

export interface RunOptions {
  /** A journal file: the run survives a crash and can be resumed from it. */
  journal?: string;
  engine?: 'interpreter' | 'native';
}

/** staalc's version, for example "0.23.0". */
export const version: string;

export function check(program: string): Promise<CheckResult>;
export function run(program: string, config: RunConfig, options?: RunOptions): Promise<RunResult>;
export function resume(program: string, config: RunConfig, journal: string, options?: Pick<RunOptions, 'engine'>): Promise<RunResult>;
export function resolve(journal: string, resolution: Resolution): Promise<ResolveResult>;

/** A host or tool could not be opened, or a call was malformed: nothing ran. */
export class StaalError extends Error {}

export interface HostOptions {
  /** Days a settled run's files are kept (default 90), or "forever". */
  keep?: number | 'forever';
  max_concurrent_runs?: number;
  notify?: string[];
  notify_env?: string[];
}

/** Where tools keep their runs: one journal directory, its ledger and its quotas. */
export class Host {
  static open(journals: string, options?: HostOptions): Promise<Host>;
  readonly journals: string;
  /** What happened to a run, by its id. Changes nothing. */
  /** A run made in a scope is described only when the same scope is given. */
  status(run: string, options?: { scope?: string }): Promise<RunStatus>;
  close(): Promise<void>;
}

export type RunStatus = {
  run: string;
  status: 'running' | 'waiting_for_operator' | 'done' | 'failed' | 'not_started' | 'expired' | 'unknown';
  observed_at: string;
  result?: Json;
  failure?: { kind: string; message: string };
  reason?: string;
};

export interface ToolOptions<Context = unknown> {
  name: string;
  description: string;
  /** The same grants for every call, or a function of the call's context giving that call's. */
  grants?: { [name: string]: Json } | ((context: Context) => { [name: string]: Json } | Promise<{ [name: string]: Json }>);
  /**
   * The call's trusted scope (a tenant's or a user's id, from the app's own records,
   * never from the arguments), from its context: `once` keys are held within a scope,
   * and a run's status is given only in its scope.
   */
  scope?: (context: Context) => string | Promise<string>;
  /** A journal directory, or an open host: one is required. */
  journals?: string;
  host?: Host;
  title?: string;
  entry?: string;
  quota?: { window: string; calls?: number; money?: { amount: string; currency: string } };
  once?: { by: string[]; keep?: string };
  deadline?: string;
  max_concurrent_runs?: number;
  engine?: 'interpreter' | 'native';
  model?: Json;
  secrets?: Json;
  net?: Json;
  fs?: Json;
  adapters?: Json;
  limits?: Json;
}

export interface ToolResult {
  status: 'done' | 'failed' | 'stopped' | 'in_progress' | 'not_run';
  isError: boolean;
  /** What to show a model. */
  text: string;
  structured?: { result: Json } | { status: 'waiting_for_operator'; run: string };
  run?: string;
  /** The program's result, when done. */
  value?: Json;
}

/** A program's entry function as a tool, with quotas, once and recovery across calls. */
export class Tool<Context = unknown> {
  static open<Context = unknown>(program: string, options: ToolOptions<Context>): Promise<Tool<Context>>;
  readonly name: string;
  readonly title?: string;
  readonly description: string;
  /** The JSON Schema of its arguments. */
  readonly schema: Json;
  readonly outputSchema: Json;
  readonly annotations: { readOnlyHint: boolean; destructiveHint: boolean; idempotentHint: boolean; openWorldHint: boolean; title?: string };
  readonly warnings: string[];
  readonly perCallGrants: boolean;
  readonly host: Host;
  call(args?: { [name: string]: Json }, options?: { context?: Context }): Promise<ToolResult>;
  /** A run's status, in the scope the tool's scope function gives for the context. */
  status(run: string, options?: { context?: Context }): Promise<RunStatus>;
  close(): Promise<void>;
}
