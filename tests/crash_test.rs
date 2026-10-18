// Crash-injection tests for durable execution (RUNTIME.md section 11).
//
// For every point at which a run can be killed, kill it there, resume it, and check
// that the run ends exactly as an uninterrupted run does: the same result, the same
// actions in the world, and no action performed twice.
//
// Crash points:
//   after_write:K    the process dies right after the K-th journal record is durable
//                    (every K from the start record to the end record);
//   before_result:N  operation N has happened in the world, but the process dies
//                    before its result is recorded (the in-doubt window).
//
// Run with: cargo test --features failpoints --test crash_test

#![cfg(feature = "failpoints")]

use serde_json::Value as Json;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_intentc");
const ROOT: &str = env!("CARGO_MANIFEST_DIR");

struct Out {
    /// None when the process was killed by a signal (the injected crash).
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn intentc(args: &[&str], failpoint: Option<&str>) -> Out {
    let mut cmd = Command::new(BIN);
    cmd.args(args);
    match failpoint {
        Some(fp) => cmd.env("INTENTC_FAILPOINT", fp),
        None => cmd.env_remove("INTENTC_FAILPOINT"),
    };
    let o = cmd.output().expect("run intentc");
    Out {
        code: o.status.code(),
        stdout: String::from_utf8_lossy(&o.stdout).trim().to_string(),
        stderr: String::from_utf8_lossy(&o.stderr).trim().to_string(),
    }
}

/// A fresh directory with the scenario's config pointing at its own world file.
struct Case {
    dir: PathBuf,
}

impl Case {
    fn new(name: &str, idempotent: bool) -> Case {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("crash").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg: Json =
            serde_json::from_str(&std::fs::read_to_string(format!("{ROOT}/tests/crash/workflow.json")).unwrap()).unwrap();
        let sim = &mut cfg["adapters"]["simulated"];
        sim["world"] = Json::String(dir.join("world.jsonl").to_string_lossy().into());
        sim["idempotent"] = Json::Bool(idempotent);
        std::fs::write(dir.join("cfg.json"), cfg.to_string()).unwrap();
        Case { dir }
    }

    fn path(&self, f: &str) -> String {
        self.dir.join(f).to_string_lossy().into()
    }

    fn args(&self, cmd: &'static str) -> Vec<String> {
        vec![
            cmd.into(),
            format!("{ROOT}/tests/crash/workflow.int"),
            "--config".into(),
            self.path("cfg.json"),
            "--journal".into(),
            self.path("journal"),
        ]
    }

    fn run(&self, failpoint: Option<&str>) -> Out {
        let a = self.args("run");
        intentc(&a.iter().map(String::as_str).collect::<Vec<_>>(), failpoint)
    }

    fn resume(&self) -> Out {
        let a = self.args("resume");
        intentc(&a.iter().map(String::as_str).collect::<Vec<_>>(), None)
    }

    fn resolve(&self, how: &[&str]) -> Out {
        let j = self.path("journal");
        let mut a = vec!["resolve", "--journal", j.as_str()];
        a.extend_from_slice(how);
        intentc(&a, None)
    }

    /// The world's actions, without their idempotency keys (which differ between runs).
    fn world(&self) -> Vec<Json> {
        self.world_entries()
            .into_iter()
            .map(|mut e| {
                e.as_object_mut().unwrap().remove("key");
                e
            })
            .collect()
    }

    fn world_entries(&self) -> Vec<Json> {
        match std::fs::read_to_string(self.path("world.jsonl")) {
            Ok(t) => t.lines().map(|l| serde_json::from_str(l).unwrap()).collect(),
            Err(_) => Vec::new(),
        }
    }

