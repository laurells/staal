// Executables built by `intentc build` (phase 5): the same results, failures, logs
// and journals as `intentc run` on the interpreter, for every runtime case; journals
// that each can resume for the other; refusal of a damaged program; and the build's
// own errors.

#![cfg(all(feature = "native", target_os = "linux"))]

mod common;

use serde_json::{json, Value as Json};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_intentc");
const ROOT: &str = env!("CARGO_MANIFEST_DIR");

/// The runtime library cargo built for this test run: next to the test binary.
fn runtime() -> PathBuf {
    let dir = std::env::current_exe().unwrap().parent().unwrap().to_path_buf();
    let lib = dir.join("libintentc.a");
    assert!(lib.is_file(), "no libintentc.a in {}", dir.display());
    lib
}

fn tmp(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR")).join("aot").join(name);
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

struct Out {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn cmd(program: &Path, args: &[&str], failpoint: Option<&str>) -> Out {
    let mut c = Command::new(program);
    c.args(args);
    match failpoint {
        Some(fp) => c.env("INTENTC_FAILPOINT", fp),
        None => c.env_remove("INTENTC_FAILPOINT"),
    };
    let o = c.output().unwrap();
    Out {
        code: o.status.code(),
        stdout: String::from_utf8_lossy(&o.stdout).trim().to_string(),
        stderr: String::from_utf8_lossy(&o.stderr).trim().to_string(),
    }
}

fn build(program: &Path, out: &Path) -> Out {
    let rt = runtime();
    cmd(Path::new(BIN), &["build", program.to_str().unwrap(), "-o", out.to_str().unwrap(), "--runtime", rt.to_str().unwrap()], None)
}

/// Lines of a log or journal, with the run's random id replaced wherever it appears.
fn normal_lines(path: &Path) -> Vec<Json> {
    let text = fs::read_to_string(path).unwrap_or_default();
    let id = text
        .lines()
        .filter_map(|l| serde_json::from_str::<Json>(l).ok())
        .find_map(|v| v["run_id"].as_str().map(str::to_string))
        .unwrap_or_default();
    let text = if id.is_empty() { text } else { text.replace(&id, "RUN") };
    text.lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

#[test]
fn every_runtime_case_is_identical_as_an_executable() {
    let dir = Path::new(ROOT).join("tests/run");
    let mut by_program: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for e in fs::read_dir(&dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == "json") {
            let stem = p.file_stem().unwrap().to_string_lossy().to_string();
            by_program.entry(stem.split("__").next().unwrap().to_string()).or_default().push(p);
        }
    }
    let work = tmp("cases");
    let (mut cases, mut failures) = (0, Vec::new());
    for (program, configs) in &by_program {
        let src = dir.join(format!("{program}.int"));
        let app = work.join(program);
        let b = build(&src, &app);
        assert_eq!(b.code, Some(0), "{program}: build failed: {}", b.stderr);
        for case in configs {
            let stem = case.file_stem().unwrap().to_string_lossy().to_string();
            let mut cfg: Json = serde_json::from_str(&fs::read_to_string(case).unwrap()).unwrap();
            cfg.as_object_mut().unwrap().remove("expect");
            let cfg_path = work.join(format!("{stem}.json"));
            fs::write(&cfg_path, cfg.to_string()).unwrap();
            let p = |s: &str| work.join(format!("{stem}.{s}")).to_str().unwrap().to_string();
            let c = cfg_path.to_str().unwrap();
            let i = cmd(Path::new(BIN), &["run", src.to_str().unwrap(), "--config", c, "--journal", &p("i.journal"), "--log", &p("i.log")], None);
            let a = cmd(&app, &["run", "--config", c, "--journal", &p("a.journal"), "--log", &p("a.log")], None);
            let same = i.code == a.code
                && i.stdout == a.stdout
                && i.stderr == a.stderr
                && normal_lines(Path::new(&p("i.log"))) == normal_lines(Path::new(&p("a.log")))
                && normal_lines(Path::new(&p("i.journal"))) == normal_lines(Path::new(&p("a.journal")));
            if !same {
                failures.push(format!(
                    "{stem}:\n  intentc {:?} {} {}\n  app     {:?} {} {}",
                    i.code, i.stdout, i.stderr, a.code, a.stdout, a.stderr
                ));
            }
            cases += 1;
        }
    }
    assert!(failures.is_empty(), "{} of {cases} cases differ:\n{}", failures.len(), failures.join("\n"));
    println!("{cases} runtime cases identical as executables ({} programs built)", by_program.len());
}

/// A program with modules: the executable carries every module's text.
#[test]
fn a_program_with_modules_runs_as_an_executable() {
    let s = common::server::Server::start();
    let work = tmp("modules");
    let src = Path::new(ROOT).join("examples/capabilities/remind.int");
    let app = work.join("remind");
    assert_eq!(build(&src, &app).code, Some(0));
    let cfg = json!({
        "entry": "remind",
        "inputs": { "patients": [ { "name": "Ada", "phone": "+234 801 234 5678" }, { "name": "Bo", "phone": "2348098765432" } ] },
        "grants": { "texts": { "cap": "termii.sms.send", "only": ["2348012345678"], "needs": {
            "http": { "cap": "net.http.post", "only": [s.url("/api/sms/send")] },
            "key": { "env": "INTENT_AOT_KEY" }, "base": s.url(""), "sender": "Clinic" } } },
        "model": { "provider": "script", "responses": [] }
    });
    let c = work.join("cfg.json");
    fs::write(&c, cfg.to_string()).unwrap();
    let mut command = Command::new(&app);
    command.args(["run", "--config", c.to_str().unwrap()]).env("INTENT_AOT_KEY", "sk_aot_test_123456");
    let o = command.output().unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), "1");
    assert_eq!(s.requests().len(), 1, "one number was allowed, one refused");
}

