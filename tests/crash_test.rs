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

const BIN: &str = env!("CARGO_BIN_EXE_staalc");
const ROOT: &str = env!("CARGO_MANIFEST_DIR");

struct Out {
    /// None when the process was killed by a signal (the injected crash).
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn staalc(args: &[&str], failpoint: Option<&str>) -> Out {
    let mut cmd = Command::new(BIN);
    cmd.args(args);
    match failpoint {
        Some(fp) => cmd.env("STAALC_FAILPOINT", fp),
        None => cmd.env_remove("STAALC_FAILPOINT"),
    };
    let o = cmd.output().expect("run staalc");
    Out {
        code: o.status.code(),
        stdout: String::from_utf8_lossy(&o.stdout).trim().to_string(),
        stderr: String::from_utf8_lossy(&o.stderr).trim().to_string(),
    }
}

/// A program, its run config (relative to the crate root; an "expect" key is ignored),
/// and how many actions an uninterrupted run performs.
struct Scenario {
    program: &'static str,
    config: &'static str,
    actions: usize,
}

/// Refund workflow: two model calls, three actions (one refused path).
const WORKFLOW: Scenario = Scenario { program: "tests/crash/workflow.staal", config: "tests/crash/workflow.json", actions: 3 };

/// Building access: six model calls and ten actions under `uses` and `only` limits,
/// two of them refused. A resumed run must rebuild the broker's counts exactly, or it
/// could open a fourth door.
const ACCESS: Scenario = Scenario {
    program: "tests/run/domain_access.staal",
    config: "tests/run/domain_access__mixed_batch.json",
    actions: 10,
};

/// Pure Int functions (native code on the native engine) between three actions.
const NATIVE: Scenario = Scenario { program: "tests/crash/native.staal", config: "tests/crash/native.json", actions: 2 };

/// Random numbers, the clock and printing around two actions that carry their values.
const EFFECTS: Scenario = Scenario { program: "tests/crash/effects.staal", config: "tests/crash/effects.json", actions: 2 };

/// A fresh directory with the scenario's config pointing at its own world file.
struct Case {
    dir: PathBuf,
    program: String,
    /// The engine of the first run, and of every resume.
    engines: (&'static str, &'static str),
}

impl Case {
    fn new(name: &str, idempotent: bool) -> Case {
        Case::of(&WORKFLOW, name, idempotent)
    }

    fn of(sc: &Scenario, name: &str, idempotent: bool) -> Case {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("crash").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg: Json =
            serde_json::from_str(&std::fs::read_to_string(format!("{ROOT}/{}", sc.config)).unwrap()).unwrap();
        cfg.as_object_mut().unwrap().remove("expect");
        let sim = &mut cfg["adapters"]["simulated"];
        sim["world"] = Json::String(dir.join("world.jsonl").to_string_lossy().into());
        sim["idempotent"] = Json::Bool(idempotent);
        std::fs::write(dir.join("cfg.json"), cfg.to_string()).unwrap();
        Case { dir, program: format!("{ROOT}/{}", sc.program), engines: ("interpreter", "interpreter") }
    }

    fn path(&self, f: &str) -> String {
        self.dir.join(f).to_string_lossy().into()
    }

    fn args(&self, cmd: &'static str) -> Vec<String> {
        let engine = if cmd == "run" { self.engines.0 } else { self.engines.1 };
        vec![
            cmd.into(),
            self.program.clone(),
            "--config".into(),
            self.path("cfg.json"),
            "--journal".into(),
            self.path("journal"),
            "--engine".into(),
            engine.into(),
        ]
    }

    fn run(&self, failpoint: Option<&str>) -> Out {
        let a = self.args("run");
        staalc(&a.iter().map(String::as_str).collect::<Vec<_>>(), failpoint)
    }

    fn resume(&self) -> Out {
        let a = self.args("resume");
        staalc(&a.iter().map(String::as_str).collect::<Vec<_>>(), None)
    }

