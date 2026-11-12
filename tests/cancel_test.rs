// Cancelling a run from outside (RUNTIME.md section 22), on both engines: it stops at its
// next step, native code included; an operation under way finishes and is journaled
// first; the run records no end and resumes; tasks stop too, before any ordinary failure;
// `try` cannot catch it.

use serde_json::{Value as Json, json};
use staalc::interp::adapter::Adapter;
use staalc::interp::cancel::Cancel;
use staalc::interp::money::Money;
use staalc::interp::observe::{End, Observation, Observed, Observer};
use staalc::interp::provider::of_run;
use staalc::interp::value::Value;
use staalc::interp::values::{Given, ProcessEnvironment, Values};
use staalc::interp::{Attached, Durability, Engine, RunResult, run_attached, run_config_attached};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn engines() -> Vec<Engine> {
    if cfg!(feature = "native") { vec![Engine::Interpreter, Engine::Native] } else { vec![Engine::Interpreter] }
}

fn tmp(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR")).join("cancel");
    std::fs::create_dir_all(&d).unwrap();
    let p = d.join(name);
    let _ = std::fs::remove_file(&p);
    p
}

fn records(p: &Path) -> Vec<Json> {
    std::fs::read_to_string(p).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

fn kind(r: &RunResult) -> String {
    r.result.as_ref().err().map(|f| f.kind.to_string()).unwrap_or_else(|| "ok".into())
}

/// A pure Int loop, which the native engine compiles, that runs for hours unless stopped.
const SPIN: &str = "fn spin(n: Int) -> Int {
    let i = 0
    loop {
        if i == n {
            break
        }
        set i = i + 1
    }
    return i
}

fn main() -> Int {
    return spin(1000000000000)
}

fn caught() -> Bool {
    let r = try spin(1000000000000)
    return r.ok
}

fn part(n: Int) -> Int {
    if n == 0 {
        return 1 / n
    }
    return spin(n)
}

fn tasks() -> Int {
    return sum(parallel([0, 1000000000000, 1000000000000], part))
}
";

fn spin_config(entry: &str) -> Json {
    json!({ "entry": entry, "limits": { "max_steps": 1_000_000_000_000_000u64 } })
}

/// Starts `entry` of SPIN with `cancel`, cancels it after a moment, and gives what it ended
/// with and how long it took to stop once cancelled.
fn cancelled_after_a_moment(entry: &str, engine: Engine, journal: Option<&Path>) -> (RunResult, Duration) {
    let prog = staalc::check_program(SPIN).unwrap();
    let cancel = Cancel::new();
    let c = cancel.clone();
    std::thread::scope(|s| {
        let h = s.spawn(|| {
            let d = match journal {
                Some(p) => Durability::Create(p),
                None => Durability::None,
            };
            run_config_attached(&prog, SPIN, &spin_config(entry), d, engine, Attached { cancel: Some(c), ..Attached::default() }).unwrap()
        });
        std::thread::sleep(Duration::from_millis(300));
        assert!(!h.is_finished(), "{entry} on {engine:?} ended before it was cancelled");
        let at = Instant::now();
        cancel.cancel();
        let r = h.join().unwrap();
        (r, at.elapsed())
    })
}

#[test]
fn a_cancelled_run_stops_at_its_next_step_on_both_engines_and_records_no_end() {
    for engine in engines() {
        let j = tmp(&format!("spin-{engine:?}.journal"));
        let (r, took) = cancelled_after_a_moment("main", engine, Some(&j));
        assert_eq!(kind(&r), "cancelled", "{engine:?}: {:?}", r.result);
        assert!(took < Duration::from_secs(5), "{engine:?}: took {took:?} to stop");
        let recs = records(&j);
        assert!(recs.iter().all(|r| r["j"] != "end"), "{engine:?}: a cancelled run recorded an end: {recs:?}");
        assert!(r.log.iter().any(|e| e["event"] == "run_end" && e["failure"] == "cancelled"));
    }
}

#[test]
fn try_cannot_catch_a_cancellation() {
    for engine in engines() {
        let (r, _) = cancelled_after_a_moment("caught", engine, None);
        assert_eq!(kind(&r), "cancelled", "{engine:?}");
    }
}

#[test]
fn concurrent_tasks_stop_and_the_cancellation_comes_before_an_ordinary_failure() {
    for engine in engines() {
        let (r, took) = cancelled_after_a_moment("tasks", engine, None);
        assert_eq!(kind(&r), "cancelled", "{engine:?}: {:?}", r.result);
        assert!(took < Duration::from_secs(5), "{engine:?}: took {took:?}");
    }
}

/// An adapter whose first action waits until the test lets it go, counting what it performs.
#[derive(Clone)]
struct Gate {
    performed: Arc<AtomicUsize>,
    started: Arc<Mutex<Option<Sender<()>>>>,
    release: Arc<Mutex<Option<Receiver<()>>>>,
}

impl Adapter for Gate {
    fn amount(&self, _c: &str, _m: &str, _a: &[Value]) -> Option<Money> {
        None
    }
    fn target(&self, _c: &str, _m: &str, _a: &[Value]) -> Option<String> {
        None
    }
    fn idempotent(&self, _c: &str, _m: &str) -> bool {
        true
    }
    fn perform(&mut self, _k: &str, _c: &str, _m: &str, _a: &[Json]) -> Result<String, String> {
        if let Some(tx) = self.started.lock().unwrap().take() {
            tx.send(()).unwrap();
            let rx = self.release.lock().unwrap().take().unwrap();
            rx.recv().unwrap();
        }
        Ok(format!("r-{}", self.performed.fetch_add(1, Ordering::SeqCst) + 1))
    }
    fn fork(&self) -> Box<dyn Adapter + Send> {
        Box::new(self.clone())
    }
}

