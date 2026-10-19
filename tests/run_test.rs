// Runtime tests. Each case is tests/run/NAME.staal plus tests/run/NAME.json: a run
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

mod common;

use std::fs;
use std::path::Path;

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
        // NAME.json runs NAME.staal, or BASE.staal for NAME = BASE__variant.
        let stem = case.file_stem().unwrap().to_string_lossy().to_string();
        let base = stem.split("__").next().unwrap().to_string();
        let prog = dir.join(format!("{base}.staal"));
        let checked = fs::read_to_string(&prog).map_err(|e| e.to_string()).and_then(|src| {
            staalc::check_program(&src).map_err(|d| format!("program has check errors: {d:?}"))
        });
        if let Err(e) = checked.and_then(|p| common::check_case(&p, case, "")) {
            failures.push(format!("{}: {e}", case.file_name().unwrap().to_string_lossy()));
        }
    }
    assert!(failures.is_empty(), "{} of {} runtime cases failed:\n{}", failures.len(), cases.len(), failures.join("\n"));
    println!("{} runtime cases passed", cases.len());
}
