// net.http and fs at runtime, against a local HTTP server (tests/common/server.rs)
// and real files. Every test checks what reached the server, not only what the
// program saw.

mod common;

use common::server::Server;
use intentc::interp::{run_config, run_config_durable, Durability, FailKind, RunResult};
use serde_json::{json, Value as Json};
use std::sync::Once;

const KEY: &str = "sk_test_4f9a8b7c6d5e3f21";

fn init() {
    static ONCE: Once = Once::new();
    // SAFETY: set once, before any test reads the environment (every test calls init first).
    ONCE.call_once(|| unsafe {
        std::env::set_var("INTENT_TEST_KEY", KEY);
        std::env::set_var("INTENT_TEST_SHORT", "abc");
        std::env::set_var("INTENT_TEST_SPACE", "has a space in it");
    });
}

fn prog(src: &str) -> intentc::ast::Program {
    intentc::check_program(src).unwrap_or_else(|d| panic!("check errors: {d:?}\n{src}"))
}

fn run(src: &str, cfg: Json) -> RunResult {
    init();
    run_config(&prog(src), &cfg).unwrap_or_else(|e| panic!("config error: {e}"))
}

fn base(entry: &str) -> Json {
    json!({ "entry": entry, "model": { "provider": "script", "responses": [] } })
}

fn with(mut cfg: Json, key: &str, v: Json) -> Json {
    cfg[key] = v;
    cfg
}

fn all_text(out: &RunResult) -> String {
    format!("{:?} {}", out.result, Json::Array(out.log.clone()))
}

#[test]
fn a_secret_reaches_only_the_header_and_never_the_log() {
    let s = Server::start();
    let src = format!(
        r#"fn f(key: Secret, http: cap net.http.post(only: ["{}"])) -> Response {{
    return http.post("{}", "hello", [header("Authorization", fill_secret("Bearer {{secret}}", key))])
}}"#,
        s.url("/echo-auth"),
        s.url("/echo-auth")
    );
    let cfg = json!({ "entry": "f", "grants": { "http": { "cap": "net.http.post" } },
                      "secrets": { "key": { "env": "INTENT_TEST_KEY" } },
                      "model": { "provider": "script", "responses": [] } });
    let out = run(&src, cfg);
    let reqs = s.requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].headers["authorization"], format!("Bearer {KEY}"), "the server got the real key");
    assert_eq!(reqs[0].body, "hello");
    // The server echoed the key back; the program and the log see it removed.
    let r = out.result.as_ref().unwrap();
    assert_eq!(r["body"], "Bearer [secret removed]");
    assert!(!all_text(&out).contains(KEY), "the key appears in the result or log");
}

