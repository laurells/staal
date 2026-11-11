// `staalc mcp` killed at every point a call makes something durable (docs/MCP.md
// sections 7 and 9): every journal write, every ledger write, between an action and
// its result, and between a waiting record and the answer. After each crash the server
// is started again and the client sends the same call again, as a client that saw the
// connection close may. Then:
//   - with `once`, the refund reached Stripe exactly once, the client gets the result,
//     and the ledger holds one settled run that charged exactly the refund;
//   - without `once`, a call sent again after the refund is a new run that refunds
//     again, as the design says it can;
//   - a stopped run's waiting record survives a crash before the answer.
//
// Run with: cargo test --features failpoints --test mcp_crash_test

#![cfg(all(unix, feature = "failpoints"))]

mod common;

use common::mcp::{case_dir, write_manifest, Mcp};
use common::server::Server;
use serde_json::{json, Value as Json};
use std::path::{Path, PathBuf};

const KEY: &str = "sk_test_mcp_crash_9a8b7c6d5e";

struct Case {
    dir: PathBuf,
    manifest: PathBuf,
    server: Server,
}

fn usd(a: &str) -> Json {
    json!({ "amount": a, "currency": "USD" })
}

impl Case {
    fn new(name: &str) -> Case {
        let dir = case_dir(&format!("crash-{name}"));
        let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/capabilities");
        std::fs::copy(examples.join("stripe.staal"), dir.join("stripe.staal")).unwrap();
        std::fs::write(
            dir.join("refund.staal"),
            "import stripe\n\nfn main(payment: Text, amount: Money, refunds: cap stripe.refunds.create(max: 100 USD)) -> Receipt {\n    return refunds.create(payment, amount)\n}\n",
        )
        .unwrap();
        let server = Server::start();
        let s = |p: &str| server.url(p);
        std::fs::write(dir.join("doubt.staal"), format!("fn main(note: Text, post: cap net.http.post) -> Int {{\n    let r = post.post(\"{}\", note)\n    return r.status\n}}\n", s("/hang")))
            .unwrap();
        let tool = |name: &str, once: bool| {
            let mut t = json!({
                "name": name, "description": "Refund a payment.", "program": "refund.staal",
                "grants": { "refunds": { "cap": "stripe.refunds.create", "each": usd("30.00"), "uses": 1, "only": ["pi_usd"], "needs": {
                    "get": { "cap": "net.http.get", "private_addresses": ["127.0.0.1", "::1"], "only": [s("/v1/payment_intents/*")] },
                    "post": { "cap": "net.http.post", "private_addresses": ["127.0.0.1", "::1"], "only": [s("/v1/refunds")], "idempotency_key": true },
                    "key": { "env": "STAAL_MCP_CRASH_KEY" },
                    "base": s("")
                } } },
                "net": { "connect_timeout_ms": 2000, "timeout_ms": 5000 },
                "quota": { "window": "1h", "calls": 50, "money": usd("1000.00") }
            });
            if once {
                t["once"] = json!({ "by": ["payment", "amount"] });
            }
            t
        };
        let doubt = json!({
            "name": "note", "description": "Send a note.", "program": "doubt.staal",
            "grants": { "post": { "cap": "net.http.post", "private_addresses": ["127.0.0.1", "::1"], "only": [s("/hang")] } },
            "net": { "connect_timeout_ms": 2000, "timeout_ms": 300 }
        });
        let manifest = write_manifest(&dir, json!({ "server": { "name": "shop" }, "tools": [tool("refund", true), tool("refund_any", false), doubt] }));
        Case { dir, manifest, server }
    }

    fn start(&self, failpoint: Option<(&str, &str)>) -> Mcp {
        let mut env = vec![("STAAL_MCP_CRASH_KEY", KEY)];
        if let Some(fp) = failpoint {
            env.push(fp);
        }
        Mcp::start(&self.manifest, &env)
    }

