// The run observer (RUNTIME.md section 20; Staal Studio's design, section 9), each of its
// guarantees: it cannot change a run, its failure cannot fail the run, it cannot block
// the run, every event names its run, task and operation in its task's order, its schema
// is named with the staalc that emits it, and a run's result, log and journal are the
// same with an observer as without, on both engines, for every runtime case.

use serde_json::{Value as Json, json};
use staalc::interp::observe::{End, Observation, Observed, Observer, SCHEMA};
use staalc::interp::{Durability, Engine, RunResult, run_config_engine, run_config_observed, same_run};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn engines() -> Vec<Engine> {
    if cfg!(feature = "native") { vec![Engine::Interpreter, Engine::Native] } else { vec![Engine::Interpreter] }
}

/// Keeps every event, and tells the test when the run is over.
struct Collect {
    seen: Arc<Mutex<Vec<Observed>>>,
    done: Sender<End>,
}

impl Observer for Collect {
    fn event(&mut self, e: Observed) -> Result<(), String> {
        self.seen.lock().unwrap().push(e);
        Ok(())
    }
    fn end(&mut self, end: End) {
        let _ = self.done.send(end);
    }
}

fn collect(capacity: usize) -> (Observation, Arc<Mutex<Vec<Observed>>>, Receiver<End>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (tx, rx) = channel();
    (Observation { observer: Box::new(Collect { seen: seen.clone(), done: tx }), capacity }, seen, rx)
}

fn wait(rx: &Receiver<End>) -> End {
    rx.recv_timeout(Duration::from_secs(60)).expect("the observer was never told the run ended")
}

fn tmp(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR")).join("observer");
    std::fs::create_dir_all(&d).unwrap();
    let p = d.join(name);
    let _ = std::fs::remove_file(&p);
    p
}

/// A journal's records with the run's id replaced, for comparing two runs.
fn journal_of(p: &Path, run_id: &str) -> String {
    std::fs::read_to_string(p).unwrap().replace(run_id, "<run>")
}

fn run_id(r: &RunResult) -> String {
    r.log.first().and_then(|e| e["run_id"].as_str()).unwrap_or_default().to_string()
}

/// The final log's events of `task`, as the observer is given them: without the log's
/// `seq`, and without the `task` the log adds when it gathers a task's events.
fn log_of_task(r: &RunResult, task: &str) -> Vec<Json> {
    r.log
        .iter()
        .filter(|e| e.get("task").and_then(Json::as_str).unwrap_or("") == task)
        .map(|e| {
            let mut e = e.clone();
            let o = e.as_object_mut().unwrap();
            o.remove("seq");
            o.remove("task");
            e
        })
        .collect()
}

/// Which operation an observed event names, checked: an event naming one in its own
/// fields names that; an event reporting what an operation gave (`replayed`) names one; a
/// request, made before its operation begins, and an event about no operation, name none.
/// With the run's journal, the operation named is the task's, and of the event's kind.
fn check_operation(e: &Observed, journal: Option<&Path>, what: &str) {
    let name = e.event["event"].as_str().unwrap_or_default();
    let named = match &e.event["operation"] {
        Json::Number(n) => Some(n.to_string()),
        Json::String(s) => Some(s.clone()),
        _ => None,
    };
    if named.is_some() {
        assert_eq!(e.operation, named, "{what}: {e:?}");
    } else if e.event.get("replayed").is_some() {
        assert!(e.operation.is_some(), "{what}: an outcome without its operation: {e:?}");
    } else if name != "client_dropped" {
        assert_eq!(e.operation, None, "{what}: {e:?}");
    }
    let Some(op) = &e.operation else { return };
    let n: u64 = if e.task.is_empty() {
        op.parse().unwrap_or_else(|_| panic!("{what}: {op} is not the program's own operation"))
    } else {
        op.strip_prefix(&format!("{}:", e.task)).unwrap_or_else(|| panic!("{what}: {op} is not task {}'s", e.task)).parse().unwrap()
    };
    let Some(journal) = journal else { return };
    let intent = std::fs::read_to_string(journal)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str::<Json>(l).unwrap())
        .find(|r| r["j"] == "intent" && r["n"] == n && r.get("t").and_then(Json::as_str).unwrap_or("") == e.task)
        .unwrap_or_else(|| panic!("{what}: no operation {op} of task {:?} in the journal: {e:?}", e.task));
    let kind = intent["op"].as_str().unwrap_or_default();
    let want = match name {
        n if n.starts_with("action_") || n == "client_dropped" => "action",
        "model_reply" | "model_error" => "model",
        "print" | "clock" | "random" | "input" => name,
        _ => kind,
    };
    assert_eq!(kind, want, "{what}: {e:?} names {op}, a {kind}");
    if kind == "action" && e.event.get("capability").is_some() {
        assert_eq!(intent["body"]["capability"], e.event["capability"], "{what}: {e:?} names {op}: {intent}");
    }
}