#[test]
fn a_secret_in_a_json_body_and_the_journal_never_holds_it() {
    let s = Server::start();
    let dir = std::env::temp_dir().join(format!("intentc-http-journal-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let journal = dir.join("run.journal");
    let src = format!(
        r#"type Sms {{ to: Text, sms: Text, api_key: Text }}
fn f(key: Secret, http: cap net.http.post(only: ["{0}"])) -> Int {{
    let body = fill_secret(json(Sms {{ to: "+2348000000000", sms: "hi", api_key: "{{secret}}" }}), key)
    let r = http.post("{0}", body)
    return r.status
}}"#,
        s.url("/ok")
    );
    init();
    let cfg = json!({ "entry": "f", "grants": { "http": { "cap": "net.http.post" } },
                      "secrets": { "key": { "env": "INTENT_TEST_KEY" } },
                      "model": { "provider": "script", "responses": [] } });
    let p = prog(&src);
    let out = run_config_durable(&p, &src, &cfg, Durability::Create(&journal)).unwrap();
    assert_eq!(out.result.unwrap(), json!(200));
    let sent: Json = serde_json::from_str(&s.requests()[0].body).unwrap();
    assert_eq!(sent, json!({ "to": "+2348000000000", "sms": "hi", "api_key": KEY }));
    let text = std::fs::read_to_string(&journal).unwrap();
    assert!(!text.contains(KEY), "the journal holds the key");
    assert!(text.contains("{secret INTENT_TEST_KEY}"), "the journal records the template: {text}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&journal).unwrap().permissions().mode() & 0o777, 0o600);
    }
    // Resuming the finished run replays it without contacting the server.
    let again = run_config_durable(&p, &src, &cfg, Durability::Resume(&journal)).unwrap();
    assert_eq!(again.result.unwrap(), json!(200));
    assert_eq!(s.requests().len(), 1);
}

#[test]
fn only_and_traversal_are_refused_before_anything_is_sent() {
    let s = Server::start();
    let src = format!(
        r#"fn f(url: Text, http: cap net.http.get(only: ["{}"])) -> Int {{
    let r = http.get(url)
    return r.status
}}"#,
        s.url("/api/*")
    );
    for (url, why) in [
        (s.url("/admin"), "may only act on"),
        (s.url("/api/../admin"), "may only act on"),
        (s.url("/api/%2e%2e/admin"), "may only act on"),
        (s.url("/api/..%2Fadmin"), "encoded"),
        ("http://example.com/api/x".to_string(), "only https"),
        (format!("http://user:pw@127.0.0.1:{}/api/x", s.port), "user name"),
    ] {
        let out = run(&src, with(with(base("f"), "inputs", json!({ "url": url })), "grants", json!({ "http": { "cap": "net.http.get" } })));
        let f = out.result.unwrap_err();
        assert_eq!(f.kind, FailKind::Capability, "{url}: {}", f.message);
        assert!(f.message.contains(why), "{url}: {}", f.message);
    }
    assert!(s.requests().is_empty(), "a refused request reached the server: {:?}", s.requests());
    // The same URL written differently is the same target, and allowed.
    let out = run(&src, with(with(base("f"), "inputs", json!({ "url": s.url("/api/x/../ok") })), "grants", json!({ "http": { "cap": "net.http.get" } })));
    assert_eq!(out.result.unwrap(), json!(404)); // /api/ok is not a route; it was sent
    assert_eq!(s.requests()[0].path, "/api/ok");
}

#[test]
fn responses_are_data_whatever_their_status() {
    let s = Server::start();
    let src = format!(
        r#"fn f(path: Text, http: cap net.http.get(only: ["{}"])) -> Response {{
    return http.get("{}" + path)
}}"#,
        s.url("/*"),
        s.url("")
    );
    let get = |path: &str| {
        run(&src, with(with(base("f"), "inputs", json!({ "path": path })), "grants", json!({ "http": { "cap": "net.http.get" } })))
            .result
            .unwrap()
    };
    assert_eq!(get("/error"), json!({ "status": 500, "body": "boom", "complete": true }));
    // A redirect is returned, never followed: one request only.
    assert_eq!(get("/redirect")["status"], 302);
    assert_eq!(s.requests().iter().filter(|r| r.path == "/ok").count(), 0);
    assert_eq!(get("/binary"), json!({ "status": 200, "body": "", "complete": false }));
}

#[test]
fn a_body_over_the_limit_is_marked_incomplete() {
    let s = Server::start();
    let src = format!(
        r#"fn f(http: cap net.http.get(only: ["{0}"])) -> Response {{
    return http.get("{0}")
}}"#,
        s.url("/big")
    );
    let cfg = with(with(base("f"), "grants", json!({ "http": { "cap": "net.http.get" } })), "net", json!({ "max_response_bytes": 1000 }));
    assert_eq!(run(&src, cfg).result.unwrap(), json!({ "status": 200, "body": "", "complete": false }));
    let cfg = with(with(base("f"), "grants", json!({ "http": { "cap": "net.http.get" } })), "net", json!({ "max_response_bytes": 5000 }));
    assert_eq!(run(&src, cfg).result.unwrap()["body"].as_str().unwrap().len(), 4096);
}

