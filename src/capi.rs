// The C interface (roadmap phase 5): one stable way for any language to check and
// run Staal programs. Python uses it through `python/staal`, with no Python inside
// Staal.
//
//   char *staal_check(const char *program_path);
//   char *staal_run(const char *program_path, const char *config_json,
//                    const char *journal_path /* or NULL */, const char *engine /* or NULL */);
//   char *staal_resume(const char *program_path, const char *config_json,
//                       const char *journal_path, const char *engine /* or NULL */);
//   char *staal_resolve(const char *journal_path, const char *resolution_json);
//   void  staal_free(char *s);
//   const char *staal_version(void);
//
// Every function but `staal_free` and `staal_version` returns a JSON object as a
// NUL-terminated UTF-8 string that the caller must give back to `staal_free`. Its
// "status" says what happened, like the command line's exit codes:
//
//   "done"     the run finished: "result" holds the entry function's value
//   "failed"   the run failed: "failure" holds {"kind", "message"}
//   "stopped"  the run stopped and can continue (an action in doubt: resolve, then
//              resume; or the journal could not be written: resume)
//   "invalid"  the program has check errors: "errors" lists them; nothing ran
//   "error"    a bad argument, file, config or journal: "error" says why; nothing ran
//
// "done", "failed" and "stopped" also carry "log", the run's events. `staal_check`
// returns "ok" or "invalid". `staal_resolve` returns "resolved" or "error".
//
// Nothing here panics across the boundary: a panic inside is caught and reported as
// "error". Strings passed in are borrowed for the call only. Calls may come from any
// thread; runs do not share state.

use crate::interp::{self, journal, Durability, Engine, FailKind};
use serde_json::{json, Value as Json};
use std::ffi::{c_char, CStr, CString};
use std::path::Path;

/// Reads a borrowed C string argument.
///
/// # Safety
/// `p` must be NULL or point to a NUL-terminated string valid for this call.
unsafe fn arg(p: *const c_char, name: &str) -> Result<Option<String>, String> {
    if p.is_null() {
        return Ok(None);
    }
    // SAFETY: the caller promises a valid NUL-terminated string.
    let s = unsafe { CStr::from_ptr(p) };
    s.to_str().map(|s| Some(s.to_string())).map_err(|_| format!("{name} is not UTF-8"))
}

fn required(v: Option<String>, name: &str) -> Result<String, String> {
    v.ok_or(format!("{name} is NULL"))
}

/// Returns a JSON value to C. A string with a NUL inside cannot occur: JSON escapes it.
fn out(v: Json) -> *mut c_char {
    match CString::new(v.to_string()) {
        Ok(c) => c.into_raw(),
        Err(_) => CString::new(r#"{"status":"error","error":"the result contained a NUL byte"}"#).expect("no NUL").into_raw(),
    }
}

fn error(msg: impl Into<String>) -> Json {
    json!({ "status": "error", "error": msg.into() })
}

/// Runs `f`, turning a panic into an "error" result: a panic must never unwind into C.
fn guarded(f: impl FnOnce() -> Json) -> *mut c_char {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(v) => out(v),
        Err(_) => out(error("staalc panicked; this is a bug in staalc")),
    }
}

fn engine(e: Option<String>) -> Result<Engine, String> {
    match e.as_deref() {
        None | Some("interpreter") => Ok(Engine::Interpreter),
        Some("native") => Ok(Engine::Native),
        Some(other) => Err(format!("engine is \"native\" or \"interpreter\", not {other:?}")),
    }
}

fn diagnostics(checked: &crate::Checked, diags: &[crate::checker::Diagnostic]) -> Json {
    Json::Array(
        diags
            .iter()
            .map(|d| {
                json!({
                    "code": d.code,
                    "file": checked.files[d.pos.file as usize].name,
                    "line": d.pos.line,
                    "col": d.pos.col,
                    "message": d.msg,
                    "text": crate::format_diag_in(&checked.files, d),
                })
            })
            .collect(),
    )
}

/// # Safety
/// `program` must be NULL or a valid NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn staal_check(program: *const c_char) -> *mut c_char {
    // SAFETY: forwarded from the caller's promise.
    let program = unsafe { arg(program, "program_path") };
    guarded(move || {
        let program = match program.and_then(|p| required(p, "program_path")) {
            Ok(p) => p,
            Err(e) => return error(e),
        };
        let checked = match crate::check_path(Path::new(&program)) {
            Ok(c) => c,
            Err(e) => return error(e),
        };
        match &checked.result {
            Ok(_) => json!({ "status": "ok" }),
            Err(d) => json!({ "status": "invalid", "errors": diagnostics(&checked, d) }),
        }
    })
}