/// Checks one observed run against its final log: every event names the run, each task's
/// events are its part of the log in order, and every event is accounted for.
fn check_observed(r: &RunResult, seen: &[Observed], end: &End, journal: Option<&Path>, what: &str) {
    let id = run_id(r);
    let mut tasks: Vec<String> = seen.iter().map(|e| e.task.clone()).collect();
    tasks.sort();
    tasks.dedup();
    let mut log_tasks: Vec<String> = r.log.iter().map(|e| e.get("task").and_then(Json::as_str).unwrap_or("").to_string()).collect();
    log_tasks.sort();
    log_tasks.dedup();
    if end.missed == 0 && seen.iter().all(|e| e.missed_before == 0) {
        assert_eq!(tasks, log_tasks, "{what}: tasks");
        for t in &tasks {
            let observed: Vec<Json> = seen.iter().filter(|e| &e.task == t).map(|e| e.event.clone()).collect();
            assert_eq!(observed, log_of_task(r, t), "{what}: task {t:?}");
        }
    }
    for e in seen {
        assert_eq!((e.schema, e.staalc, e.run_id.as_str()), (SCHEMA, staalc::VERSION, id.as_str()), "{what}: {e:?}");
        check_operation(e, journal, what);
    }
    let missed: u64 = seen.iter().map(|e| e.missed_before).sum::<u64>() + end.missed;
    assert_eq!(end.delivered + missed, r.log.len() as u64, "{what}: delivered {} missed {missed}", end.delivered);
    assert_eq!(end.delivered, seen.len() as u64, "{what}");
    assert_eq!(end.detached, None, "{what}");
}

/// Every runtime case: the same result and log, and the same journal, with an observer as
/// without, on each engine; and what the observer saw is the log, task by task.
#[test]
fn every_runtime_case_is_the_same_observed_on_both_engines() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/run");
    let mut cases: Vec<PathBuf> =
        std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).filter(|p| p.extension().is_some_and(|e| e == "json")).collect();
    cases.sort();
    let mut ran = 0;
    for case in &cases {
        let stem = case.file_stem().unwrap().to_string_lossy().to_string();
        let base = stem.split("__").next().unwrap();
        let src = std::fs::read_to_string(dir.join(format!("{base}.staal"))).unwrap();
        let Ok(prog) = staalc::check_program(&src) else { continue };
        let mut cfg: Json = serde_json::from_str(&std::fs::read_to_string(case).unwrap()).unwrap();
        cfg.as_object_mut().unwrap().remove("expect");
        for engine in engines() {
            let what = format!("{stem} on {engine:?}");
            let (pa, pb) = (tmp(&format!("{stem}-{engine:?}-plain.journal")), tmp(&format!("{stem}-{engine:?}-observed.journal")));
            let plain = run_config_engine(&prog, &src, &cfg, Durability::Create(&pa), engine);
            let (obs, seen, done) = collect(1 << 16);
            let observed = run_config_observed(&prog, &src, &cfg, Durability::Create(&pb), engine, obs);
            match (plain, observed) {
                (Ok(a), Ok(b)) => {
                    assert!(same_run(&a, &b), "{what}: the observed run differs\n{:?}\n{:?}", a.result, b.result);
                    assert_eq!(journal_of(&pa, &run_id(&a)), journal_of(&pb, &run_id(&b)), "{what}: journals differ");
                    let end = wait(&done);
                    check_observed(&b, &seen.lock().unwrap(), &end, Some(&pb), &what);
                    ran += 1;
                }
                (Err(a), Err(b)) => assert_eq!(a, b, "{what}"),
                (a, b) => panic!("{what}: {:?} vs {:?}", a.map(|r| r.result), b.map(|r| r.result)),
            }
        }
    }
    assert!(ran > 300, "only {ran} observed runs");
}

const PARALLEL: &str = include_str!("parallel/send.staal");

/// Actions one after another: two runs of it give the same log. (Concurrent tasks share
/// the simulated adapter, which numbers receipts in the order the tasks reach it, so two
/// runs of PARALLEL can differ in which task got which receipt, observed or not.)
const SEQUENTIAL: &str = "fn send_each(mail: cap email.send) -> Int {
    let n = 0
    for to in [\"+2348000000001\", \"+2348000000002\", \"+2348000000003\", \"+2348000000004\", \"+2348000000005\"] {
        let r = mail.send(to)
        set n = n + 1
    }
    return n
}
";

fn sequential_config() -> Json {
    json!({ "entry": "send_each", "grants": { "mail": { "cap": "email.send" } }, "model": { "provider": "script", "responses": [] } })
}

