// `staalc mcp` over Streamable HTTP (docs/MCP.md section 12), stage 1: every check of
// section 12.3 in its order, answers as JSON and as an SSE stream, the time and size
// limits, TLS, and a clean stop on SIGTERM that still answers the call in progress.
// Requests are written byte by byte, so each malformed one is exactly what is sent.
//
// A release build refuses `server.http` until OAuth is built; the last test checks so
// in the build without the `failpoints` feature.

// Without `failpoints` only the last test runs; the helpers serve the others.
#![cfg_attr(not(feature = "failpoints"), allow(dead_code, unused_imports))]

mod common;

use common::mcp::{case_dir, write_manifest, BIN};
use common::server::Server;
use serde_json::{json, Value as Json};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const REFUND: &str = r#"fn main(order: Text, amount: Money, pay: cap payments.refund(each: 500 USD)) -> Text {
    let receipt = pay.refund(order, amount)
    return "Refunded " + to_text(amount) + " for order " + order
}
"#;

fn usd(a: &str) -> Json {
    json!({ "amount": a, "currency": "USD" })
}

fn free_address() -> String {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().to_string()
}

/// A server over HTTP, stopped with SIGTERM when dropped.
struct Http {
    child: Child,
    stderr: std::sync::Arc<std::sync::Mutex<String>>,
}

impl Http {
    /// Starts `staalc mcp` on `manifest` and waits until it accepts connections.
    fn start(manifest: &Path, _addr: &str, env: &[(&str, &str)]) -> Http {
        let mut cmd = Command::new(BIN);
        cmd.arg("mcp").arg(manifest).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().unwrap();
        let mut pipe = child.stderr.take().unwrap();
        let stderr = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let e2 = stderr.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = pipe.read(&mut buf) {
                if n == 0 {
                    break;
                }
                e2.lock().unwrap().push_str(&String::from_utf8_lossy(&buf[..n]));
            }
        });
        let start = Instant::now();
        while !stderr.lock().unwrap().contains("serving") {
            if let Ok(Some(status)) = child.try_wait() {
                panic!("the server stopped ({status}): {}", stderr.lock().unwrap());
            }
            assert!(start.elapsed() < Duration::from_secs(30), "the server never started: {}", stderr.lock().unwrap());
            std::thread::sleep(Duration::from_millis(20));
        }
        Http { child, stderr }
    }

    /// Sends SIGTERM and returns the exit code and standard error.
    fn stop(mut self) -> (Option<i32>, String) {
        // SAFETY: kill with a valid signal on our own child's pid.
        unsafe { libc::kill(self.child.id() as i32, libc::SIGTERM) };
        let status = self.child.wait().unwrap();
        let err = self.stderr.lock().unwrap().clone();
        (status.code(), err)
    }
}

impl Drop for Http {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// One HTTP answer.
#[derive(Debug)]
struct Answer {
    status: u16,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

impl Answer {
    fn json(&self) -> Json {
        serde_json::from_slice(&self.body).unwrap_or_else(|e| panic!("not JSON ({e}): {}", String::from_utf8_lossy(&self.body)))
    }
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }
}

/// Reads one answer from `r`: the status line, the headers, and a body by its
/// Content-Length, chunked, or to the end of the connection.
fn read_answer(r: &mut impl Read) -> Option<Answer> {
    let mut head = Vec::new();
    let mut b = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        match r.read(&mut b) {
            Ok(1) => head.push(b[0]),
            _ => return None,
        }
    }
    let head = String::from_utf8(head).unwrap();
    let mut lines = head.split("\r\n");
    let status: u16 = lines.next().unwrap().split(' ').nth(1).unwrap().parse().unwrap();
    let mut headers = HashMap::new();
    for l in lines.filter(|l| !l.is_empty()) {
        let (k, v) = l.split_once(':').unwrap();
        headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
    }
    let mut body = Vec::new();
    if let Some(n) = headers.get("content-length") {
        body.resize(n.parse().unwrap(), 0);
        r.read_exact(&mut body).unwrap();
    } else if headers.get("transfer-encoding").is_some_and(|t| t.eq_ignore_ascii_case("chunked")) {
        loop {
            let mut size = Vec::new();
            while !size.ends_with(b"\r\n") {
                r.read_exact(&mut b).unwrap();
                size.push(b[0]);
            }
            let n = usize::from_str_radix(std::str::from_utf8(&size).unwrap().trim(), 16).unwrap();
            let mut chunk = vec![0u8; n + 2];
            r.read_exact(&mut chunk).unwrap();
            if n == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..n]);
        }
    } else if status != 204 && status != 202 && status != 304 {
        let _ = r.read_to_end(&mut body);
    }
    Some(Answer { status, headers, body })
}

