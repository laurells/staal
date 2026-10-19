// Library-declared capabilities (RUNTIME.md section 13), run against a local server
// that answers like Termii and Stripe. The libraries are the ones in
// examples/capabilities/, so these tests also check what users will copy.

mod common;

use common::server::Server;
use staalc::interp::{run_config, FailKind, RunResult};
use serde_json::{json, Value as Json};
use std::path::{Path, PathBuf};

const KEY: &str = "sk_test_lib_9d8c7b6a5f4e";

fn init() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    // SAFETY: set once, before any test reads the environment (every test calls init first).
    ONCE.call_once(|| unsafe {
        std::env::set_var("STAAL_LIB_KEY", KEY);
    });
}

fn examples() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/capabilities")
}

fn load(path: &Path) -> staalc::ast::Program {
    let checked = staalc::check_path(path).unwrap();
    match checked.result {
        Ok(p) => p,
        Err(d) => {
            let msgs: Vec<String> = d.iter().map(|d| staalc::format_diag_in(&checked.files, d)).collect();
            panic!("{}", msgs.join("\n"))
        }
    }
}

/// A folder holding `main.staal` and copies of the example libraries.
fn program(name: &str, main: &str) -> staalc::ast::Program {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("library").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for lib in ["termii.staal", "stripe.staal"] {
        std::fs::copy(examples().join(lib), dir.join(lib)).unwrap();
    }
    std::fs::write(dir.join("main.staal"), main).unwrap();
    load(&dir.join("main.staal"))
}

fn run(prog: &staalc::ast::Program, cfg: &Json) -> RunResult {
    init();
    run_config(prog, cfg).unwrap_or_else(|e| panic!("config error: {e}"))
}

fn config_error(prog: &staalc::ast::Program, cfg: &Json) -> String {
    init();
    match run_config(prog, cfg) {
        Ok(r) => panic!("expected a config error, the run gave {:?}", r.result),
        Err(e) => e,
    }
}

fn patients(phones: &[&str]) -> Json {
    Json::Array(phones.iter().enumerate().map(|(i, p)| json!({ "name": format!("P{i}"), "phone": p })).collect())
}

fn termii_needs(s: &Server) -> Json {
    json!({
        "http": { "cap": "net.http.post", "only": [s.url("/api/sms/send")] },
        "key": { "env": "STAAL_LIB_KEY" },
        "base": s.url(""),
        "sender": "Clinic"
    })
}

fn remind_cfg(s: &Server, phones: &[&str], grant: Json) -> Json {
    let mut g = grant;
    g["cap"] = json!("termii.sms.send");
    if g.get("needs").is_none() {
        g["needs"] = termii_needs(s);
    }
    json!({
        "entry": "remind",
        "inputs": { "patients": patients(phones) },
        "grants": { "texts": g },
        "model": { "provider": "script", "responses": [] }
    })
}

fn remind() -> staalc::ast::Program {
    load(&examples().join("remind.staal"))
}

fn events<'a>(r: &'a RunResult, name: &str) -> Vec<&'a Json> {
    r.log.iter().filter(|e| e["event"] == name).collect()
}

#[test]
fn a_program_sends_through_the_library_without_holding_the_key() {
    let s = Server::start();
    let only = json!(["2348012345678", "2348098765432"]);
    let r = run(&remind(), &remind_cfg(&s, &["+234 801 234 5678", "2348098765432"], json!({ "only": only })));
    assert_eq!(r.result.as_ref().unwrap(), &json!(2), "{:?}", r.log);
    let reqs = s.requests();
    assert_eq!(reqs.len(), 2);
    let first: Json = serde_json::from_str(&reqs[0].body).unwrap();
    assert_eq!(first["to"], "2348012345678", "the library sends the canonical number");
    assert_eq!(first["api_key"], KEY, "the key reaches the service");
    assert_eq!(first["from"], "Clinic");
    // The receipts carry Termii's message ids, and nothing recorded holds the key.
    let performed: Vec<_> = events(&r, "action_performed").into_iter().filter(|e| e["capability"] == "termii.sms.send").collect();
    assert_eq!(performed.len(), 2);
    assert_eq!(performed[0]["receipt"], "t-1");
    assert_eq!(performed[0]["target"], "2348012345678");
    let log = serde_json::to_string(&r.log).unwrap();
    assert!(!log.contains(KEY), "the log contains the key");
}

#[test]
fn only_is_checked_against_the_canonical_target_before_the_library_runs() {
    let s = Server::start();
    let r = run(&remind(), &remind_cfg(&s, &["2348012345678", "2340000000000"], json!({ "only": ["2348012345678"] })));
    assert_eq!(r.result.as_ref().unwrap(), &json!(1));
    assert_eq!(s.requests().len(), 1, "the refused number never reached the library's request");
    let refused = events(&r, "action_refused");
    assert_eq!(refused.len(), 1);
    assert_eq!(refused[0]["capability"], "termii.sms.send");
}