    fn journal_lines(&self) -> Vec<Json> {
        std::fs::read_to_string(self.path("journal"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    /// The action an in-doubt journal is waiting on: its key.
    fn doubtful_key(&self) -> String {
        let lines = self.journal_lines();
        let intent = lines.iter().rev().find(|r| r["j"] == "intent").expect("an intent in doubt");
        assert_eq!(intent["op"], "action", "only actions are ever left for an operator");
        intent["key"].as_str().unwrap().to_string()
    }

    /// No idempotency key appears twice in the world: nothing was performed twice.
    fn assert_no_duplicates(&self, label: &str) {
        let mut seen = HashSet::new();
        for e in self.world_entries() {
            let key = e["key"].as_str().unwrap().to_string();
            assert!(seen.insert(key.clone()), "{label}: action with key {key} was performed twice");
        }
    }
}

struct Baseline {
    result: String,
    world: Vec<Json>,
    writes: usize,
    ops: usize,
}

/// Tests run in parallel, so each gets its own baseline directory (`tag`).
fn baseline(tag: &str) -> Baseline {
    let c = Case::new(&format!("baseline-{tag}"), true);
    let out = c.run(None);
    assert_eq!(out.code, Some(0), "baseline failed: {}", out.stderr);
    let lines = c.journal_lines();
    let ops = lines.iter().filter(|r| r["j"] == "intent").count();
    assert_eq!(lines.len(), 2 * ops + 2, "start + intent/result per op + end");
    assert_eq!(c.world().len(), 3, "the scenario performs three actions");
    Baseline { result: out.stdout, world: c.world(), writes: lines.len(), ops }
}

/// Resumes until the run finishes, resolving in-doubt actions as a correct operator
/// would: by looking at what really happened in the world.
fn finish(c: &Case, label: &str) -> Out {
    for _ in 0..10 {
        let out = c.resume();
        match out.code {
            Some(0) => return out,
            Some(3) => {
                assert!(out.stderr.contains("in_doubt"), "{label}: exit 3 without in_doubt: {}", out.stderr);
                let key = c.doubtful_key();
                let done = c.world_entries().into_iter().find(|e| e["key"] == key);
                let r = match done {
                    Some(e) => c.resolve(&["--performed", e["receipt"].as_str().unwrap()]),
                    None => c.resolve(&["--retry"]),
                };
                assert_eq!(r.code, Some(0), "{label}: resolve failed: {}", r.stderr);
            }
            other => panic!("{label}: resume exited {other:?}: {}", out.stderr),
        }
    }
    panic!("{label}: the run did not finish after 10 resumes");
}

fn check_recovered(c: &Case, b: &Baseline, label: &str) {
    let out = finish(c, label);
    assert_eq!(out.stdout, b.result, "{label}: result differs from the uninterrupted run");
    assert_eq!(c.world(), b.world, "{label}: the world differs from the uninterrupted run");
    c.assert_no_duplicates(label);
    // Resuming a finished run changes nothing.
    let again = c.resume();
    assert_eq!(again.code, Some(0), "{label}: resume of a finished run: {}", again.stderr);
    assert_eq!(again.stdout, b.result, "{label}: replay of a finished run gave a different result");
    assert_eq!(c.world(), b.world, "{label}: replay of a finished run changed the world");
}

fn crash_everywhere(idempotent: bool) {
    let mode = if idempotent { "idempotent" } else { "plain" };
    let b = baseline(mode);
    let mut points = Vec::new();
    for k in 1..=b.writes {
        points.push(format!("after_write:{k}"));
    }
    for n in 1..=b.ops {
        points.push(format!("before_result:{n}"));
    }
    for fp in &points {
        let label = format!("{mode} {fp}");
        let c = Case::new(&format!("{mode}-{}", fp.replace(':', "-")), idempotent);
        let out = c.run(Some(fp));
        assert_eq!(out.code, None, "{label}: the injected crash did not happen (exit {:?}): {}", out.code, out.stderr);
        check_recovered(&c, &b, &label);
    }
}

#[test]
fn every_crash_point_recovers_with_an_idempotent_service() {
    crash_everywhere(true);
}

#[test]
fn every_crash_point_recovers_with_a_service_that_does_not_deduplicate() {
    crash_everywhere(false);
}

#[test]
fn an_in_doubt_action_is_never_guessed() {
    // Crash after the refund (operation 5) happened but before its result was recorded.
    let c = Case::new("never-guessed", false);
    assert_eq!(c.run(Some("before_result:5")).code, None);
    assert_eq!(c.world().len(), 1, "the refund happened");
    let out = c.resume();
    assert_eq!(out.code, Some(3), "resume must stop: {}", out.stderr);
    assert!(out.stderr.contains("payments.refund.refund"), "{}", out.stderr);
    assert_eq!(c.world().len(), 1, "resume must not perform the refund again on its own");
    let again = c.resume();
    assert_eq!(again.code, Some(3), "it stays stopped until an operator answers");
    assert_eq!(c.world().len(), 1);
}

#[test]
fn a_crash_during_an_authorized_retry_puts_the_action_back_in_doubt() {
    // Crash right after the refund's intent is durable: it never reached the service.
    // after_write:10 is the intent of operation 5 (start, then 4 ops x 2 records, then intent 5).
    let c = Case::new("retry-interrupted", false);
    assert_eq!(c.run(Some("after_write:10")).code, None);
    assert_eq!(c.world().len(), 0, "the refund did not happen");
    assert_eq!(c.resume().code, Some(3));
    assert_eq!(c.resolve(&["--retry"]).code, Some(0));
    // The authorized attempt: its record is written (write 1 of this process), the refund
    // happens, and the process dies before recording the result.
    let a = c.args("resume");
    let out = intentc(&a.iter().map(String::as_str).collect::<Vec<_>>(), Some("before_result:5"));
    assert_eq!(out.code, None, "{}", out.stderr);
    assert_eq!(c.world().len(), 1, "the retried refund happened once");
    // The attempt may have happened, so it is in doubt again: never retried silently.
    let out = c.resume();
    assert_eq!(out.code, Some(3), "{}", out.stderr);
    assert_eq!(c.world().len(), 1);
    check_recovered(&c, &baseline("retry-interrupted"), "retry-interrupted");
}
