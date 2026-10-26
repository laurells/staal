// Any JSON as the run config of the crash-test workflow: inputs, grants, limits and
// settings must be refused with a message or run, never panic. Only the scripted
// model is allowed, so nothing reaches the network.
#![no_main]
use libfuzzer_sys::fuzz_target;
use serde_json::Value as Json;
use std::sync::OnceLock;

fn prog() -> &'static staalc::ast::Program {
    static P: OnceLock<staalc::ast::Program> = OnceLock::new();
    P.get_or_init(|| staalc::check_program(include_str!("../../tests/crash/workflow.staal")).expect("the workflow checks"))
}

/// The workflow's own config, which every input changes.
fn base() -> &'static Json {
    static B: OnceLock<Json> = OnceLock::new();
    B.get_or_init(|| serde_json::from_str(include_str!("../../tests/crash/workflow.json")).unwrap())
}

fuzz_target!(|data: &[u8]| {
    let Ok(input) = serde_json::from_slice::<Json>(data) else { return };
    // An object replaces the base config's keys it names, so most inputs get past the
    // config's outer shape to its inputs, grants and settings; anything else is the
    // whole config.
    let cfg = match input {
        Json::Object(changes) => {
            let mut c = base().clone();
            for (k, v) in changes {
                c[k] = v;
            }
            c
        }
        other => other,
    };
    // The scripted model only: the others call a service.
    if cfg.get("model").and_then(|m| m.get("provider")).and_then(Json::as_str).is_some_and(|p| p != "script") {
        return;
    }
    let _ = staalc::interp::run_config(prog(), &cfg);
});
