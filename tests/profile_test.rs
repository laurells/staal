// `staalc profile`: what each function spent, exactly where it is deterministic
// (calls, steps, model calls, tokens, actions), consistent where it is not (time),
// on both engines, and the run itself unchanged.

use serde_json::Value as Json;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_staalc");
const ROOT: &str = env!("CARGO_MANIFEST_DIR");

fn staalc(args: &[&str]) -> Output {
    Command::new(BIN).args(args).current_dir(ROOT).output().unwrap()
}

fn profile(engine: &str, name: &str) -> (Output, Json) {
    let out = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = fs::remove_file(&out);
    let o = staalc(&["profile", "tests/profile/work.staal", "--config", "tests/profile/work.json", "--engine", engine, "-o", out.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(0), "{}", String::from_utf8_lossy(&o.stderr));
    (o, serde_json::from_str(&fs::read_to_string(&out).unwrap()).unwrap())
}

fn n(p: &Json, f: &str, k: &str) -> u64 {
    p[f][k].as_u64().unwrap_or_else(|| panic!("{f}.{k} in {p}"))
}

#[test]
fn each_function_is_charged_exactly_what_it_spent() {
    let (o, p) = profile("interpreter", "profile-interp.json");
    // The run is as `staalc run` gives it.
    let run = staalc(&["run", "tests/profile/work.staal", "--config", "tests/profile/work.json", "--log", "target/profile-run.jsonl"]);
    assert_eq!(o.stdout, run.stdout);
    assert_eq!(String::from_utf8_lossy(&o.stdout), "74\n");
    // fib(10) is 177 calls of two steps each; `work` runs six statements itself.
    for (f, calls, steps, own) in [("work", 1, 366, 6), ("fib", 177, 354, 354), ("square", 3, 3, 3), ("send", 2, 2, 2), ("ask", 1, 1, 1)] {
        assert_eq!((n(&p, f, "calls"), n(&p, f, "steps"), n(&p, f, "own_steps")), (calls, steps, own), "{f}");
    }
    assert_eq!(p.as_object().unwrap().len(), 5);
    // The model call and the actions are charged to the functions that made them.
    assert_eq!((n(&p, "ask", "model_calls"), n(&p, "send", "actions")), (1, 2));
    assert_eq!(n(&p, "work", "model_calls") + n(&p, "work", "actions") + n(&p, "fib", "actions"), 0);
    // Its tokens are what the model's reply in the run's log says.
    let log = fs::read_to_string(Path::new(ROOT).join("target/profile-run.jsonl")).unwrap();
    let reply: Json = log.lines().map(|l| serde_json::from_str::<Json>(l).unwrap()).find(|e| e["event"] == "model_reply").unwrap();
    assert_eq!(n(&p, "ask", "tokens"), reply["input_tokens"].as_u64().unwrap() + reply["output_tokens"].as_u64().unwrap());
    // Own parts add up to the whole; a total is never less than its own part.
    let own: u64 = p.as_object().unwrap().keys().map(|f| n(&p, f, "own_steps")).sum();
    assert_eq!(own, n(&p, "work", "steps"));
    for f in p.as_object().unwrap().keys() {
        assert!(n(&p, f, "own_nanos") <= n(&p, f, "nanos"), "{f}");
        assert!(n(&p, f, "nanos") <= n(&p, "work", "nanos"), "{f}");
    }
    // The table, on standard error.
    let table = String::from_utf8_lossy(&o.stderr);
    let lines: Vec<&str> = table.lines().collect();
    assert!(lines[0].starts_with("function") && lines[0].contains("own steps") && lines[0].ends_with("actions"), "{table}");
    assert!(lines[1].starts_with("work ") && lines[2].starts_with("fib "), "{table}");
    assert_eq!(lines.len(), 6);
    // The same program and input: the same counts every time.
    let (_, again) = profile("interpreter", "profile-again.json");
    for f in p.as_object().unwrap().keys() {
        for k in ["calls", "steps", "own_steps", "model_calls", "tokens", "actions"] {
            assert_eq!(n(&p, f, k), n(&again, f, k), "{f}.{k}");
        }
    }
}

#[test]
fn the_native_engine_spends_the_same_steps() {
    if !cfg!(feature = "native") {
        return;
    }
    let (o, native) = profile("native", "profile-native.json");
    let (_, interp) = profile("interpreter", "profile-interp2.json");
    assert_eq!(String::from_utf8_lossy(&o.stdout), "74\n");
    for f in ["work", "fib", "square", "send", "ask"] {
        for k in ["steps", "own_steps", "model_calls", "tokens", "actions"] {
            assert_eq!(n(&native, f, k), n(&interp, f, k), "{f}.{k}");
        }
    }
    // Native code calls native code directly: fib's own recursion is one call.
    assert_eq!(n(&native, "fib", "calls"), 1);
    assert_eq!(n(&native, "square", "calls"), 3);
}

#[test]
fn a_failing_run_is_still_profiled_and_problems_are_refused() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("profile-fail");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("f.staal"), "fn inner(n: Int) -> Int {\n    return 10 / n\n}\n\nfn main() -> Int {\n    return inner(0)\n}\n").unwrap();
    fs::write(dir.join("f.json"), r#"{"entry": "main", "model": {"provider": "script", "responses": []}}"#).unwrap();
    let f = dir.join("f.staal");
    let c = dir.join("f.json");
    let o = staalc(&["profile", f.to_str().unwrap(), "--config", c.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(1));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("run failed: value: division by zero"), "{err}");
    assert!(err.lines().any(|l| l.starts_with("inner ")), "{err}");
    // Check errors: nothing runs, no table.
    fs::write(dir.join("bad.staal"), "fn main() -> Int {\n    return y\n}\n").unwrap();
    let o = staalc(&["profile", dir.join("bad.staal").to_str().unwrap(), "--config", c.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(1));
    assert!(!String::from_utf8_lossy(&o.stderr).contains("own steps"));
    // Without a config, `main` runs with no inputs (here it fails as above); a
    // function that is not there, or an output that cannot be written, is exit 2.
    assert_eq!(staalc(&["profile", f.to_str().unwrap()]).status.code(), Some(1));
    assert_eq!(staalc(&["profile", f.to_str().unwrap(), "--entry", "nope"]).status.code(), Some(2));
    let o = staalc(&["profile", f.to_str().unwrap(), "--config", c.to_str().unwrap(), "-o", "/nonexistent/dir/p.json"]);
    assert_eq!(o.status.code(), Some(2));
    assert_eq!(staalc(&["profile", f.to_str().unwrap(), "--config", c.to_str().unwrap(), "--engine", "fast"]).status.code(), Some(2));
}