fn parallel_config() -> Json {
    json!({ "entry": "send_all", "grants": { "mail": { "cap": "email.send" } }, "model": { "provider": "script", "responses": [] } })
}

/// Concurrent tasks: each task's events, named by task and operation, in its order.
#[test]
fn concurrent_tasks_are_observed_each_in_its_own_order() {
    let prog = staalc::check_program(PARALLEL).unwrap();
    for engine in engines() {
        for round in 0..20 {
            let j = tmp(&format!("tasks-{engine:?}-{round}.journal"));
            let (obs, seen, done) = collect(1 << 12);
            let r = run_config_observed(&prog, PARALLEL, &parallel_config(), Durability::Create(&j), engine, obs).unwrap();
            let end = wait(&done);
            let seen_now = seen.lock().unwrap().clone();
            assert!(seen_now.iter().filter(|e| !e.task.is_empty()).count() >= 3, "{engine:?} {round}: {seen_now:?}");
            check_observed(&r, &seen_now, &end, Some(&j), &format!("{engine:?} round {round}"));
            // Resumed, the finished run replays every operation from the journal: each
            // replayed outcome names the operation it came from.
            let (obs, seen, done) = collect(1 << 12);
            let again = run_config_observed(&prog, PARALLEL, &parallel_config(), Durability::Resume(&j), engine, obs).unwrap();
            let end = wait(&done);
            let seen = seen.lock().unwrap();
            assert!(seen.iter().any(|e| e.event["replayed"] == true && e.operation.is_some()), "{engine:?}: nothing replayed: {seen:?}");
            check_observed(&again, &seen, &end, Some(&j), &format!("{engine:?} resumed {round}"));
        }
    }
}

/// An observer that never returns cannot hold the run, and what it misses is counted.
#[test]
fn an_observer_that_never_returns_does_not_hold_the_run() {
    struct Stuck {
        first: Sender<()>,
        release: Arc<Mutex<Receiver<()>>>,
        seen: Arc<Mutex<Vec<Observed>>>,
        done: Sender<End>,
    }
    impl Observer for Stuck {
        fn event(&mut self, e: Observed) -> Result<(), String> {
            let first = self.seen.lock().unwrap().is_empty();
            self.seen.lock().unwrap().push(e);
            if first {
                let _ = self.first.send(());
                let _ = self.release.lock().unwrap().recv();
            }
            Ok(())
        }
        fn end(&mut self, end: End) {
            let _ = self.done.send(end);
        }
    }
    let prog = staalc::check_program(PARALLEL).unwrap();
    for engine in engines() {
        let (first_tx, first_rx) = channel();
        let (release_tx, release_rx) = channel();
        let (done_tx, done_rx) = channel();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let o = Stuck { first: first_tx, release: Arc::new(Mutex::new(release_rx)), seen: seen.clone(), done: done_tx };
        let started = std::time::Instant::now();
        let r = run_config_observed(&prog, PARALLEL, &parallel_config(), Durability::None, engine, Observation { observer: Box::new(o), capacity: 1 })
            .unwrap();
        // The run ended while the observer was still stuck on its first event.
        first_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(started.elapsed() < Duration::from_secs(30), "{engine:?}: the run waited");
        assert!(done_rx.try_recv().is_err(), "{engine:?}: the observer ended before it was released");
        release_tx.send(()).unwrap();
        let end = wait(&done_rx);
        let seen = seen.lock().unwrap();
        let missed: u64 = seen.iter().map(|e| e.missed_before).sum::<u64>() + end.missed;
        assert!(missed > 0, "{engine:?}: nothing was dropped from a buffer of one ({} events)", r.log.len());
        check_observed(&r, &seen, &end, None, &format!("{engine:?} stuck"));
    }
}

/// An observer that panics or errs is detached; the run goes on, the same as unobserved.
#[test]
fn an_observer_that_fails_is_detached_and_the_run_goes_on() {
    struct Fails {
        after: usize,
        n: usize,
        panic: bool,
        done: Sender<End>,
    }
    impl Observer for Fails {
        fn event(&mut self, _e: Observed) -> Result<(), String> {
            self.n += 1;
            if self.n > self.after {
                if self.panic {
                    panic!("the observer broke");
                }
                return Err("the observer gave up".into());
            }
            Ok(())
        }
        fn end(&mut self, end: End) {
            let _ = self.done.send(end);
        }
    }
    let prog = staalc::check_program(SEQUENTIAL).unwrap();
    for engine in engines() {
        let plain = run_config_engine(&prog, SEQUENTIAL, &sequential_config(), Durability::None, engine).unwrap();
        for (panic, want) in [(true, "it panicked: the observer broke"), (false, "the observer gave up")] {
            let (tx, rx) = channel();
            let o = Fails { after: 1, n: 0, panic, done: tx };
            let r = run_config_observed(&prog, SEQUENTIAL, &sequential_config(), Durability::None, engine, Observation { observer: Box::new(o), capacity: 64 })
                .unwrap();
            assert!(same_run(&plain, &r), "{engine:?} panic={panic}");
            let end = wait(&rx);
            assert_eq!((end.delivered, end.detached.as_deref()), (1, Some(want)), "{engine:?}");
        }
    }
}