fn run_program(program: String, config: String, journal_path: Option<String>, resume: bool, engine: Engine) -> Json {
    let checked = match crate::check_path(Path::new(&program)) {
        Ok(c) => c,
        Err(e) => return error(e),
    };
    let prog = match &checked.result {
        Ok(p) => p,
        Err(d) => return json!({ "status": "invalid", "errors": diagnostics(&checked, d) }),
    };
    let cfg: Json = match serde_json::from_str(&config) {
        Ok(v) => v,
        Err(e) => return error(format!("config_json is not valid JSON: {e}")),
    };
    let durability = match (&journal_path, resume) {
        (None, false) => Durability::None,
        (Some(j), false) => Durability::Create(Path::new(j)),
        (Some(j), true) => Durability::Resume(Path::new(j)),
        (None, true) => return error("resume needs journal_path"),
    };
    let res = match interp::run_config_engine(prog, &checked.fingerprint, &cfg, durability, engine) {
        Ok(r) => r,
        Err(e) => return error(e),
    };
    let unchecked = res.unchecked();
    let mut out = match res.result {
        Ok(v) => json!({ "status": "done", "result": v, "log": res.log }),
        Err(f) => {
            let status = if matches!(f.kind, FailKind::InDoubt | FailKind::Journal) { "stopped" } else { "failed" };
            json!({ "status": status, "failure": { "kind": f.kind.to_string(), "message": f.message }, "log": res.log })
        }
    };
    // What each grant used, so a host can account for a run (RUNTIME.md section 6).
    out["usage"] = res.usage;
    // A run that loaded C code says so (RUNTIME.md section 19).
    if !unchecked.is_empty() {
        out["unchecked"] = json!(unchecked);
    }
    out
}

/// # Safety
/// Each argument must be NULL or a valid NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn staal_run(
    program: *const c_char,
    config_json: *const c_char,
    journal_path: *const c_char,
    engine_name: *const c_char,
) -> *mut c_char {
    // SAFETY: forwarded from the caller's promise.
    let args = unsafe {
        (arg(program, "program_path"), arg(config_json, "config_json"), arg(journal_path, "journal_path"), arg(engine_name, "engine"))
    };
    guarded(move || {
        let parsed = (|| {
            Ok::<_, String>((
                required(args.0?, "program_path")?,
                required(args.1?, "config_json")?,
                args.2?,
                engine(args.3?)?,
            ))
        })();
        match parsed {
            Ok((p, c, j, e)) => run_program(p, c, j, false, e),
            Err(e) => error(e),
        }
    })
}

/// # Safety
/// Each argument must be NULL or a valid NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn staal_resume(
    program: *const c_char,
    config_json: *const c_char,
    journal_path: *const c_char,
    engine_name: *const c_char,
) -> *mut c_char {
    // SAFETY: forwarded from the caller's promise.
    let args = unsafe {
        (arg(program, "program_path"), arg(config_json, "config_json"), arg(journal_path, "journal_path"), arg(engine_name, "engine"))
    };
    guarded(move || {
        let parsed = (|| {
            Ok::<_, String>((
                required(args.0?, "program_path")?,
                required(args.1?, "config_json")?,
                required(args.2?, "journal_path")?,
                engine(args.3?)?,
            ))
        })();
        match parsed {
            Ok((p, c, j, e)) => run_program(p, c, Some(j), true, e),
            Err(e) => error(e),
        }
    })
}