#[test]
fn uses_limits_count_requests() {
    let s = Server::start();
    let src = format!(
        r#"fn f(http: cap net.http.get(only: ["{0}"], uses: 2)) -> Int {{
    let a = http.get("{0}")
    let b = http.get("{0}")
    let c = http.get("{0}")
    return c.status
}}"#,
        s.url("/ok")
    );
    let f = run(&src, with(base("f"), "grants", json!({ "http": { "cap": "net.http.get" } }))).result.unwrap_err();
    assert_eq!(f.kind, FailKind::Capability);
    assert_eq!(s.requests().len(), 2);
}

#[test]
fn nothing_listening_is_an_ordinary_failure() {
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let src = format!(
        r#"fn f(http: cap net.http.post) -> Bool {{
    let r = try http.post("http://127.0.0.1:{port}/x", "x")
    return r.ok
}}"#
    );
    let out = run(&src, with(base("f"), "grants", json!({ "http": { "cap": "net.http.post" } })));
    assert_eq!(out.result.unwrap(), json!(false), "not sent: caught by try");
}

#[test]
fn a_post_without_an_answer_stops_the_run_in_doubt() {
    let s = Server::start();
    let dir = std::env::temp_dir().join(format!("intentc-http-doubt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let journal = dir.join("run.journal");
    let src = format!(
        r#"fn f(http: cap net.http.post) -> Int {{
    let r = try http.post("{}", "pay")
    if r.ok {{
        return 1
    }}
    return 0
}}"#,
        s.url("/hang")
    );
    init();
    let cfg = with(with(base("f"), "grants", json!({ "http": { "cap": "net.http.post" } })), "net", json!({ "timeout_ms": 300 }));
    let p = prog(&src);
    let out = run_config_durable(&p, &src, &cfg, Durability::Create(&journal)).unwrap();
    // Not a failure `try` may catch: the request may have been acted on.
    let f = out.result.unwrap_err();
    assert_eq!(f.kind, FailKind::InDoubt, "{}", f.message);
    assert_eq!(s.requests().len(), 1);
    // Resuming does not send it again.
    let f = run_config_durable(&p, &src, &cfg, Durability::Resume(&journal)).unwrap().result.unwrap_err();
    assert_eq!(f.kind, FailKind::InDoubt);
    assert_eq!(s.requests().len(), 1, "resume must not repeat the request");
    // A receipt is the wrong answer for an HTTP request; the operator records the response.
    use intentc::interp::journal::{Journal, Resolution};
    assert!(Journal::resolve(&journal, Resolution::Performed { receipt: "x".into() }).unwrap_err().contains("--status"));
    Journal::resolve(&journal, Resolution::Responded { status: 200, body: "{}".into() }).unwrap();
    let out = run_config_durable(&p, &src, &cfg, Durability::Resume(&journal)).unwrap();
    assert_eq!(out.result.unwrap(), json!(1));
    assert_eq!(s.requests().len(), 1);
}

#[test]
fn a_get_without_an_answer_is_safe_to_report_as_failed() {
    let s = Server::start();
    let src = format!(
        r#"fn f(http: cap net.http.get) -> Bool {{
    let r = try http.get("{}")
    return r.ok
}}"#,
        s.url("/hang")
    );
    let cfg = with(with(base("f"), "grants", json!({ "http": { "cap": "net.http.get" } })), "net", json!({ "timeout_ms": 300 }));
    assert_eq!(run(&src, cfg).result.unwrap(), json!(false));
}

#[test]
fn an_idempotency_key_is_sent_only_when_the_operator_says_so() {
    let s = Server::start();
    let src = format!(
        r#"fn f(http: cap net.http.post) -> Int {{
    let r = http.post("{}", "x")
    return r.status
}}"#,
        s.url("/send")
    );
    run(&src, with(base("f"), "grants", json!({ "http": { "cap": "net.http.post" } })));
    run(&src, with(base("f"), "grants", json!({ "http": { "cap": "net.http.post", "idempotency_key": true } })));
    let reqs = s.requests();
    assert!(!reqs[0].headers.contains_key("idempotency-key"));
    let key = &reqs[1].headers["idempotency-key"];
    assert!(key.ends_with("-1") && key.len() > 30, "key {key}: <run id>-<operation>");
    // And then a POST with no answer is retryable, so it is reported, not left in doubt.
    let hang = format!(
        r#"fn f(http: cap net.http.post) -> Bool {{
    let r = try http.post("{}", "x")
    return r.ok
}}"#,
        s.url("/hang")
    );
    let cfg = with(with(base("f"), "grants", json!({ "http": { "cap": "net.http.post", "idempotency_key": true } })), "net", json!({ "timeout_ms": 300 }));
    assert_eq!(run(&hang, cfg).result.unwrap(), json!(false));
}