const TWO: &str = "fn main(mail: cap email.send) -> Int {
    let a = mail.send(\"+2348000000001\")
    let b = mail.send(\"+2348000000002\")
    return 2
}
";

/// Two actions in one statement: no step comes between them, so only the check before an
/// operation stops the second.
const ONE_STATEMENT: &str = "fn both(a: Receipt, b: Receipt) -> Int {
    return 2
}

fn main(mail: cap email.send) -> Int {
    return both(mail.send(\"+2348000000001\"), mail.send(\"+2348000000002\"))
}
";

#[test]
fn an_action_under_way_finishes_and_is_journaled_then_the_run_stops_and_resumes() {
    let cfg = json!({ "entry": "main", "grants": { "mail": { "cap": "email.send" } } });
    for (src, engine) in [TWO, ONE_STATEMENT].into_iter().flat_map(|s| engines().into_iter().map(move |e| (s, e))) {
        let prog = staalc::check_program(src).unwrap();
        let j = tmp(&format!("two-{engine:?}-{}.journal", src.len()));
        let performed = Arc::new(AtomicUsize::new(0));
        let (started_tx, started_rx) = channel();
        let (release_tx, release_rx) = channel();
        let gate = Gate { performed: performed.clone(), started: Arc::new(Mutex::new(Some(started_tx))), release: Arc::new(Mutex::new(Some(release_rx))) };
        let cancel = Cancel::new();
        let r = std::thread::scope(|s| {
            let c = cancel.clone();
            let (prog, cfg, j, gate) = (&prog, &cfg, &j, gate.clone());
            let h = s.spawn(move || {
                let provider = of_run(None, &ProcessEnvironment).unwrap();
                run_attached(prog, src, cfg, Durability::Create(j), provider, Box::new(gate), engine, Attached { cancel: Some(c), ..Attached::default() }).unwrap()
            });
            started_rx.recv_timeout(Duration::from_secs(30)).unwrap();
            // Cancelled while the first action is under way; then it is let finish.
            cancel.cancel();
            release_tx.send(()).unwrap();
            h.join().unwrap()
        });
        assert_eq!(kind(&r), "cancelled", "{engine:?}: {:?}", r.result);
        assert_eq!(performed.load(Ordering::SeqCst), 1, "{engine:?}");
        let recs = records(&j);
        let kinds: Vec<&str> = recs.iter().map(|r| r["j"].as_str().unwrap()).collect();
        assert_eq!(kinds, ["start", "intent", "result"], "{engine:?}: the action finished and was journaled, nothing after");
        // Resumed: the first action is replayed, not made again; the second is made.
        let provider = of_run(None, &ProcessEnvironment).unwrap();
        let again = Gate { performed: performed.clone(), started: Arc::new(Mutex::new(None)), release: Arc::new(Mutex::new(None)) };
        let r = run_attached(&prog, src, &cfg, Durability::Resume(&j), provider, Box::new(again), engine, Attached::default()).unwrap();
        assert_eq!(r.result.unwrap(), json!(2), "{engine:?}");
        assert_eq!(performed.load(Ordering::SeqCst), 2, "{engine:?}");
        assert_eq!(records(&j).last().unwrap()["j"], "end");
    }
}

struct Ends(Sender<(Vec<Json>, End)>, Vec<Json>);

impl Observer for Ends {
    fn event(&mut self, e: Observed) -> Result<(), String> {
        self.1.push(e.event);
        Ok(())
    }
    fn end(&mut self, end: End) {
        let _ = self.0.send((std::mem::take(&mut self.1), end));
    }
}

#[test]
fn a_run_cancelled_before_it_starts_stops_at_its_first_step_and_lets_its_values_go() {
    let prog = staalc::check_program(SPIN).unwrap();
    for engine in engines() {
        let cancel = Cancel::new();
        cancel.cancel();
        assert!(cancel.is_cancelled());
        let values: Arc<dyn Values> = Arc::new(Given::new([]));
        let weak = Arc::downgrade(&values);
        let (tx, rx) = channel();
        let attached = Attached {
            cancel: Some(cancel),
            values: Some(values),
            observation: Some(Observation { observer: Box::new(Ends(tx, Vec::new())), capacity: 64 }),
        };
        let j = tmp(&format!("before-{engine:?}.journal"));
        let r = run_config_attached(&prog, SPIN, &spin_config("main"), Durability::Create(&j), engine, attached).unwrap();
        assert_eq!(kind(&r), "cancelled", "{engine:?}");
        assert!(weak.upgrade().is_none(), "{engine:?}: the run's values outlived it");
        let (events, end) = rx.recv_timeout(Duration::from_secs(30)).unwrap();
        assert_eq!(end.delivered as usize, r.log.len());
        assert!(events.iter().any(|e| e["event"] == "run_end" && e["failure"] == "cancelled"), "{events:?}");
        assert!(records(&j).iter().all(|r| r["j"] != "end"));
    }
}
