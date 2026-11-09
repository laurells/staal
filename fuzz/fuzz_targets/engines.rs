// The two engines on the same run: a runtime case from tests/run, its run config
// changed by the fuzzer, run by the interpreter and by native code, must give the same
// result (or the same config error) and the same log. This is the fuzz target that runs
// native code (src/native.rs, docs/security-review/UNSAFE.md), so run it with
// AddressSanitizer.
//
// Input: two bytes choosing the case, then a JSON object whose keys replace the case's
// config keys (anything else is ignored). Only the scripted model is allowed, and the
// tests/run programs use no network, files, databases or C, so nothing leaves the
// process.
#![no_main]
use libfuzzer_sys::fuzz_target;
use serde_json::{json, Value as Json};
use staalc::interp::{run_config_engine, Durability, Engine, RunResult};
use std::sync::OnceLock;

struct Case {
    prog: staalc::ast::Program,
    config: Json,
}

fn cases() -> &'static Vec<Case> {
    static C: OnceLock<Vec<Case>> = OnceLock::new();
    C.get_or_init(|| {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/run");
        let mut files: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
            .collect();
        files.sort();
        files
            .iter()
            .map(|f| {
                let stem = f.file_stem().unwrap().to_string_lossy().to_string();
                let base = stem.split("__").next().unwrap().to_string();
                let src = std::fs::read_to_string(dir.join(format!("{base}.staal"))).unwrap();
                let mut config: Json = serde_json::from_str(&std::fs::read_to_string(f).unwrap()).unwrap();
                config.as_object_mut().unwrap().remove("expect");
                Case { prog: staalc::check_program(&src).unwrap(), config }
            })
            .collect()
    })
}

/// The run's random id taken out, wherever it appears.
fn without_id(v: &Json, id: &str) -> Json {
    match v {
        Json::String(s) if !id.is_empty() => Json::String(s.replace(id, "RUN")),
        Json::Array(a) => Json::Array(a.iter().map(|x| without_id(x, id)).collect()),
        Json::Object(o) => Json::Object(o.iter().map(|(k, x)| (k.clone(), without_id(x, id))).collect()),
        other => other.clone(),
    }
}

fn outcome(r: &Result<RunResult, String>) -> Json {
    match r {
        Err(e) => json!({ "config_error": e }),
        Ok(out) => {
            let id = out.log.first().and_then(|e| e["run_id"].as_str()).unwrap_or("").to_string();
            let result = match &out.result {
                Ok(v) => json!({ "ok": v }),
                Err(f) => json!({ "failed": f.kind.to_string(), "message": f.message }),
            };
            json!({ "result": without_id(&result, &id), "log": without_id(&Json::Array(out.log.clone()), &id) })
        }
    }
}

fuzz_target!(|data: &[u8]| {
    let [a, b, rest @ ..] = data else { return };
    let all = cases();
    let case = &all[usize::from(u16::from_le_bytes([*a, *b])) % all.len()];
    let Ok(Json::Object(changes)) = serde_json::from_slice::<Json>(rest) else { return };
    let mut cfg = case.config.clone();
    for (k, v) in changes {
        cfg[k] = v;
    }
    // The scripted model only: the others call a service.
    if cfg.get("model").and_then(|m| m.get("provider")).and_then(Json::as_str).is_some_and(|p| p != "script") {
        return;
    }
    let i = run_config_engine(&case.prog, "", &cfg, Durability::None, Engine::Interpreter);
    let n = run_config_engine(&case.prog, "", &cfg, Durability::None, Engine::Native);
    let (i, n) = (outcome(&i), outcome(&n));
    assert!(i == n, "the engines differ:\ninterpreter {i}\nnative      {n}");
});