/// Sends `raw` bytes on a new connection and reads one answer.
fn send_raw(addr: &str, raw: &[u8]) -> Option<Answer> {
    let mut c = TcpStream::connect(addr).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    c.write_all(raw).unwrap();
    read_answer(&mut c)
}

/// A POST to /mcp with every header a 2026-07-28 client sends, `extra` added after
/// (a header named with an empty value is left out).
fn request_bytes(addr: &str, body: &Json, extra: &[(&str, &str)]) -> Vec<u8> {
    let text = body.to_string();
    let mut h: Vec<(String, String)> = vec![
        ("Host".into(), addr.into()),
        ("Content-Type".into(), "application/json".into()),
        ("Accept".into(), "application/json, text/event-stream".into()),
        ("MCP-Protocol-Version".into(), "2026-07-28".into()),
    ];
    if let Some(m) = body["method"].as_str() {
        h.push(("Mcp-Method".into(), m.into()));
        if m == "tools/call" {
            h.push(("Mcp-Name".into(), body["params"]["name"].as_str().unwrap_or_default().into()));
        }
    }
    for (k, v) in extra {
        h.retain(|(n, _)| !n.eq_ignore_ascii_case(k));
        if !v.is_empty() {
            h.push((k.to_string(), v.to_string()));
        }
    }
    let mut out = String::from("POST /mcp HTTP/1.1\r\n");
    for (k, v) in &h {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str(&format!("Content-Length: {}\r\nConnection: close\r\n\r\n{text}", text.len()));
    out.into_bytes()
}

fn post(addr: &str, body: &Json, extra: &[(&str, &str)]) -> Answer {
    send_raw(addr, &request_bytes(addr, body, extra)).expect("an answer")
}

fn meta() -> Json {
    json!({ "io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {} })
}

fn rpc(id: i64, method: &str, mut params: Json) -> Json {
    params["_meta"] = meta();
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

/// A case folder holding the refund program and a manifest serving it over HTTP on
/// `addr`, with `http` keys added to (or replacing) the defaults.
fn case(name: &str, addr: &str, http: Json, tools: Option<Json>) -> (PathBuf, PathBuf) {
    let dir = case_dir(&format!("http-{name}"));
    std::fs::write(dir.join("refund.staal"), REFUND).unwrap();
    let mut h = json!({ "listen": addr, "resource": "https://mcp.shop.example/mcp", "auth": "none" });
    for (k, v) in http.as_object().unwrap() {
        if v.is_null() {
            h.as_object_mut().unwrap().remove(k);
        } else {
            h[k] = v.clone();
        }
    }
    let tools = tools.unwrap_or_else(|| {
        json!([{ "name": "refund_order", "description": "Refund part of an order.", "program": "refund.staal",
                 "grants": { "pay": { "cap": "payments.refund", "each": usd("50.00"), "uses": 1 } } }])
    });
    let m = write_manifest(&dir, json!({ "server": { "name": "shop", "http": h }, "tools": tools }));
    (dir, m)
}

fn refund_call(id: i64) -> Json {
    rpc(id, "tools/call", json!({ "name": "refund_order", "arguments": { "order": "o-1", "amount": usd("10.00") } }))
}

#[test]
#[cfg(feature = "failpoints")]
fn requests_are_served_as_over_stdio() {
    let addr = free_address();
    let (_dir, m) = case("served", &addr, json!({}), None);
    let s = Http::start(&m, &addr, &[]);

    let d = post(&addr, &rpc(1, "server/discover", json!({})), &[]);
    assert_eq!(d.status, 200);
    assert_eq!(d.header("content-type"), Some("application/json"));
    let d = d.json();
    assert_eq!(d["id"], 1);
    assert_eq!(d["result"]["supportedVersions"], json!(["2026-07-28"]));
    assert_eq!(d["result"]["resultType"], "complete");

    let l = post(&addr, &rpc(2, "tools/list", json!({})), &[]).json();
    let names: Vec<&str> = l["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["refund_order", "staal_run_status"]);
    assert_eq!(l["result"]["cacheScope"], "private");

    let c = post(&addr, &refund_call(3), &[]);
    assert_eq!(c.status, 200);
    let c = c.json();
    assert_eq!(c["result"]["isError"], false, "{c}");
    assert_eq!(c["result"]["structuredContent"]["result"], "Refunded 10.00 USD for order o-1");

    // A run's failure is a tool result, so 200 too.
    let over = rpc(4, "tools/call", json!({ "name": "refund_order", "arguments": { "order": "o-2", "amount": usd("300.00") } }));
    let r = post(&addr, &over, &[]);
    assert_eq!(r.status, 200);
    assert_eq!(r.json()["result"]["isError"], true);

    // Invalid parameters of a served method: a JSON-RPC error, as over stdio.
    let r = post(&addr, &rpc(5, "tools/call", json!({ "name": "no_such_tool" })), &[]);
    assert_eq!((r.status, r.json()["error"]["code"].as_i64()), (200, Some(-32602)));

    // A session id is ignored, and none is ever sent.
    let r = post(&addr, &rpc(6, "tools/list", json!({})), &[("Mcp-Session-Id", "abc")]);
    assert_eq!(r.status, 200);
    assert!(r.header("mcp-session-id").is_none());

    let (code, err) = s.stop();
    assert_eq!(code, Some(0), "{err}");
    assert!(err.contains("SIGTERM"), "{err}");
}

#[test]
#[cfg(feature = "failpoints")]
fn each_check_refuses_with_its_own_answer_in_order() {
    let addr = free_address();
    let (_dir, m) = case("checks", &addr, json!({ "allowed_origins": ["https://app.shop.example"] }), None);
    let _s = Http::start(&m, &addr, &[]);
    let list = rpc(1, "tools/list", json!({}));
    let code = |a: &Answer| a.json()["error"]["code"].as_i64();

    // Host: another one is misdirected; none (or two) is a bad request.
    let a = post(&addr, &list, &[("Host", "evil.example")]);
    assert_eq!(a.status, 421);
    assert_eq!(a.json()["id"], Json::Null);
    let a = send_raw(&addr, b"POST /mcp HTTP/1.0\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}").unwrap();
    assert_eq!(a.status, 400);
    // The other names the server answers for by default on loopback.
    let port = addr.rsplit_once(':').unwrap().1;
    assert_eq!(post(&addr, &list, &[("Host", &format!("localhost:{port}"))]).status, 200);
    assert_eq!(post(&addr, &list, &[("Host", "mcp.shop.example")]).status, 200);
    assert_eq!(post(&addr, &list, &[("Host", "MCP.Shop.Example")]).status, 200, "host names compare case-insensitively");

    // Origin: absent is fine, listed is fine, anything else is forbidden.
    assert_eq!(post(&addr, &list, &[("Origin", "https://app.shop.example")]).status, 200);
    for bad in ["https://evil.example", "null", "https://app.shop.example:8443", "http://app.shop.example"] {
        let a = post(&addr, &list, &[("Origin", bad)]);
        assert_eq!((a.status, a.json()["id"].clone()), (403, Json::Null), "{bad}");
    }

    // The path, then the method.
    let mut raw = request_bytes(&addr, &list, &[]);
    raw.splice(5..9, b"/mcpx".iter().copied());
    assert_eq!(send_raw(&addr, &raw).unwrap().status, 404);
    for m in ["GET", "DELETE", "PUT"] {
        let a = send_raw(&addr, format!("{m} /mcp HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n").as_bytes()).unwrap();
        assert_eq!((a.status, a.header("allow")), (405, Some("POST")), "{m}");
    }

    // Content-Type and Accept.
    for ct in ["text/plain", "application/json; charset=latin1", ""] {
        assert_eq!(post(&addr, &list, &[("Content-Type", ct)]).status, 415, "{ct:?}");
    }
    assert_eq!(post(&addr, &list, &[("Content-Type", "application/json; charset=UTF-8")]).status, 200);
    for acc in ["application/json", "text/event-stream", "application/json, text/event-stream;q=0", ""] {
        assert_eq!(post(&addr, &list, &[("Accept", acc)]).status, 406, "{acc:?}");
    }
    assert_eq!(post(&addr, &list, &[("Accept", "*/*")]).status, 200);

    // The protocol version header: required, then supported.
    let a = post(&addr, &list, &[("MCP-Protocol-Version", "")]);
    assert_eq!((a.status, code(&a)), (400, Some(-32020)));
    let a = post(&addr, &list, &[("MCP-Protocol-Version", "2025-11-25")]);
    assert_eq!((a.status, code(&a)), (400, Some(-32022)));
    assert_eq!(a.json()["error"]["data"]["supported"], json!(["2026-07-28"]));

    // The body: one request or notification, as JSON.
    let bad_body = |text: &str| {
        let raw = format!(
            "POST /mcp HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\n\
             MCP-Protocol-Version: 2026-07-28\r\nMcp-Method: tools/list\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
            text.len()
        );
        send_raw(&addr, raw.as_bytes()).unwrap()
    };
    let a = bad_body("{not json");
    assert_eq!((a.status, code(&a)), (400, Some(-32700)));
    let a = bad_body(&format!("[{list}]"));
    assert_eq!((a.status, code(&a)), (400, Some(-32600)), "a batch");
    let a = bad_body(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#);
    assert_eq!((a.status, code(&a)), (400, Some(-32600)), "a response");
    let a = bad_body(&format!("{}1{}", "[".repeat(100), "]".repeat(100)));
    assert_eq!((a.status, code(&a)), (400, Some(-32600)), "too deep");

    // The headers must say what the body says.
    let a = post(&addr, &list, &[("Mcp-Method", "tools/call")]);
    assert_eq!((a.status, code(&a), a.json()["id"].clone()), (400, Some(-32020), json!(1)));
    let a = post(&addr, &list, &[("Mcp-Method", "")]);
    assert_eq!((a.status, code(&a)), (400, Some(-32020)));
    let mut other = list.clone();
    other["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"] = json!("2026-01-01");
    let a = post(&addr, &other, &[]);
    assert_eq!((a.status, code(&a)), (400, Some(-32020)));
    let call = rpc(2, "tools/call", json!({ "name": "refund_order", "arguments": { "order": "o-9", "amount": usd("1.00") } }));
    let a = post(&addr, &call, &[("Mcp-Name", "")]);
    assert_eq!((a.status, code(&a)), (400, Some(-32020)), "Mcp-Name is required for tools/call");
    let a = post(&addr, &call, &[("Mcp-Name", "staal_run_status")]);
    assert_eq!((a.status, code(&a)), (400, Some(-32020)));
    // The base64 form names it too: "refund_order".
    let a = post(&addr, &call, &[("Mcp-Name", "=?base64?cmVmdW5kX29yZGVy?=")]);
    assert_eq!(a.status, 200, "{:?}", String::from_utf8_lossy(&a.body));

    // Methods the server does not serve over HTTP: 404 with -32601.
    for m in ["initialize", "ping", "resources/read", "prompts/get", "subscriptions/listen"] {
        let a = post(&addr, &rpc(3, m, json!({})), &[]);
        assert_eq!((a.status, code(&a)), (404, Some(-32601)), "{m}");
    }

    // A notification: 202 and nothing more.
    let n = json!({ "jsonrpc": "2.0", "method": "notifications/initialized", "params": {} });
    let a = post(&addr, &n, &[]);
    assert_eq!((a.status, a.body.len()), (202, 0));
}

#[test]
#[cfg(feature = "failpoints")]
fn bodies_are_framed_by_hyper_and_bounded() {
    let addr = free_address();
    let (_dir, m) = case("bodies", &addr, json!({}), None);
    let _s = Http::start(&m, &addr, &[("STAALC_TEST_HTTP_TIMES_MS", "500,500")]);
    let list = rpc(1, "tools/list", json!({})).to_string();
    let head = |extra: &str| {
        format!(
            "POST /mcp HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\n\
             MCP-Protocol-Version: 2026-07-28\r\nMcp-Method: tools/list\r\nConnection: close\r\n{extra}"
        )
    };
    // A chunked body.
    let (a, b) = list.split_at(10);
    let raw = format!("{}Transfer-Encoding: chunked\r\n\r\n{:x}\r\n{a}\r\n{:x}\r\n{b}\r\n0\r\n\r\n", head(""), a.len(), b.len());
    assert_eq!(send_raw(&addr, raw.as_bytes()).unwrap().status, 200);
    // Content-Length and Transfer-Encoding together, with a second request smuggled
    // after: refused (RFC 9112 section 6.3), and the second never served.
    let smuggled = request_bytes(&addr, &rpc(9, "tools/list", json!({})), &[]);
    let raw = format!("{}Content-Length: 4\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n{}", head(""), String::from_utf8_lossy(&smuggled));
    let mut c = TcpStream::connect(&addr).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    c.write_all(raw.as_bytes()).unwrap();
    let first = read_answer(&mut c).unwrap();
    assert_eq!(first.status, 400, "{first:?}");
    assert!(read_answer(&mut c).is_none(), "the smuggled request was served");

    // Over 4 MiB by its Content-Length: refused before reading.
    let raw = format!("{}Content-Length: {}\r\n\r\n", head(""), 4 * 1024 * 1024 + 1);
    let a = send_raw(&addr, raw.as_bytes()).unwrap();
    assert_eq!((a.status, a.header("connection")), (413, Some("close")));
    // Over 4 MiB as it arrives (chunked, no length given).
    let mut c = TcpStream::connect(&addr).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    c.write_all(format!("{}Transfer-Encoding: chunked\r\n\r\n", head("")).as_bytes()).unwrap();
    let chunk = vec![b' '; 1024 * 1024];
    for _ in 0..5 {
        if c.write_all(format!("{:x}\r\n", chunk.len()).as_bytes()).and_then(|_| c.write_all(&chunk)).and_then(|_| c.write_all(b"\r\n")).is_err() {
            break;
        }
    }
    assert_eq!(read_answer(&mut c).unwrap().status, 413);

    // A body that stops coming: 408 at the body limit (500 ms here).
    let mut c = TcpStream::connect(&addr).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    c.write_all(format!("{}Content-Length: 100\r\n\r\n{{\"jsonrpc\"", head("")).as_bytes()).unwrap();
    let start = Instant::now();
    assert_eq!(read_answer(&mut c).unwrap().status, 408);
    assert!(start.elapsed() >= Duration::from_millis(450), "{:?}", start.elapsed());

    // Headers that stop coming, or an idle connection: closed at the header limit.
    for partial in [&b"POST /mcp HTTP/1.1\r\nHost: x"[..], &b""[..]] {
        let mut c = TcpStream::connect(&addr).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        c.write_all(partial).unwrap();
        let start = Instant::now();
        let mut buf = Vec::new();
        let _ = c.read_to_end(&mut buf);
        let took = start.elapsed();
        assert!(took >= Duration::from_millis(450) && took < Duration::from_secs(5), "{took:?}");
        assert!(!String::from_utf8_lossy(&buf).contains("200 OK"));
    }
}

#[test]
#[cfg(feature = "failpoints")]
fn a_call_that_asks_for_progress_is_answered_as_a_stream() {
    let addr = free_address();
    let (_dir, m) = case("stream", &addr, json!({}), None);
    let _s = Http::start(&m, &addr, &[]);
    let mut call = refund_call(7);
    call["params"]["_meta"]["progressToken"] = json!("p1");
    let a = post(&addr, &call, &[]);
    assert_eq!(a.status, 200);
    assert_eq!(a.header("content-type"), Some("text/event-stream"));
    assert_eq!(a.header("x-accel-buffering"), Some("no"));
    let text = String::from_utf8(a.body).unwrap();
    let events: Vec<Json> = text
        .split("\n\n")
        .filter(|e| !e.is_empty())
        .map(|e| {
            let data = e.lines().find_map(|l| l.strip_prefix("data: ")).unwrap();
            serde_json::from_str(data).unwrap()
        })
        .collect();
    assert_eq!(events.len(), 2, "{text}");
    assert_eq!(events[0]["method"], "notifications/progress");
    assert_eq!(events[0]["params"]["progressToken"], "p1");
    assert!(events[0]["params"]["message"].as_str().unwrap().starts_with("run "));
    assert_eq!(events[1]["id"], 7);
    assert_eq!(events[1]["result"]["isError"], false);
}

#[test]
#[cfg(feature = "failpoints")]
fn a_stream_closed_after_the_run_began_leaves_the_run_to_finish() {
    let fake = Server::start();
    let addr = free_address();
    let dir = case_dir("http-closed");
    let slow = format!("fn main(n: Int, get: cap net.http.get) -> Int {{\n    let a = get.get(\"{}/slow\")\n    let b = get.get(\"{}/slow\")\n    return n\n}}\n", fake.url(""), fake.url(""));
    std::fs::write(dir.join("slow.staal"), slow).unwrap();
    let tools = json!([{ "name": "slow", "description": "d", "program": "slow.staal",
        "grants": { "get": { "cap": "net.http.get" } }, "net": { "connect_timeout_ms": 2000, "timeout_ms": 5000 } }]);
    let m = write_manifest(&dir, json!({ "server": { "name": "s", "http": { "listen": addr, "resource": "https://mcp.shop.example/mcp", "auth": "none" } }, "tools": tools }));
    let s = Http::start(&m, &addr, &[]);
    let mut call = rpc(1, "tools/call", json!({ "name": "slow", "arguments": { "n": 5 } }));
    call["params"]["_meta"]["progressToken"] = json!(1);
    let mut c = TcpStream::connect(&addr).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    c.write_all(&request_bytes(&addr, &call, &[])).unwrap();
    // Read until the progress event names the run, then close the stream.
    let mut seen = Vec::new();
    let mut b = [0u8; 256];
    let run = loop {
        let n = c.read(&mut b).unwrap();
        assert!(n > 0, "the stream ended before the run began");
        seen.extend_from_slice(&b[..n]);
        let t = String::from_utf8_lossy(&seen).into_owned();
        if let Some(i) = t.find("run ") {
            break t[i + 4..i + 36].to_string();
        }
    };
    drop(c);
    // The run finishes regardless, and its result is kept.
    let status = rpc(2, "tools/call", json!({ "name": "staal_run_status", "arguments": { "run": run } }));
    let start = Instant::now();
    loop {
        let r = post(&addr, &status, &[]).json();
        let st = r["result"]["structuredContent"]["status"].as_str().unwrap_or_default().to_string();
        if st == "done" {
            assert_eq!(r["result"]["structuredContent"]["result"], 5);
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(20), "the run never finished: {r}");
        std::thread::sleep(Duration::from_millis(100));
    }
    let (code, err) = s.stop();
    assert_eq!(code, Some(0), "{err}");
}

#[test]
#[cfg(feature = "failpoints")]
fn sigterm_lets_the_call_in_progress_finish_and_be_answered() {
    let fake = Server::start();
    let addr = free_address();
    let dir = case_dir("http-sigterm");
    std::fs::write(dir.join("slow.staal"), format!("fn main(n: Int, get: cap net.http.get) -> Int {{\n    let a = get.get(\"{}/slow\")\n    return n\n}}\n", fake.url(""))).unwrap();
    let tools = json!([{ "name": "slow", "description": "d", "program": "slow.staal",
        "grants": { "get": { "cap": "net.http.get" } }, "net": { "connect_timeout_ms": 2000, "timeout_ms": 5000 } }]);
    let m = write_manifest(&dir, json!({ "server": { "name": "s", "http": { "listen": addr, "resource": "https://mcp.shop.example/mcp", "auth": "none" } }, "tools": tools }));
    let s = Http::start(&m, &addr, &[]);
    let call = rpc(1, "tools/call", json!({ "name": "slow", "arguments": { "n": 3 } }));
    let client = {
        let addr = addr.clone();
        std::thread::spawn(move || post(&addr, &call, &[]))
    };
    std::thread::sleep(Duration::from_millis(150)); // inside the 400 ms GET
    let (code, err) = s.stop();
    assert_eq!(code, Some(0), "{err}");
    let a = client.join().unwrap();
    assert_eq!(a.status, 200);
    assert_eq!(a.json()["result"]["structuredContent"]["result"], 3, "{:?}", a.json());
    // New connections are refused once stopping began.
    assert!(TcpStream::connect(&addr).is_err());
}

#[test]
#[cfg(feature = "failpoints")]
fn connections_over_the_limit_are_closed_at_once() {
    let addr = free_address();
    let (_dir, m) = case("limit", &addr, json!({ "max_connections": 1 }), None);
    let _s = Http::start(&m, &addr, &[]);
    let held = TcpStream::connect(&addr).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let mut second = TcpStream::connect(&addr).unwrap();
    second.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let _ = second.write_all(&request_bytes(&addr, &rpc(1, "tools/list", json!({})), &[]));
    let mut buf = Vec::new();
    let _ = second.read_to_end(&mut buf);
    assert!(buf.is_empty(), "{}", String::from_utf8_lossy(&buf));
    drop(held);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(post(&addr, &rpc(2, "tools/list", json!({})), &[]).status, 200);
}

#[test]
#[cfg(feature = "failpoints")]
fn it_serves_over_tls_with_the_checked_certificate() {
    let ca = common::tls::Ca::new("HTTP Test CA");
    let addr = free_address();
    let dir = case_dir("http-tls-files");
    let (chain, key) = ca.listen_files(&dir, &["localhost"], None);
    let (_d, m) = case("tls", &addr, json!({ "tls": { "certificate_chain": chain, "private_key": key } }), None);
    let s = Http::start(&m, &addr, &[]);
    let name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
    let st = TcpStream::connect(&addr).unwrap();
    st.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut t = rustls::StreamOwned::new(rustls::ClientConnection::new(ca.client(), name).unwrap(), st);
    t.write_all(&request_bytes(&addr, &rpc(1, "tools/list", json!({})), &[])).unwrap();
    let a = read_answer(&mut t).unwrap();
    assert_eq!(a.status, 200);
    assert_eq!(a.json()["result"]["tools"][0]["name"], "refund_order");
    // Plain HTTP to the TLS port gets no HTTP answer.
    assert!(send_raw(&addr, &request_bytes(&addr, &rpc(2, "tools/list", json!({})), &[])).is_none_or(|a| a.status != 200));
    let (code, err) = s.stop();
    assert_eq!(code, Some(0), "{err}");
}

#[test]
#[cfg(feature = "failpoints")]
fn the_http_settings_are_checked_before_anything_is_served() {
    let refused = |name: &str, http: Json, want: &str| {
        let (_d, m) = case(name, "127.0.0.1:1", http, None);
        let (code, err) = common::mcp::start_and_stop(&m, &[]);
        assert_eq!(code, Some(2), "{name}: {err}");
        assert!(err.contains(want), "{name}: expected {want:?} in {err}");
    };
    refused("listen-name", json!({ "listen": "localhost:8443" }), "server.http.listen: expected an IP address");
    refused("listen-zero", json!({ "listen": "127.0.0.1:0" }), "server.http.listen");
    refused("no-tls", json!({ "listen": "0.0.0.0:8443" }), "server.http.tls: required unless");
    refused("resource-http", json!({ "resource": "http://mcp.shop.example/mcp" }), "server.http.resource: expected an https URI");
    refused("resource-case", json!({ "resource": "https://MCP.shop.example/mcp" }), "canonical form");
    refused("resource-slash", json!({ "resource": "https://mcp.shop.example/mcp/" }), "server.http.resource");
    refused("resource-fragment", json!({ "resource": "https://mcp.shop.example/mcp#x" }), "server.http.resource");
    refused("resource-path", json!({ "resource": "https://mcp.shop.example/other" }), "must end with server.http.path");
    refused("path", json!({ "path": "/mcp/../x" }), "server.http.path");
    refused("auth-missing", json!({ "auth": null }), "server.http.auth: missing");
    refused("auth-oauth", json!({ "auth": { "issuer": "https://auth.example" } }), "OAuth is not built yet");
    refused("origin", json!({ "allowed_origins": ["https://app.example/"] }), "server.http.allowed_origins[0]");
    refused("host", json!({ "allowed_hosts": ["Mixed.Case"] }), "server.http.allowed_hosts[0]");
    refused("unknown", json!({ "colour": 1 }), "unknown key 'colour'");
    refused("max", json!({ "max_connections": 0 }), "server.http.max_connections");
}

#[test]
#[cfg(not(feature = "failpoints"))]
fn a_release_build_refuses_http_until_it_has_authentication() {
    let (_d, m) = case("release", "127.0.0.1:1", json!({}), None);
    let (code, err) = common::mcp::start_and_stop(&m, &[]);
    assert_eq!(code, Some(2), "{err}");
    assert!(err.contains("the HTTP transport is not finished"), "{err}");
}