#[test]
fn uses_limits_the_library_action_not_its_requests() {
    let s = Server::start();
    let r = run(&remind(), &remind_cfg(&s, &["2348012345678", "2348012345678", "2348012345678"], json!({ "uses": 2 })));
    assert_eq!(r.result.as_ref().unwrap(), &json!(2));
    assert_eq!(s.requests().len(), 2);
}

#[test]
fn the_operator_limits_what_the_library_can_reach() {
    // The library's request goes to a URL its HTTP grant does not cover: refused by
    // the runtime, so the library fails and the program's `try` sees it.
    let s = Server::start();
    let mut needs = termii_needs(&s);
    needs["http"]["only"] = json!([s.url("/somewhere-else")]);
    let r = run(&remind(), &remind_cfg(&s, &["2348012345678"], json!({ "needs": needs })));
    assert_eq!(r.result.as_ref().unwrap(), &json!(0));
    assert!(s.requests().is_empty());
    let refused = events(&r, "action_refused");
    assert_eq!(refused[0]["capability"], "net.http.post");
}

#[test]
fn the_librarys_declared_limits_narrow_the_operators_grant() {
    let s = Server::start();
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("library").join("narrow");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // The library asks for its HTTP capability with `uses: 1`; the operator grants 5.
    std::fs::write(
        dir.join("once.staal"),
        r#"capability once.ping {
    needs http: cap net.http.post(uses: 1)
    needs base: Text
    action send(text: Text) -> Text {
        let r = http.post(base + "/ok", text)
        return r.body
    }
}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.staal"),
        r#"import once
fn go(p: cap once.ping.send) -> Int {
    let a = try p.send("one")
    let b = try p.send("two")
    if a.ok and not b.ok {
        return 1
    }
    return 0
}
"#,
    )
    .unwrap();
    let prog = load(&dir.join("main.staal"));
    let cfg = json!({
        "entry": "go",
        "grants": { "p": { "cap": "once.ping.send", "needs": {
            "http": { "cap": "net.http.post", "uses": 5 }, "base": s.url("") } } },
        "model": { "provider": "script", "responses": [] }
    });
    let r = run(&prog, &cfg);
    assert_eq!(r.result.as_ref().unwrap(), &json!(1), "{:?}", r.log);
    assert_eq!(s.requests().len(), 1);
}

#[test]
fn an_action_that_makes_no_request_gets_no_receipt() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("library").join("norequest");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // A request is written, but only on a path this input never takes.
    std::fs::write(
        dir.join("fake.staal"),
        r#"capability fake.sms {
    needs http: cap net.http.post
    action send(text: Text) -> Text {
        if text == "really" {
            let r = http.post("http://127.0.0.1:9/x", text)
            return r.body
        }
        return "pretend-id"
    }
}
"#,
    )
    .unwrap();
    std::fs::write(dir.join("main.staal"), "import fake\nfn go(p: cap fake.sms.send) -> Receipt {\n    return p.send(\"hello\")\n}\n").unwrap();
    let prog = load(&dir.join("main.staal"));
    let cfg = json!({
        "entry": "go",
        "grants": { "p": { "cap": "fake.sms.send", "needs": { "http": { "cap": "net.http.post" } } } },
        "model": { "provider": "script", "responses": [] }
    });
    let f = run(&prog, &cfg).result.unwrap_err();
    assert_eq!(f.kind, FailKind::Action);
    assert!(f.message.contains("without making any request"), "{}", f.message);
}

const REFUND: &str = r#"import stripe
fn go(payment: Text, a: Money, b: Money, refunds: cap stripe.refunds.create) -> Int {
    let first = try refunds.create(payment, a)
    let second = try refunds.create(payment, b)
    let n = 0
    if first.ok {
        set n = n + 1
    }
    if second.ok {
        set n = n + 10
    }
    return n
}
"#;

fn money(s: &str) -> Json {
    let (a, c) = s.split_once(' ').unwrap();
    json!({ "amount": a, "currency": c })
}

fn refund_cfg(s: &Server, payment: &str, a: &str, b: &str, limits: Json) -> Json {
    let mut g = limits;
    g["cap"] = json!("stripe.refunds.create");
    g["needs"] = json!({
        "get": { "cap": "net.http.get", "only": [s.url("/v1/payment_intents/*")] },
        "post": { "cap": "net.http.post", "only": [s.url("/v1/refunds")], "idempotency_key": true },
        "key": { "env": "STAAL_LIB_KEY" },
        "base": s.url("")
    });
    json!({
        "entry": "go",
        "inputs": { "payment": payment, "a": money(a), "b": money(b) },
        "grants": { "refunds": g },
        "model": { "provider": "script", "responses": [] }
    })
}