    fn resolve(&self, how: &[&str]) -> Out {
        let j = self.path("journal");
        let mut a = vec!["resolve", "--journal", j.as_str()];
        a.extend_from_slice(how);
        staalc(&a, None)
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
    baseline_of(&WORKFLOW, tag)
}

fn baseline_of(sc: &Scenario, tag: &str) -> Baseline {
    let c = Case::of(sc, &format!("baseline-{tag}"), true);
    let out = c.run(None);
    assert_eq!(out.code, Some(0), "baseline failed: {}", out.stderr);
    let lines = c.journal_lines();
    let ops = lines.iter().filter(|r| r["j"] == "intent").count();
    assert_eq!(lines.len(), 2 * ops + 2, "start + intent/result per op + end");
    assert_eq!(c.world().len(), sc.actions, "actions performed by an uninterrupted run");
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

fn crash_everywhere(sc: &Scenario, tag: &str, idempotent: bool) {
    crash_everywhere_on(sc, tag, idempotent, ("interpreter", "interpreter"));
}

/// Crashes on one engine at every point and resumes on another. The baseline is
/// always the interpreter: the reference.
fn crash_everywhere_on(sc: &Scenario, tag: &str, idempotent: bool, engines: (&'static str, &'static str)) {
    let mode = format!("{tag}-{}-{}-{}", if idempotent { "idempotent" } else { "plain" }, engines.0, engines.1);
    let b = baseline_of(sc, &mode);
    let mut points = Vec::new();
    for k in 1..=b.writes {
        points.push(format!("after_write:{k}"));
    }
    for n in 1..=b.ops {
        points.push(format!("before_result:{n}"));
    }
    for fp in &points {
        let label = format!("{mode} {fp}");
        let mut c = Case::of(sc, &format!("{mode}-{}", fp.replace(':', "-")), idempotent);
        c.engines = engines;
        let out = c.run(Some(fp));
        assert_eq!(out.code, None, "{label}: the injected crash did not happen (exit {:?}): {}", out.code, out.stderr);
        check_recovered(&c, &b, &label);
    }
}

#[test]
fn every_crash_point_recovers_with_an_idempotent_service() {
    crash_everywhere(&WORKFLOW, "workflow", true);
}

#[test]
fn every_crash_point_recovers_with_a_service_that_does_not_deduplicate() {
    crash_everywhere(&WORKFLOW, "workflow", false);
}

#[test]
fn limited_capabilities_recover_exactly_after_any_crash() {
    crash_everywhere(&ACCESS, "access", true);
    crash_everywhere(&ACCESS, "access", false);
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
    let out = staalc(&a.iter().map(String::as_str).collect::<Vec<_>>(), Some("before_result:5"));
    assert_eq!(out.code, None, "{}", out.stderr);
    assert_eq!(c.world().len(), 1, "the retried refund happened once");
    // The attempt may have happened, so it is in doubt again: never retried silently.
    let out = c.resume();
    assert_eq!(out.code, Some(3), "{}", out.stderr);
    assert_eq!(c.world().len(), 1);
    check_recovered(&c, &baseline("retry-interrupted"), "retry-interrupted");
}

/// The phase 8 gate: what differs between runs (random numbers, the clock) is
/// journaled, so a run resumed after a crash at any point uses exactly the values it
/// had already given the world, and prints each line once (a line printed just before
/// the crash, before it was recorded, may be printed once more).
#[test]
fn what_differs_between_runs_is_replayed_after_any_crash() {
    for idempotent in [true, false] {
        let mode = format!("effects-{}", if idempotent { "idempotent" } else { "plain" });
        let b = baseline_of(&EFFECTS, &mode);
        let print_ops: Vec<u64> = {
            let c = Case::of(&EFFECTS, &format!("{mode}-shape"), idempotent);
            assert_eq!(c.run(None).code, Some(0));
            c.journal_lines().iter().filter(|r| r["j"] == "intent" && r["op"] == "print").map(|r| r["n"].as_u64().unwrap()).collect()
        };
        assert_eq!(print_ops.len(), 3, "the scenario prints three lines");
        let mut points: Vec<String> = (1..=b.writes).map(|k| format!("after_write:{k}")).collect();
        points.extend((1..=b.ops).map(|n| format!("before_result:{n}")));
        for fp in &points {
            let label = format!("{mode} {fp}");
            let c = Case::of(&EFFECTS, &format!("{mode}-{}", fp.replace(':', "-")), idempotent);
            let crashed = c.run(Some(fp));
            assert_eq!(crashed.code, None, "{label}: the injected crash did not happen: {}", crashed.stderr);
            let mut printed: Vec<String> = crashed.stdout.lines().map(String::from).collect();
            // Resume, resolving actions in doubt as an operator would, keeping every line printed.
            let mut last = None;
            for _ in 0..10 {
                let out = c.resume();
                match out.code {
                    Some(0) => {
                        last = Some(out);
                        break;
                    }
                    Some(3) => {
                        printed.extend(out.stdout.lines().map(String::from));
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
            let out = last.unwrap_or_else(|| panic!("{label}: the run did not finish"));
            let mut lines: Vec<String> = out.stdout.lines().map(String::from).collect();
            let result: String = serde_json::from_str(&lines.pop().expect("a result")).unwrap();
            printed.extend(lines);
            let parts: Vec<&str> = result.split(' ').collect();
            let (code, at, roll) = (parts[0], parts[1], parts[2]);
            // The world was given the values the run returns: none was drawn again.
            let world: Vec<Json> = c.world().iter().map(|e| e["args"][0].clone()).collect();
            assert_eq!(
                world,
                [Json::String(format!("user@example.com: your code is {code}")), Json::String(format!("audit@example.com: {code} at {at}"))],
                "{label}: the world saw other values than the run used"
            );
            c.assert_no_duplicates(&label);
            // Each line once, in order; a line may repeat only when the crash came after
            // it was printed and before that was recorded.
            let want = [format!("code {code}"), format!("sent at {at}"), format!("rolled {roll}")];
            let mut collapsed = printed.clone();
            collapsed.dedup();
            assert_eq!(collapsed, want, "{label}: printed {printed:?}");
            let crash_after_print = print_ops.iter().any(|n| *fp == format!("before_result:{n}"));
            assert!(
                printed.len() == 3 || (crash_after_print && printed.len() == 4),
                "{label}: a line was printed more than once: {printed:?}"
            );
            // Resuming the finished run changes nothing and prints nothing.
            let again = c.resume();
            assert_eq!(again.code, Some(0), "{label}: {}", again.stderr);
            assert_eq!(again.stdout, out.stdout.lines().last().unwrap(), "{label}: replay of a finished run printed or differed");
        }
    }
}

/// A journal whose recorded random number is outside its bounds, or whose clock
/// reading is not a time, is refused on resume, as any malformed journal is.
#[test]
fn a_tampered_random_number_or_time_is_refused() {
    for (op, bad) in [("random", Json::from(0)), ("random", Json::from("7")), ("clock", Json::from(253402300800000i64)), ("clock", Json::Null)] {
        let c = Case::of(&EFFECTS, &format!("tampered-{op}-{}", bad.to_string().replace('"', "")), true);
        // Crash after the first operation of this kind has its result recorded.
        let lines = {
            let shape = Case::of(&EFFECTS, &format!("tampered-shape-{op}"), true);
            assert_eq!(shape.run(None).code, Some(0));
            shape.journal_lines()
        };
        let n = lines.iter().find(|r| r["j"] == "intent" && r["op"] == op).unwrap()["n"].as_u64().unwrap();
        let k = lines.iter().position(|r| r["j"] == "result" && r["n"] == n).unwrap() + 1;
        assert_eq!(c.run(Some(&format!("after_write:{k}"))).code, None);
        let mut recs = c.journal_lines();
        let rec = recs.iter_mut().find(|r| r["j"] == "result" && r["n"] == n).unwrap();
        rec["ok"] = bad.clone();
        let text: String = recs.iter().map(|r| format!("{r}\n")).collect();
        std::fs::write(c.path("journal"), text).unwrap();
        let out = c.resume();
        assert_eq!(out.code, Some(3), "{op} {bad}: resumed with a tampered value: {}", out.stdout);
        assert!(out.stderr.contains("journal") && out.stderr.contains("malformed"), "{op} {bad}: {}", out.stderr);
    }
}

#[test]
fn native_code_recovers_from_a_crash_on_either_engine() {
    // Every crash point, every pairing: a run that crashed on native code resumes on
    // the interpreter, and the other way round, with the same result and world.
    for engines in [("native", "native"), ("native", "interpreter"), ("interpreter", "native")] {
        crash_everywhere_on(&NATIVE, "native", true, engines);
        crash_everywhere_on(&NATIVE, "native", false, engines);
        crash_everywhere_on(&WORKFLOW, "workflow", false, engines);
    }
}

/// Each line typed, sent on in a message: three lines, three actions.
const INPUT: Scenario = Scenario { program: "tests/crash/input.staal", config: "tests/crash/input.json", actions: 3 };

/// Runs staalc with `stdin` as its standard input.
fn staalc_with_input(args: &[String], failpoint: Option<&str>, stdin: &str) -> Out {
    use std::io::Write;
    let mut cmd = Command::new(BIN);
    cmd.args(args).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    match failpoint {
        Some(fp) => cmd.env("STAALC_FAILPOINT", fp),
        None => cmd.env_remove("STAALC_FAILPOINT"),
    };
    let mut child = cmd.spawn().expect("run staalc");
    // The process may die before it reads everything: a closed pipe is expected then.
    let _ = child.stdin.take().unwrap().write_all(stdin.as_bytes());
    let o = child.wait_with_output().expect("run staalc");
    Out {
        code: o.status.code(),
        stdout: String::from_utf8_lossy(&o.stdout).trim().to_string(),
        stderr: String::from_utf8_lossy(&o.stderr).trim().to_string(),
    }
}

#[test]
fn console_input_is_replayed_and_only_what_was_not_recorded_is_read_again() {
    let lines = ["ada", "bola", "chidi"];
    let all = lines.join("\n") + "\n";
    for idempotent in [true, false] {
        let mode = format!("input-{}", if idempotent { "idempotent" } else { "plain" });
        let base = Case::of(&INPUT, &format!("{mode}-baseline"), idempotent);
        let out = staalc_with_input(&base.args("run"), None, &all);
        assert_eq!(out.code, Some(0), "baseline: {}", out.stderr);
        assert_eq!(out.stdout, "\"ada;bola;chidi;\"");
        let journal = base.journal_lines();
        let ops = journal.iter().filter(|r| r["j"] == "intent").count();
        let inputs = journal.iter().filter(|r| r["j"] == "intent" && r["op"] == "input").count();
        assert_eq!(inputs, 3, "three lines read");
        let world = base.world();
        assert_eq!(world.len(), INPUT.actions);
        let mut points: Vec<String> = (1..=journal.len()).map(|k| format!("after_write:{k}")).collect();
        points.extend((1..=ops).map(|n| format!("before_result:{n}")));
        for fp in &points {
            let label = format!("{mode} {fp}");
            let c = Case::of(&INPUT, &format!("{mode}-{}", fp.replace(':', "-")), idempotent);
            let crashed = staalc_with_input(&c.args("run"), Some(fp), &all);
            assert_eq!(crashed.code, None, "{label}: the injected crash did not happen: {}", crashed.stderr);
            let mut finished = None;
            for _ in 0..10 {
                // As a person would: type again only what the run has not recorded.
                let recorded = c.journal_lines().iter().filter(|r| r["j"] == "result" && is_input(&c, r)).count();
                let rest: String = lines[recorded..].iter().map(|l| format!("{l}\n")).collect();
                let out = staalc_with_input(&c.args("resume"), None, &rest);
                match out.code {
                    Some(0) => {
                        finished = Some(out);
                        break;
                    }
                    Some(3) => {
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
            let out = finished.unwrap_or_else(|| panic!("{label}: the run did not finish"));
            assert_eq!(out.stdout, "\"ada;bola;chidi;\"", "{label}");
            assert_eq!(c.world(), world, "{label}: the world differs from the uninterrupted run");
            c.assert_no_duplicates(&label);
            // A finished run replays: it reads nothing, even with nothing to read.
            let again = staalc_with_input(&c.args("resume"), None, "");
            assert_eq!((again.code, again.stdout.as_str()), (Some(0), "\"ada;bola;chidi;\""), "{label}: {}", again.stderr);
        }
    }
}

/// Whether a result record is for a console read.
fn is_input(c: &Case, result: &Json) -> bool {
    c.journal_lines().iter().any(|r| r["j"] == "intent" && r["n"] == result["n"] && r["op"] == "input")
}

/// A database scenario's directory: its config (with `idempotency_key` on the execute
/// grant or not), its database and its journal.
#[cfg(target_os = "linux")]
fn db_case(name: &str, keyed: bool) -> (PathBuf, Vec<String>, Vec<String>) {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("crash-db").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = std::fs::canonicalize(&dir).unwrap();
    let only = format!("{}/*.db", dir.display());
    let cfg = serde_json::json!({
        "entry": "store",
        "inputs": { "path": format!("{}/items.db", dir.display()) },
        "grants": {
            "db": { "cap": "db.sqlite.execute", "only": [only], "idempotency_key": keyed },
            "read": { "cap": "db.sqlite.query", "only": [only] },
        },
        "model": { "provider": "script", "responses": [] },
    });
    std::fs::write(dir.join("cfg.json"), cfg.to_string()).unwrap();
    let program = format!("{ROOT}/tests/crash/db.staal");
    let (c, j) = (dir.join("cfg.json").to_string_lossy().to_string(), dir.join("journal").to_string_lossy().to_string());
    let run = vec!["run".into(), program.clone(), "--config".into(), c.clone(), "--journal".into(), j.clone()];
    let resume = vec!["resume".into(), program, "--config".into(), c, "--journal".into(), j];
    (dir, run, resume)
}

#[cfg(target_os = "linux")]
#[test]
fn every_crash_point_leaves_each_row_once() {
    let names = |dir: &Path| -> Vec<String> {
        let Ok(c) = rusqlite::Connection::open(dir.join("items.db")) else { return Vec::new() };
        let Ok(mut s) = c.prepare("SELECT name FROM items ORDER BY rowid") else { return Vec::new() };
        s.query_map([], |r| r.get::<_, String>(0)).unwrap().map(Result::unwrap).collect()
    };
    for keyed in [true, false] {
        let mode = if keyed { "keyed" } else { "plain" };
        let (dir, run, _) = db_case(&format!("{mode}-baseline"), keyed);
        let out = staalc(&run.iter().map(String::as_str).collect::<Vec<_>>(), None);
        assert_eq!(out.code, Some(0), "{mode} baseline: {}", out.stderr);
        assert_eq!(out.stdout, "\"[{\\\"name\\\":\\\"a\\\"},{\\\"name\\\":\\\"b\\\"}]\"");
        let journal: Vec<Json> =
            std::fs::read_to_string(dir.join("journal")).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        let ops = journal.iter().filter(|r| r["j"] == "intent").count();
        assert_eq!(ops, 4, "three statements and a query");
        let mut points: Vec<String> = (1..=journal.len()).map(|k| format!("after_write:{k}")).collect();
        points.extend((1..=ops).map(|n| format!("before_result:{n}")));
        for fp in &points {
            let label = format!("{mode} {fp}");
            let (dir, run, resume) = db_case(&format!("{mode}-{}", fp.replace(':', "-")), keyed);
            let crashed = staalc(&run.iter().map(String::as_str).collect::<Vec<_>>(), Some(fp));
            assert_eq!(crashed.code, None, "{label}: the injected crash did not happen: {}", crashed.stderr);
            let mut finished = None;
            for _ in 0..10 {
                let out = staalc(&resume.iter().map(String::as_str).collect::<Vec<_>>(), None);
                match out.code {
                    Some(0) => {
                        finished = Some(out);
                        break;
                    }
                    Some(3) => {
                        // Only a statement without a key is ever left for an operator.
                        assert!(!keyed, "{label}: a keyed statement was left in doubt: {}", out.stderr);
                        // As a correct operator would: look in the database.
                        let lines = std::fs::read_to_string(dir.join("journal")).unwrap();
                        let intent: Json = lines.lines().rev().map(|l| serde_json::from_str::<Json>(l).unwrap()).find(|r| r["j"] == "intent").unwrap();
                        let n = intent["n"].as_u64().unwrap();
                        let have = names(&dir);
                        let happened = match n {
                            1 => dir.join("items.db").exists() && rusqlite::Connection::open(dir.join("items.db")).unwrap().prepare("SELECT 1 FROM items").is_ok(),
                            2 => have.contains(&"a".to_string()),
                            3 => have.contains(&"b".to_string()),
                            other => panic!("{label}: operation {other} in doubt; a query is never left for an operator"),
                        };
                        let j = dir.join("journal").to_string_lossy().to_string();
                        let how: &[&str] = if happened { &["--performed", "sqlite:1"] } else { &["--retry"] };
                        let mut args = vec!["resolve", "--journal", j.as_str()];
                        args.extend_from_slice(how);
                        let r = staalc(&args, None);
                        assert_eq!(r.code, Some(0), "{label}: resolve failed: {}", r.stderr);
                    }
                    other => panic!("{label}: resume exited {other:?}: {}", out.stderr),
                }
            }
            let out = finished.unwrap_or_else(|| panic!("{label}: the run did not finish"));
            assert_eq!(out.stdout, "\"[{\\\"name\\\":\\\"a\\\"},{\\\"name\\\":\\\"b\\\"}]\"", "{label}");
            assert_eq!(names(&dir), ["a", "b"], "{label}: the database differs from an uninterrupted run");
        }
    }
}

/// net.tcp.exchange after a crash at any point: the exchange is made once, or left
/// for an operator, who records the reply the server really gave.
#[test]
fn a_tcp_exchange_is_never_made_twice_whatever_the_crash() {
    use std::io::{BufRead, BufReader, Write};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap().to_string();
    let seen = std::sync::Arc::new(AtomicUsize::new(0));
    let count = seen.clone();
    std::thread::spawn(move || {
        for c in l.incoming() {
            let Ok(mut c) = c else { continue };
            count.fetch_add(1, Ordering::SeqCst);
            let mut line = String::new();
            if BufReader::new(&c).read_line(&mut line).is_ok() {
                let _ = c.write_all(b"+hello\r\n");
            }
        }
    });
    let case = |name: &str| -> (PathBuf, Vec<String>, Vec<String>) {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("crash-tcp").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = serde_json::json!({
            "entry": "ask", "inputs": { "address": addr, "word": "hello" },
            "grants": { "conn": { "cap": "net.tcp.exchange", "only": ["127.0.0.1:*"] } },
            "model": { "provider": "script", "responses": [] },
        });
        std::fs::write(dir.join("cfg.json"), cfg.to_string()).unwrap();
        let p = format!("{ROOT}/tests/tcp/echo.staal");
        let (c, j) = (dir.join("cfg.json").to_string_lossy().to_string(), dir.join("journal").to_string_lossy().to_string());
        (dir, vec!["run".into(), p.clone(), "--config".into(), c.clone(), "--journal".into(), j.clone()], vec!["resume".into(), p, "--config".into(), c, "--journal".into(), j])
    };
    let (dir, run, _) = case("baseline");
    let out = staalc(&run.iter().map(String::as_str).collect::<Vec<_>>(), None);
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let writes = std::fs::read_to_string(dir.join("journal")).unwrap().lines().count();
    let mut points: Vec<String> = (1..=writes).map(|k| format!("after_write:{k}")).collect();
    points.push("before_result:1".into());
    for fp in &points {
        let (dir, run, resume) = case(&fp.replace(':', "-"));
        let before = seen.load(Ordering::SeqCst);
        let crashed = staalc(&run.iter().map(String::as_str).collect::<Vec<_>>(), Some(fp));
        assert_eq!(crashed.code, None, "{fp}: {}", crashed.stderr);
        let mut out = staalc(&resume.iter().map(String::as_str).collect::<Vec<_>>(), None);
        if out.code == Some(3) {
            // The server did answer; the operator records what it said.
            let j = dir.join("journal").to_string_lossy().to_string();
            let r = staalc(&["resolve", "--journal", &j, "--reply", "+hello\r\n"], None);
            assert_eq!(r.code, Some(0), "{fp}: {}", r.stderr);
            out = staalc(&resume.iter().map(String::as_str).collect::<Vec<_>>(), None);
        }
        assert_eq!((out.code, out.stdout.as_str()), (Some(0), "\"+hello\""), "{fp}: {}", out.stderr);
        let made = seen.load(Ordering::SeqCst) - before;
        // A crash before the intent was recorded is the only one after which the
        // exchange is made again: the first attempt never began.
        assert!(made <= 1, "{fp}: the exchange was made {made} times");
    }
}

/// Serving: three requests accepted and answered. A crash can lose a connection (it
/// does not outlive the process), never answer a request twice: after any crash the
/// resumed run ends as an uninterrupted one, or, when a request it had accepted is
/// left unanswered, fails saying so rather than answering again.
#[test]
fn a_served_request_is_never_answered_twice_whatever_the_crash() {
    use std::io::{Read, Write};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let addr = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().to_string();
    let answered = std::sync::Arc::new(AtomicUsize::new(0));
    // Clients keep coming, one at a time, whether or not a run is listening.
    for _ in 0..2 {
        let (addr, answered) = (addr.clone(), answered.clone());
        std::thread::spawn(move || loop {
            let Ok(mut c) = std::net::TcpStream::connect(&addr) else {
                std::thread::sleep(std::time::Duration::from_millis(5));
                continue;
            };
            let _ = c.write_all(b"hi\n");
            let _ = c.set_read_timeout(Some(std::time::Duration::from_secs(30)));
            let mut got = String::new();
            let _ = c.read_to_string(&mut got);
            if !got.is_empty() {
                assert_eq!(got, "+hi\n");
                answered.fetch_add(1, Ordering::SeqCst);
            }
        });
    }
    let case = |name: &str| -> (PathBuf, Vec<String>, Vec<String>) {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("crash-serve").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = serde_json::json!({
            "entry": "serve", "inputs": { "address": addr },
            "grants": { "listen": { "cap": "net.tcp.accept", "only": ["127.0.0.1:*"] }, "answer": { "cap": "net.tcp.reply" } },
            "model": { "provider": "script", "responses": [] },
        });
        std::fs::write(dir.join("cfg.json"), cfg.to_string()).unwrap();
        let p = format!("{ROOT}/tests/serve/serve.staal");
        let (c, j) = (dir.join("cfg.json").to_string_lossy().to_string(), dir.join("journal").to_string_lossy().to_string());
        (dir, vec!["run".into(), p.clone(), "--config".into(), c.clone(), "--journal".into(), j.clone()], vec!["resume".into(), p, "--config".into(), c, "--journal".into(), j])
    };
    // Answers counted once every client in flight has read its answer.
    let settled = || {
        let mut last = answered.load(Ordering::SeqCst);
        loop {
            std::thread::sleep(std::time::Duration::from_millis(150));
            let now = answered.load(Ordering::SeqCst);
            if now == last {
                return now;
            }
            last = now;
        }
    };
    let (dir, run, _) = case("baseline");
    let before = settled();
    let out = staalc(&run.iter().map(String::as_str).collect::<Vec<_>>(), None);
    assert_eq!((out.code, out.stdout.as_str()), (Some(0), "3"), "{}", out.stderr);
    assert_eq!(settled() - before, 3);
    let writes = std::fs::read_to_string(dir.join("journal")).unwrap().lines().count();
    let mut points: Vec<String> = (1..=writes).map(|k| format!("after_write:{k}")).collect();
    points.extend((1..=6).map(|n| format!("before_result:{n}")));
    let (mut lost, mut whole) = (0, 0);
    for fp in &points {
        let (dir, run, resume) = case(&fp.replace(':', "-"));
        let before = settled();
        let crashed = staalc(&run.iter().map(String::as_str).collect::<Vec<_>>(), Some(fp));
        assert_eq!(crashed.code, None, "{fp}: {}", crashed.stderr);
        let out = staalc(&resume.iter().map(String::as_str).collect::<Vec<_>>(), None);
        let delivered = settled() - before;
        let lines: Vec<Json> = std::fs::read_to_string(dir.join("journal")).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        let reply_intents: Vec<u64> = lines.iter().filter(|r| r["j"] == "intent" && r["body"]["capability"] == "net.tcp.reply").map(|r| r["n"].as_u64().unwrap()).collect();
        let reply_ok = lines.iter().filter(|r| r["j"] == "result" && r.get("ok").is_some() && reply_intents.contains(&r["n"].as_u64().unwrap())).count();
        // Every answer a client got is one the journal records an attempt at, and no
        // reply was ever attempted twice.
        assert!(delivered <= reply_intents.len(), "{fp}: {delivered} answers for {} replies", reply_intents.len());
        assert!(delivered >= reply_ok, "{fp}: {reply_ok} replies recorded, {delivered} answers");
        if out.code == Some(0) {
            assert_eq!(out.stdout, "3", "{fp}");
            assert_eq!((reply_ok, delivered), (3, 3), "{fp}");
            whole += 1;
        } else {
            assert!(out.stderr.contains("is not open"), "{fp}: {}", out.stderr);
            lost += 1;
        }
    }
    // Both endings are reached: a crash before or after a request's whole exchange
    // costs nothing; one between its accept and its reply loses that request.
    assert!(whole > 0 && lost > 0, "{whole} whole, {lost} lost");
}

/// Concurrency: three actions and three model calls in concurrent tasks, then one more
/// action.
const PARALLEL: Scenario = Scenario { program: "tests/crash/parallel.staal", config: "tests/crash/parallel.json", actions: 4 };

/// The phase 11 gate: concurrent work is still crash-safe. Killed at every journal
/// write and inside every task's in-doubt window, resumed (on the other engine too),
/// with each action in doubt resolved as a correct operator would, the run ends
/// exactly as an uninterrupted one: the same result, the same world, nothing twice.
/// Several actions can be in doubt at once: every task in flight when the process died.
#[test]
fn concurrent_tasks_recover_exactly_after_any_crash() {
    for idempotent in [true, false] {
        for engines in [("interpreter", "interpreter"), ("interpreter", "native")] {
            if engines.1 == "native" && !cfg!(feature = "native") {
                continue;
            }
            let mode = format!("par-{}-{}", if idempotent { "idem" } else { "plain" }, engines.1);
            let base = Case::of(&PARALLEL, &format!("{mode}-baseline"), idempotent);
            let out = base.run(None);
            assert_eq!(out.code, Some(0), "{mode} baseline: {}", out.stderr);
            assert_eq!(out.stdout, "\"R,S,W 3\"");
            // Which actions happened, not the simulated receipt numbers, which follow the
            // order concurrent tasks reached the service in.
            let unreceipted = |w: Vec<Json>| {
                let mut w: Vec<Json> = w.into_iter().map(|mut e| { e.as_object_mut().unwrap().remove("receipt"); e }).collect();
                w.sort_by_key(|e| e.to_string());
                w
            };
            let world = unreceipted(base.world());
            assert_eq!(world.len(), PARALLEL.actions);
            let journal = base.journal_lines();
            // Every operation, by id: the program's own (n) and each task's (t:n).
            let ids: Vec<String> = journal
                .iter()
                .filter(|r| r["j"] == "intent")
                .map(|r| match r["t"].as_str() {
                    Some(t) => format!("{t}:{}", r["n"]),
                    None => r["n"].to_string(),
                })
                .collect();
            assert_eq!(ids.len(), 3 + 3 * 2 + 1, "3 sends, 3 counts and 3 model calls, 1 send");
            let mut points: Vec<String> = (1..=journal.len()).map(|k| format!("after_write:{k}")).collect();
            points.extend(ids.iter().map(|id| format!("before_result:{id}")));
            for fp in &points {
                let label = format!("{mode} {fp}");
                let mut c = Case::of(&PARALLEL, &format!("{mode}-{}", fp.replace([':', '/'], "-")), idempotent);
                c.engines = engines;
                let crashed = c.run(Some(fp));
                assert_eq!(crashed.code, None, "{label}: the injected crash did not happen: {}", crashed.stderr);
                let mut finished = None;
                for _ in 0..20 {
                    let out = c.resume();
                    match out.code {
                        Some(0) => {
                            finished = Some(out);
                            break;
                        }
                        Some(3) => {
                            // Each action in doubt, resolved by what the world shows.
                            for (task, key) in open_actions(&c) {
                                let done = c.world_entries().into_iter().find(|e| e["key"] == key);
                                let mut how: Vec<String> = match done {
                                    Some(e) => vec!["--performed".into(), e["receipt"].as_str().unwrap().into()],
                                    None => vec!["--retry".into()],
                                };
                                how.extend(["--operation".to_string(), task]);
                                let r = c.resolve(&how.iter().map(String::as_str).collect::<Vec<_>>());
                                assert_eq!(r.code, Some(0), "{label}: resolve failed: {}", r.stderr);
                            }
                        }
                        other => panic!("{label}: resume exited {other:?}: {}", out.stderr),
                    }
                }
                let out = finished.unwrap_or_else(|| panic!("{label}: the run did not finish"));
                assert_eq!(out.stdout, "\"R,S,W 3\"", "{label}");
                assert_eq!(unreceipted(c.world()), world, "{label}: the world differs from an uninterrupted run");
                c.assert_no_duplicates(&label);
                let again = c.resume();
                assert_eq!((again.code, again.stdout.as_str()), (Some(0), "\"R,S,W 3\""), "{label}: {}", again.stderr);
            }
        }
    }
}

/// The actions a journal leaves open: each one's operation id and key.
fn open_actions(c: &Case) -> Vec<(String, String)> {
    let mut last: std::collections::BTreeMap<String, Json> = std::collections::BTreeMap::new();
    for r in c.journal_lines() {
        let t = r["t"].as_str().unwrap_or("").to_string();
        last.insert(t, r);
    }
    let mut open = Vec::new();
    for (t, r) in &last {
        if r["j"] == "intent" || r["j"] == "attempt" {
            let n = r["n"].as_u64().unwrap();
            let intent = c.journal_lines().into_iter().find(|x| x["j"] == "intent" && x["n"] == n && x["t"].as_str().unwrap_or("") == t).unwrap();
            assert_eq!(intent["op"], "action", "only actions are ever left for an operator");
            let id = if t.is_empty() { n.to_string() } else { format!("{t}:{n}") };
            open.push((id, intent["key"].as_str().unwrap().to_string()));
        }
    }
    open
}
