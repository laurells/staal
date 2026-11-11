// Runs a package's tests (packages/<name>/tests/*.json) against a fake service, the
// way packages/README.md ("Testing a package") describes. Each test is one JSON file:
//
//   {
//     "about": "What it shows, in one sentence.",
//     "service": "https://api.example.com",       the base URL the package reaches
//     "routes": [ ... ],                          the fake service (common/fake.rs)
//     "program": "import example.notes\n...",     a program using the package
//     "entry": "main",                            optional, default "main"
//     "inputs": { ... },                          optional
//     "grants": { ... },                          as in a run config
//     "expect": {
//       "result": ...,  or  "failure": { "kind": "capability", "message": "part of it" },
//       "requests": [ { "method", "path", "json" | "form" | "body", "headers" } ]
//     }
//   }
//
// The package is copied with `service` replaced by the fake's address in its code and
// manifest, committed to a git repository and fetched with `staalc fetch`, so the
// compiler holds it to its manifest's authority as it would for a user. In `grants`,
// "{service}" stands for the fake's address, and every grant of a capability that
// connects is given the fake's loopback address (`private_addresses`), which the real
// service's public one would not need; every secret is {"env":
// "STAAL_PACKAGE_KEY"}, whose value is KEY, and "{key}" in an expected header stands
// for it. `requests`, when given, is every request the service received, in order:
// `[]` says the run was refused before any request.
//
// Every test runs on both engines (the interpreter, and native code when built with
// it), each against its own fake service, with the same expectations.
//
// Every test also checks that the key never appears in the run's log or journal. Built
// with the `failpoints` feature, each test is then run again, killed after every
// journal write and between every action and its result, resumed (and, for an action
// left in doubt, resolved from what the fake service really received), and must end as
// the uninterrupted run did, with the same effects on the service: none lost, none
// made twice.

use super::fake::{Fake, Route};
use serde_json::{json, Map, Value as Json};
use staalc::interp::{Durability, Engine};
use std::path::{Path, PathBuf};
use std::process::Command;

/// What took effect at a fake service, by method and path.
type Effects = std::collections::HashMap<(String, String), usize>;

pub const KEY: &str = "pkgcase_secret_8c7d6e5f4a3b2c1d";
pub const KEY_ENV: &str = "STAAL_PACKAGE_KEY";
const BIN: &str = env!("CARGO_BIN_EXE_staalc");

/// Sets the key every package test grants. Call before any test runs a program.
pub fn init() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    // SAFETY: set once, before any run reads the environment.
    ONCE.call_once(|| unsafe { std::env::set_var(KEY_ENV, KEY) });
}

struct Case {
    service: String,
    routes: Vec<Route>,
    program: String,
    entry: String,
    inputs: Json,
    grants: Json,
    result: Option<Json>,
    failure: Option<(String, String)>,
    requests: Option<Vec<Json>>,
}

fn load(path: &Path) -> Result<Case, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let j: Json = serde_json::from_str(&text).map_err(|e| format!("not JSON: {e}"))?;
    let o = j.as_object().ok_or("expected an object")?;
    for k in o.keys() {
        if !["about", "service", "routes", "program", "entry", "inputs", "grants", "expect"].contains(&k.as_str()) {
            return Err(format!("unknown key '{k}'"));
        }
    }
    let s = |k: &str| o.get(k).and_then(Json::as_str).map(str::to_string).ok_or(format!("\"{k}\": expected a string"));
    s("about")?;
    let service = s("service")?;
    if !service.starts_with("https://") || service.ends_with('/') {
        return Err("\"service\": expected the service's base URL, https:// and no / at the end".into());
    }
    let e = o.get("expect").and_then(Json::as_object).ok_or("\"expect\": expected an object")?;
    for k in e.keys() {
        if !["result", "failure", "requests"].contains(&k.as_str()) {
            return Err(format!("expect: unknown key '{k}'"));
        }
    }
    let failure = match e.get("failure") {
        None => None,
        Some(f) => Some((
            f["kind"].as_str().ok_or("expect.failure.kind: expected a string")?.to_string(),
            f["message"].as_str().ok_or("expect.failure.message: expected a string")?.to_string(),
        )),
    };
    if e.contains_key("result") == failure.is_some() {
        return Err("expect: give exactly one of \"result\" and \"failure\"".into());
    }
    let requests = match e.get("requests") {
        None => None,
        Some(r) => Some(r.as_array().ok_or("expect.requests: expected a list")?.clone()),
    };
    Ok(Case {
        service,
        routes: Route::list(o.get("routes").unwrap_or(&json!([])))?,
        program: s("program")?,
        entry: o.get("entry").and_then(Json::as_str).unwrap_or("main").to_string(),
        inputs: o.get("inputs").cloned().unwrap_or(json!({})),
        grants: o.get("grants").cloned().ok_or("\"grants\": missing")?,
        result: e.get("result").cloned(),
        failure,
        requests,
    })
}

