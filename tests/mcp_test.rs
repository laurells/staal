// `staalc mcp` (docs/MCP.md), end to end: the server is started with a manifest and
// spoken to over its standard input and output, as an MCP client would, against the
// local fake services in tests/common/server.rs. Crashes are in mcp_crash_test.rs.

#![cfg(unix)]

mod common;

use common::mcp::{case_dir, files_under, meta, start_and_stop, write_manifest, Mcp, BIN};
use common::server::Server;
use serde_json::{json, Value as Json};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

const KEY: &str = "sk_test_mcp_secret_7f6e5d4c3b2a";

fn usd(a: &str) -> Json {
    json!({ "amount": a, "currency": "USD" })
}

fn text(r: &Json) -> String {
    r["content"][0]["text"].as_str().unwrap_or_default().to_string()
}

fn write(dir: &Path, name: &str, src: &str) {
    std::fs::write(dir.join(name), src).unwrap();
}

/// A tool refunding through the simulated payments adapter.
const REFUND: &str = r#"// Refunds an order, never by more than the server allows.
fn main(order: Text, amount: Money, pay: cap payments.refund(each: 500 USD)) -> Text {
    let receipt = pay.refund(order, amount)
    return "Refunded " + to_text(amount) + " for order " + order
}
"#;

fn refund_tool(extra: Json) -> Json {
    let mut t = json!({
        "name": "refund_order",
        "title": "Refund an order",
        "description": "Refund part of an order, within the shop's limits.",
        "program": "refund.staal",
        "grants": { "pay": { "cap": "payments.refund", "each": usd("50.00"), "uses": 1 } }
    });
    for (k, v) in extra.as_object().unwrap() {
        t[k] = v.clone();
    }
    t
}

fn refund_case(name: &str, extra: Json) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = case_dir(name);
    write(&dir, "refund.staal", REFUND);
    let m = write_manifest(&dir, json!({ "server": { "name": "shop" }, "tools": [refund_tool(extra)] }));
    (dir, m)
}

fn slow_program(base: &str, gets: usize) -> String {
    let mut body = String::new();
    for i in 0..gets {
        body.push_str(&format!("    let r{i} = get.get(\"{base}/slow\")\n"));
    }
    format!("fn main(n: Int, get: cap net.http.get) -> Int {{\n{body}    return n\n}}\n")
}

fn net() -> Json {
    json!({ "connect_timeout_ms": 2000, "timeout_ms": 5000 })
}

