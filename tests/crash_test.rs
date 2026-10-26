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
