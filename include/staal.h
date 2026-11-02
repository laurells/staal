/* The C interface to Staal (src/capi.rs). Link with libstaalc.
 *
 * Every function returning char* returns a JSON object that the caller must free
 * with staal_free. Its "status" is one of:
 *   "done"     the run finished; "result" holds the entry function's value
 *   "failed"   the run failed; "failure" holds {"kind", "message"}
 *   "stopped"  the run stopped and can continue: resolve the action in doubt, then
 *              resume (or just resume, if the journal could not be written)
 *   "invalid"  the program has check errors; "errors" lists them; nothing ran
 *   "error"    a bad argument, file, config or journal; "error" says why; nothing ran
 *   "ok"       (staal_check) the program is valid
 *   "resolved" (staal_resolve) the action in doubt was resolved
 * Runs also carry "log", the run's events, and "usage": for each capability parameter
 * of the entry function, {"capability", "uses", "charged": [money...]} (RUNTIME.md
 * section 6).
 *
 * Arguments are UTF-8 strings, borrowed for the call. journal_path and engine may be
 * NULL; engine is "interpreter" (the default) or "native". The functions may be
 * called from any thread.
 */
#ifndef STAAL_H
#define STAAL_H

#ifdef __cplusplus
extern "C" {
#endif

char *staal_check(const char *program_path);
char *staal_run(const char *program_path, const char *config_json,
                 const char *journal_path, const char *engine);
char *staal_resume(const char *program_path, const char *config_json,
                    const char *journal_path, const char *engine);
/* resolution_json: {"performed": "<receipt>"}, {"status": 200, "body": "..."},
   {"failed": "<reason>"} or {"retry": true}. */
char *staal_resolve(const char *journal_path, const char *resolution_json);

/* Tools (Linux and macOS): a program's entry function as a tool an app calls again
 * and again, by the same host code `staalc mcp` uses (docs/MCP.md): quotas across
 * calls, `once`, concurrency limits, deadlines, and crash recovery from a journal
 * directory. Handles are decimal strings. Each returns {"status": "ok", ...} or
 * {"status": "error", "error": "..."}.
 *
 * settings_json: {"journals": "<dir>", "keep": 90 | "forever", "max_concurrent_runs": 8,
 *   "notify": [...], "notify_env": [...]}, as `server` in an MCP manifest, paths
 *   relative to the current directory. Opening a directory resumes or settles whatever
 *   an earlier process left there. One process uses a directory at a time; this
 *   process opening it again gets the same host ("host": "<handle>").
 * spec_json: a tool as in an MCP manifest ("name", "description", "program", "entry",
 *   "quota", "once", "deadline", "model", "net", ...). With "grants", every call has
 *   them; without, each call gives its own in grants_json, computed by the app.
 *   Returns "tool", "name", "description", "input_schema", "output_schema",
 *   "annotations", "grants" ("fixed" or "per_call") and "warnings".
 * staal_tool_call returns {"status", "is_error", "text", "structured"?, "run"?}, where
 *   status is "done", "failed", "stopped" (waiting for an operator), "in_progress" (a
 *   repeated call of an operation still running) or "not_run" (refused before any
 *   run: its arguments, the quota, the concurrency limit), and "structured" is
 *   {"result": <value>} for a done run. It returns when the run ends.
 * staal_run_status returns {"run", "status", "observed_at", "result"?, "failure"?}. */
char *staal_host_open(const char *settings_json);
char *staal_host_close(const char *host);
char *staal_tool_open(const char *host, const char *spec_json);
char *staal_tool_call(const char *tool, const char *arguments_json, const char *grants_json /* or NULL */);
char *staal_tool_close(const char *tool);
char *staal_run_status(const char *host, const char *run);

void staal_free(char *s);
/* A static string: do not free it. */
const char *staal_version(void);

#ifdef __cplusplus
}
#endif

#endif