#[test]
fn headers_cannot_smuggle_or_override() {
    let s = Server::start();
    // Header names and values often come from input data: that is where an attack would be.
    let src = format!(
        r#"fn f(name: Text, value: Text, http: cap net.http.get) -> Int {{
    let r = http.get("{}", [header(name, value)])
    return r.status
}}"#,
        s.url("/ok")
    );
    for (name, value, why) in [
        ("X-A", "a\r\nX-Evil: 1", "control character"),
        ("X-A\r\nX-Evil", "1", "not a valid header name"),
        ("Host", "evil.com", "set by the runtime"),
        ("Content-Length", "0", "set by the runtime"),
        ("Idempotency-Key", "mine", "set by the runtime"),
    ] {
        let cfg = with(with(base("f"), "inputs", json!({ "name": name, "value": value })), "grants", json!({ "http": { "cap": "net.http.get" } }));
        let f = run(&src, cfg).result.unwrap_err();
        assert_eq!(f.kind, FailKind::Value, "{name}");
        assert!(f.message.contains(why), "{name}: {}", f.message);
    }
    assert!(s.requests().is_empty(), "a bad header reached the server");
}

#[test]
fn parsed_responses_must_be_validated_and_bad_json_fails() {
    let s = Server::start();
    let src = format!(
        r#"type Reply {{ message_id: Text }}
fn f(path: Text, http: cap net.http.get(only: ["{}"])) -> Text {{
    let r = http.get("{}" + path)
    let reply: Reply = validate parse(Reply, r.body)
    return reply.message_id
}}"#,
        s.url("/*"),
        s.url("")
    );
    let go = |path: &str| run(&src, with(with(base("f"), "inputs", json!({ "path": path })), "grants", json!({ "http": { "cap": "net.http.get" } }))).result;
    assert_eq!(go("/ok").unwrap(), json!("m-1"));
    let f = go("/error").unwrap_err();
    assert_eq!(f.kind, FailKind::Validation, "{}", f.message);
    assert!(f.message.contains("not JSON"), "{}", f.message);
}

#[test]
fn secrets_come_only_from_the_environment() {
    init();
    let src = "fn f(key: Secret) -> Int {\n    return 1\n}";
    let p = prog(src);
    let err = |cfg: Json| match run_config(&p, &cfg) {
        Ok(_) => panic!("expected a config error"),
        Err(e) => e,
    };
    assert!(err(base("f")).contains("missing"));
    assert!(err(with(base("f"), "inputs", json!({ "key": "inline" }))).contains("never written into a run config"));
    assert!(err(with(base("f"), "secrets", json!({ "key": { "env": "INTENT_TEST_UNSET_9" } }))).contains("is not set"));
    assert!(err(with(base("f"), "secrets", json!({ "key": { "env": "INTENT_TEST_SHORT" } }))).contains("shorter"));
    assert!(err(with(base("f"), "secrets", json!({ "key": { "env": "INTENT_TEST_SPACE" } }))).contains("space"));
    assert!(err(with(base("f"), "secrets", json!({ "key": { "value": "x" } }))).contains("only from the environment"));
    assert!(run_config(&p, &with(base("f"), "secrets", json!({ "key": { "env": "INTENT_TEST_KEY" } }))).is_ok());
}