/// Journals are bound to the program, not to the engine: a run started by the
/// executable resumes under intentc, and the other way round, after a crash at every
/// point.
#[cfg(feature = "failpoints")]
#[test]
fn a_crash_on_one_resumes_on_the_other() {
    let work = tmp("crash");
    let src = Path::new(ROOT).join("tests/crash/native.int");
    let app = work.join("native_app");
    assert_eq!(build(&src, &app).code, Some(0));
    let mut cfg: Json = serde_json::from_str(&fs::read_to_string(Path::new(ROOT).join("tests/crash/native.json")).unwrap()).unwrap();
    let mut run_dir = |name: &str| -> (PathBuf, PathBuf) {
        let d = work.join(name);
        fs::create_dir_all(&d).unwrap();
        cfg["adapters"]["simulated"]["world"] = json!(d.join("world.jsonl").to_str().unwrap());
        cfg["adapters"]["simulated"]["idempotent"] = json!(true);
        let c = d.join("cfg.json");
        fs::write(&c, cfg.to_string()).unwrap();
        (d, c)
    };
    let world = |d: &Path| -> Vec<Json> {
        fs::read_to_string(d.join("world.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|l| {
                let mut v: Json = serde_json::from_str(l).unwrap();
                v.as_object_mut().unwrap().remove("key");
                v
            })
            .collect()
    };
    let intentc = |args: &[&str], fp: Option<&str>| cmd(Path::new(BIN), args, fp);
    let src_s = src.to_str().unwrap();
    // The reference: the interpreter, uninterrupted.
    let (bd, bc) = run_dir("baseline");
    let bj = bd.join("journal");
    let base = intentc(&["run", src_s, "--config", bc.to_str().unwrap(), "--journal", bj.to_str().unwrap()], None);
    assert_eq!(base.code, Some(0), "{}", base.stderr);
    let writes = normal_lines(&bj).len();
    let ops = normal_lines(&bj).iter().filter(|r| r["j"] == "intent").count();
    let mut points: Vec<String> = (1..=writes).map(|k| format!("after_write:{k}")).collect();
    points.extend((1..=ops).map(|n| format!("before_result:{n}")));
    for crash_on_app in [true, false] {
        for fp in &points {
            let label = format!("{} {fp}", if crash_on_app { "app->intentc" } else { "intentc->app" });
            let (d, c) = run_dir(&format!("{}-{}", crash_on_app, fp.replace(':', "-")));
            let j = d.join("journal");
            let (c, j) = (c.to_str().unwrap().to_string(), j.to_str().unwrap().to_string());
            let first = if crash_on_app {
                cmd(&app, &["run", "--config", &c, "--journal", &j], Some(fp))
            } else {
                intentc(&["run", src_s, "--config", &c, "--journal", &j], Some(fp))
            };
            assert_eq!(first.code, None, "{label}: the crash did not happen: {}", first.stderr);
            let finished = if crash_on_app {
                intentc(&["resume", src_s, "--config", &c, "--journal", &j], None)
            } else {
                cmd(&app, &["resume", "--config", &c, "--journal", &j], None)
            };
            assert_eq!(finished.code, Some(0), "{label}: {}", finished.stderr);
            assert_eq!(finished.stdout, base.stdout, "{label}: a different result");
            assert_eq!(world(&d), world(&bd), "{label}: the world differs");
        }
    }
}