/// `resolution_json` is one of {"performed": "<receipt>"}, {"status": 200, "body": "..."},
/// {"failed": "<reason>"} or {"retry": true}, as `staalc resolve`.
///
/// # Safety
/// Each argument must be NULL or a valid NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn staal_resolve(journal_path: *const c_char, resolution_json: *const c_char) -> *mut c_char {
    // SAFETY: forwarded from the caller's promise.
    let args = unsafe { (arg(journal_path, "journal_path"), arg(resolution_json, "resolution_json")) };
    guarded(move || {
        let parsed = (|| Ok::<_, String>((required(args.0?, "journal_path")?, required(args.1?, "resolution_json")?)))();
        let (j, r) = match parsed {
            Ok(v) => v,
            Err(e) => return error(e),
        };
        let r: Json = match serde_json::from_str(&r) {
            Ok(v) => v,
            Err(e) => return error(format!("resolution_json is not valid JSON: {e}")),
        };
        let Some(obj) = r.as_object().filter(|o| !o.is_empty()) else {
            return error("resolution_json must be an object with one of performed, status, failed or retry");
        };
        let keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        let resolution = match keys.as_slice() {
            ["performed"] => match obj["performed"].as_str() {
                Some(receipt) => journal::Resolution::Performed { receipt: receipt.to_string() },
                None => return error("performed: expected the receipt as a string"),
            },
            ["failed"] => match obj["failed"].as_str() {
                Some(m) => journal::Resolution::Failed { message: m.to_string() },
                None => return error("failed: expected the reason as a string"),
            },
            ["retry"] if obj["retry"] == json!(true) => journal::Resolution::Retry,
            ks if ks.contains(&"status") && ks.iter().all(|k| *k == "status" || *k == "body") => {
                let Some(status) = obj["status"].as_u64().and_then(|s| u16::try_from(s).ok()) else {
                    return error("status: expected an HTTP status code such as 200");
                };
                let body = match obj.get("body") {
                    None => String::new(),
                    Some(Json::String(b)) => b.clone(),
                    Some(_) => return error("body: expected a string"),
                };
                journal::Resolution::Responded { status, body }
            }
            _ => return error("resolution_json must have exactly one of performed, status (with body), failed or retry: true"),
        };
        match journal::Journal::resolve(Path::new(&j), resolution, None) {
            Ok(summary) => json!({ "status": "resolved", "summary": summary }),
            Err(e) => error(e),
        }
    })
}

// ---------------------------------------------------------------------------------
// Tools (include/staal.h, "Tools"): a program's entry function as a tool an app calls
// again and again, with quotas, `once`, concurrency and crash recovery across calls,
// by the same host code `staalc mcp` uses (src/host). Handles are decimal strings.