#[test]
fn grants_for_runtime_capabilities_are_checked() {
    init();
    let src = "fn f(http: cap net.http.post) -> Int {\n    return 1\n}";
    let p = prog(src);
    let err = |g: Json| match run_config(&p, &with(base("f"), "grants", json!({ "http": g }))) {
        Ok(_) => panic!("expected a config error"),
        Err(e) => e,
    };
    assert!(err(json!({ "cap": "net.http.post", "only": ["https://API.x.org/*"] })).contains("canonical"));
    assert!(err(json!({ "cap": "net.http.post", "only": ["https://x.org*"] })).contains("host"));
    assert!(err(json!({ "cap": "net.http.post", "max": { "amount": "1.00", "currency": "USD" } })).contains("no money"));
    let get = prog("fn f(http: cap net.http.get) -> Int {\n    return 1\n}");
    let e = run_config(&get, &with(base("f"), "grants", json!({ "http": { "cap": "net.http.get", "idempotency_key": true } }))).err().unwrap();
    assert!(e.contains("only for net.http.post"), "{e}");
}

#[cfg(unix)]
#[test]
fn files_are_read_and_written_inside_only() {
    let d = std::fs::canonicalize(std::env::temp_dir()).unwrap().join(format!("intentc-http-fs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("in")).unwrap();
    std::fs::create_dir_all(d.join("out")).unwrap();
    std::fs::create_dir_all(d.join("private")).unwrap();
    std::fs::write(d.join("in/day.csv"), "a,b\n1,2\n").unwrap();
    std::fs::write(d.join("private/key"), "TOP SECRET").unwrap();
    std::os::unix::fs::symlink(d.join("private"), d.join("in/link")).unwrap();
    let root = d.to_str().unwrap();
    let src = format!(
        r#"fn f(name: Text, rd: cap fs.read(only: ["{root}/in/*"]), wr: cap fs.write(only: ["{root}/out/*"])) -> Done {{
    let text = rd.read("{root}/in/" + name)
    let w = wr.write("{root}/out/copy.txt", text)
    return done(evidence: [w])
}}"#
    );
    let cfg = |name: &str| {
        with(with(base("f"), "inputs", json!({ "name": name })), "grants", json!({ "rd": { "cap": "fs.read" }, "wr": { "cap": "fs.write" } }))
    };
    let out = run(&src, cfg("day.csv"));
    assert!(out.result.is_ok(), "{:?}", out.result);
    assert_eq!(std::fs::read_to_string(d.join("out/copy.txt")).unwrap(), "a,b\n1,2\n");
    for (name, why) in [("../private/key", "may only act on"), ("link/key", "symbolic link"), ("missing", "does not exist")] {
        let f = run(&src, cfg(name)).result.unwrap_err();
        assert!(f.message.contains(why), "{name}: {}", f.message);
    }
}

#[test]
fn text_from_outside_cannot_ask_for_the_key() {
    // The attack: the message text is "{secret}", so a naive fill would put the API key
    // into the SMS and send it to the attacker's number.
    let s = Server::start();
    let src = format!(
        r#"type Sms {{ to: Text, sms: Text, api_key: Text }}
fn f(to: Text, text: Text, key: Secret, http: cap net.http.post(only: ["{0}"])) -> Int {{
    let body = fill_secret(json(Sms {{ to: to, sms: text, api_key: "{{secret}}" }}), key)
    let r = http.post("{0}", body)
    return r.status
}}"#,
        s.url("/ok")
    );
    let cfg = |text: &str| {
        json!({ "entry": "f", "inputs": { "to": "+2348000000000", "text": text },
                "grants": { "http": { "cap": "net.http.post" } },
                "secrets": { "key": { "env": "INTENT_TEST_KEY" } },
                "model": { "provider": "script", "responses": [] } })
    };
    let f = run(&src, cfg("Your code is {secret}")).result.unwrap_err();
    assert_eq!(f.kind, FailKind::Value);
    assert!(f.message.contains("exactly once"), "{}", f.message);
    assert!(s.requests().is_empty(), "the request was sent");
    // An ordinary message goes through.
    assert_eq!(run(&src, cfg("Your code is 4821")).result.unwrap(), json!(200));
    let sent: Json = serde_json::from_str(&s.requests()[0].body).unwrap();
    assert_eq!(sent, json!({ "to": "+2348000000000", "sms": "Your code is 4821", "api_key": KEY }));
}
