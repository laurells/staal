// Runtime tests. Each case is tests/run/NAME.int plus tests/run/NAME.json: a run
// config with an extra "expect" object. Expectations:
//   "result": <json>             the entry function's return value, exactly
//   "failure": "<kind>"          the run must fail with this kind instead
//   "message_contains": "..."    the failure message contains this text
//   "performed": [[cap, method]] exactly these actions were performed, in order
//   "model_calls": N             exactly N model requests were sent
//   "model_never_sees": ["..."]  no model request contains any of these strings
//   "evidence": N                the run's evidence lists exactly N receipts
//   "events": ["...", ...]       these log events appear, in this order (others may interleave)
// A case may instead expect a config error: "config_error_contains": "...".

use serde_json::Value as Json;
use std::fs;
use std::path::Path;

fn check_case(prog_path: &Path, case_path: &Path) -> Result<(), String> {
    let src = fs::read_to_string(prog_path).map_err(|e| e.to_string())?;
    let prog = intentc::check_program(&src).map_err(|d| format!("program has check errors: {d:?}"))?;
    let mut cfg: Json = serde_json::from_str(&fs::read_to_string(case_path).map_err(|e| e.to_string())?)
        .map_err(|e| format!("case JSON: {e}"))?;
    let expect = cfg.as_object_mut().and_then(|o| o.remove("expect")).ok_or("case has no \"expect\"")?;
    let known = [
        "result", "failure", "message_contains", "performed", "model_calls",
        "model_never_sees", "evidence", "events", "config_error_contains",
    ];
    for k in expect.as_object().ok_or("expect: not an object")?.keys() {
        if !known.contains(&k.as_str()) {
            return Err(format!("expect: unknown key '{k}' (typo?)"));
        }
    }

    let out = match intentc::interp::run_config(&prog, &cfg) {
        Ok(out) => out,
        Err(e) => {
            return match expect.get("config_error_contains").and_then(Json::as_str) {
                Some(want) if e.contains(want) => Ok(()),
                Some(want) => Err(format!("config error {e:?} does not contain {want:?}")),
                None => Err(format!("unexpected config error: {e}")),
            };
        }
    };
    if let Some(want) = expect.get("config_error_contains") {
        return Err(format!("expected a config error containing {want}, but the program ran"));
    }

    match (&out.result, expect.get("result"), expect.get("failure").and_then(Json::as_str)) {
        (Ok(v), Some(want), None) if v == want => {}
        (Ok(v), Some(want), None) => return Err(format!("result {v} != expected {want}")),
        (Err(f), None, Some(kind)) if f.kind.to_string() == kind => {
            if let Some(m) = expect.get("message_contains").and_then(Json::as_str)
                && !f.message.contains(m) {
                    return Err(format!("failure message {:?} does not contain {m:?}", f.message));
                }
        }
        (Err(f), None, Some(kind)) => return Err(format!("failed with {}: {} (expected {kind})", f.kind, f.message)),
        (Ok(v), None, Some(kind)) => return Err(format!("expected failure {kind}, got result {v}")),
        (Err(f), Some(_), None) => return Err(format!("unexpected failure {}: {}", f.kind, f.message)),
        _ => return Err("expect exactly one of \"result\" or \"failure\"".into()),
    }

    let events = |name: &'static str| events_named(&out.log, name);
    if let Some(want) = expect.get("performed") {
        let got: Vec<Json> =
            events("action_performed").map(|e| serde_json::json!([e["capability"], e["method"]])).collect();
        if &Json::Array(got.clone()) != want {
            return Err(format!("performed {got:?}, expected {want}"));
        }
    }
    if let Some(n) = expect.get("model_calls").and_then(Json::as_u64) {
        let got = events("model_request").count() as u64;
        if got != n {
            return Err(format!("{got} model requests sent, expected {n}"));
        }
    }
    if let Some(list) = expect.get("model_never_sees").and_then(Json::as_array) {
        for req in events("model_request") {
            let text = req["request"].to_string();
            for s in list.iter().filter_map(Json::as_str) {
                if text.contains(s) {
                    return Err(format!("a model request contained {s:?}: {text}"));
                }
            }
        }
    }
    if let Some(n) = expect.get("evidence").and_then(Json::as_u64) {
        let got = events("evidence").next().and_then(|e| e["items"].as_array()).map_or(0, |a| a.len()) as u64;
        if got != n {
            return Err(format!("{got} evidence items, expected {n}"));
        }
    }
    if let Some(list) = expect.get("events").and_then(Json::as_array) {
        let mut it = out.log.iter();
        for want in list.iter().filter_map(Json::as_str) {
            if !it.any(|e| e["event"] == want) {
                let all: Vec<&Json> = out.log.iter().map(|e| &e["event"]).collect();
                return Err(format!("event {want:?} not found in order; log events: {all:?}"));
            }
        }
    }
    Ok(())
}

fn events_named<'a>(log: &'a [Json], name: &'static str) -> impl Iterator<Item = &'a Json> + 'a {
    log.iter().filter(move |e| e["event"] == name)
}

#[test]
fn runtime_cases() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/run");
    let mut cases: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    cases.sort();
    assert!(!cases.is_empty(), "no runtime cases");
    let mut failures = Vec::new();
    for case in &cases {
        // NAME.json runs NAME.int, or BASE.int for NAME = BASE__variant.
        let stem = case.file_stem().unwrap().to_string_lossy().to_string();
        let base = stem.split("__").next().unwrap().to_string();
        let prog = dir.join(format!("{base}.int"));
        if let Err(e) = check_case(&prog, case) {
            failures.push(format!("{}: {e}", case.file_name().unwrap().to_string_lossy()));
        }
    }
    assert!(failures.is_empty(), "{} of {} runtime cases failed:\n{}", failures.len(), cases.len(), failures.join("\n"));
    println!("{} runtime cases passed", cases.len());
}
