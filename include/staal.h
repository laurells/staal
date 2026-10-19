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
 * Runs also carry "log", the run's events.
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
void staal_free(char *s);
/* A static string: do not free it. */
const char *staal_version(void);

#ifdef __cplusplus
}
#endif

#endif
