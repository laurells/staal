/* The C interface to Intent (src/capi.rs). Link with libintentc.
 *
 * Every function returning char* returns a JSON object that the caller must free
 * with intent_free. Its "status" is one of:
 *   "done"     the run finished; "result" holds the entry function's value
 *   "failed"   the run failed; "failure" holds {"kind", "message"}
 *   "stopped"  the run stopped and can continue: resolve the action in doubt, then
 *              resume (or just resume, if the journal could not be written)
 *   "invalid"  the program has check errors; "errors" lists them; nothing ran
 *   "error"    a bad argument, file, config or journal; "error" says why; nothing ran
 *   "ok"       (intent_check) the program is valid
 *   "resolved" (intent_resolve) the action in doubt was resolved
 * Runs also carry "log", the run's events.
 *
 * Arguments are UTF-8 strings, borrowed for the call. journal_path and engine may be
 * NULL; engine is "interpreter" (the default) or "native". The functions may be
 * called from any thread.
 */
#ifndef INTENT_H
#define INTENT_H

#ifdef __cplusplus
extern "C" {
#endif

char *intent_check(const char *program_path);
char *intent_run(const char *program_path, const char *config_json,
                 const char *journal_path, const char *engine);
char *intent_resume(const char *program_path, const char *config_json,
                    const char *journal_path, const char *engine);
/* resolution_json: {"performed": "<receipt>"}, {"status": 200, "body": "..."},
   {"failed": "<reason>"} or {"retry": true}. */
char *intent_resolve(const char *journal_path, const char *resolution_json);
void intent_free(char *s);
/* A static string: do not free it. */
const char *intent_version(void);

#ifdef __cplusplus
}
#endif

#endif