#[cfg(unix)]
mod tools {
    use crate::host::tool::{self, Grants, Tool};
    use crate::host::{Ctl, Host};
    use serde_json::{json, Map, Value as Json};
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex, OnceLock};

    struct Open {
        dir: PathBuf,
        host: Arc<Host>,
        /// How many `staal_host_open` calls hold it.
        holders: usize,
    }

    #[derive(Default)]
    struct Registry {
        next: u64,
        hosts: HashMap<u64, Open>,
        tools: HashMap<u64, (u64, Arc<Tool>)>,
    }

    fn registry() -> &'static Mutex<Registry> {
        static R: OnceLock<Mutex<Registry>> = OnceLock::new();
        R.get_or_init(|| Mutex::new(Registry::default()))
    }

    fn reg() -> Result<std::sync::MutexGuard<'static, Registry>, String> {
        registry().lock().map_err(|_| "the library's tools were lost after a crash in another call".to_string())
    }

    fn id(s: &str, what: &str) -> Result<u64, String> {
        s.parse().map_err(|_| format!("{what} '{s}' is not a handle this library gave"))
    }

    fn object(text: &str, what: &str) -> Result<Map<String, Json>, String> {
        match serde_json::from_str::<Json>(text) {
            Ok(Json::Object(o)) => Ok(o),
            Ok(_) => Err(format!("{what} must be a JSON object")),
            Err(e) => Err(format!("{what} is not valid JSON: {e}")),
        }
    }

    fn cwd() -> Result<PathBuf, String> {
        std::env::current_dir().map_err(|e| format!("cannot read the current directory: {e}"))
    }

    /// Opens the host for a journal directory, or gives the one this process already
    /// has open for it (with the same settings).
    pub fn host_open(settings: &str) -> Result<Json, String> {
        let o = object(settings, "settings_json")?;
        let settings = tool::parse_settings(&o, "settings", &cwd()?, &[])?;
        let mut r = reg()?;
        if let Some((hid, open)) = r.hosts.iter_mut().find(|(_, h)| h.dir == settings.journals) {
            if open.host.settings != settings {
                return Err(format!(
                    "{} is already open in this process with other settings; open it with the same settings, or close it first",
                    settings.journals.display()
                ));
            }
            open.holders += 1;
            return Ok(json!({ "status": "ok", "host": hid.to_string() }));
        }
        let dir = settings.journals.clone();
        let host = Host::open(settings, "with the tool's status, by its run id", &[])?;
        r.next += 1;
        let hid = r.next;
        r.hosts.insert(hid, Open { dir, host, holders: 1 });
        Ok(json!({ "status": "ok", "host": hid.to_string() }))
    }

    /// Lets go of a host; the last holder closes it, with every tool opened on it.
    pub fn host_close(host: &str) -> Result<Json, String> {
        let hid = id(host, "host")?;
        let closed = {
            let mut r = reg()?;
            let open = r.hosts.get_mut(&hid).ok_or(format!("host {hid} is not open"))?;
            open.holders -= 1;
            if open.holders > 0 {
                None
            } else {
                r.tools.retain(|_, (h, _)| *h != hid);
                r.hosts.remove(&hid)
            }
        };
        // Outside the registry's lock: notify commands may take their full time.
        if let Some(open) = closed {
            open.host.wait_for_notifies();
        }
        Ok(json!({ "status": "ok" }))
    }

    /// Defines a tool on a host: the keys of a tool in an MCP manifest. Without
    /// "grants", each call gives them.
    pub fn tool_open(host: &str, spec: &str) -> Result<Json, String> {
        let hid = id(host, "host")?;
        let spec_json = Json::Object(object(spec, "spec_json")?);
        let h = reg()?.hosts.get(&hid).map(|o| o.host.clone()).ok_or(format!("host {hid} is not open"))?;
        let grants = if spec_json.get("grants").is_some() { Grants::Fixed } else { Grants::PerCall };
        let mut warnings = Vec::new();
        let t = tool::parse_tool(&spec_json, "tool", &cwd()?, None, &h.settings, tool::Definer::App(grants), &mut warnings)?;
        let described = json!({
            "status": "ok",
            "name": t.name,
            "title": t.title,
            "description": t.description,
            "input_schema": t.input_schema,
            "output_schema": t.output_schema,
            "annotations": t.annotations,
            "grants": if t.grants == Grants::Fixed { "fixed" } else { "per_call" },
            "app_inputs": t.app_inputs,
            "warnings": warnings,
        });
        let mut r = reg()?;
        if !r.hosts.contains_key(&hid) {
            return Err(format!("host {hid} was closed while the tool was being opened"));
        }
        if r.tools.values().any(|(h, x)| *h == hid && x.name == t.name) {
            return Err(format!("a tool named '{}' is already open on this host", t.name));
        }
        r.next += 1;
        let tid = r.next;
        r.tools.insert(tid, (hid, Arc::new(t)));
        let mut out = described;
        out["tool"] = json!(tid.to_string());
        Ok(out)
    }

    pub fn tool_close(tool: &str) -> Result<Json, String> {
        let tid = id(tool, "tool")?;
        reg()?.tools.remove(&tid).ok_or(format!("tool {tid} is not open"))?;
        Ok(json!({ "status": "ok" }))
    }

    /// One call: a run, or the answer of an earlier one (`once`), or a refusal.
    pub fn tool_call(
        tool: &str,
        arguments: &str,
        app_inputs: Option<&str>,
        grants: Option<&str>,
        scope: Option<&str>,
    ) -> Result<Json, String> {
        let tid = id(tool, "tool")?;
        let args = object(arguments, "arguments_json")?;
        let app_inputs = app_inputs.map(|i| object(i, "app_inputs_json")).transpose()?;
        let grants = grants.map(|g| object(g, "grants_json")).transpose()?;
        let (host, t) = {
            let r = reg()?;
            let (hid, t) = r.tools.get(&tid).ok_or(format!("tool {tid} is not open"))?;
            let host = r.hosts.get(hid).map(|o| o.host.clone()).ok_or(format!("the host of tool {tid} is closed"))?;
            (host, t.clone())
        };
        let ctl = Ctl::default();
        let a = host.call(&t, &args, app_inputs.as_ref(), grants.as_ref(), scope, &ctl, &|_| {}).ok_or("the call was cancelled")?;
        let mut out = json!({ "status": a.status, "is_error": a.is_error, "text": a.text });
        if let Some(run) = a.run {
            out["run"] = json!(run);
        }
        if let Some(s) = a.structured {
            out["structured"] = s;
        }
        Ok(out)
    }

    pub fn run_status(host: &str, run: &str, scope: Option<&str>) -> Result<Json, String> {
        let hid = id(host, "host")?;
        let ok = run.len() == 32 && run.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        if !ok {
            return Err(format!("'{run}' is not a run id: 32 lowercase hexadecimal characters"));
        }
        let h = reg()?.hosts.get(&hid).map(|o| o.host.clone()).ok_or(format!("host {hid} is not open"))?;
        Ok(h.status(run, scope))
    }

}

/// Opens the host that keeps tool runs in a journal directory (include/staal.h, Tools).
///
/// # Safety
/// The argument must be a valid NUL-terminated string.
#[cfg(unix)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn staal_host_open(settings_json: *const c_char) -> *mut c_char {
    // SAFETY: forwarded from the caller's promise.
    let a = unsafe { arg(settings_json, "settings_json") };
    guarded(move || match a.and_then(|a| required(a, "settings_json")).and_then(|s| tools::host_open(&s)) {
        Ok(v) => v,
        Err(e) => error(e),
    })
}

