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
  | 'internal'
  | 'journal'
  | 'in_doubt';

export interface Failure {
  kind: FailureKind;
  message: string;
}

interface Ran {
  log: LogEvent[];
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