/// `grants` with each grant of a capability that connects (at any depth of `needs`)
/// allowed the loopback address the fake service listens on.
fn local(grants: &Json) -> Json {
    match grants {
        Json::Object(o) => {
            let mut o: serde_json::Map<String, Json> = o.iter().map(|(k, x)| (k.clone(), local(x))).collect();
            let connects = o.get("cap").and_then(Json::as_str).is_some_and(|c| c.starts_with("net.http.") || c == "net.tcp.exchange" || c == "net.tls.exchange");
            if connects && !o.contains_key("private_addresses") {
                o.insert("private_addresses".into(), serde_json::json!(["127.0.0.1"]));
            }
            Json::Object(o)
        }
        other => other.clone(),
    }
}

/// `v` with every "{service}" in its strings replaced.
fn fill(v: &Json, service: &str) -> Json {
    match v {
        Json::String(s) => Json::String(s.replace("{service}", service)),
        Json::Array(a) => Json::Array(a.iter().map(|x| fill(x, service)).collect()),
        Json::Object(o) => Json::Object(o.iter().map(|(k, x)| (k.clone(), fill(x, service))).collect()),
        x => x.clone(),
    }
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false"])
        .args(args)
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if !out.status.success() {
        return Err(format!("git {args:?}: {}", String::from_utf8_lossy(&out.stderr)));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Copies the package (all but its tests) with `from` replaced by `to` in every file.
fn copy_package(src: &Path, dst: &Path, from: &str, to: &str) -> Result<usize, String> {
    let mut replaced = 0;
    std::fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    for e in std::fs::read_dir(src).map_err(|e| format!("{}: {e}", src.display()))? {
        let e = e.map_err(|e| e.to_string())?;
        let name = e.file_name().to_string_lossy().into_owned();
        if name == "tests" || name.starts_with('.') {
            continue;
        }
        if e.path().is_dir() {
            replaced += copy_package(&e.path(), &dst.join(&name), from, to)?;
            continue;
        }
        let text = std::fs::read_to_string(e.path()).map_err(|e| e.to_string())?;
        replaced += text.matches(from).count();
        std::fs::write(dst.join(&name), text.replace(from, to)).map_err(|e| e.to_string())?;
    }
    Ok(replaced)
}

fn check_request(i: usize, want: &Json, got: &super::fake::Seen) -> Result<(), String> {
    let at = format!("request {}", i + 1);
    let o = want.as_object().ok_or(format!("expect.requests[{i}]: expected an object"))?;
    for k in o.keys() {
        if !["method", "path", "json", "form", "body", "headers"].contains(&k.as_str()) {
            return Err(format!("expect.requests[{i}]: unknown key '{k}'"));
        }
    }
    if let Some(m) = o.get("method").and_then(Json::as_str)
        && m != got.method
    {
        return Err(format!("{at}: method {} where {m} was expected", got.method));
    }
    if let Some(p) = o.get("path").and_then(Json::as_str)
        && p != got.path
    {
        return Err(format!("{at}: path {} where {p} was expected", got.path));
    }
    if let Some(j) = o.get("json") {
        let body: Json = serde_json::from_str(&got.body).map_err(|e| format!("{at}: the body is not JSON ({e}): {}", got.body))?;
        if &body != j {
            return Err(format!("{at}: JSON body {body} where {j} was expected"));
        }
    }
    if let Some(f) = o.get("form") {
        let mut fields = Map::new();
        for pair in got.body.split('&').filter(|p| !p.is_empty()) {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            fields.insert(decode(k), json!(decode(v)));
        }
        if &Json::Object(fields.clone()) != f {
            return Err(format!("{at}: form body {} where {f} was expected", Json::Object(fields)));
        }
    }
    if let Some(b) = o.get("body").and_then(Json::as_str)
        && b != got.body
    {
        return Err(format!("{at}: body {:?} where {b:?} was expected", got.body));
    }
    if let Some(h) = o.get("headers").and_then(Json::as_object) {
        for (k, v) in h {
            let want = v.as_str().unwrap_or_default().replace("{key}", KEY);
            match got.headers.get(&k.to_ascii_lowercase()) {
                Some(have) if *have == want => {}
                Some(have) => return Err(format!("{at}: header {k} is {have:?}, not {:?}", v.as_str().unwrap_or_default())),
                None => return Err(format!("{at}: no header {k}")),
            }
        }
    }
    Ok(())
}

/// Percent-decoding of a form field (`+` is a space).
fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok());
                match hex {
                    Some(v) => {
                        out.push(v);
                        i += 2;
                    }
                    None => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Runs every test of the package in `pkg` (its `tests/*.json`), in `work`. Returns
/// the number of tests run, or every failure, each naming its test.
pub fn run_package(pkg: &Path, work: &Path) -> Result<usize, Vec<String>> {
    init();
    let tests = pkg.join("tests");
    let mut files: Vec<PathBuf> = match std::fs::read_dir(&tests) {
        Ok(rd) => rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")).collect(),
        Err(e) => return Err(vec![format!("{}: {e}", tests.display())]),
    };
    files.sort();
    if files.is_empty() {
        return Err(vec![format!("{} has no tests", tests.display())]);
    }
    let mut failures = Vec::new();
    for f in &files {
        let name = f.file_stem().unwrap().to_string_lossy().into_owned();
        let dir = work.join(&name);
        let _ = std::fs::remove_dir_all(&dir);
        if let Err(e) = load(f).and_then(|c| run_case(pkg, &c, &dir)) {
            failures.push(format!("{}: {e}", f.display()));
        }
    }
    if failures.is_empty() { Ok(files.len()) } else { Err(failures) }
}

/// The engines a test runs on: native code too, when this build has it.
fn engines() -> Vec<Engine> {
    if cfg!(feature = "native") { vec![Engine::Interpreter, Engine::Native] } else { vec![Engine::Interpreter] }
}

fn engine_name(engine: Engine) -> &'static str {
    match engine {
        Engine::Interpreter => "interpreter",
        Engine::Native => "native",
    }
}

fn run_case(pkg: &Path, c: &Case, dir: &Path) -> Result<(), String> {
    for engine in engines() {
        run_on(pkg, c, &dir.join(engine_name(engine)), engine).map_err(|e| format!("({}) {e}", engine_name(engine)))?;
    }
    Ok(())
}

fn run_on(pkg: &Path, c: &Case, dir: &Path, engine: Engine) -> Result<(), String> {
    let fake = Fake::start(c.routes.clone());
    let base = fake.base().to_string();
    let manifest: Json = serde_json::from_str(&std::fs::read_to_string(pkg.join("staal.json")).map_err(|e| format!("staal.json: {e}"))?)
        .map_err(|e| format!("staal.json: {e}"))?;
    let name = manifest["package"].as_str().ok_or("staal.json has no \"package\"")?.to_string();
    if !manifest["authority"].to_string().contains(&c.service) {
        return Err(format!("the package's staal.json declares no authority under {}", c.service));
    }
    let repo = dir.join("repos").join(&name);
    copy_package(pkg, &repo, &c.service, &base)?;
    git(&repo, &["init", "-q"])?;
    git(&repo, &["add", "-A"])?;
    git(&repo, &["commit", "-q", "-m", "package"])?;
    let commit = git(&repo, &["rev-parse", "HEAD"])?;
    let app = dir.join("app");
    std::fs::create_dir_all(&app).map_err(|e| e.to_string())?;
    let deps = json!({ "dependencies": { name.clone(): { "git": repo.to_string_lossy(), "commit": commit } } });
    std::fs::write(app.join("staal.json"), deps.to_string()).map_err(|e| e.to_string())?;
    std::fs::write(app.join("main.staal"), &c.program).map_err(|e| e.to_string())?;
    staalc::packages::fetch(&app)?;
    let checked = staalc::check_path(&app.join("main.staal"))?;
    let prog = match checked.result {
        Ok(p) => p,
        Err(d) => {
            let shown: Vec<String> = d.iter().map(|d| staalc::format_diag_in(&checked.files, d)).collect();
            return Err(format!("the program or the package has check errors:\n{}", shown.join("\n")));
        }
    };
    let cfg = json!({
        "entry": c.entry, "inputs": c.inputs, "grants": local(&fill(&c.grants, &base)),
        "model": { "provider": "script", "responses": [] },
        "net": { "connect_timeout_ms": 2000, "timeout_ms": 5000 }
    });
    std::fs::write(app.join("run.json"), cfg.to_string()).map_err(|e| e.to_string())?;

    // The run, uninterrupted.
    let journal = app.join("clean.journal");
    let out = staalc::interp::run_config_engine(&prog, &checked.fingerprint, &cfg, Durability::Create(&journal), engine)?;
    let clean_effects = fake.effects();
    let seen = fake.requests();
    match (&out.result, &c.result, &c.failure) {
        (Ok(v), Some(want), _) if v == want => {}
        (Ok(v), Some(want), _) => return Err(format!("the result is {v}, not {want}")),
        (Ok(v), None, Some((k, _))) => return Err(format!("the run gave {v}; a {k} failure was expected")),
        (Err(f), _, Some((k, m))) if f.kind.to_string() == *k && f.message.contains(m.as_str()) => {}
        (Err(f), _, _) => return Err(format!("the run failed ({}): {}", f.kind, f.message)),
        (Ok(_), None, None) => unreachable!("checked when loaded"),
    }
    if let Some(want) = &c.requests {
        if want.len() != seen.len() {
            let got: Vec<String> = seen.iter().map(|r| format!("{} {}", r.method, r.path)).collect();
            return Err(format!("{} request(s) where {} were expected: {got:?}", seen.len(), want.len()));
        }
        for (i, (w, g)) in want.iter().zip(&seen).enumerate() {
            check_request(i, w, g)?;
        }
    }
    let log = serde_json::to_string(&out.log).unwrap_or_default();
    if log.contains(KEY) {
        return Err("the run's log contains the key".into());
    }
    if std::fs::read_to_string(&journal).unwrap_or_default().contains(KEY) {
        return Err("the journal contains the key".into());
    }
    let clean_end = end_of(&journal)?;
    if cfg!(feature = "failpoints") {
        crashes(&app, &fake, &clean_end, &clean_effects, engine)?;
    }
    Ok(())
}

/// How a journaled run ended: done, or failed and of which kind.
fn end_of(journal: &Path) -> Result<String, String> {
    let text = std::fs::read_to_string(journal).map_err(|e| e.to_string())?;
    let last: Json = text.lines().last().and_then(|l| serde_json::from_str(l).ok()).ok_or("the journal is empty")?;
    if last["j"] != "end" {
        return Err(format!("the journal does not end with an end record: {last}"));
    }
    Ok(if last["ok"] == true { "done".to_string() } else { format!("failed ({})", last["failure"].as_str().unwrap_or("?")) })
}

fn staalc(app: &Path, args: &[&str], failpoint: Option<&str>) -> std::process::Output {
    let mut cmd = Command::new(BIN);
    cmd.current_dir(app).args(args).env(KEY_ENV, KEY).env_remove("STAALC_FAILPOINT");
    if let Some(fp) = failpoint {
        cmd.env("STAALC_FAILPOINT", fp);
    }
    cmd.output().expect("staalc runs")
}

/// Kills the run at every point it makes something durable, resumes it, and checks it
/// ends as `clean_end` with the same effects on the service as the uninterrupted run.
fn crashes(app: &Path, fake: &Fake, clean_end: &str, clean: &Effects, engine: Engine) -> Result<(), String> {
    let mut points: Vec<String> = Vec::new();
    for kind in ["after_write", "before_result"] {
        for n in 1.. {
            let fp = format!("{kind}:{n}");
            match crash_once(app, fake, &fp, engine)? {
                Some((end, effects)) => {
                    if end != clean_end {
                        return Err(format!("killed at {fp}, the run ended {end}, not {clean_end}"));
                    }
                    if &effects != clean {
                        return Err(format!("killed at {fp}, the service saw {effects:?}, not {clean:?}"));
                    }
                    points.push(fp);
                }
                // The failpoint was never reached: there is no further point to try.
                None => break,
            }
            if n > 200 {
                return Err("more than 200 crash points; the test loops".into());
            }
        }
    }
    if points.is_empty() {
        return Err("no crash point was reached".into());
    }
    Ok(())
}

/// One run killed at `fp`, then resumed (and resolved) to its end. None if `fp` was
/// never reached. Gives how it ended and what took effect at the service meanwhile.
fn crash_once(app: &Path, fake: &Fake, fp: &str, engine: Engine) -> Result<Option<(String, Effects)>, String> {
    let journal = app.join("crash.journal");
    let _ = std::fs::remove_file(&journal);
    let before = fake.effects();
    let seen_before = fake.requests().len();
    let j = journal.to_string_lossy().into_owned();
    let native: &[&str] = if engine == Engine::Native { &["--engine", "native"] } else { &[] };
    let run = [&["run", "main.staal", "--config", "run.json", "--journal", &j][..], native].concat();
    let out = staalc(app, &run, Some(fp));
    if out.status.code().is_some() {
        return Ok(None);
    }
    for _ in 0..5 {
        let resume = [&["resume", "main.staal", "--config", "run.json", "--journal", &j][..], native].concat();
        let out = staalc(app, &resume, None);
        match out.status.code() {
            Some(0) | Some(1) => {
                let after = fake.effects();
                let mut delta = std::collections::HashMap::new();
                for (k, n) in after {
                    let d = n - before.get(&k).copied().unwrap_or(0);
                    if d > 0 {
                        delta.insert(k, d);
                    }
                }
                return Ok(Some((end_of(&journal)?, delta)));
            }
            Some(3) => {
                // An action in doubt: resolve it from what the service really received.
                let sent_now = fake.requests()[seen_before..].to_vec();
                let resolution: Vec<String> = match (fp.starts_with("before_result"), sent_now.last()) {
                    (true, Some(r)) => vec!["--status".into(), r.status.to_string(), "--body".into(), r.answer.clone()],
                    _ => vec!["--retry".into()],
                };
                let mut args = vec!["resolve".to_string(), "--journal".into(), j.clone()];
                args.extend(resolution);
                let args: Vec<&str> = args.iter().map(String::as_str).collect();
                let r = staalc(app, &args, None);
                if !r.status.success() {
                    return Err(format!("killed at {fp}: resolve failed: {}", String::from_utf8_lossy(&r.stderr)));
                }
            }
            _ => {
                return Err(format!("killed at {fp}: resume exited {:?}: {}", out.status.code(), String::from_utf8_lossy(&out.stderr)));
            }
        }
    }
    Err(format!("killed at {fp}: the run did not end after five resumes"))
}
