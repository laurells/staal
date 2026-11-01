// `limits.deadline_ms` (RUNTIME.md section 4), and what a run reports each grant used
// (`RunResult::usage`, RUNTIME.md section 6). A deadline is checked before each action
// or model call starts, never during one, cannot be caught, and a resumed run decides
// as the journal says it did.

mod common;

use common::server::Server;
use serde_json::{json, Value as Json};
use staalc::interp::{run_config_engine, Durability, Engine, FailKind};

fn engines() -> Vec<Engine> {
    if cfg!(feature = "native") { vec![Engine::Interpreter, Engine::Native] } else { vec![Engine::Interpreter] }
}

fn scratch(name: &str) -> std::path::PathBuf {
    let d = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("deadline").join(name);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A slow request, then another the deadline should stop. The second is under `try`,
/// which must not catch a deadline.
fn two_gets(s: &Server) -> String {
    format!(
        r#"fn f(http: cap net.http.get) -> Int {{
    let a = http.get("{}")
    let b = try http.get("{}")
    return 1
}}
"#,
        s.url("/slow"),
        s.url("/fixed")
    )
}

fn cfg(deadline_ms: u64) -> Json {
    json!({ "entry": "f", "grants": { "http": { "cap": "net.http.get" } },
            "limits": { "deadline_ms": deadline_ms }, "net": { "timeout_ms": 5000 },
            "model": { "provider": "script", "responses": [] } })
}

#[test]
fn a_run_past_its_deadline_stops_before_its_next_action_and_never_during_one() {
    for engine in engines() {
        let s = Server::start();
        let src = two_gets(&s);
        let prog = staalc::check_program(&src).unwrap();
        let out = run_config_engine(&prog, &src, &cfg(150), Durability::None, engine).unwrap();
        let f = out.result.unwrap_err();
        assert_eq!(f.kind, FailKind::Deadline, "{engine:?}: {}", f.message);
        assert_eq!(f.message, format!("the run passed its deadline of 150 ms; it stopped before starting the action net.http.get {}", s.url("/fixed")));
        // The slow request started inside the deadline and was allowed to finish; the
        // next never started.
        let paths: Vec<String> = s.requests().iter().map(|r| r.path.clone()).collect();
        assert_eq!(paths, ["/slow"], "{engine:?}");
        assert!(out.log.iter().any(|e| e["event"] == "deadline" && e["deadline_ms"] == 150), "{engine:?}");
        // Inside the deadline, the same run finishes.
        let s2 = Server::start();
        let src2 = two_gets(&s2);
        let prog2 = staalc::check_program(&src2).unwrap();
        let ok = run_config_engine(&prog2, &src2, &cfg(10_000), Durability::None, engine).unwrap();
        assert_eq!(ok.result.unwrap(), json!(1), "{engine:?}");
        assert_eq!(s2.requests().len(), 2);
    }
}

#[test]
fn a_resumed_run_ends_at_its_deadline_as_its_journal_says_and_sends_nothing_more() {
    for engine in engines() {
        let s = Server::start();
        let src = two_gets(&s);
        let prog = staalc::check_program(&src).unwrap();
        let journal = scratch(&format!("resume-{engine:?}")).join("run.journal");
        let first = run_config_engine(&prog, &src, &cfg(150), Durability::Create(&journal), engine).unwrap();
        let failure = first.result.unwrap_err();
        assert_eq!(failure.kind, FailKind::Deadline);
        // Resumed at once, inside the deadline by the clock: the journal decides.
        let again = run_config_engine(&prog, &src, &cfg(150), Durability::Resume(&journal), engine).unwrap();
        assert_eq!(again.result.unwrap_err(), failure, "{engine:?}");
        assert_eq!(s.requests().len(), 1, "{engine:?}: resume must not send the second request");
    }
}