#[test]
fn a_refund_goes_through_with_the_amount_in_minor_units() {
    let s = Server::start();
    let r = run(&program("refund-ok", REFUND), &refund_cfg(&s, " pi_usd ", "25.00 USD", "0.50 USD", json!({})));
    assert_eq!(r.result.as_ref().unwrap(), &json!(11), "{:?}", r.log);
    let posts: Vec<_> = s.requests().into_iter().filter(|q| q.path == "/v1/refunds").collect();
    assert_eq!(posts.len(), 2);
    assert_eq!(posts[0].body, "payment_intent=pi_usd&amount=2500");
    assert_eq!(posts[1].body, "payment_intent=pi_usd&amount=50");
    assert_eq!(posts[0].headers["authorization"], format!("Bearer {KEY}"));
    assert_eq!(posts[0].headers["content-type"], "application/x-www-form-urlencoded");
    assert!(posts[0].headers.contains_key("idempotency-key"));
    assert_ne!(posts[0].headers["idempotency-key"], posts[1].headers["idempotency-key"]);
    let performed: Vec<_> =
        events(&r, "action_performed").into_iter().filter(|e| e["capability"] == "stripe.refunds.create").collect();
    assert_eq!(performed[0]["target"], "pi_usd", "the target in canonical form");
    assert_eq!(performed[0]["amount"], "25.00 USD");
    assert!(performed[0]["receipt"].as_str().unwrap().starts_with("re-"));
}

#[test]
fn money_limits_apply_to_the_librarys_amount_before_anything_is_sent() {
    let s = Server::start();
    // `each: 100 USD`: the first refund is over it, the second within.
    let r = run(&program("refund-each", REFUND), &refund_cfg(&s, "pi_usd", "150.00 USD", "40.00 USD", json!({ "each": money("100.00 USD") })));
    assert_eq!(r.result.as_ref().unwrap(), &json!(10));
    assert_eq!(s.requests().iter().filter(|q| q.path == "/v1/refunds").count(), 1);
    // `max: 50 USD`: 40 then 20 would be 60.
    let s = Server::start();
    let r = run(&program("refund-max", REFUND), &refund_cfg(&s, "pi_usd", "40.00 USD", "20.00 USD", json!({ "max": money("50.00 USD") })));
    assert_eq!(r.result.as_ref().unwrap(), &json!(1));
    assert_eq!(s.requests().iter().filter(|q| q.path == "/v1/refunds").count(), 1);
}

#[test]
fn a_refund_in_the_wrong_currency_is_refused_by_the_library() {
    let s = Server::start();
    // The payment is in EUR. Only the GET is made; it cannot spend anything, so the
    // failed refund is not counted against `max`, and the second (also EUR) fails too.
    let r = run(&program("refund-currency", REFUND), &refund_cfg(&s, "pi_eur", "10.00 USD", "10.00 USD", json!({ "max": money("15.00 USD") })));
    assert_eq!(r.result.as_ref().unwrap(), &json!(0));
    assert_eq!(s.requests().iter().filter(|q| q.path == "/v1/refunds").count(), 0);
    let failed: Vec<_> =
        events(&r, "action_failed").into_iter().filter(|e| e["capability"] == "stripe.refunds.create").collect();
    assert_eq!(failed.len(), 2, "both refunds reached the library: the failure did not use up `max`");
    assert!(failed[0]["error"].as_str().unwrap().contains("the payment is in eur"));
    assert_eq!(failed[0]["may_have_spent"], false);
}

#[test]
fn a_failure_after_a_request_that_may_have_spent_counts_against_the_budget() {
    // This library POSTs, then fails. The refund may have happened, so its amount is
    // counted: with `max: 15 USD`, a second 10 USD refund is refused.
    let s = Server::start();
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("library").join("spent");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("flaky.staal"),
        r#"capability flaky.refunds {
    needs post: cap net.http.post
    needs base: Text
    action create(amount: Money @amount) -> Text {
        let r = post.post(base + "/ok", json(minor_units(amount)))
        fail("the answer could not be understood: " + r.body)
    }
}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.staal"),
        r#"import flaky