/// What an observer does to its copy reaches nothing of the run's.
#[test]
fn an_observer_cannot_change_the_run() {
    struct Meddles(Sender<End>);
    impl Observer for Meddles {
        fn event(&mut self, mut e: Observed) -> Result<(), String> {
            e.event["event"] = json!("changed");
            if let Some(o) = e.event.as_object_mut() {
                o.clear();
            }
            Ok(())
        }
        fn end(&mut self, end: End) {
            let _ = self.0.send(end);
        }
    }
    let prog = staalc::check_program(SEQUENTIAL).unwrap();
    for engine in engines() {
        let plain = run_config_engine(&prog, SEQUENTIAL, &sequential_config(), Durability::None, engine).unwrap();
        let (tx, rx) = channel();
        let r = run_config_observed(&prog, SEQUENTIAL, &sequential_config(), Durability::None, engine, Observation { observer: Box::new(Meddles(tx)), capacity: 64 })
            .unwrap();
        wait(&rx);
        assert!(same_run(&plain, &r), "{engine:?}");
        assert!(r.log.iter().all(|e| e["event"] != "changed"));
    }
}

/// A bad config, or a buffer that holds nothing, starts nothing: no observer, no journal.
#[test]
fn a_run_that_cannot_start_starts_no_observer_and_no_journal() {
    let prog = staalc::check_program(PARALLEL).unwrap();
    let p = tmp("not-started.journal");
    let (obs, _seen, done) = collect(16);
    let mut bad = parallel_config();
    bad["entry"] = json!("nothing_called_this");
    assert!(run_config_observed(&prog, PARALLEL, &bad, Durability::Create(&p), Engine::Interpreter, obs).is_err());
    assert!(!p.exists());
    assert!(done.recv_timeout(Duration::from_millis(200)).is_err(), "an observer was started");
    let (mut obs, _seen, _done) = collect(16);
    obs.capacity = 0;
    let e = run_config_observed(&prog, PARALLEL, &parallel_config(), Durability::Create(&p), Engine::Interpreter, obs).unwrap_err();
    assert!(e.contains("at least one event"), "{e}");
    assert!(!p.exists());
}

/// An action left in doubt by a crash, resumed under an observer: the event saying so names
/// the operation as the journal numbers it, whether resume retries it (the simulated
/// service deduplicates by key) or stops (it does not).
#[test]
fn an_operation_in_doubt_is_named_as_the_journal_names_it() {
    let prog = staalc::check_program(SEQUENTIAL).unwrap();
    for engine in engines() {
        for (dedups, event) in [(true, "retrying_in_doubt"), (false, "in_doubt")] {
            let mut cfg = sequential_config();
            cfg["adapters"] = json!({ "simulated": { "idempotent": dedups } });
            let j = tmp(&format!("in-doubt-{engine:?}-{dedups}.journal"));
            run_config_engine(&prog, SEQUENTIAL, &cfg, Durability::Create(&j), engine).unwrap();
            // As a crash after the first action began leaves it: its intent, and no result.
            let text = std::fs::read_to_string(&j).unwrap();
            let kept: Vec<&str> = text.lines().take_while(|l| !l.contains("\"j\":\"result\"")).collect();
            assert!(kept.last().unwrap().contains("\"j\":\"intent\""), "{kept:?}");
            std::fs::write(&j, kept.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
            let (obs, seen, done) = collect(1 << 10);
            let r = run_config_observed(&prog, SEQUENTIAL, &cfg, Durability::Resume(&j), engine, obs).unwrap();
            let failed_in_doubt = r.result.as_ref().is_err_and(|f| f.kind.to_string() == "in_doubt");
            assert_eq!(failed_in_doubt, !dedups, "{engine:?}: {:?}", r.result);
            let end = wait(&done);
            let seen = seen.lock().unwrap();
            let e = seen.iter().find(|e| e.event["event"] == event).unwrap_or_else(|| panic!("{engine:?}: no {event}"));
            assert_eq!(e.event["operation"], 1);
            assert_eq!(e.operation.as_deref(), Some("1"), "{engine:?}: {e:?}");
            check_observed(&r, &seen, &end, Some(&j), &format!("{engine:?} {event}"));
        }
    }
}
