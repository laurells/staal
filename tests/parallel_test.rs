// `parallel` at run time: which capabilities concurrent tasks may share, the step
// limit across tasks, and the same log on every run whatever order tasks ran in.

use serde_json::{json, Value as Json};
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_staalc");
const ROOT: &str = env!("CARGO_MANIFEST_DIR");
const PROGRAM: &str = "tests/parallel/send.staal";

fn scratch(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR")).join("parallel").join(name);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn run(dir: &Path, cfg: Json, engine: &str) -> (Option<i32>, String, String, Vec<Json>) {
    let c = dir.join("cfg.json");
    std::fs::write(&c, cfg.to_string()).unwrap();
    let log = dir.join("log.jsonl");
    let o = Command::new(BIN)
        .args(["run", PROGRAM, "--config", c.to_str().unwrap(), "--log", log.to_str().unwrap(), "--engine", engine])
        .current_dir(ROOT)
        .output()
        .unwrap();
    let events = std::fs::read_to_string(&log).unwrap_or_default().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    (o.status.code(), String::from_utf8_lossy(&o.stdout).trim().to_string(), String::from_utf8_lossy(&o.stderr).trim().to_string(), events)
}

fn send_config(grant: Json) -> Json {
    json!({ "entry": "send_all", "grants": { "mail": grant }, "model": { "provider": "script", "responses": [] }, "adapters": { "simulated": {} } })
}

#[test]
fn a_capability_counted_by_uses_or_max_is_never_shared_by_tasks() {
    let dir = scratch("counted");
    for grant in [json!({ "cap": "email.send", "uses": 5 }), json!({ "cap": "email.send", "max": { "amount": "5.00", "currency": "USD" } })] {
        let (code, _, err, events) = run(&dir, send_config(grant), "interpreter");
        assert_eq!(code, Some(1), "{err}");
        assert!(err.contains("cannot be shared by concurrent tasks"), "{err}");
        assert!(!events.iter().any(|e| e["event"] == "action_performed"), "an action ran");
    }
    // Limits checked action by action are fine.
    let (code, out, err, _) = run(&dir, send_config(json!({ "cap": "email.send", "only": ["+234*"] })), "interpreter");
    assert_eq!((code, out.as_str()), (Some(0), "3"), "{err}");
    let (code, _, err, events) = run(&dir, send_config(json!({ "cap": "email.send", "only": ["+2348000000002"] })), "interpreter");
    assert_eq!(code, Some(1), "{err}");
    assert_eq!(events.iter().filter(|e| e["event"] == "action_refused").count(), 2);
}

#[test]
fn the_step_limit_counts_every_task() {
    let dir = scratch("steps");
    let cfg = |max: u64| json!({ "entry": "heavy", "model": { "provider": "script", "responses": [] }, "limits": { "max_steps": max } });
    let (code, out, err, _) = run(&dir, cfg(100_000), "interpreter");
    assert_eq!((code, out.as_str()), (Some(0), "1000"), "{err}");
    // Each task alone fits in 500 steps; four of them do not.
    let (code, _, err, _) = run(&dir, cfg(500), "interpreter");
    assert_eq!(code, Some(1));
    assert!(err.contains("steps"), "{err}");
}

#[test]
fn the_log_is_the_same_on_every_run_and_engine() {
    let dir = scratch("log");
    let strip = |events: Vec<Json>| -> Vec<Json> {
        events
            .into_iter()
            .map(|mut e| {
                let o = e.as_object_mut().unwrap();
                o.remove("run_id");
                // Receipts follow the order tasks reached the service in.
                o.remove("receipt");
                if let Some(Json::Array(items)) = o.get_mut("items") {
                    items.iter_mut().for_each(|i| { i.as_object_mut().map(|m| m.remove("receipt")); });
                }
                e
            })
            .collect()
    };
    let cfg = send_config(json!({ "cap": "email.send" }));
    let (_, _, _, first) = run(&dir, cfg.clone(), "interpreter");
    let first = strip(first);
    let tasks: Vec<&str> = first.iter().filter_map(|e| e["task"].as_str()).collect();
    assert_eq!(tasks, ["1.0", "1.0", "1.1", "1.1", "1.2", "1.2"], "each task's events together, in the list's order");
    for i in 0..10 {
        let engine = if i % 2 == 1 && cfg!(feature = "native") { "native" } else { "interpreter" };
        let (code, _, err, events) = run(&dir, cfg.clone(), engine);
        assert_eq!(code, Some(0), "{err}");
        assert_eq!(strip(events), first, "run {i} on {engine}");
    }
}