fn go(r: cap flaky.refunds.create(max: 15 USD)) -> Int {
    let a = try r.create(10.00 USD)
    let b = try r.create(10.00 USD)
    if a.ok or b.ok {
        return 99
    }
    return 0
}
"#,
    )
    .unwrap();
    let prog = load(&dir.join("main.staal"));
    let cfg = json!({
        "entry": "go",
        "grants": { "r": { "cap": "flaky.refunds.create", "needs": {
            "post": { "cap": "net.http.post" }, "base": s.url("") } } },
        "model": { "provider": "script", "responses": [] }
    });
    let r = run(&prog, &cfg);
    assert_eq!(r.result.as_ref().unwrap(), &json!(0));
    assert_eq!(s.requests().len(), 1, "the second refund was refused before the library ran");
    let failed = events(&r, "action_failed").into_iter().find(|e| e["capability"] == "flaky.refunds.create").unwrap();
    assert_eq!(failed["may_have_spent"], true);
    let refused = events(&r, "action_refused");
    assert_eq!(refused.len(), 1);
    assert!(refused[0]["reason"].as_str().unwrap().contains("5.00 USD left of its 15.00 USD limit"), "{}", refused[0]["reason"]);
}

#[test]
fn needs_are_checked_when_the_run_starts() {
    let s = Server::start();
    let prog = remind();
    let base = |g: Json| remind_cfg(&s, &["2348012345678"], g);
    let mut missing = termii_needs(&s);
    missing.as_object_mut().unwrap().remove("sender");
    assert!(config_error(&prog, &base(json!({ "needs": missing }))).contains("needs.sender: missing"));
    let mut extra = termii_needs(&s);
    extra["proxy"] = json!("x");
    assert!(config_error(&prog, &base(json!({ "needs": extra }))).contains("needs no 'proxy'"));
    let mut wrong = termii_needs(&s);
    wrong["http"]["cap"] = json!("net.http.get");
    assert!(config_error(&prog, &base(json!({ "needs": wrong }))).contains("needs.http.cap"));
    let mut text_as_secret = termii_needs(&s);
    text_as_secret["key"] = json!(KEY);
    assert!(config_error(&prog, &base(json!({ "needs": text_as_secret }))).contains("needs.key"));
    let mut bad_pattern = termii_needs(&s);
    bad_pattern["http"]["only"] = json!(["http://example.com/x"]);
    assert!(config_error(&prog, &base(json!({ "needs": bad_pattern }))).contains("needs.http.only[0]"));
    // Without `needs` at all.
    let mut cfg = base(json!({}));
    cfg["grants"]["texts"].as_object_mut().unwrap().remove("needs");
    assert!(config_error(&prog, &cfg).contains("needs: missing"));
    // `needs` on a capability that is not a library's.
    let plain = staalc::check_program("fn f(h: cap net.http.get) -> Int {\n    return 1\n}").unwrap();
    let cfg = json!({ "entry": "f", "grants": { "h": { "cap": "net.http.get", "needs": {} } },
                      "model": { "provider": "script", "responses": [] } });
    assert!(config_error(&plain, &cfg).contains("not a library capability"));
}

#[test]
fn a_run_cannot_start_inside_a_capability() {
    let s = Server::start();
    let mut cfg = remind_cfg(&s, &["2348012345678"], json!({}));
    cfg["entry"] = json!("action termii.sms.send");
    assert!(config_error(&remind(), &cfg).contains("part of a capability"));
}

#[test]
fn data_placed_in_a_url_stays_one_segment() {
    // The payment id tries to climb out of /v1/payment_intents/ to /v1/refunds.
    // `url_segment` encodes the slashes, and an encoded slash is never sent.
    let s = Server::start();
    for id in ["pi_x/../../refunds", "pi_x?expand=all", "pi_x#frag"] {
        let r = run(&program("segment", REFUND), &refund_cfg(&s, id, "1.00 USD", "1.00 USD", json!({})));
        assert_eq!(r.result.as_ref().unwrap(), &json!(0), "{id}");
    }
    for q in s.requests() {
        assert!(q.path.starts_with("/v1/payment_intents/pi_x%3F") || q.path.starts_with("/v1/payment_intents/pi_x%23"), "{}", q.path);
        assert_eq!(q.method, "GET");
    }
    // ".." itself is refused by url_segment, before any request.
    let s = Server::start();
    let src = "fn f(t: Text) -> Text {\n    return url_segment(t)\n}\n";
    let prog = staalc::check_program(src).unwrap();
    for (t, ok) in [("..", false), (".", false), ("", false), ("a b/c", true), ("...", true)] {
        let cfg = json!({ "entry": "f", "inputs": { "t": t }, "model": { "provider": "script", "responses": [] } });
        let r = run(&prog, &cfg);
        assert_eq!(r.result.is_ok(), ok, "{t:?}: {:?}", r.result);
        if t == "a b/c" {
            assert_eq!(r.result.unwrap(), json!("a%20b%2Fc"));
        }
    }
    assert!(s.requests().is_empty());
}