#[test]
fn a_damaged_program_is_refused() {
    // The executable carries its program's text. Change one character of it, keeping
    // the length, and the executable refuses to run: its native code was made for the
    // program as it was.
    let work = tmp("damaged");
    let src = work.join("p.int");
    fs::write(&src, "fn go(n: Int) -> Int {\n    let marker = \"QQMARKERQQ\"\n    return n * 2\n}\n").unwrap();
    let app = work.join("app");
    assert_eq!(build(&src, &app).code, Some(0));
    let c = work.join("c.json");
    fs::write(&c, r#"{"entry": "go", "inputs": {"n": 21}, "model": {"provider": "script", "responses": []}}"#).unwrap();
    let ok = cmd(&app, &["run", "--config", c.to_str().unwrap()], None);
    assert_eq!((ok.code, ok.stdout.as_str()), (Some(0), "42"));
    let mut bytes = fs::read(&app).unwrap();
    let at = bytes.windows(10).position(|w| w == b"QQMARKERQQ").expect("the program text is in the executable");
    bytes[at + 2] = b'N';
    let bad = work.join("bad");
    fs::write(&bad, &bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&bad, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let r = cmd(&bad, &["run", "--config", c.to_str().unwrap()], None);
    assert_eq!(r.code, Some(2), "{}", r.stderr);
    assert!(r.stderr.contains("does not match the one it was built from"), "{}", r.stderr);
}

#[test]
fn build_errors_are_reported() {
    let work = tmp("errors");
    let bad = work.join("bad.int");
    fs::write(&bad, "fn f() -> Int {\n    return true\n}\n").unwrap();
    let r = build(&bad, &work.join("x"));
    assert_eq!(r.code, Some(1), "{}", r.stderr);
    assert!(r.stderr.contains("E101"), "{}", r.stderr);
    assert!(!work.join("x").exists());
    let good = work.join("good.int");
    fs::write(&good, "fn f() -> Int {\n    return 1\n}\n").unwrap();
    let r = cmd(Path::new(BIN), &["build", good.to_str().unwrap(), "-o", work.join("y").to_str().unwrap(), "--runtime", "/nonexistent/libintentc.a"], None);
    assert_eq!(r.code, Some(2));
    assert!(r.stderr.contains("runtime library"), "{}", r.stderr);
    // A program with no native functions still builds and runs.
    let text = work.join("text.int");
    fs::write(&text, "fn f(s: Text) -> Text {\n    let r = try g(s)\n    return s\n}\nfn g(s: Text) -> Text {\n    return s\n}\n").unwrap();
    assert_eq!(build(&text, &work.join("t")).code, Some(0));
    let c = work.join("c.json");
    fs::write(&c, r#"{"entry": "f", "inputs": {"s": "hi"}, "model": {"provider": "script", "responses": []}}"#).unwrap();
    let r = cmd(&work.join("t"), &["run", "--config", c.to_str().unwrap()], None);
    assert_eq!((r.code, r.stdout.as_str()), (Some(0), "\"hi\""), "{}", r.stderr);
    // Usage errors.
    let r = cmd(&work.join("t"), &["frobnicate"], None);
    assert_eq!(r.code, Some(2));
}