    fn ledger(&self) -> Vec<Json> {
        std::fs::read_to_string(self.dir.join("journals/server.ledger"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn settled(&self) -> Vec<Json> {
        self.ledger().into_iter().filter(|e| e["e"] == "settle").collect()
    }
}

const ARGS: fn() -> Json = || json!({ "payment": "pi_usd", "amount": usd("25.00") });

/// Kills the server at `fp` during a call of `tool`, then starts it again and sends the
/// same call; returns the answer to the call sent again.
fn crash_then_retry(case: &Case, tool: &str, fp: (&str, &str)) -> Json {
    let mut c = case.start(Some(fp));
    c.ask(json!(1), "tools/call", json!({ "name": tool, "arguments": ARGS() }));
    let got = c.wait_for(30, |m| m["id"] == 1);
    let (code, err) = c.wait_exit();
    assert!(got.is_none(), "{fp:?}: no answer before the crash");
    assert_ne!(code, Some(0), "{fp:?}: the server was killed: {err}");
    let mut c = case.start(None);
    let r = c.call(2, tool, ARGS());
    let (code, err, _) = c.close();
    assert_eq!(code, Some(0), "{err}");
    r
}

#[test]
fn with_once_a_call_sent_again_after_any_crash_refunds_exactly_once() {
    // An uninterrupted call, for comparison.
    let clean = Case::new("clean");
    let mut c = clean.start(None);
    let expected = c.call(1, "refund", ARGS());
    c.close();
    assert_eq!(expected["isError"], false, "{expected}");
    assert_eq!(clean.server.effects("/v1/refunds"), 1);

    let mut points: Vec<(String, String)> = Vec::new();
    for n in 1..=2 {
        points.push(("STAALC_MCP_FAILPOINT".into(), format!("ledger_write:{n}")));
    }
    // The journal of one refund: start, GET intent and result, POST intent and result, end.
    for n in 1..=6 {
        points.push(("STAALC_FAILPOINT".into(), format!("after_write:{n}")));
    }
    for op in ["1", "2"] {
        points.push(("STAALC_FAILPOINT".into(), format!("before_result:{op}")));
    }
    for (k, v) in &points {
        let case = Case::new(&format!("once-{}", v.replace(':', "-")));
        let r = crash_then_retry(&case, "refund", (k, v));
        assert_eq!(r["isError"], false, "{v}: {r}");
        let receipt = &r["structuredContent"]["result"];
        assert_eq!(receipt["capability"], expected["structuredContent"]["result"]["capability"], "{v}");
        assert_eq!(receipt["method"], expected["structuredContent"]["result"]["method"], "{v}");
        assert_eq!(case.server.effects("/v1/refunds"), 1, "{v}: refunded exactly once");
        // Every attempt carried the same key, so Stripe could deduplicate.
        let keys: std::collections::HashSet<String> = case
            .server
            .requests()
            .iter()
            .filter(|q| q.path == "/v1/refunds")
            .map(|q| q.headers.get("idempotency-key").cloned().unwrap_or_default())
            .collect();
        assert_eq!(keys.len(), 1, "{v}: {keys:?}");
        let settled = case.settled();
        assert_eq!(settled.len(), 1, "{v}: one settled run: {:?}", case.ledger());
        assert_eq!(settled[0]["charged"], json!([usd("25.00")]), "{v}");
        assert_eq!(settled[0]["outcome"], "done", "{v}");
        // Every reservation ended: settled, or released if its run never started.
        let ends = case.ledger().iter().filter(|e| e["e"] == "settle" || e["e"] == "release").count();
        assert_eq!(ends, case.ledger().iter().filter(|e| e["e"] == "reserve").count(), "{v}: {:?}", case.ledger());
        // Nothing is left that no reservation explains, and no arguments are kept.
        let leftovers: Vec<PathBuf> = common::mcp::files_under(&case.dir.join("journals/refund"))
            .into_iter()
            .filter(|p| p.to_string_lossy().ends_with(".run.json"))
            .collect();
        assert!(leftovers.is_empty(), "{v}: {leftovers:?}");
        // The key appears in no file.
        for f in common::mcp::files_under(&case.dir.join("journals")) {
            assert!(!std::fs::read_to_string(&f).unwrap_or_default().contains(KEY), "{v}: {} holds the key", f.display());
        }
    }
}

#[test]
fn without_once_a_call_sent_again_after_the_refund_refunds_again() {
    // Killed after the run was settled, before the client was answered.
    let case = Case::new("any-after-settle");
    let r = crash_then_retry(&case, "refund_any", ("STAALC_MCP_FAILPOINT", "ledger_write:2"));
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(case.server.effects("/v1/refunds"), 2, "a second run is a second refund");
    assert_eq!(case.settled().len(), 2);
    // Killed before the refund was sent: the retry is the only refund.
    let case = Case::new("any-after-reserve");
    let r = crash_then_retry(&case, "refund_any", ("STAALC_MCP_FAILPOINT", "ledger_write:1"));
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(case.server.effects("/v1/refunds"), 1);
    // The run that never started was released, and holds nothing against the quota.
    assert_eq!(case.settled().len(), 1);
}

#[test]
fn a_waiting_record_survives_a_crash_before_the_answer() {
    let case = Case::new("waiting");
    let mut c = case.start(Some(("STAALC_MCP_FAILPOINT", "after_waiting")));
    c.ask(json!(1), "tools/call", json!({ "name": "note", "arguments": { "note": "hi" } }));
    assert!(c.wait_for(30, |m| m["id"] == 1).is_none());
    c.wait_exit();
    let waiting: Vec<PathBuf> = common::mcp::files_under(&case.dir.join("journals/waiting"));
    assert_eq!(waiting.len(), 1, "{waiting:?}");
    let run = waiting[0].file_stem().unwrap().to_string_lossy().into_owned();
    let mut c = case.start(None);
    let st = c.call(1, "staal_run_status", json!({ "run": run }));
    assert_eq!(st["structuredContent"]["status"], "waiting_for_operator", "{st}");
    let (_, err, _) = c.close();
    assert!(err.contains("still waiting for an operator"), "{err}");
    assert_eq!(case.server.requests().iter().filter(|r| r.path == "/hang").count(), 1, "never sent again");
    assert!(case.ledger().iter().all(|e| e["e"] != "settle"), "a run in doubt keeps its reservation");
}

/// Kills the server during a `refund` call, after the run's GET and before its POST:
/// the run is reserved, started and unfinished.
fn crash_mid_refund(case: &Case, env: &[(&str, &str)]) {
    let mut all = vec![("STAALC_FAILPOINT", "after_write:3")];
    all.extend_from_slice(env);
    let mut full = vec![("STAAL_MCP_CRASH_KEY", KEY)];
    full.extend(all);
    let mut c = Mcp::start(&case.manifest, &full);
    c.ask(json!(1), "tools/call", json!({ "name": "refund", "arguments": ARGS() }));
    assert!(c.wait_for(30, |m| m["id"] == 1).is_none(), "no answer before the crash");
    let (code, err) = c.wait_exit();
    assert_ne!(code, Some(0), "the server was killed: {err}");
    assert_eq!(case.server.effects("/v1/refunds"), 0, "the crash came before the refund");
}

/// Starts the server after a crash, so that it resumes the unfinished run, and stops it.
fn restart(case: &Case, env: &[(&str, &str)]) -> (Option<i32>, String) {
    let mut full = vec![("STAAL_MCP_CRASH_KEY", KEY)];
    full.extend_from_slice(env);
    let (code, err, _) = Mcp::start(&case.manifest, &full).close();
    (code, err)
}

/// The records of the one `refund` run's journal.
fn refund_journal(case: &Case) -> Vec<Json> {
    let journals: Vec<PathBuf> = common::mcp::files_under(&case.dir.join("journals/refund"))
        .into_iter()
        .filter(|p| p.to_string_lossy().ends_with(".journal"))
        .collect();
    assert_eq!(journals.len(), 1, "{journals:?}");
    std::fs::read_to_string(&journals[0]).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

/// The run resumed under the program and config it started with, and refunded exactly
/// what the original call asked, once.
fn resumed_as_started(case: &Case, what: &str) {
    let records = refund_journal(case);
    let start = &records[0];
    let resumes: Vec<&Json> = records.iter().filter(|r| r["j"] == "resume").collect();
    assert_eq!(resumes.len(), 1, "{what}: one resume: {records:?}");
    for k in ["program_sha256", "config_sha256", "proxies"] {
        assert_eq!(resumes[0][k], start[k], "{what}: {k}");
    }
    let end = records.last().unwrap();
    assert_eq!(end["j"], "end", "{what}: {records:?}");
    assert_eq!(end["ok"], true, "{what}: the run failed: {end}");
    let posts: Vec<_> = case.server.requests().into_iter().filter(|q| q.path == "/v1/refunds").collect();
    assert_eq!(posts.len(), 1, "{what}: one refund request");
    assert!(posts[0].body.contains("payment_intent=pi_usd") && posts[0].body.contains("amount=2500"), "{what}: {}", posts[0].body);
    let settled = case.settled();
    assert_eq!(settled.len(), 1, "{what}: {:?}", case.ledger());
    assert_eq!(settled[0]["charged"], json!([usd("25.00")]), "{what}");
}

fn edit_tool(case: &Case, name: &str, change: impl Fn(&mut Json)) {
    let mut m: Json = serde_json::from_str(&std::fs::read_to_string(&case.manifest).unwrap()).unwrap();
    let tool = m["tools"].as_array_mut().unwrap().iter_mut().find(|t| t["name"] == name).unwrap();
    change(tool);
    std::fs::write(&case.manifest, serde_json::to_string_pretty(&m).unwrap()).unwrap();
}

/// A second entry in the tool's program and a second program, either of which would
/// refund 1.00 USD instead of the 25.00 USD the call asked for.
fn with_other_entries(case: &Case) {
    let main = std::fs::read_to_string(case.dir.join("refund.staal")).unwrap();
    let alt = "\nfn alt(payment: Text, amount: Money, refunds: cap stripe.refunds.create(max: 100 USD)) -> Receipt {\n    return refunds.create(payment, 1.00 USD)\n}\n";
    std::fs::write(case.dir.join("refund.staal"), format!("{main}{alt}")).unwrap();
    std::fs::write(
        case.dir.join("other.staal"),
        "import stripe\n\nfn main(payment: Text, amount: Money, refunds: cap stripe.refunds.create(max: 100 USD)) -> Receipt {\n    return refunds.create(payment, 1.00 USD)\n}\n",
    )
    .unwrap();
}

/// A change made to a tool's object in the manifest.
type ToolChange = Box<dyn Fn(&mut Json)>;

/// AGENT_MANIFEST.md section 16, R7: a run is resumed from what it started with, never
/// from the current manifest. Each field a run depends on is changed, alone, between a
/// crash and the resume. Read by the resumed run, the program and entry changes would
/// refund 1.00 USD, the grant changes would refuse the refund, and the proxy change
/// would send it to a proxy that accepts nothing; `once`, the deadline and the quota
/// would change the run's config or reservation. In every case the resume record binds
/// the same program, config and proxies as the start record, and the run refunds
/// 25.00 USD once. (`app_inputs` cannot be given in this manifest; it is tested over
/// HTTP, in mcp_http_test.rs.)
#[test]
fn a_resumed_run_never_takes_a_change_to_the_manifest() {
    let changes: Vec<(&str, ToolChange)> = vec![
        ("program", Box::new(|t| t["program"] = json!("other.staal"))),
        ("entry", Box::new(|t| t["entry"] = json!("alt"))),
        ("once", Box::new(|t| t["once"] = json!({ "by": ["payment"] }))),
        ("deadline", Box::new(|t| t["deadline"] = json!("1s"))),
        ("quota", Box::new(|t| t["quota"] = json!({ "window": "1m", "calls": 1, "money": usd("30.00") }))),
        ("grants", Box::new(|t| t["grants"]["refunds"]["only"] = json!(["pi_other"]))),
        ("grants-each", Box::new(|t| t["grants"]["refunds"]["each"] = usd("1.00"))),
        // A proxy that accepts nothing: a request sent through it would fail.
        ("proxy", Box::new(|t| t["net"]["proxy"] = json!("environment"))),
    ];
    for (what, change) in &changes {
        let case = Case::new(&format!("r7-{what}"));
        with_other_entries(&case);
        crash_mid_refund(&case, &[]);
        edit_tool(&case, "refund", change);
        let (code, err) = restart(&case, &[("HTTPS_PROXY", "http://127.0.0.1:9"), ("HTTP_PROXY", "http://127.0.0.1:9")]);
        assert_eq!(code, Some(0), "{what}: {err}");
        assert!(err.contains(": resuming"), "{what}: {err}");
        resumed_as_started(&case, what);
    }
}

/// The condition on R7 for the program path: a run file pointed at another file cannot
/// run different source under the original fingerprint. Different text is refused, and
/// the server does not start; the same text at another path is the same program.
#[test]
fn a_run_file_pointed_at_another_program_is_refused() {
    let case = Case::new("r7-run-file");
    with_other_entries(&case);
    crash_mid_refund(&case, &[]);
    let run_files: Vec<PathBuf> = common::mcp::files_under(&case.dir.join("journals/refund"))
        .into_iter()
        .filter(|p| p.to_string_lossy().ends_with(".run.json"))
        .collect();
    assert_eq!(run_files.len(), 1);
    let original = std::fs::read_to_string(&run_files[0]).unwrap();
    let point_at = |file: &str| {
        let mut f: Json = serde_json::from_str(&original).unwrap();
        f["program"] = json!(case.dir.join(file).to_str().unwrap());
        std::fs::write(&run_files[0], f.to_string()).unwrap();
    };
    point_at("other.staal");
    let (code, err) = restart(&case, &[]);
    assert_eq!(code, Some(2), "{err}");
    assert!(err.contains("cannot be resumed") && err.contains("different program"), "{err}");
    assert_eq!(case.server.effects("/v1/refunds"), 0);
    std::fs::copy(case.dir.join("refund.staal"), case.dir.join("same.staal")).unwrap();
    point_at("same.staal");
    let (code, err) = restart(&case, &[]);
    assert_eq!(code, Some(0), "{err}");
    resumed_as_started(&case, "the same text at another path");
}

/// The proxy a run used is part of what it is resumed with (RUNTIME.md section 11): a
/// run started with `"proxy": "environment"` is not resumed under other proxy
/// variables, and a proxy URL holding a credential is refused before anything runs.
#[test]
fn a_run_resumes_only_under_the_proxy_it_started_with() {
    let case = Case::new("r7-proxy-env");
    edit_tool(&case, "refund", |t| t["net"]["proxy"] = json!("environment"));
    // The local server is reached directly, by NO_PROXY; the proxy is still recorded.
    let env = [("HTTPS_PROXY", "http://127.0.0.1:9"), ("NO_PROXY", "127.0.0.1")];
    crash_mid_refund(&case, &env);
    let start = &refund_journal(&case)[0];
    assert_eq!(
        start["proxies"]["net"],
        json!({ "from": "HTTPS_PROXY", "scheme": "http", "host": "127.0.0.1", "port": 9, "no_proxy": ["127.0.0.1"] })
    );
    for (what, other) in [
        ("another proxy", vec![("HTTPS_PROXY", "http://127.0.0.1:10"), ("NO_PROXY", "127.0.0.1")]),
        ("another NO_PROXY", vec![("HTTPS_PROXY", "http://127.0.0.1:9"), ("NO_PROXY", "127.0.0.1,localhost")]),
        ("a variable read before it", vec![("ALL_PROXY", "http://127.0.0.1:9"), ("HTTPS_PROXY", "http://127.0.0.1:9"), ("NO_PROXY", "127.0.0.1")]),
        ("no proxy", vec![]),
    ] {
        let (code, err) = restart(&case, &other);
        assert_eq!(code, Some(2), "{what}: {err}");
        assert!(err.contains("cannot be resumed") && err.contains("proxies"), "{what}: {err}");
        assert_eq!(case.server.effects("/v1/refunds"), 0, "{what}");
    }
    let (code, err) = restart(&case, &env);
    assert_eq!(code, Some(0), "{err}");
    resumed_as_started(&case, "the same proxy");

    // A credential in the proxy URL: the server does not start, and does not show it.
    let case = Case::new("r7-proxy-credential");
    edit_tool(&case, "refund", |t| t["net"]["proxy"] = json!("environment"));
    let (code, err) = restart(&case, &[("HTTPS_PROXY", "http://user:hunter2@127.0.0.1:9")]);
    assert_eq!(code, Some(2), "{err}");
    assert!(err.contains("user name or password") && !err.contains("hunter2"), "{err}");
}