/// # Safety
/// The argument must be a valid NUL-terminated string.
#[cfg(unix)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn staal_host_close(host: *const c_char) -> *mut c_char {
    // SAFETY: forwarded from the caller's promise.
    let a = unsafe { arg(host, "host") };
    guarded(move || match a.and_then(|a| required(a, "host")).and_then(|h| tools::host_close(&h)) {
        Ok(v) => v,
        Err(e) => error(e),
    })
}

/// # Safety
/// Each argument must be a valid NUL-terminated string.
#[cfg(unix)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn staal_tool_open(host: *const c_char, spec_json: *const c_char) -> *mut c_char {
    // SAFETY: forwarded from the caller's promise.
    let a = unsafe { (arg(host, "host"), arg(spec_json, "spec_json")) };
    guarded(move || {
        let r = (|| tools::tool_open(&required(a.0?, "host")?, &required(a.1?, "spec_json")?))();
        r.unwrap_or_else(error)
    })
}

/// # Safety
/// Each argument must be a valid NUL-terminated string; `grants_json` and `scope` may be
/// NULL.
#[cfg(unix)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn staal_tool_call(
    tool: *const c_char,
    arguments_json: *const c_char,
    grants_json: *const c_char,
    scope: *const c_char,
) -> *mut c_char {
    // SAFETY: forwarded from the caller's promise.
    let a = unsafe { (arg(tool, "tool"), arg(arguments_json, "arguments_json"), arg(grants_json, "grants_json"), arg(scope, "scope")) };
    guarded(move || {
        let r = (|| tools::tool_call(&required(a.0?, "tool")?, &required(a.1?, "arguments_json")?, None, a.2?.as_deref(), a.3?.as_deref()))();
        r.unwrap_or_else(error)
    })
}

/// `staal_tool_call` with the app's inputs for the call, which a tool defined with
/// "app_inputs" needs (include/staal.h).
///
/// # Safety
/// Each argument must be a valid NUL-terminated string; `app_inputs_json`,
/// `grants_json` and `scope` may be NULL.
#[cfg(unix)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn staal_tool_call_with_inputs(
    tool: *const c_char,
    arguments_json: *const c_char,
    app_inputs_json: *const c_char,
    grants_json: *const c_char,
    scope: *const c_char,
) -> *mut c_char {
    // SAFETY: forwarded from the caller's promise.
    let a = unsafe {
        (
            arg(tool, "tool"),
            arg(arguments_json, "arguments_json"),
            arg(app_inputs_json, "app_inputs_json"),
            arg(grants_json, "grants_json"),
            arg(scope, "scope"),
        )
    };
    guarded(move || {
        let r = (|| {
            tools::tool_call(
                &required(a.0?, "tool")?,
                &required(a.1?, "arguments_json")?,
                a.2?.as_deref(),
                a.3?.as_deref(),
                a.4?.as_deref(),
            )
        })();
        r.unwrap_or_else(error)
    })
}

/// # Safety
/// The argument must be a valid NUL-terminated string.
#[cfg(unix)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn staal_tool_close(tool: *const c_char) -> *mut c_char {
    // SAFETY: forwarded from the caller's promise.
    let a = unsafe { arg(tool, "tool") };
    guarded(move || match a.and_then(|a| required(a, "tool")).and_then(|t| tools::tool_close(&t)) {
        Ok(v) => v,
        Err(e) => error(e),
    })
}

/// # Safety
/// Each argument must be a valid NUL-terminated string; `scope` may be NULL.
#[cfg(unix)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn staal_run_status(host: *const c_char, run: *const c_char, scope: *const c_char) -> *mut c_char {
    // SAFETY: forwarded from the caller's promise.
    let a = unsafe { (arg(host, "host"), arg(run, "run"), arg(scope, "scope")) };
    guarded(move || {
        let r = (|| tools::run_status(&required(a.0?, "host")?, &required(a.1?, "run")?, a.2?.as_deref()))();
        r.unwrap_or_else(error)
    })
}

/// Frees a string returned by this library. NULL is ignored.
///
/// # Safety
/// `s` must be NULL or a string returned by a `staal_*` function, freed only once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn staal_free(s: *mut c_char) {
    if !s.is_null() {
        // SAFETY: `s` came from CString::into_raw in `out`, per the caller's promise.
        drop(unsafe { CString::from_raw(s) });
    }
}

/// The library's version, a static string: do not free it.
#[unsafe(no_mangle)]
pub extern "C" fn staal_version() -> *const c_char {
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr().cast::<c_char>()
}