#[test]
fn a_deadline_must_be_a_positive_whole_number_of_milliseconds() {
    let src = "fn f() -> Int { return 1 }";
    let prog = staalc::check_program(src).unwrap();
    for bad in [json!(0), json!(-5), json!(1.5), json!("100ms")] {
        let c = json!({ "entry": "f", "limits": { "deadline_ms": bad }, "model": { "provider": "script", "responses": [] } });
        let e = run_config_engine(&prog, src, &c, Durability::None, Engine::Interpreter).map(|_| ()).unwrap_err();
        assert!(e.contains("limits.deadline_ms: expected a positive whole number"), "{bad}: {e}");
    }
}

#[test]
fn usage_reports_what_each_grant_attempted_and_charged() {
    let src = r#"fn f(pay: cap payments.refund(each: 50 USD), texts: cap sms.send) -> Int {
    let a = pay.refund("order-1", 30.00 USD)
    let refused = try pay.refund("order-2", 60.00 USD)
    let b = pay.refund("order-3", 20.00 USD)
    return 1
}
"#;
    let prog = staalc::check_program(src).unwrap();
    let c = json!({ "entry": "f",
                    "grants": { "pay": { "cap": "payments.refund", "uses": 3 }, "texts": { "cap": "sms.send" } },
                    "model": { "provider": "script", "responses": [] } });
    for engine in engines() {
        let out = run_config_engine(&prog, src, &c, Durability::None, engine).unwrap();
        assert_eq!(out.result.unwrap(), json!(1));
        // The refusal by `each` was never attempted, so it is not a use.
        assert_eq!(
            out.usage,
            json!({ "pay": { "capability": "payments.refund", "uses": 2, "charged": [{ "amount": "50.00", "currency": "USD" }] },
                    "texts": { "capability": "sms.send", "uses": 0, "charged": [] } }),
            "{engine:?}"
        );
    }
}

#[test]
fn usage_counts_the_whole_run_when_it_is_resumed() {
    let src = r#"fn f(pay: cap payments.refund) -> Int {
    let a = pay.refund("order-1", 30.00 USD)
    let b = pay.refund("order-2", 5.00 USD)
    return 1
}
"#;
    let prog = staalc::check_program(src).unwrap();
    let c = json!({ "entry": "f", "grants": { "pay": { "cap": "payments.refund" } },
                    "model": { "provider": "script", "responses": [] } });
    let journal = scratch("usage-resume").join("run.journal");
    let first = run_config_engine(&prog, src, &c, Durability::Create(&journal), Engine::Interpreter).unwrap();
    let again = run_config_engine(&prog, src, &c, Durability::Resume(&journal), Engine::Interpreter).unwrap();
    let want = json!({ "pay": { "capability": "payments.refund", "uses": 2, "charged": [{ "amount": "35.00", "currency": "USD" }] } });
    assert_eq!(first.usage, want);
    assert_eq!(again.usage, want, "the replayed actions count");
}

#[test]
fn concurrent_tasks_count_from_when_their_run_began() {
    for engine in engines() {
        let s = Server::start();
        let src = format!(
            r#"fn fetch(path: Text, http: cap net.http.get) -> Int {{
    let r = http.get("{base}" + path)
    return r.status
}}

fn f(http: cap net.http.get) -> Int {{
    let a = http.get("{slow}")
    let codes = parallel(["/fixed", "/fixed"], fetch, http)
    return 1
}}
"#,
            base = s.url(""),
            slow = s.url("/slow")
        );
        let prog = staalc::check_program(&src).unwrap();
        let out = run_config_engine(&prog, &src, &cfg(150), Durability::None, engine).unwrap();
        assert_eq!(out.result.unwrap_err().kind, FailKind::Deadline, "{engine:?}");
        let paths: Vec<String> = s.requests().iter().map(|r| r.path.clone()).collect();
        assert_eq!(paths, ["/slow"], "{engine:?}: no task may start an action past the deadline");
    }
}