#[test]
fn both_protocol_versions_are_served_and_what_mcp_forbids_is_refused() {
    let (_dir, m) = refund_case("protocol", json!({}));
    let mut c = Mcp::start(&m, &[]);

    // 2026-07-28: no handshake; every request carries _meta.
    let d = c.request(json!(1), "server/discover", json!({}));
    let r = &d["result"];
    assert_eq!(r["supportedVersions"], json!(["2026-07-28"]));
    assert_eq!(r["resultType"], "complete");
    assert_eq!(r["capabilities"]["tools"]["listChanged"], false);
    assert_eq!(r["_meta"]["io.modelcontextprotocol/serverInfo"]["name"], "shop");
    assert_eq!(r["cacheScope"], "private");
    assert!(r["ttlMs"].as_u64().unwrap() > 0);
    let l = c.request(json!(2), "tools/list", json!({}));
    let r = &l["result"];
    assert_eq!(r["resultType"], "complete");
    assert_eq!(r["cacheScope"], "private");
    assert!(r["ttlMs"].is_u64());
    let names: Vec<&str> = r["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["refund_order", "staal_run_status"]);
    let t = &r["tools"][0];
    assert_eq!(t["title"], "Refund an order");
    assert_eq!(t["inputSchema"]["required"], json!(["order", "amount"]));
    assert!(t["inputSchema"]["properties"].get("pay").is_none(), "a grant is never an input");
    assert_eq!(t["outputSchema"]["type"], "object");
    assert_eq!(t["outputSchema"]["properties"]["result"], json!({ "type": "string" }));
    // ping was removed in 2026-07-28.
    assert_eq!(c.request(json!(3), "ping", json!({}))["error"]["code"], -32601);
    // A request missing either _meta field, or naming another version.
    let e = c.request(json!(4), "tools/list", json!({ "_meta": { "io.modelcontextprotocol/protocolVersion": "2026-07-28" } }));
    assert_eq!(e["error"]["code"], -32602);
    c.send(&json!({ "jsonrpc": "2.0", "id": 5, "method": "tools/list" }));
    assert_eq!(c.response(&json!(5))["error"]["code"], -32602);
    let e = c.request(json!(6), "tools/list", json!({ "_meta": { "io.modelcontextprotocol/protocolVersion": "1900-01-01", "io.modelcontextprotocol/clientCapabilities": {} } }));
    assert_eq!(e["error"]["code"], -32022);
    assert_eq!(e["error"]["data"], json!({ "supported": ["2026-07-28"], "requested": "1900-01-01" }));
    // Unknown methods and tools, bad params, cursors.
    assert_eq!(c.request(json!(7), "resources/list", json!({}))["error"]["code"], -32601);
    assert_eq!(c.request(json!(8), "tools/call", json!({ "name": "nope", "arguments": {} }))["error"]["code"], -32602);
    assert_eq!(c.request(json!(9), "tools/call", json!({ "name": "refund_order", "arguments": [1] }))["error"]["code"], -32602);
    assert_eq!(c.request(json!(10), "tools/list", json!({ "cursor": "x" }))["error"]["code"], -32602);
    // What JSON-RPC and MCP forbid: batches, non-JSON, a null id, huge or deep messages.
    c.send_raw(r#"[{"jsonrpc":"2.0","id":11,"method":"tools/list","params":{}}]"#);
    let e = c.wait_for(10, |m| m["error"]["code"] == -32600 && m["id"].is_null()).expect("a batch is refused");
    assert!(e["error"]["message"].as_str().unwrap().contains("batch"));
    c.send_raw("this is not json");
    assert!(c.wait_for(10, |m| m["error"]["code"] == -32700).is_some());
    c.send_raw(&format!("{{\"jsonrpc\":\"2.0\",\"id\":12,\"method\":\"tools/list\",\"params\":{{\"x\":\"{}\"}}}}", "a".repeat(5 * 1024 * 1024)));
    let e = c.wait_for(30, |m| m["error"]["code"] == -32700 && m["id"].is_null()).expect("a 5 MiB line is refused");
    assert!(e["error"]["message"].as_str().unwrap().contains("longer than"));
    let deep = format!("{{\"jsonrpc\":\"2.0\",\"id\":13,\"method\":\"tools/list\",\"params\":{{\"x\":{}{}}}}}", "[".repeat(100), "]".repeat(100));
    c.send_raw(&deep);
    let e = c.wait_for(10, |m| m["error"]["code"] == -32600 && m["error"]["message"].as_str().is_some_and(|s| s.contains("nested"))).unwrap();
    assert!(e["id"].is_null());
    c.send(&json!({ "jsonrpc": "2.0", "id": null, "method": "tools/list", "params": { "_meta": meta() } }));
    assert!(c.wait_for(10, |m| m["error"]["code"] == -32600 && m["id"].is_null()).is_some());
    // The server still serves after all of that.
    assert!(c.request(json!(14), "tools/list", json!({}))["result"]["tools"].is_array());

    // 2025-11-25: initialize, then requests without _meta; ping is there.
    let i = c.request(json!(20), "initialize", json!({ "protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": { "name": "t", "version": "1" } }));
    assert_eq!(i["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(i["result"]["capabilities"]["tools"]["listChanged"], false);
    assert_eq!(i["result"]["serverInfo"]["name"], "shop");
    c.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
    c.send(&json!({ "jsonrpc": "2.0", "id": 21, "method": "ping" }));
    assert_eq!(c.response(&json!(21))["result"], json!({}));
    c.send(&json!({ "jsonrpc": "2.0", "id": 22, "method": "tools/list", "params": {} }));
    let l = c.response(&json!(22));
    assert!(l["result"].get("resultType").is_none() && l["result"].get("ttlMs").is_none(), "no 2026 field leaks: {l}");
    c.send(&json!({ "jsonrpc": "2.0", "id": 23, "method": "tools/call", "params": { "name": "refund_order", "arguments": { "order": "1", "amount": usd("5.00") } } }));
    let r = c.response(&json!(23));
    assert_eq!(r["result"]["structuredContent"], json!({ "result": "Refunded 5.00 USD for order 1" }));
    assert!(r["result"].get("resultType").is_none());
    // A request with 2026 _meta is still served the 2026 way on the same connection.
    assert_eq!(c.request(json!(24), "tools/list", json!({}))["result"]["resultType"], "complete");

    let (code, _, raw) = c.close();
    assert_eq!(code, Some(0));
    for line in raw {
        let v: Json = serde_json::from_str(&line).unwrap();
        assert_eq!(v["jsonrpc"], "2.0", "{line}");
    }
}

#[test]
fn each_outcome_becomes_the_result_the_design_says() {
    let s = Server::start();
    let dir = case_dir("outcomes");
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/capabilities");
    std::fs::copy(examples.join("stripe.staal"), dir.join("stripe.staal")).unwrap();
    write(
        &dir,
        "stripe_refund.staal",
        "import stripe\n\nfn main(payment: Text, amount: Money, refunds: cap stripe.refunds.create(max: 100 USD)) -> Receipt {\n    return refunds.create(payment, amount)\n}\n",
    );
    write(&dir, "doubt.staal", &format!("fn main(note: Text, post: cap net.http.post) -> Int {{\n    let r = post.post(\"{}\", note)\n    return r.status\n}}\n", s.url("/hang")));
    write(&dir, "big.staal", "fn main() -> Text {\n    let s = \"x\"\n    repeat 21 {\n        set s = s + s\n    }\n    return s\n}\n");
    let stripe = json!({
        "name": "stripe_refund", "description": "Refund a payment through Stripe.", "program": "stripe_refund.staal",
        "grants": { "refunds": { "cap": "stripe.refunds.create", "each": usd("30.00"), "only": ["pi_usd"], "needs": {
            "get": { "cap": "net.http.get", "only": [s.url("/v1/payment_intents/*")] },
            "post": { "cap": "net.http.post", "only": [s.url("/v1/refunds")], "idempotency_key": true },
            "key": { "env": "STAAL_MCP_TEST_KEY" },
            "base": s.url("")
        } } },
        "net": net()
    });
    let doubt = json!({
        "name": "note", "description": "Send a note.", "program": "doubt.staal",
        "grants": { "post": { "cap": "net.http.post", "only": [s.url("/hang")] } },
        "net": { "connect_timeout_ms": 2000, "timeout_ms": 300 }
    });
    let big = json!({ "name": "big", "description": "A large result.", "program": "big.staal" });
    let m = write_manifest(&dir, json!({ "server": { "name": "shop" }, "tools": [stripe, doubt, big] }));
    let mut c = Mcp::start(&m, &[("STAAL_MCP_TEST_KEY", KEY)]);

    // done: the receipt, as structured content and as text.
    let r = c.call(1, "stripe_refund", json!({ "payment": "pi_usd", "amount": usd("25.00") }));
    assert_eq!(r["isError"], false, "{r}");
    let receipt = &r["structuredContent"]["result"];
    assert_eq!(receipt["capability"], "stripe.refunds.create");
    assert_eq!(serde_json::from_str::<Json>(&text(&r)).unwrap(), *receipt);
    assert_eq!(s.effects("/v1/refunds"), 1);
    // capability: the broker's message, and the service never sees the request.
    let before = s.requests().len();
    let r = c.call(2, "stripe_refund", json!({ "payment": "pi_usd", "amount": usd("40.00") }));
    assert_eq!(r["isError"], true);
    assert_eq!(text(&r), "'stripe.refunds.create' allows at most 30.00 USD per action; this action needs 40.00 USD");
    let r = c.call(3, "stripe_refund", json!({ "payment": "pi_eur", "amount": usd("5.00") }));
    assert!(text(&r).contains("may only act on"), "{r}");
    assert_eq!(s.requests().len(), before, "a refused action never reaches the service");
    // Arguments the program's types refuse are a tool result the model can correct.
    let r = c.call(4, "stripe_refund", json!({ "payment": "pi_usd", "amount": { "amount": "1.234", "currency": "USD" } }));
    assert_eq!(r["isError"], true);
    assert!(text(&r).contains("decimal place"), "{r}");
    let r = c.call(5, "stripe_refund", json!({ "payment": "pi_usd", "amount": usd("1.00"), "pay": { "cap": "x" } }));
    assert!(text(&r).contains("has no parameter named 'pay'"), "{r}");
    let r = c.call(6, "stripe_refund", json!({ "amount": usd("1.00") }));
    assert!(text(&r).contains("inputs.payment: missing"), "{r}");
    assert_eq!(s.requests().len(), before);

    // stopped: the action may have happened; the waiting record is on disk first.
    let r = c.call(7, "note", json!({ "note": "hello" }));
    assert_eq!(r["isError"], true);
    let run = r["structuredContent"]["run"].as_str().unwrap().to_string();
    assert_eq!(r["structuredContent"]["status"], "waiting_for_operator");
    assert!(text(&r).contains("Do not call this tool again"), "{r}");
    let waiting = dir.join("journals/waiting").join(format!("{run}.json"));
    let w: Json = serde_json::from_str(&std::fs::read_to_string(&waiting).unwrap()).unwrap();
    assert_eq!(w["tool"], "note");
    assert_eq!(w["kind"], "in_doubt");
    assert!(w.get("note").is_none() && !w.to_string().contains("hello"), "the record holds no arguments: {w}");
    let st = c.call(8, "staal_run_status", json!({ "run": run }));
    assert_eq!(st["structuredContent"]["status"], "waiting_for_operator");
    assert_eq!(st["structuredContent"]["failure"]["kind"], "in_doubt");

    // A result too large to send.
    let r = c.call(9, "big", json!({}));
    assert_eq!(r["isError"], true);
    let t = text(&r);
    assert!(t.contains("too large") && t.contains("run "), "{t}");
    // Nor is it sent through the status tool.
    let big_run = &t[t.find("run ").unwrap() + 4..][..32];
    let st = c.call(90, "staal_run_status", json!({ "run": big_run }));
    let st = &st["structuredContent"];
    assert_eq!(st["status"], "done");
    assert!(st.get("result").is_none(), "{st}");
    assert_eq!(st["result_bytes"], json!(2 * 1024 * 1024 + 2));
    assert!(st["reason"].as_str().unwrap().contains("too large to send"), "{st}");

    // The status of runs that ended, of an unknown run, and a malformed question.
    let first = c.call(10, "stripe_refund", json!({ "payment": "pi_usd", "amount": usd("2.00") }));
    assert_eq!(first["isError"], false);
    let unknown = c.call(11, "staal_run_status", json!({ "run": "0".repeat(32) }));
    assert_eq!(unknown["structuredContent"]["status"], "unknown");
    assert!(unknown["structuredContent"]["observed_at"].as_str().unwrap().ends_with('Z'));
    assert_eq!(c.call(12, "staal_run_status", json!({ "run": "nope" }))["isError"], true);
    assert_eq!(c.call(13, "staal_run_status", json!({ "run": run, "extra": 1 }))["isError"], true);

    let (code, err, raw) = c.close();
    assert_eq!(code, Some(0));
    // The run ids of finished runs answer from their journals.
    let ids: Vec<String> = err.lines().filter(|l| l.contains("tool stripe_refund run ") && l.contains("done")).map(|l| l.split("run ").nth(1).unwrap()[..32].to_string()).collect();
    assert_eq!(ids.len(), 2, "{err}");
    let mut c = Mcp::start(&m, &[("STAAL_MCP_TEST_KEY", KEY)]);
    let st = c.call(1, "staal_run_status", json!({ "run": ids[0] }));
    assert_eq!(st["structuredContent"]["status"], "done");
    assert_eq!(st["structuredContent"]["result"]["capability"], "stripe.refunds.create");
    let failed: Vec<String> = err.lines().filter(|l| l.contains("tool stripe_refund run ") && l.contains("failed")).map(|l| l.split("run ").nth(1).unwrap()[..32].to_string()).collect();
    let st = c.call(2, "staal_run_status", json!({ "run": failed[0] }));
    assert_eq!(st["structuredContent"]["status"], "failed");
    assert_eq!(st["structuredContent"]["failure"]["kind"], "capability");
    // The stopped run was resumed at start, and still waits: nothing is guessed.
    let st = c.call(3, "staal_run_status", json!({ "run": run }));
    assert_eq!(st["structuredContent"]["status"], "waiting_for_operator");
    let (_, err2, raw2) = c.close();
    assert_eq!(s.requests().iter().filter(|r| r.path == "/hang").count(), 1, "the note in doubt was never sent again");

    // The key appears nowhere: not on the stream, the log, a journal or the ledger.
    for f in files_under(&dir.join("journals")) {
        assert!(!std::fs::read_to_string(&f).unwrap_or_default().contains(KEY), "{} holds the key", f.display());
    }
    for out in [raw.join("\n"), raw2.join("\n"), err, err2] {
        assert!(!out.contains(KEY));
    }
    assert!(s.requests().iter().any(|r| r.headers.get("authorization").is_some_and(|a| a.contains(KEY))), "the key did reach Stripe");
}

#[test]
fn once_makes_a_call_one_operation_however_often_it_is_sent() {
    let s = Server::start();
    let dir = case_dir("once");
    write(&dir, "refund.staal", REFUND);
    write(&dir, "slow.staal", &slow_program(&s.url(""), 2));
    let slow = json!({
        "name": "slow", "description": "Slow.", "program": "slow.staal",
        "grants": { "get": { "cap": "net.http.get", "only": [s.url("/slow")] } }, "net": net(),
        "once": { "by": ["n"], "keep": "1h" }
    });
    let m = write_manifest(&dir, json!({ "server": { "name": "shop" }, "tools": [refund_tool(json!({ "once": { "by": ["order"] } })), slow] }));
    let mut c = Mcp::start(&m, &[]);
    let a = c.call(1, "refund_order", json!({ "order": "4417", "amount": usd("35.00") }));
    assert_eq!(a["isError"], false);
    // The same operation again, even with money written differently: the same answer.
    let b = c.call(2, "refund_order", json!({ "order": "4417", "amount": usd("35") }));
    assert_eq!(a, b);
    // The same order with another amount is a different operation pretending to be the same.
    let d = c.call(3, "refund_order", json!({ "order": "4417", "amount": usd("36.00") }));
    assert_eq!(d["isError"], true);
    assert!(text(&d).contains("different arguments"), "{d}");
    // A failed run's failure is given again, and nothing runs.
    let f1 = c.call(4, "refund_order", json!({ "order": "9", "amount": usd("80.00") }));
    let f2 = c.call(5, "refund_order", json!({ "order": "9", "amount": usd("80.00") }));
    assert_eq!(f1["isError"], true);
    assert_eq!(f1, f2);
    // While a run is in progress, a repeat is told so.
    c.ask(json!(6), "tools/call", json!({ "name": "slow", "arguments": { "n": 1 } }));
    std::thread::sleep(std::time::Duration::from_millis(300));
    let p = c.call(7, "slow", json!({ "n": 1 }));
    assert_eq!(p["isError"], true);
    assert!(text(&p).contains("already in progress"), "{p}");
    assert_eq!(c.response(&json!(6))["result"]["structuredContent"]["result"], 1);
    assert_eq!(c.call(8, "slow", json!({ "n": 1 }))["structuredContent"]["result"], 1);
    let (_, err, _) = c.close();
    assert_eq!(err.matches("tool refund_order run ").count(), 2, "two runs, not five: {err}");
    assert_eq!(s.requests().iter().filter(|r| r.path == "/slow").count(), 2, "the slow tool ran once");
    // Across a restart too: the key is in the ledger.
    let mut c = Mcp::start(&m, &[]);
    assert_eq!(c.call(1, "refund_order", json!({ "order": "4417", "amount": usd("35.00") })), a);
    let (_, err, _) = c.close();
    assert!(!err.contains("tool refund_order run "), "nothing ran: {err}");
}

#[test]
fn concurrency_is_bounded_and_refused_calls_reserve_nothing() {
    let s = Server::start();
    let dir = case_dir("concurrency");
    write(&dir, "slow.staal", &slow_program(&s.url(""), 3));
    let slow = json!({
        "name": "slow", "description": "Slow.", "program": "slow.staal",
        "grants": { "get": { "cap": "net.http.get", "only": [s.url("/slow")] } }, "net": net(),
        "max_concurrent_runs": 2, "quota": { "window": "1h", "calls": 10 }
    });
    let m = write_manifest(&dir, json!({ "server": { "name": "shop", "max_concurrent_runs": 4 }, "tools": [slow] }));
    let mut c = Mcp::start(&m, &[]);
    for id in 1..=3 {
        c.ask(json!(id), "tools/call", json!({ "name": "slow", "arguments": { "n": id } }));
    }
    let answers: Vec<Json> = (1..=3).map(|id| c.response(&json!(id))["result"].clone()).collect();
    let busy: Vec<&Json> = answers.iter().filter(|a| a["isError"] == true).collect();
    assert_eq!(busy.len(), 1, "{answers:?}");
    assert!(text(busy[0]).contains("busy"));
    // Once a run ends, a slot is free again.
    assert_eq!(c.call(4, "slow", json!({ "n": 4 }))["isError"], false);
    let (_, err, _) = c.close();
    assert_eq!(err.matches(": done;").count(), 3, "{err}");
    let ledger = std::fs::read_to_string(dir.join("journals/server.ledger")).unwrap();
    assert_eq!(ledger.matches("\"reserve\"").count(), 3, "the busy call reserved nothing");
}

#[test]
fn quotas_hold_across_calls_and_restarts() {
    let (dir, m) = refund_case("quota", json!({ "quota": { "window": "24h", "calls": 3, "money": usd("120.00") } }));
    let mut c = Mcp::start(&m, &[]);
    for (i, a) in ["10.00", "20.00"].iter().enumerate() {
        assert_eq!(c.call(i as u64 + 1, "refund_order", json!({ "order": format!("o{i}"), "amount": usd(a) }))["isError"], false);
    }
    // 30.00 settled; 50.00 more would be 80.00, within 120.00; a third run holds 50.00.
    c.close();
    let mut c = Mcp::start(&m, &[]);
    assert_eq!(c.call(1, "refund_order", json!({ "order": "o3", "amount": usd("1.00") }))["isError"], false);
    let r = c.call(2, "refund_order", json!({ "order": "o4", "amount": usd("1.00") }));
    assert_eq!(r["isError"], true);
    let t = text(&r);
    assert!(t.contains("quota of 3 calls per 1d") && t.contains("next call can start at"), "{t}");
    c.close();
    let out = Command::new(BIN).args(["mcp", "status"]).arg(&m).output().unwrap();
    let report = String::from_utf8_lossy(&out.stdout);
    assert!(report.contains("tool refund_order: 3 of 3 calls, 31.00 USD of 120.00 USD in the last 1d"), "{report}");
    assert!(report.contains("no run is unfinished"), "{report}");

    // The money quota: each run reserves its worst case (here 50.00) until it settles.
    let (_, m) = refund_case("quota-money", json!({ "quota": { "window": "1h", "money": usd("120.00") } }));
    let mut c = Mcp::start(&m, &[]);
    assert_eq!(c.call(1, "refund_order", json!({ "order": "a", "amount": usd("45.00") }))["isError"], false);
    assert_eq!(c.call(2, "refund_order", json!({ "order": "b", "amount": usd("45.00") }))["isError"], false);
    // 90.00 spent: a run that could spend 50.00 more would exceed 120.00.
    let r = c.call(3, "refund_order", json!({ "order": "c", "amount": usd("1.00") }));
    assert_eq!(r["isError"], true);
    assert!(text(&r).contains("can spend up to 50.00 USD"), "{r}");
    c.close();
    let _ = dir;
}

#[test]
fn a_stopped_run_is_told_to_the_operator_and_finishes_once_resolved() {
    // What this test process holds before it starts any server: descriptors the
    // environment passed down (a CI runner leaves some open), which every child has.
    let ambient: Vec<String> = std::fs::read_dir("/proc/self/fd")
        .map(|d| d.flatten().filter_map(|e| std::fs::read_link(e.path()).ok()).map(|p| p.to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    let s = Server::start();
    let dir = case_dir("operator");
    write(&dir, "doubt.staal", &format!("fn main(note: Text, post: cap net.http.post) -> Int {{\n    let r = post.post(\"{}\", note)\n    return r.status\n}}\n", s.url("/hang")));
    let notify = dir.join("notify.sh");
    // The first attempt fails; later ones record what the command saw.
    std::fs::write(
        &notify,
        "#!/bin/sh\nd=$(dirname \"$0\")\nif [ ! -e \"$d/failed-once\" ]; then touch \"$d/failed-once\"; exit 1; fi\nenv > \"$d/notify.env\"\ncat \"$1\" > \"$d/notify.record\"\nif [ -d /proc/$$/fd ]; then ls -l /proc/$$/fd > \"$d/notify.fds\"; fi\n",
    )
    .unwrap();
    std::fs::set_permissions(&notify, std::fs::Permissions::from_mode(0o755)).unwrap();
    let tool = json!({
        "name": "note", "description": "Send a note.", "program": "doubt.staal",
        "grants": { "post": { "cap": "net.http.post", "only": [s.url("/hang")] } },
        "net": { "connect_timeout_ms": 2000, "timeout_ms": 300 },
        "secrets": {}
    });
    let m = write_manifest(&dir, json!({ "server": { "name": "shop", "notify": ["notify.sh"], "notify_env": ["STAAL_MCP_NOTIFY_OK"] }, "tools": [tool] }));
    let env = [("STAAL_MCP_NOTIFY_OK", "visible"), ("STAAL_MCP_TEST_KEY", KEY)];
    let mut c = Mcp::start(&m, &env);
    let r = c.call(1, "note", json!({ "note": "hi" }));
    let run = r["structuredContent"]["run"].as_str().unwrap().to_string();
    let (_, err, _) = c.close();
    assert!(err.contains("waiting for an operator") && err.contains("the notify command failed"), "{err}");
    assert!(!dir.join("notify.env").exists());
    let notified = dir.join("journals/waiting").join(format!("{run}.notify"));
    assert!(std::fs::read_to_string(&notified).unwrap().starts_with("failed"));

    // The next start resumes the run (it stops again: nothing is guessed) and runs the
    // notify command again, which now succeeds, with a cleared environment.
    let (code, err) = start_and_stop(&m, &env);
    assert_eq!(code, Some(0), "{err}");
    assert!(err.contains("still waiting for an operator"), "{err}");
    let seen = std::fs::read_to_string(dir.join("notify.env")).unwrap();
    assert!(seen.contains("STAAL_MCP_NOTIFY_OK=visible"), "{seen}");
    assert!(!seen.contains(KEY) && !seen.contains("STAAL_MCP_TEST_KEY"), "the command never sees a secret: {seen}");
    for line in seen.lines() {
        let k = line.split('=').next().unwrap();
        assert!(["PATH", "HOME", "LANG", "TZ", "STAAL_MCP_NOTIFY_OK", "PWD", "SHLVL", "_", "OLDPWD"].contains(&k), "unexpected {k}");
    }
    // The protocol's private descriptors are close-on-exec: the command holds no pipe or
    // socket beyond its standard ones (Linux, where /proc shows them).
    if cfg!(target_os = "linux") {
        // Beyond 0 to 2 the shell holds only the script it reads, a copy of its own
        // standard output (the server's standard error) while it redirects, and what the
        // environment gave this test; anything else came from the server.
        let fds = std::fs::read_to_string(dir.join("notify.fds")).unwrap();
        let fds: Vec<(u32, &str)> = fds
            .lines()
            .filter_map(|l| l.split_once(" -> "))
            .map(|(left, target)| (left.rsplit(' ').next().unwrap().parse().unwrap(), target))
            .collect();
        let own = fds.iter().find(|(fd, _)| *fd == 2).map(|(_, t)| *t).unwrap();
        for (fd, target) in &fds {
            if *fd > 2 {
                let allowed = *target == own || target.ends_with("/notify.sh") || ambient.iter().any(|a| a == target);
                assert!(allowed, "the command inherited fd {fd}: {target} ({fds:?})");
            }
        }
    }
    let rec: Json = serde_json::from_str(&std::fs::read_to_string(dir.join("notify.record")).unwrap()).unwrap();
    assert_eq!(rec["run"], run.as_str());
    assert_eq!(std::fs::read_to_string(&notified).unwrap(), "ok\n");
    // A third start does not tell the operator again.
    std::fs::remove_file(dir.join("notify.record")).unwrap();
    let (code, _) = start_and_stop(&m, &env);
    assert_eq!(code, Some(0));
    assert!(!dir.join("notify.record").exists());
    let out = Command::new(BIN).args(["mcp", "status"]).arg(&m).output().unwrap();
    let report = String::from_utf8_lossy(&out.stdout);
    assert!(report.contains(&format!("run {run} (tool note): waiting for an operator (in_doubt)")), "{report}");
    assert!(report.contains("staalc resolve --journal"), "{report}");

    // The operator checks the service and records what happened; the next start
    // finishes the run and settles it.
    let journal = dir.join("journals/note").join(&run[..2]).join(format!("{run}.journal"));
    let out = Command::new(BIN).args(["resolve", "--journal"]).arg(&journal).args(["--status", "202", "--body", "queued"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let mut c = Mcp::start(&m, &env);
    let st = c.call(1, "staal_run_status", json!({ "run": run }));
    assert_eq!(st["structuredContent"]["status"], "done", "{st}");
    assert_eq!(st["structuredContent"]["result"], 202);
    c.close();
    assert!(!dir.join("journals/waiting").join(format!("{run}.json")).exists(), "the record goes once the run is resolved");
    assert_eq!(s.requests().iter().filter(|r| r.path == "/hang").count(), 1);
}

#[test]
fn nothing_but_protocol_messages_reach_standard_output() {
    let dir = case_dir("stream");
    write(&dir, "print.staal", "fn main(n: Int) -> Int {\n    print(\"printed by the program\")\n    print(\"{\\\"jsonrpc\\\": \\\"2.0\\\", \\\"id\\\": 99, \\\"result\\\": {}}\")\n    return n\n}\n");
    let m = write_manifest(&dir, json!({ "server": { "name": "s" }, "tools": [{ "name": "p", "description": "Prints.", "program": "print.staal" }] }));
    let mut c = Mcp::start(&m, &[]);
    for i in 0..5 {
        assert_eq!(c.call(i, "p", json!({ "n": i }))["structuredContent"]["result"], i);
    }
    let (_, err, raw) = c.close();
    assert!(err.contains("printed by the program"), "a print goes to standard error: {err}");
    for line in &raw {
        let v: Json = serde_json::from_str(line).unwrap();
        assert!(v["id"] != 99, "a print reached the stream: {line}");
        assert!(v.get("result").is_some() || v.get("error").is_some() || v.get("method").is_some(), "{line}");
    }
    assert_eq!(raw.len(), 5);
}

#[test]
fn a_manifest_that_could_go_wrong_stops_the_server_before_it_answers() {
    let s = Server::start();
    let dir = case_dir("refusals");
    write(&dir, "refund.staal", REFUND);
    write(&dir, "ask.staal", "fn main() -> Text {\n    match read_line() {\n        some(l) => { return l }\n        none => { return \"\" }\n    }\n}\n");
    write(&dir, "listen.staal", "fn main(l: cap net.tcp.accept) -> Text {\n    let i = l.accept(\"127.0.0.1:9\")\n    return i.data\n}\n");
    write(&dir, "get.staal", &slow_program(&s.url(""), 1));
    write(&dir, "bad.staal", "fn main() -> Int { return \"x\" }\n");
    let refuse = |name: &str, manifest: Json, expect: &str| {
        let d = dir.join(name);
        std::fs::create_dir_all(&d).unwrap();
        for f in ["refund.staal", "ask.staal", "listen.staal", "get.staal", "bad.staal"] {
            std::fs::copy(dir.join(f), d.join(f)).unwrap();
        }
        let m = write_manifest(&d, manifest);
        let (code, err) = start_and_stop(&m, &[("STAAL_MCP_TEST_KEY", KEY)]);
        assert_eq!(code, Some(2), "{name}: {err}");
        assert!(err.contains(expect), "{name}: expected '{expect}' in:\n{err}");
    };
    let tool = |extra: Json| refund_tool(extra);
    refuse("console", json!({ "server": { "name": "s" }, "tools": [{ "name": "a", "description": "d", "program": "ask.staal" }] }), "read_line");
    refuse("accept", json!({ "server": { "name": "s" }, "tools": [{ "name": "a", "description": "d", "program": "listen.staal",
        "grants": { "l": { "cap": "net.tcp.accept" } }, "net": net() }] }), "net.tcp.accept");
    refuse("timeouts", json!({ "server": { "name": "s" }, "tools": [{ "name": "a", "description": "d", "program": "get.staal",
        "grants": { "get": { "cap": "net.http.get" } } }] }), "net.connect_timeout_ms: required");
    refuse("checks", json!({ "server": { "name": "s" }, "tools": [{ "name": "a", "description": "d", "program": "bad.staal" }] }), "check errors");
    refuse("unknown", json!({ "server": { "name": "s" }, "tools": [tool(json!({ "colour": 1 }))] }), "unknown key 'colour'");
    refuse("inputs", json!({ "server": { "name": "s" }, "tools": [tool(json!({ "inputs": {} }))] }), "inputs: not allowed");
    refuse("description", json!({ "server": { "name": "s" }, "tools": [{ "name": "a", "program": "refund.staal" }] }), "description: missing");
    refuse("status-name", json!({ "server": { "name": "s" }, "tools": [tool(json!({ "name": "staal_run_status" }))] }), "the server's own tool");
    refuse("dots", json!({ "server": { "name": "s" }, "tools": [tool(json!({ "name": ".." }))] }), "not only dots");
    refuse("twice", json!({ "server": { "name": "s" }, "tools": [tool(json!({})), tool(json!({}))] }), "used by an earlier tool");
    refuse("grant", json!({ "server": { "name": "s" }, "tools": [tool(json!({ "grants": {} }))] }), "grants.pay: missing");
    refuse("once", json!({ "server": { "name": "s" }, "tools": [tool(json!({ "once": { "by": ["pay"] } }))] }), "takes no argument 'pay'");
    refuse("once-keep", json!({ "server": { "name": "s", "keep": 1 }, "tools": [tool(json!({ "once": { "by": ["order"], "keep": "2d" } }))] }), "longer than server.keep");
    refuse("quota-unbounded", json!({ "server": { "name": "s" }, "tools": [tool(json!({
        "grants": { "pay": { "cap": "payments.refund" } }, "quota": { "window": "1h", "money": usd("100.00") } }))] }), "can spend money");
    refuse("quota-currency", json!({ "server": { "name": "s" }, "tools": [tool(json!({
        "quota": { "window": "1h", "money": { "amount": "100.00", "currency": "EUR" } } }))] }), "the money quota is in EUR");
    refuse("quota-small", json!({ "server": { "name": "s" }, "tools": [tool(json!({ "quota": { "window": "1h", "money": usd("10.00") } }))] }), "no call could start");
    refuse("quota-window", json!({ "server": { "name": "s" }, "tools": [tool(json!({ "quota": { "window": "31d", "calls": 1 } }))] }), "at most 30d");
    refuse("deadline", json!({ "server": { "name": "s" }, "tools": [tool(json!({ "limits": { "deadline_ms": 5 } }))] }), "give the tool's \"deadline\"");
    refuse("concurrency", json!({ "server": { "name": "s", "max_concurrent_runs": 2 }, "tools": [tool(json!({ "max_concurrent_runs": 3 }))] }), "more than server.max_concurrent_runs");

    // The journal directory must be private to the server's user.
    let d = dir.join("perms");
    std::fs::create_dir_all(d.join("journals")).unwrap();
    std::fs::copy(dir.join("refund.staal"), d.join("refund.staal")).unwrap();
    std::fs::set_permissions(d.join("journals"), std::fs::Permissions::from_mode(0o777)).unwrap();
    let m = write_manifest(&d, json!({ "server": { "name": "s" }, "tools": [tool(json!({}))] }));
    let (code, err) = start_and_stop(&m, &[]);
    assert_eq!(code, Some(2));
    assert!(err.contains("can be written by its group or by others"), "{err}");
    std::fs::set_permissions(d.join("journals"), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(start_and_stop(&m, &[]).0, Some(0));
    // One server per journal directory.
    let c = Mcp::start(&m, &[]);
    std::thread::sleep(std::time::Duration::from_millis(500));
    let (code, err) = start_and_stop(&m, &[]);
    assert_eq!(code, Some(2));
    assert!(err.contains("another process is using"), "{err}");
    c.close();
}

#[test]
fn a_notify_variable_may_not_hold_a_secret_a_tool_uses() {
    let dir = case_dir("notify-env");
    write(&dir, "s.staal", "fn main(key: Secret) -> Int {\n    return 1\n}\n");
    std::fs::write(dir.join("n.sh"), "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(dir.join("n.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let m = write_manifest(&dir, json!({ "server": { "name": "s", "notify": ["n.sh"], "notify_env": ["STAAL_MCP_TEST_KEY"] },
        "tools": [{ "name": "a", "description": "d", "program": "s.staal", "secrets": { "key": { "env": "STAAL_MCP_TEST_KEY" } } }] }));
    let (code, err) = start_and_stop(&m, &[("STAAL_MCP_TEST_KEY", KEY)]);
    assert_eq!(code, Some(2));
    assert!(err.contains("is one of the notify command's variables, and holds a secret this tool uses"), "{err}");
}

#[test]
fn annotations_never_claim_more_than_the_broker_enforces() {
    let s = Server::start();
    let dir = case_dir("annotations");
    write(&dir, "pure.staal", "fn plus(a: Int, b: Int) -> Int {\n    return a + b\n}\n");
    write(&dir, "get.staal", &slow_program(&s.url(""), 1));
    write(&dir, "read.staal", "fn main(path: Text, files: cap fs.read) -> Text {\n    return files.read(path)\n}\n");
    write(&dir, "post.staal", "fn main(n: Int, get: cap net.http.get, post: cap net.http.post) -> Int {\n    return n\n}\n");
    write(&dir, "refund.staal", REFUND);
    let tools = json!([
        { "name": "pure", "description": "d", "program": "pure.staal", "entry": "plus" },
        { "name": "get", "description": "d", "program": "get.staal", "grants": { "get": { "cap": "net.http.get", "only": [s.url("/slow")] } }, "net": net() },
        { "name": "read", "description": "d", "program": "read.staal", "grants": { "files": { "cap": "fs.read", "only": ["/tmp/*"] } } },
        { "name": "post", "description": "d", "program": "post.staal", "net": net(),
          "grants": { "get": { "cap": "net.http.get" }, "post": { "cap": "net.http.post", "idempotency_key": true } } },
        refund_tool(json!({})),
        refund_tool(json!({ "name": "refund_once", "once": { "by": ["order"] } }))
    ]);
    let m = write_manifest(&dir, json!({ "server": { "name": "s" }, "tools": tools }));
    let mut c = Mcp::start(&m, &[]);
    let l = c.request(json!(1), "tools/list", json!({}));
    let a = |name: &str| l["result"]["tools"].as_array().unwrap().iter().find(|t| t["name"] == name).unwrap()["annotations"].clone();
    let hints = |name: &str| {
        let x = a(name);
        (x["readOnlyHint"].as_bool().unwrap(), x["destructiveHint"].as_bool().unwrap(), x["idempotentHint"].as_bool().unwrap(), x["openWorldHint"].as_bool().unwrap())
    };
    assert_eq!(hints("pure"), (true, false, true, false));
    assert_eq!(hints("get"), (true, false, true, true));
    assert_eq!(hints("read"), (true, false, true, false));
    // An idempotency key makes one run's retries safe, not a second call: not idempotent.
    assert_eq!(hints("post"), (false, true, false, true));
    assert_eq!(hints("refund_order"), (false, true, false, true));
    // `once` keeps its key for a time (here server.keep, 90 days): after it, the same
    // call runs again, so the tool is not idempotent.
    assert_eq!(hints("refund_once"), (false, true, false, true));
    assert_eq!(a("refund_order")["title"], "Refund an order");
    c.close();
    // Kept for ever, it is.
    let m = write_manifest(&dir, json!({ "server": { "name": "s", "keep": "forever" },
        "tools": [refund_tool(json!({ "name": "refund_once", "once": { "by": ["order"] } }))] }));
    let mut c = Mcp::start(&m, &[]);
    let l = c.request(json!(1), "tools/list", json!({}));
    assert_eq!(l["result"]["tools"][0]["annotations"]["idempotentHint"], true);
    c.close();
}

#[test]
fn a_deadline_stops_a_run_before_its_next_action_on_both_engines() {
    let s = Server::start();
    let dir = case_dir("deadline");
    write(&dir, "slow.staal", &slow_program(&s.url(""), 5));
    let tool = |name: &str, engine: &str| json!({
        "name": name, "description": "Slow.", "program": "slow.staal", "engine": engine, "deadline": "1s",
        "grants": { "get": { "cap": "net.http.get", "only": [s.url("/slow")] } }, "net": net()
    });
    let mut tools = vec![tool("i", "interpreter")];
    if cfg!(feature = "native") {
        tools.push(tool("n", "native"));
    }
    let names: Vec<String> = tools.iter().map(|t| t["name"].as_str().unwrap().to_string()).collect();
    let m = write_manifest(&dir, json!({ "server": { "name": "s" }, "tools": tools }));
    let mut c = Mcp::start(&m, &[]);
    for (i, name) in names.iter().enumerate() {
        let before = s.requests().len();
        let r = c.call(i as u64, name, json!({ "n": 1 }));
        assert_eq!(r["isError"], true);
        assert!(text(&r).starts_with("deadline: the run passed its deadline of 1000 ms"), "{r}");
        // 400 ms each: the fourth cannot start before 1 s has passed, and on a loaded
        // machine fewer may have; but at least one starts, and none after the deadline.
        let made = s.requests().len() - before;
        assert!((1..=3).contains(&made), "{made} requests");
    }
    c.close();
}

#[test]
fn a_call_cancelled_after_its_run_started_gets_no_answer_and_its_run_ends() {
    let s = Server::start();
    let dir = case_dir("cancel");
    write(&dir, "slow.staal", &slow_program(&s.url(""), 2));
    let m = write_manifest(&dir, json!({ "server": { "name": "s" }, "tools": [{ "name": "slow", "description": "Slow.", "program": "slow.staal",
        "grants": { "get": { "cap": "net.http.get", "only": [s.url("/slow")] } }, "net": net() }] }));
    let mut c = Mcp::start(&m, &[]);
    let mut params = json!({ "name": "slow", "arguments": { "n": 7 } });
    params["_meta"] = meta();
    params["_meta"]["progressToken"] = json!("p1");
    c.send(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": params }));
    let progress = c.wait_for(10, |m| m["method"] == "notifications/progress").expect("progress at the start");
    assert_eq!(progress["params"]["progressToken"], "p1");
    let msg = progress["params"]["message"].as_str().unwrap();
    let run = msg.strip_prefix("run ").unwrap()[..32].to_string();
    c.send(&json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": { "requestId": 1, "reason": "changed my mind" } }));
    assert!(c.wait_for(3, |m| m["id"] == 1).is_none(), "nothing more for a cancelled request");
    let st = c.call(2, "staal_run_status", json!({ "run": run }));
    assert_eq!(st["structuredContent"]["status"], "done", "the run ended as it would have: {st}");
    assert_eq!(st["structuredContent"]["result"], 7);
    assert_eq!(s.requests().len(), 2);
    c.close();
}

#[test]
fn settled_runs_files_go_after_server_keep_and_never_before() {
    let dir = case_dir("keep");
    write(&dir, "refund.staal", REFUND);
    let m = write_manifest(&dir, json!({ "server": { "name": "s", "keep": 1 }, "tools": [refund_tool(json!({ "quota": { "window": "30d", "calls": 100 } })),
        refund_tool(json!({ "name": "other" }))] }));
    let mut c = Mcp::start(&m, &[]);
    assert_eq!(c.call(1, "refund_order", json!({ "order": "1", "amount": usd("1.00") }))["isError"], false);
    assert_eq!(c.call(2, "other", json!({ "order": "1", "amount": usd("1.00") }))["isError"], false);
    let (_, err, _) = c.close();
    let ids: Vec<String> = err.lines().filter(|l| l.contains(" run ") && l.contains("done")).map(|l| l.split(" run ").nth(1).unwrap()[..32].to_string()).collect();
    // A fresh start keeps everything: nothing is a day old.
    assert_eq!(start_and_stop(&m, &[]).0, Some(0));
    assert_eq!(files_under(&dir.join("journals/refund_order")).len(), 2);
    // Move the ledger's settlements back three days, as if they were that old.
    let ledger = dir.join("journals/server.ledger");
    let old = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64 - 3 * 86_400_000;
    let text = std::fs::read_to_string(&ledger).unwrap();
    // Every event moved by the same amount, so their order and spacing are kept.
    let first = text.lines().map(|l| serde_json::from_str::<Json>(l).unwrap()["t"].as_u64().unwrap()).min().unwrap();
    let moved: Vec<String> = text.lines().map(|l| {
        let mut v: Json = serde_json::from_str(l).unwrap();
        v["t"] = json!(v["t"].as_u64().unwrap() - first + old);
        v.to_string()
    }).collect();
    std::fs::write(&ledger, moved.join("\n") + "\n").unwrap();
    let (code, err) = start_and_stop(&m, &[]);
    assert_eq!(code, Some(0), "{err}");
    assert!(files_under(&dir.join("journals/refund_order")).is_empty() && files_under(&dir.join("journals/other")).is_empty());
    let mut c = Mcp::start(&m, &[]);
    // Within its tool's 30-day quota window, the run is still known: expired.
    let statuses: Vec<String> = ids.iter().enumerate().map(|(i, id)| c.call(i as u64, "staal_run_status", json!({ "run": id }))["structuredContent"]["status"].as_str().unwrap().to_string()).collect();
    let mut sorted = statuses.clone();
    sorted.sort();
    assert_eq!(sorted, ["expired", "unknown"], "{statuses:?}");
    c.close();
}

#[test]
fn what_an_earlier_process_left_is_settled_or_refused_never_guessed() {
    let dir = case_dir("leftovers");
    write(&dir, "refund.staal", REFUND);
    let m = write_manifest(&dir, json!({ "server": { "name": "s" }, "tools": [refund_tool(json!({}))] }));
    assert_eq!(start_and_stop(&m, &[]).0, Some(0));
    // A journal no reservation explains.
    let stray = dir.join("journals/refund_order/ab");
    std::fs::create_dir_all(&stray).unwrap();
    let id = format!("ab{}", "0".repeat(30));
    std::fs::write(stray.join(format!("{id}.journal")), "").unwrap();
    let (code, err) = start_and_stop(&m, &[]);
    assert_eq!(code, Some(2));
    assert!(err.contains("has no reservation in the ledger"), "{err}");
    std::fs::remove_dir_all(&stray).unwrap();
    // A reservation whose run never got a journal is released.
    let ledger = dir.join("journals/server.ledger");
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
    std::fs::write(&ledger, format!("{{\"e\":\"reserve\",\"run\":\"{id}\",\"tool\":\"refund_order\",\"t\":{now}}}\n")).unwrap();
    let (code, err) = start_and_stop(&m, &[]);
    assert_eq!(code, Some(0), "{err}");
    assert!(err.contains("released; it had not started"), "{err}");
    // Released, and then forgotten: a run that never started holds nothing.
    assert!(!std::fs::read_to_string(&ledger).unwrap().contains(&id));
    // An unfinished run of a tool whose program changed refuses to resume, by name.
    let mut c = Mcp::start(&m, &[]);
    let r = c.call(1, "refund_order", json!({ "order": "1", "amount": usd("1.00") }));
    assert_eq!(r["isError"], false);
    c.close();
    // As if the server died after the run ended and before it was settled: no
    // settle, and the run file still there (it goes only once a run is settled).
    let text = std::fs::read_to_string(&ledger).unwrap();
    let kept: Vec<&str> = text.lines().filter(|l| !l.contains("\"settle\"")).collect();
    std::fs::write(&ledger, kept.join("\n") + "\n").unwrap();
    let journal = files_under(&dir.join("journals/refund_order")).into_iter().find(|p| p.extension().is_some_and(|e| e == "journal")).unwrap();
    let config = json!({
        "entry": "main",
        "grants": { "pay": { "cap": "payments.refund", "each": usd("50.00"), "uses": 1 } },
        "inputs": { "order": "1", "amount": usd("1.00") }
    });
    let run_file = json!({ "program": dir.join("refund.staal"), "engine": "interpreter", "config": config });
    std::fs::write(journal.with_extension("run.json"), run_file.to_string()).unwrap();
    write(&dir, "refund.staal", &REFUND.replace("Refunded ", "Refund of "));
    let (code, err) = start_and_stop(&m, &[]);
    assert_eq!(code, Some(2));
    assert!(err.contains("is unfinished and cannot be resumed") && err.contains("different program"), "{err}");
    // Put back as it was, the run is replayed (not run again) and settled.
    write(&dir, "refund.staal", REFUND);
    let (code, err) = start_and_stop(&m, &[]);
    assert_eq!(code, Some(0), "{err}");
    assert!(err.contains("resuming") && err.contains(": done;"), "{err}");
    assert_eq!(std::fs::read_to_string(&ledger).unwrap().matches("\"settle\"").count(), 1);
    // The usage statement is the same on replay: what the run charged.
    assert!(std::fs::read_to_string(&ledger).unwrap().contains("\"amount\":\"1.00\""));
}

#[cfg(feature = "native")]
#[test]
fn the_native_engine_answers_as_the_interpreter_does() {
    let dir = case_dir("native");
    write(&dir, "pure.staal", "fn fib(n: Int) -> Int {\n    if n < 2 {\n        return n\n    }\n    return fib(n - 1) + fib(n - 2)\n}\n");
    let t = |name: &str, engine: &str| json!({ "name": name, "description": "d", "program": "pure.staal", "entry": "fib", "engine": engine });
    let m = write_manifest(&dir, json!({ "server": { "name": "s" }, "tools": [t("i", "interpreter"), t("n", "native")] }));
    let mut c = Mcp::start(&m, &[]);
    let a = c.call(1, "i", json!({ "n": 20 }));
    let b = c.call(2, "n", json!({ "n": 20 }));
    assert_eq!(a["structuredContent"]["result"], 6765);
    assert_eq!(a, b);
    c.close();
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[test]
fn a_c_library_writing_to_standard_output_never_reaches_the_stream() {
    let dir = case_dir("ffi-stdout");
    let ext = if cfg!(target_os = "macos") { "dylib" } else { "so" };
    let lib = dir.join(format!("libstaaltest.{ext}"));
    let o = Command::new("cc")
        .args(["-shared", "-fPIC", "-O1", "-o"])
        .arg(&lib)
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/ffi/staaltest.c"))
        .output()
        .expect("a C compiler (cc) builds the test library");
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let sha = staalc::packages::sha256_hex(&std::fs::read(&lib).unwrap());
    write(
        &dir,
        "c.staal",
        "foreign ffi.staaltest {\n    fn append_line(path: text, line: text) -> i64\n}\n\nfn main(line: Text, w: cap ffi.staaltest.append_line) -> Int {\n    return w.append_line(\"/dev/stdout\", line)\n}\n",
    );
    let m = write_manifest(&dir, json!({ "server": { "name": "s" }, "tools": [{ "name": "c", "description": "Writes a line.", "program": "c.staal",
        "grants": { "w": { "cap": "ffi.staaltest.append_line", "library": lib, "sha256": sha } } }] }));
    let mut c = Mcp::start(&m, &[]);
    let forged = r#"{"jsonrpc": "2.0", "id": 98, "result": {}}"#;
    assert_eq!(c.call(1, "c", json!({ "line": forged }))["structuredContent"]["result"], 1);
    let (_, err, raw) = c.close();
    assert!(err.contains(forged), "the C library's line went to standard error: {err}");
    assert!(err.contains("unchecked"), "a run that loaded C code is logged as unchecked: {err}");
    assert!(err.contains("calls C code, which no timeout or deadline can stop"), "the operator is warned at start: {err}");
    assert_eq!(raw.len(), 1, "only the answer reached the stream: {raw:?}");
    assert!(!raw[0].contains("\"id\":98") && !raw[0].contains("\"id\": 98"));
}
