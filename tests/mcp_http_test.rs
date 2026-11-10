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

/// The principal key every test server is started with (docs/MCP.md section 12.6).
const PRINCIPAL_KEY_VAR: &str = "MCP_TEST_PRINCIPAL_KEY";
const PRINCIPAL_KEY: &str = "test-principal-key-of-at-least-32-bytes";

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
        cmd.env(PRINCIPAL_KEY_VAR, PRINCIPAL_KEY);
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

/// Starts the server on a manifest it must refuse, and returns its exit code and
/// standard error. A server that does not stop within 20 s is killed and reported as
/// having started (code None), so a check that fails to refuse fails the test instead
/// of hanging it (an HTTP server does not stop when its standard input closes).
fn refused_at_start(manifest: &Path) -> (Option<i32>, String) {
    refused_at_start_with(manifest, PRINCIPAL_KEY)
}

fn refused_at_start_with(manifest: &Path, principal_key: &str) -> (Option<i32>, String) {
    let mut child = Command::new(BIN)
        .arg("mcp")
        .arg(manifest)
        .env(PRINCIPAL_KEY_VAR, principal_key)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    let status = loop {
        if let Some(st) = child.try_wait().unwrap() {
            break st.code();
        }
        if start.elapsed() > Duration::from_secs(20) {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let mut err = String::new();
    let _ = child.stderr.take().unwrap().read_to_string(&mut err);
    (status, err)
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
    // Two slow requests (each answered after 400 ms): SIGTERM goes during the second.
    let base = fake.url("");
    std::fs::write(dir.join("slow.staal"), format!("fn main(n: Int, get: cap net.http.get) -> Int {{\n    let a = get.get(\"{base}/slow\")\n    let b = get.get(\"{base}/slow\")\n    return n\n}}\n")).unwrap();
    let tools = json!([{ "name": "slow", "description": "d", "program": "slow.staal",
        "grants": { "get": { "cap": "net.http.get" } }, "net": { "connect_timeout_ms": 2000, "timeout_ms": 5000 } }]);
    let m = write_manifest(&dir, json!({ "server": { "name": "s", "http": { "listen": addr, "resource": "https://mcp.shop.example/mcp", "auth": "none" } }, "tools": tools }));
    let s = Http::start(&m, &addr, &[]);
    let call = rpc(1, "tools/call", json!({ "name": "slow", "arguments": { "n": 3 } }));
    let client = {
        let addr = addr.clone();
        std::thread::spawn(move || post(&addr, &call, &[]))
    };
    // SIGTERM once the run is surely in progress: the service has answered its first
    // request (it records one when it answers), so the second is under way. (A fixed
    // sleep raced a loaded machine.)
    let start = Instant::now();
    while fake.requests().iter().all(|r| r.path != "/slow") {
        assert!(start.elapsed() < Duration::from_secs(20), "the run never reached the service");
        std::thread::sleep(Duration::from_millis(5));
    }
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
    // The certificate names the resource's host, which clients reach the server by, and
    // localhost, which this test connects to.
    let (chain, key) = ca.listen_files(&dir, &["mcp.shop.example", "localhost"], None);
    // One that does not name the resource's host stops the server at start.
    let other = case_dir("http-tls-other");
    let (wrong_chain, wrong_key) = ca.listen_files(&other, &["localhost"], None);
    let (_w, wm) = case("tls-wrong-name", &addr, json!({ "tls": { "certificate_chain": wrong_chain, "private_key": wrong_key } }), None);
    let (code, err) = refused_at_start(&wm);
    assert_eq!(code, Some(2), "{err}");
    assert!(err.contains("is not for mcp.shop.example"), "{err}");
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
    // The private key is in no output and no file the server wrote.
    let pem = std::fs::read_to_string(&key).unwrap();
    let body: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
    assert!(!err.contains("PRIVATE KEY") && !err.contains(&body[..40]), "the key reached the log");
    for f in common::mcp::files_under(&_d.join("journals")) {
        let text = std::fs::read_to_string(&f).unwrap_or_default();
        assert!(!text.contains(&body[..40]), "{} holds the key", f.display());
    }
}

#[test]
#[cfg(feature = "failpoints")]
fn the_http_settings_are_checked_before_anything_is_served() {
    let refused = |name: &str, http: Json, want: &str| {
        let (_d, m) = case(name, "127.0.0.1:1", http, None);
        let (code, err) = refused_at_start(&m);
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
    refused("auth-issuer", json!({ "auth": { "issuer": "http://auth.example" } }), "server.http.auth.issuer: expected an https URL");
    refused("auth-offline", json!({ "auth": { "issuer": "https://auth.example", "scopes_supported": ["offline_access"] } }), "offline_access is not a resource's scope");
    refused("auth-scope", json!({ "auth": { "issuer": "https://auth.example", "scopes_supported": ["a b"] } }), "server.http.auth.scopes_supported[0]");
    refused("auth-leeway", json!({ "auth": { "issuer": "https://auth.example", "jwt_leeway_s": 301 } }), "server.http.auth.jwt_leeway_s");
    refused("auth-unknown", json!({ "auth": { "issuer": "https://auth.example", "colour": 1 } }), "unknown key 'colour'");
    refused("origin", json!({ "allowed_origins": ["https://app.example/"] }), "server.http.allowed_origins[0]");
    refused("host", json!({ "allowed_hosts": ["Mixed.Case"] }), "server.http.allowed_hosts[0]");
    refused("unknown", json!({ "colour": 1 }), "unknown key 'colour'");
    refused("max", json!({ "max_connections": 0 }), "server.http.max_connections");
}

#[test]
#[cfg(not(feature = "failpoints"))]
fn a_release_build_refuses_http_until_it_has_authentication() {
    let (_d, m) = case("release", "127.0.0.1:1", json!({}), None);
    let (code, err) = refused_at_start(&m);
    assert_eq!(code, Some(2), "{err}");
    assert!(err.contains("the HTTP transport is not finished"), "{err}");
}


// ---- OAuth (stage 2, docs/MCP.md sections 12.4 and 12.5) ----

const RESOURCE: &str = "https://mcp.shop.example/mcp";

/// An authorization server publishing `issuer`'s key, and a case whose server trusts it,
/// requiring `refunds:write` for refund_order and nothing more for echo.
struct OAuthCase {
    _ca: common::tls::Ca,
    auth: common::authserver::AuthServer,
    issuer: common::tokens::Issuer,
    manifest: PathBuf,
    addr: String,
}

fn oauth_case(name: &str) -> OAuthCase {
    use common::authserver::{AuthServer, Route};
    let ca = common::tls::Ca::new("OAuth Test CA");
    let auth = AuthServer::start(ca.server(&["localhost"], None));
    let issuer = common::tokens::Issuer::new("k1");
    auth.set("/.well-known/oauth-authorization-server", Route::json(&json!({ "issuer": auth.base, "jwks_uri": format!("{}/jwks", auth.base) })));
    auth.set("/jwks", Route::json(&json!({ "keys": [issuer.jwk()] })));
    let addr = free_address();
    let dir = case_dir(&format!("http-oauth-{name}"));
    std::fs::write(dir.join("ca.pem"), ca.pem()).unwrap();
    std::fs::write(dir.join("refund.staal"), REFUND).unwrap();
    std::fs::write(dir.join("echo.staal"), "fn main(x: Text) -> Text {\n    return x\n}\n").unwrap();
    std::fs::write(dir.join("whoami.staal"), "fn main(customer: Text, note: Text) -> Text {\n    return customer + \": \" + note\n}\n").unwrap();
    let tools = json!([
        { "name": "refund_order", "description": "Refund part of an order.", "program": "refund.staal",
          "grants": { "pay": { "cap": "payments.refund", "each": usd("50.00"), "uses": 1 } }, "scopes": ["refunds:write"],
          "once": { "by": ["order"], "keep": "30d" } },
        { "name": "echo", "description": "Echo.", "program": "echo.staal" },
        { "name": "whoami", "description": "Who the caller is.", "program": "whoami.staal",
          "app_inputs": { "customer": { "claim": "sub" } } },
        { "name": "tenant", "description": "The caller's tenant.", "program": "whoami.staal",
          "app_inputs": { "customer": { "claim": "tenant" } } }
    ]);
    let http = json!({ "listen": addr, "resource": RESOURCE,
        "auth": { "issuer": auth.base, "scopes_supported": ["tools"], "extra_root_certificates": [dir.join("ca.pem")],
                  "principal_key": { "env": PRINCIPAL_KEY_VAR } } });
    let manifest = write_manifest(&dir, json!({ "server": { "name": "shop", "http": http }, "tools": tools }));
    OAuthCase { _ca: ca, auth, issuer, manifest, addr }
}

impl OAuthCase {
    fn token(&self, scope: &str, over: Json, header: Json) -> String {
        self.issuer.token(&common::tokens::claims(&self.auth.base, RESOURCE, "user-1", scope, &over), &header)
    }

    /// A token for subject `sub` with every scope the tools need.
    fn token_for(&self, sub: &str) -> String {
        self.issuer.token(&common::tokens::claims(&self.auth.base, RESOURCE, sub, "tools refunds:write", &json!({})), &json!({}))
    }

    fn dir(&self) -> PathBuf {
        self.manifest.parent().unwrap().to_path_buf()
    }
}

fn authed(addr: &str, body: &Json, token: &str) -> Json {
    let a = post(addr, body, &[("Authorization", &format!("Bearer {token}"))]);
    assert_eq!(a.status, 200, "{:?}", String::from_utf8_lossy(&a.body));
    a.json()
}

#[test]
#[cfg(feature = "failpoints")]
fn each_principal_has_its_own_once_keys_and_runs() {
    let c = oauth_case("principals");
    let s = Http::start(&c.manifest, &c.addr, &[]);
    let (alice, bob) = (c.token_for("alice"), c.token_for("bob"));
    let refund = |id: i64| rpc(id, "tools/call", json!({ "name": "refund_order", "arguments": { "order": "o-123", "amount": usd("10.00") } }));
    let a1 = authed(&c.addr, &refund(1), &alice);
    let a2 = authed(&c.addr, &refund(2), &alice);
    let b1 = authed(&c.addr, &refund(3), &bob);
    assert_eq!(a1["result"]["isError"], false, "{a1}");
    // The same principal sending the same operation again: answered from the same run.
    assert_eq!(a1["result"]["structuredContent"], a2["result"]["structuredContent"]);
    // Another principal, the same arguments: an operation of its own.
    assert_eq!(b1["result"]["isError"], false, "{b1}");
    let (_, first_log) = s.stop();
    let err = first_log.clone();
    let runs: Vec<&str> = err.lines().filter(|l| l.contains("tool refund_order run") && l.contains(": done;")).collect();
    assert_eq!(runs.len(), 2, "two runs, one per principal: {err}");

    // Each principal sees only its own run's status.
    let ids: Vec<String> = err
        .lines()
        .filter(|l| l.contains("called by principal"))
        .map(|l| l.split("run ").nth(1).unwrap().split(':').next().unwrap().to_string())
        .collect();
    let s = Http::start(&c.manifest, &c.addr, &[]);
    let status = |run: &str, token: &str| {
        let r = authed(&c.addr, &rpc(9, "tools/call", json!({ "name": "staal_run_status", "arguments": { "run": run } })), token);
        r["result"]["structuredContent"]["status"].as_str().unwrap().to_string()
    };
    let alices = ids[0].clone();
    assert_eq!(status(&alices, &alice), "done");
    assert_eq!(status(&alices, &bob), "unknown", "a run id is not an authorization");
    let (code, second_log) = s.stop();
    assert_eq!(code, Some(0), "{second_log}");

    // The subject identifiers reach neither the log nor the ledger.
    let ledger = std::fs::read_to_string(c.dir().join("journals/server.ledger")).unwrap();
    for who in ["alice", "bob"] {
        assert!(!first_log.contains(who) && !second_log.contains(who) && !ledger.contains(who), "{who} was written");
    }
    assert!(first_log.contains("called by principal ") && first_log.contains("(client test-client)"), "{first_log}");
}

#[test]
#[cfg(feature = "failpoints")]
fn app_inputs_come_from_the_token_and_never_from_the_caller() {
    let c = oauth_case("claims");
    let _s = Http::start(&c.manifest, &c.addr, &[]);
    let t = c.token_for("carol");
    let who = authed(&c.addr, &rpc(1, "tools/call", json!({ "name": "whoami", "arguments": { "note": "hi" } })), &t);
    assert_eq!(who["result"]["structuredContent"]["result"], "carol: hi", "{who}");
    // Not offered to the model, and refused when a caller names it.
    let list = authed(&c.addr, &rpc(2, "tools/list", json!({})), &t);
    let whoami = list["result"]["tools"].as_array().unwrap().iter().find(|x| x["name"] == "whoami").unwrap().clone();
    assert!(whoami["inputSchema"]["properties"].get("customer").is_none(), "{whoami}");
    let forged = authed(&c.addr, &rpc(3, "tools/call", json!({ "name": "whoami", "arguments": { "note": "hi", "customer": "admin" } })), &t);
    assert_eq!(forged["result"]["isError"], true, "{forged}");
    assert!(forged["result"]["content"][0]["text"].as_str().unwrap().contains("never by the caller"), "{forged}");
    // A claim the token does not have: nothing runs.
    let none = authed(&c.addr, &rpc(4, "tools/call", json!({ "name": "tenant", "arguments": { "note": "x" } })), &t);
    assert_eq!(none["result"]["isError"], true);
    assert!(none["result"]["content"][0]["text"].as_str().unwrap().contains("no text claim 'tenant'"), "{none}");
    let with = c.issuer.token(&common::tokens::claims(&c.auth.base, RESOURCE, "carol", "tools", &json!({ "tenant": "shop-9" })), &json!({}));
    let r = authed(&c.addr, &rpc(5, "tools/call", json!({ "name": "tenant", "arguments": { "note": "x" } })), &with);
    assert_eq!(r["result"]["structuredContent"]["result"], "shop-9: x");
}

/// AGENT_MANIFEST.md section 16, R7, for the app input mapping: a run killed after it
/// started resumes with the app input values it started with, even when the manifest
/// now takes that input from another claim.
#[test]
#[cfg(feature = "failpoints")]
fn a_resumed_run_keeps_its_app_inputs_when_the_mapping_changes() {
    let c = oauth_case("claims-resume");
    let mut s = Http::start(&c.manifest, &c.addr, &[("STAALC_FAILPOINT", "after_write:1")]);
    let t = c.token_for("carol");
    let call = rpc(1, "tools/call", json!({ "name": "whoami", "arguments": { "note": "hi" } }));
    let raw = request_bytes(&c.addr, &call, &[("Authorization", &format!("Bearer {t}"))]);
    assert!(send_raw(&c.addr, &raw).is_none(), "no answer before the crash");
    let status = s.child.wait().unwrap();
    assert!(!status.success(), "the server was killed: {}", s.stderr.lock().unwrap());
    drop(s);
    let mut m: Json = serde_json::from_str(&std::fs::read_to_string(&c.manifest).unwrap()).unwrap();
    let whoami = m["tools"].as_array_mut().unwrap().iter_mut().find(|x| x["name"] == "whoami").unwrap();
    whoami["app_inputs"] = json!({ "customer": { "claim": "tenant" } });
    std::fs::write(&c.manifest, serde_json::to_string_pretty(&m).unwrap()).unwrap();
    let s = Http::start(&c.manifest, &c.addr, &[]);
    let (code, err) = s.stop();
    assert_eq!(code, Some(0), "{err}");
    assert!(err.contains("tool whoami run") && err.contains(": resuming"), "{err}");
    let journals: Vec<PathBuf> = common::mcp::files_under(&c.dir().join("journals/whoami"))
        .into_iter()
        .filter(|p| p.to_string_lossy().ends_with(".journal"))
        .collect();
    assert_eq!(journals.len(), 1, "{journals:?}");
    let records: Vec<Json> = std::fs::read_to_string(&journals[0]).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let resume = records.iter().find(|r| r["j"] == "resume").expect("a resume record");
    assert_eq!(resume["config_sha256"], records[0]["config_sha256"]);
    let end = records.last().unwrap();
    assert_eq!((&end["j"], &end["ok"], &end["result"]), (&json!("end"), &json!(true), &json!("carol: hi")), "{records:?}");
}

#[test]
#[cfg(feature = "failpoints")]
fn a_changed_principal_key_cannot_hide_callers_runs() {
    let c = oauth_case("key-change");
    let s = Http::start(&c.manifest, &c.addr, &[]);
    // A `once` key held for 30 days, under the first key.
    let r = authed(&c.addr, &rpc(1, "tools/call", json!({ "name": "refund_order", "arguments": { "order": "o-1", "amount": usd("1.00") } })), &c.token_for("dave"));
    assert_eq!(r["result"]["isError"], false);
    let (code, _) = s.stop();
    assert_eq!(code, Some(0));
    let recorded = std::fs::read_to_string(c.dir().join("journals/principal-key")).unwrap();
    assert_eq!(recorded.trim().len(), 64);
    assert!(!recorded.contains(PRINCIPAL_KEY));
    // Another key: refused, naming the run that would be lost.
    let (code, err) = refused_at_start_with(&c.manifest, "another-principal-key-of-32-bytes-or-more");
    assert_eq!(code, Some(2), "{err}");
    assert!(err.contains("is not the key recorded") && err.contains("1 run(s)"), "{err}");
    // Too short a key is refused before anything else.
    let (code, err) = refused_at_start_with(&c.manifest, "short");
    assert_eq!(code, Some(2), "{err}");
    assert!(err.contains("at least 32 bytes"), "{err}");
    // The key it was made with still works.
    let s = Http::start(&c.manifest, &c.addr, &[]);
    let (code, err) = s.stop();
    assert_eq!(code, Some(0), "{err}");
    // With nothing pending (a fresh journal directory), a new key is recorded.
    let fresh = oauth_case("key-fresh");
    let s = Http::start(&fresh.manifest, &fresh.addr, &[]);
    s.stop();
    assert_eq!(std::fs::read_to_string(fresh.dir().join("journals/principal-key")).unwrap(), recorded);
}

const METADATA_URL: &str = "https://mcp.shop.example/.well-known/oauth-protected-resource/mcp";

#[test]
#[cfg(feature = "failpoints")]
fn the_protected_resource_metadata_is_served_without_a_token() {
    let c = oauth_case("metadata");
    let s = Http::start(&c.manifest, &c.addr, &[]);
    let want = json!({ "resource": RESOURCE, "authorization_servers": [c.auth.base], "bearer_methods_supported": ["header"], "scopes_supported": ["tools"] });
    for path in ["/.well-known/oauth-protected-resource/mcp", "/.well-known/oauth-protected-resource"] {
        let a = send_raw(&c.addr, format!("GET {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n", c.addr).as_bytes()).unwrap();
        assert_eq!((a.status, a.header("content-type")), (200, Some("application/json")), "{path}");
        assert_eq!(a.json(), want, "{path}");
    }
    let a = send_raw(&c.addr, format!("POST /.well-known/oauth-protected-resource HTTP/1.1\r\nHost: {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n", c.addr).as_bytes()).unwrap();
    assert_eq!((a.status, a.header("allow")), (405, Some("GET")));
    // Host checking applies to it too.
    let a = send_raw(&c.addr, b"GET /.well-known/oauth-protected-resource HTTP/1.1\r\nHost: evil.example\r\nConnection: close\r\n\r\n").unwrap();
    assert_eq!(a.status, 421);
    let (code, err) = s.stop();
    assert_eq!(code, Some(0), "{err}");
}

#[test]
#[cfg(feature = "failpoints")]
fn requests_need_a_valid_token_and_tools_their_scopes() {
    let c = oauth_case("tokens");
    let s = Http::start(&c.manifest, &c.addr, &[]);
    let list = rpc(1, "tools/list", json!({}));
    let bearer = |t: &str| format!("Bearer {t}");

    // No token: 401 naming the metadata and the scopes, with no error code.
    let a = post(&c.addr, &list, &[]);
    assert_eq!(a.status, 401);
    assert_eq!(a.header("www-authenticate"), Some(format!("Bearer scope=\"tools\", resource_metadata=\"{METADATA_URL}\"").as_str()));

    // A valid token.
    let good = c.token("tools refunds:write", json!({}), json!({}));
    let a = post(&c.addr, &list, &[("Authorization", &bearer(&good))]);
    assert_eq!(a.status, 200, "{:?}", String::from_utf8_lossy(&a.body));
    // The scheme is case-insensitive.
    assert_eq!(post(&c.addr, &list, &[("Authorization", &format!("bearer {good}"))]).status, 200);
    let call = rpc(2, "tools/call", json!({ "name": "refund_order", "arguments": { "order": "o-1", "amount": usd("10.00") } }));
    let a = post(&c.addr, &call, &[("Authorization", &bearer(&good))]);
    assert_eq!(a.json()["result"]["isError"], false, "{:?}", a.json());

    // Without the tool's scope: 403 naming every scope it needs; a tool needing none is served.
    let narrow = c.token("tools", json!({}), json!({}));
    let a = post(&c.addr, &call, &[("Authorization", &bearer(&narrow))]);
    assert_eq!(a.status, 403);
    assert_eq!(a.header("www-authenticate"), Some(format!("Bearer error=\"insufficient_scope\", scope=\"refunds:write\", resource_metadata=\"{METADATA_URL}\"").as_str()));
    let echo = rpc(3, "tools/call", json!({ "name": "echo", "arguments": { "x": "hi" } }));
    let a = post(&c.addr, &echo, &[("Authorization", &bearer(&narrow))]);
    assert_eq!(a.json()["result"]["structuredContent"]["result"], "hi");

    // Each token refused: 401 with invalid_token.
    let invalid = format!("Bearer error=\"invalid_token\", scope=\"tools\", resource_metadata=\"{METADATA_URL}\"");
    let other = common::tokens::Issuer::new("k1"); // the same kid, another key
    let refused = [
        ("expired", c.token("tools", json!({ "exp": common::tokens::now() - 120, "iat": common::tokens::now() - 600 }), json!({}))),
        ("another audience", c.token("tools", json!({ "aud": "https://other.example/mcp" }), json!({}))),
        ("another issuer", c.token("tools", json!({ "iss": "https://evil.example" }), json!({}))),
        ("typ JWT", c.token("tools", json!({}), json!({ "typ": "JWT" }))),
        ("alg none", c.token("tools", json!({}), json!({ "alg": "none" }))),
        ("signed by another key", other.token(&common::tokens::claims(&c.auth.base, RESOURCE, "user-1", "tools", &json!({})), &json!({}))),
        ("no client_id", c.token("tools", json!({ "client_id": null }), json!({}))),
        ("not a JWT", "abc.def".to_string()),
    ];
    for (what, t) in &refused {
        let a = post(&c.addr, &list, &[("Authorization", &bearer(t))]);
        assert_eq!((a.status, a.header("www-authenticate")), (401, Some(invalid.as_str())), "{what}");
    }
    // A token in the query string is never read; another scheme is no credentials.
    let mut raw = request_bytes(&c.addr, &list, &[]);
    let q = format!("/mcp?access_token={good}");
    raw.splice(5..9, q.bytes());
    let a = send_raw(&c.addr, &raw).unwrap();
    assert_eq!(a.status, 401, "the path is /mcp, and no token is read from the query");
    assert!(!a.header("www-authenticate").unwrap().contains("error="), "as with no credentials at all");
    let a = post(&c.addr, &list, &[("Authorization", "Basic dXNlcjpwYXNz")]);
    assert_eq!((a.status, a.header("www-authenticate").is_some_and(|h| !h.contains("error="))), (401, true));
    // Two Authorization headers: a bad request.
    let mut raw = String::from_utf8(request_bytes(&c.addr, &list, &[("Authorization", &bearer(&good))])).unwrap();
    raw = raw.replacen("Authorization:", &format!("Authorization: {}\r\nAuthorization:", bearer(&good)), 1);
    let a = send_raw(&c.addr, raw.as_bytes()).unwrap();
    assert_eq!(a.status, 400);
    assert!(a.header("www-authenticate").unwrap().contains("error=\"invalid_request\""));

    let (code, err) = s.stop();
    assert_eq!(code, Some(0), "{err}");
    // The log says why tokens were refused, and never holds a token.
    assert!(err.contains("a token was refused (exp)") && err.contains("a token was refused (aud)") && err.contains("(signature)"), "{err}");
    for (_, t) in &refused {
        if t.len() > 20 {
            assert!(!err.contains(t.as_str()), "a token reached the log");
        }
    }
    assert!(!err.contains(&good));
}

#[test]
#[cfg(feature = "failpoints")]
fn a_rotated_key_is_fetched_once_and_made_up_keys_fetch_at_most_once_a_minute() {
    use common::authserver::Route;
    let c = oauth_case("rotation");
    let s = Http::start(&c.manifest, &c.addr, &[]);
    let list = rpc(1, "tools/list", json!({}));
    assert_eq!(c.auth.hits("/jwks"), 1, "fetched once at start");
    // The authorization server rotates to a new key.
    let next = common::tokens::Issuer::new("k2");
    c.auth.set("/jwks", Route::json(&json!({ "keys": [next.jwk()] })));
    let t = next.token(&common::tokens::claims(&c.auth.base, RESOURCE, "user-1", "tools", &json!({})), &json!({}));
    assert_eq!(post(&c.addr, &list, &[("Authorization", &format!("Bearer {t}"))]).status, 200);
    assert_eq!(c.auth.hits("/jwks"), 2);
    // The old key is no longer accepted.
    let old = c.token("tools", json!({}), json!({}));
    assert_eq!(post(&c.addr, &list, &[("Authorization", &format!("Bearer {old}"))]).status, 401);
    // Made-up kids within the minute fetch nothing more.
    for i in 0..20 {
        let fake = c.token("tools", json!({}), json!({ "kid": format!("made-up-{i}") }));
        assert_eq!(post(&c.addr, &list, &[("Authorization", &format!("Bearer {fake}"))]).status, 401);
    }
    assert_eq!(c.auth.hits("/jwks"), 2);
    let (code, err) = s.stop();
    assert_eq!(code, Some(0), "{err}");
}

#[test]
#[cfg(feature = "failpoints")]
fn an_authorization_server_that_cannot_be_trusted_stops_the_server_at_start() {
    use common::authserver::Route;
    let c = oauth_case("bad-as");
    c.auth.set("/.well-known/oauth-authorization-server", Route::json(&json!({ "issuer": "https://evil.example", "jwks_uri": format!("{}/jwks", c.auth.base) })));
    let (code, err) = refused_at_start(&c.manifest);
    assert_eq!(code, Some(2), "{err}");
    assert!(err.contains("exactly"), "{err}");
    c.auth.set("/.well-known/oauth-authorization-server", Route::status(500));
    let (code, err) = refused_at_start(&c.manifest);
    assert_eq!(code, Some(2), "{err}");
    assert!(err.contains("answered 500"), "{err}");
    // Scopes on a tool need a server that checks tokens.
    let addr = free_address();
    let tools = json!([{ "name": "refund_order", "description": "d", "program": "refund.staal",
        "grants": { "pay": { "cap": "payments.refund", "each": usd("50.00"), "uses": 1 } }, "scopes": ["refunds:write"] }]);
    let (_d, m) = case("scopes-without-auth", &addr, json!({}), Some(tools));
    let (code, err) = refused_at_start(&m);
    assert_eq!(code, Some(2), "{err}");
    assert!(err.contains("tools[0].scopes: only a server with server.http.auth checks scopes"), "{err}");
}

// ---- Limits per principal (stage 2e, docs/MCP.md section 12.6) ----

/// Changes the OAuth case's manifest with `f` and writes it back.
fn edit_manifest(c: &OAuthCase, f: impl FnOnce(&mut Json)) {
    let mut m: Json = serde_json::from_str(&std::fs::read_to_string(&c.manifest).unwrap()).unwrap();
    f(&mut m);
    std::fs::write(&c.manifest, serde_json::to_string_pretty(&m).unwrap()).unwrap();
}

#[test]
#[cfg(feature = "failpoints")]
fn a_principal_over_its_request_rate_waits_while_another_is_served() {
    let c = oauth_case("rate");
    edit_manifest(&c, |m| m["server"]["http"]["per_principal"] = json!({ "requests_per_minute": 3 }));
    let _s = Http::start(&c.manifest, &c.addr, &[]);
    let (erin, frank) = (c.token_for("erin"), c.token_for("frank"));
    let list = rpc(1, "tools/list", json!({}));
    for _ in 0..3 {
        assert_eq!(post(&c.addr, &list, &[("Authorization", &format!("Bearer {erin}"))]).status, 200);
    }
    let a = post(&c.addr, &list, &[("Authorization", &format!("Bearer {erin}"))]);
    assert_eq!(a.status, 429);
    let wait: u64 = a.header("retry-after").unwrap().parse().unwrap();
    assert!((1..=60).contains(&wait), "{wait}");
    // Another principal is not affected.
    assert_eq!(post(&c.addr, &list, &[("Authorization", &format!("Bearer {frank}"))]).status, 200);
    // A request without a valid token is not counted against anyone, and gets 401.
    assert_eq!(post(&c.addr, &list, &[]).status, 401);
}

#[test]
#[cfg(feature = "failpoints")]
fn a_principal_at_its_run_limit_is_busy_while_another_runs() {
    let fake = Server::start();
    let c = oauth_case("runs");
    // Two slow requests per run; the service records each when it answers it (400 ms).
    let base = fake.url("");
    std::fs::write(c.dir().join("slow.staal"), format!("fn main(n: Int, get: cap net.http.get) -> Int {{\n    let a = get.get(\"{base}/slow\")\n    let b = get.get(\"{base}/slow\")\n    return n\n}}\n")).unwrap();
    edit_manifest(&c, |m| {
        m["server"]["http"]["per_principal"] = json!({ "max_concurrent_runs": 1 });
        m["tools"].as_array_mut().unwrap().push(json!({ "name": "slow", "description": "d", "program": "slow.staal",
            "grants": { "get": { "cap": "net.http.get" } }, "net": { "connect_timeout_ms": 2000, "timeout_ms": 5000 } }));
    });
    let _s = Http::start(&c.manifest, &c.addr, &[]);
    let (gina, hal) = (c.token_for("gina"), c.token_for("hal"));
    let slow = |id: i64| rpc(id, "tools/call", json!({ "name": "slow", "arguments": { "n": id } }));
    let first = {
        let (addr, t, b) = (c.addr.clone(), gina.clone(), slow(1));
        std::thread::spawn(move || authed(&addr, &b, &t))
    };
    // Once gina's run is surely under way (its first request answered, its second sent)...
    let start = Instant::now();
    while fake.requests().is_empty() {
        assert!(start.elapsed() < Duration::from_secs(20), "the first run never began");
        std::thread::sleep(Duration::from_millis(5));
    }
    // ...her second call is refused at once, and nothing runs; hal is served.
    let busy = authed(&c.addr, &slow(2), &gina);
    assert_eq!(busy["result"]["isError"], true, "{busy}");
    assert!(busy["result"]["content"][0]["text"].as_str().unwrap().contains("as many calls in progress as one caller may"), "{busy}");
    let other = authed(&c.addr, &slow(3), &hal);
    assert_eq!(other["result"]["structuredContent"]["result"], 3, "{other}");
    assert_eq!(first.join().unwrap()["result"]["structuredContent"]["result"], 1);
    // Her slot is free again once her call is answered.
    assert_eq!(authed(&c.addr, &slow(4), &gina)["result"]["structuredContent"]["result"], 4);
    assert_eq!(fake.requests().len(), 6, "the refused call reached nothing");
}

#[test]
#[cfg(feature = "failpoints")]
fn a_principal_spends_its_share_of_a_quota_and_it_stays_spent_across_a_restart() {
    let c = oauth_case("share");
    edit_manifest(&c, |m| {
        m["tools"][1]["quota"] = json!({ "window": "24h", "calls": 10, "per_principal": { "calls": 2 } });
    });
    let echo = |id: i64| rpc(id, "tools/call", json!({ "name": "echo", "arguments": { "x": "hi" } }));
    let (ivy, jack) = (c.token_for("ivy"), c.token_for("jack"));
    let s = Http::start(&c.manifest, &c.addr, &[]);
    for id in 1..=2 {
        assert_eq!(authed(&c.addr, &echo(id), &ivy)["result"]["isError"], false);
    }
    let over = authed(&c.addr, &echo(3), &ivy);
    assert_eq!(over["result"]["isError"], true);
    assert!(over["result"]["content"][0]["text"].as_str().unwrap().contains("this caller's quota of 2 calls per 1d is used up"), "{over}");
    assert_eq!(authed(&c.addr, &echo(4), &jack)["result"]["isError"], false, "the tool's quota is not spent");
    s.stop();
    // Durable: it is read from the ledger.
    let _s = Http::start(&c.manifest, &c.addr, &[]);
    assert_eq!(authed(&c.addr, &echo(5), &ivy)["result"]["isError"], true);
    assert_eq!(authed(&c.addr, &echo(6), &jack)["result"]["isError"], false);
}

#[test]
#[cfg(feature = "failpoints")]
fn limits_per_principal_are_checked_before_anything_is_served() {
    let refused = |name: &str, f: &dyn Fn(&mut Json), want: &str| {
        let c = oauth_case(name);
        edit_manifest(&c, f);
        let (code, err) = refused_at_start(&c.manifest);
        assert_eq!(code, Some(2), "{name}: {err}");
        assert!(err.contains(want), "{name}: expected {want:?} in {err}");
    };
    refused("pp-zero", &|m| m["server"]["http"]["per_principal"] = json!({ "requests_per_minute": 0 }), "requests_per_minute");
    refused("pp-key", &|m| m["server"]["http"]["per_principal"] = json!({ "colour": 1 }), "unknown key 'colour'");
    refused("share-no-quota", &|m| m["tools"][1]["quota"] = json!({ "window": "1h", "money": usd("5.00"), "per_principal": { "calls": 1 } }), "the tool's quota counts no calls");
    refused("share-more", &|m| m["tools"][1]["quota"] = json!({ "window": "1h", "calls": 2, "per_principal": { "calls": 3 } }), "more than the tool's quota of 2");
    refused("tool-pp", &|m| m["tools"][1]["per_principal"] = json!({}), "per_principal.max_concurrent_runs: missing");
    // Without auth there are no principals to limit.
    let (_d, m) = case("pp-without-auth", "127.0.0.1:1", json!({ "per_principal": { "requests_per_minute": 5 } }), None);
    let (code, err) = refused_at_start(&m);
    assert_eq!(code, Some(2), "{err}");
    assert!(err.contains("only a server with server.http.auth knows its principals"), "{err}");
}

// ---- Calls over HTTP survive crashes as over stdio (stage 3, docs/MCP.md 12.7) ----
//
// The scenarios of tests/mcp_crash_test.rs, over HTTP with a token: the server is
// killed at every point a call makes something durable, started again, and the client
// sends the same call again with the same token, as a client that saw its connection
// close may.

const STRIPE_KEY: &str = "sk_test_mcp_http_crash_7f6e5d4c";

/// An OAuth case serving refunds through the Stripe library against a fake service:
/// `refund` with `once`, `refund_any` without.
struct RefundCase {
    c: OAuthCase,
    service: Server,
}

fn refund_case(name: &str) -> RefundCase {
    let c = oauth_case(name);
    let service = Server::start();
    let dir = c.dir();
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/capabilities");
    std::fs::copy(examples.join("stripe.staal"), dir.join("stripe.staal")).unwrap();
    std::fs::write(
        dir.join("stripe_refund.staal"),
        "import stripe\n\nfn main(payment: Text, amount: Money, refunds: cap stripe.refunds.create(max: 100 USD)) -> Receipt {\n    return refunds.create(payment, amount)\n}\n",
    )
    .unwrap();
    let s = |p: &str| service.url(p);
    let tool = |name: &str, once: bool| {
        let mut t = json!({
            "name": name, "description": "Refund a payment.", "program": "stripe_refund.staal",
            "grants": { "refunds": { "cap": "stripe.refunds.create", "each": usd("30.00"), "uses": 1, "only": ["pi_usd"], "needs": {
                "get": { "cap": "net.http.get", "only": [s("/v1/payment_intents/*")] },
                "post": { "cap": "net.http.post", "only": [s("/v1/refunds")], "idempotency_key": true },
                "key": { "env": "STAAL_MCP_HTTP_CRASH_KEY" },
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
    edit_manifest(&c, |m| m["tools"] = json!([tool("refund", true), tool("refund_any", false)]));
    RefundCase { c, service }
}

impl RefundCase {
    fn start(&self, extra: &[(&str, &str)]) -> Http {
        let mut env = vec![("STAAL_MCP_HTTP_CRASH_KEY", STRIPE_KEY)];
        env.extend_from_slice(extra);
        Http::start(&self.c.manifest, &self.c.addr, &env)
    }

    fn ledger(&self) -> Vec<Json> {
        std::fs::read_to_string(self.c.dir().join("journals/server.ledger"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn settled(&self) -> Vec<Json> {
        self.ledger().into_iter().filter(|e| e["e"] == "settle").collect()
    }

    fn refunds(&self) -> usize {
        self.service.effects("/v1/refunds")
    }
}

fn stripe_call(id: i64, tool: &str) -> Json {
    rpc(id, "tools/call", json!({ "name": tool, "arguments": { "payment": "pi_usd", "amount": usd("25.00") } }))
}

/// Kills the server at `fp` during a call of `tool` by `token`'s principal, then starts
/// it again and sends the same call; returns the answer to the call sent again.
fn crash_then_retry(case: &RefundCase, tool: &str, fp: (&str, &str), token: &str) -> Json {
    let mut s = case.start(&[fp]);
    let auth = format!("Bearer {token}");
    let lost = send_raw(&case.c.addr, &request_bytes(&case.c.addr, &stripe_call(1, tool), &[("Authorization", &auth)]));
    assert!(lost.is_none(), "{fp:?}: no answer before the crash, got {lost:?}");
    let status = s.child.wait().unwrap();
    assert_ne!(status.code(), Some(0), "{fp:?}: the server was killed");
    let s = case.start(&[]);
    let r = authed(&case.c.addr, &stripe_call(2, tool), token);
    let (code, err) = s.stop();
    assert_eq!(code, Some(0), "{err}");
    r["result"].clone()
}

#[test]
#[cfg(feature = "failpoints")]
fn over_http_with_once_a_call_sent_again_after_any_crash_refunds_exactly_once() {
    let clean = refund_case("http-crash-clean");
    let s = clean.start(&[]);
    let expected = authed(&clean.c.addr, &stripe_call(1, "refund"), &clean.c.token_for("kim"))["result"].clone();
    s.stop();
    assert_eq!(expected["isError"], false, "{expected}");
    assert_eq!(clean.refunds(), 1);

    let mut points: Vec<(&str, String)> = Vec::new();
    for n in 1..=2 {
        points.push(("STAALC_MCP_FAILPOINT", format!("ledger_write:{n}")));
    }
    for n in 1..=6 {
        points.push(("STAALC_FAILPOINT", format!("after_write:{n}")));
    }
    for op in ["1", "2"] {
        points.push(("STAALC_FAILPOINT", format!("before_result:{op}")));
    }
    for (k, v) in &points {
        let case = refund_case(&format!("http-crash-{}", v.replace(':', "-")));
        let token = case.c.token_for("kim");
        let r = crash_then_retry(&case, "refund", (k, v), &token);
        assert_eq!(r["isError"], false, "{v}: {r}");
        assert_eq!(r["structuredContent"]["result"]["capability"], expected["structuredContent"]["result"]["capability"], "{v}");
        assert_eq!(case.refunds(), 1, "{v}: refunded exactly once");
        let keys: std::collections::HashSet<String> = case
            .service
            .requests()
            .iter()
            .filter(|q| q.path == "/v1/refunds")
            .map(|q| q.headers.get("idempotency-key").cloned().unwrap_or_default())
            .collect();
        assert_eq!(keys.len(), 1, "{v}: {keys:?}");
        let settled = case.settled();
        assert_eq!(settled.len(), 1, "{v}: {:?}", case.ledger());
        assert_eq!(settled[0]["charged"], json!([usd("25.00")]), "{v}");
        let ends = case.ledger().iter().filter(|e| e["e"] == "settle" || e["e"] == "release").count();
        assert_eq!(ends, case.ledger().iter().filter(|e| e["e"] == "reserve").count(), "{v}: {:?}", case.ledger());
        // Every reservation is the principal's, and neither the token nor the Stripe key
        // is in any file.
        assert!(case.ledger().iter().filter(|e| e["e"] == "reserve").all(|e| e["scope"].as_str().is_some_and(|s| s.len() == 64)), "{v}");
        for f in common::mcp::files_under(&case.c.dir().join("journals")) {
            let text = std::fs::read_to_string(&f).unwrap_or_default();
            assert!(!text.contains(STRIPE_KEY) && !text.contains(&token) && !text.contains("kim"), "{v}: {}", f.display());
        }
    }
}

#[test]
#[cfg(feature = "failpoints")]
fn over_http_without_once_a_call_sent_again_after_the_refund_refunds_again() {
    let case = refund_case("http-any-after-settle");
    let token = case.c.token_for("lee");
    let r = crash_then_retry(&case, "refund_any", ("STAALC_MCP_FAILPOINT", "ledger_write:2"), &token);
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(case.refunds(), 2, "a second run is a second refund");
    let case = refund_case("http-any-after-reserve");
    let token = case.c.token_for("lee");
    let r = crash_then_retry(&case, "refund_any", ("STAALC_MCP_FAILPOINT", "ledger_write:1"), &token);
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(case.refunds(), 1);
    assert_eq!(case.settled().len(), 1);
}

#[test]
#[cfg(feature = "failpoints")]
fn after_a_crash_one_principal_never_gets_or_repeats_anothers_refund() {
    // Mia's refund succeeded and settled; the server died before she was answered.
    let case = refund_case("http-two-principals");
    let (mia, ned) = (case.c.token_for("mia"), case.c.token_for("ned"));
    let mut s = case.start(&[("STAALC_MCP_FAILPOINT", "ledger_write:2")]);
    let lost = send_raw(&case.c.addr, &request_bytes(&case.c.addr, &stripe_call(1, "refund"), &[("Authorization", &format!("Bearer {mia}"))]));
    assert!(lost.is_none());
    s.child.wait().unwrap();
    assert_eq!(case.refunds(), 1);
    let s = case.start(&[]);
    // Ned sends the same arguments: his own operation, a run of his own, not Mia's result.
    let n = authed(&case.c.addr, &stripe_call(2, "refund"), &ned)["result"].clone();
    assert_eq!(n["isError"], false, "{n}");
    assert_eq!(case.refunds(), 2);
    // Mia sends hers again: answered from her run, with no new refund.
    let m = authed(&case.c.addr, &stripe_call(3, "refund"), &mia)["result"].clone();
    assert_eq!(m["isError"], false, "{m}");
    assert_eq!(case.refunds(), 2, "Mia's retry refunded nothing more");
    let receipt = |r: &Json| r["structuredContent"]["result"]["receipt"].as_str().unwrap().to_string();
    assert_ne!(receipt(&m), receipt(&n), "each got their own refund's receipt");
    s.stop();
    let scopes: std::collections::HashSet<String> =
        case.ledger().iter().filter(|e| e["e"] == "reserve").map(|e| e["scope"].as_str().unwrap().to_string()).collect();
    assert_eq!(scopes.len(), 2);
}

#[test]
#[cfg(feature = "failpoints")]
fn a_connection_closed_before_the_reservation_does_nothing_and_after_it_refunds_once() {
    // Before: the server holds the call for 1.5 s just before reserving; the client
    // closes its connection meanwhile.
    let case = refund_case("http-cancel-before");
    let token = case.c.token_for("oli");
    let s = case.start(&[("STAALC_MCP_PAUSE", "before_reserve:1500")]);
    let mut conn = TcpStream::connect(&case.c.addr).unwrap();
    conn.write_all(&request_bytes(&case.c.addr, &stripe_call(1, "refund"), &[("Authorization", &format!("Bearer {token}"))])).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    drop(conn);
    std::thread::sleep(Duration::from_millis(2000));
    let (code, err) = s.stop();
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(case.refunds(), 0, "nothing reached the service");
    assert!(case.ledger().iter().all(|e| e["e"] != "reserve"), "nothing was reserved: {:?}", case.ledger());

    // After: the client asks for progress, and closes the stream once the run has begun.
    let case = refund_case("http-cancel-after");
    let token = case.c.token_for("oli");
    let s = case.start(&[]);
    let mut call = stripe_call(1, "refund");
    call["params"]["_meta"]["progressToken"] = json!("p");
    let mut conn = TcpStream::connect(&case.c.addr).unwrap();
    conn.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    conn.write_all(&request_bytes(&case.c.addr, &call, &[("Authorization", &format!("Bearer {token}"))])).unwrap();
    let mut seen = Vec::new();
    let mut b = [0u8; 256];
    let run = loop {
        let n = conn.read(&mut b).unwrap();
        assert!(n > 0, "the stream ended before the run began");
        seen.extend_from_slice(&b[..n]);
        let t = String::from_utf8_lossy(&seen).into_owned();
        if let Some(i) = t.find("run ") {
            break t[i + 4..i + 36].to_string();
        }
    };
    drop(conn);
    // The run goes on to its end, is recorded, and refunds exactly once; its principal
    // can find it, and the same call is answered from it.
    let start = Instant::now();
    while case.settled().is_empty() {
        assert!(start.elapsed() < Duration::from_secs(20), "the run never settled: {:?}", case.ledger());
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(case.refunds(), 1);
    let st = authed(&case.c.addr, &rpc(2, "tools/call", json!({ "name": "staal_run_status", "arguments": { "run": run } })), &token);
    assert_eq!(st["result"]["structuredContent"]["status"], "done", "{st}");
    let again = authed(&case.c.addr, &stripe_call(3, "refund"), &token);
    assert_eq!(again["result"]["isError"], false);
    assert_eq!(case.refunds(), 1, "answered from the same run");
    s.stop();
}
