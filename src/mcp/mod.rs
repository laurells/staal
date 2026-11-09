// `staalc mcp`: Staal programs served as tools to Model Context Protocol clients
// (docs/MCP.md). A tool call is an ordinary Staal run, with the same checks, broker,
// journal and recovery as `staalc run`; the server adds bounds across calls (quotas,
// concurrency, `once`) and never removes one.
//
//   staalc mcp <manifest.json>          serve on standard input and output
//   staalc mcp status <manifest.json>   what is waiting, and quota use per tool
//
// This file reads the protocol and answers it; `crate::host` runs calls, records them in
// its ledger and recovers; `manifest` reads the manifest.

pub mod http;
pub mod jwt;
pub mod keys;
pub mod manifest;
pub mod principal;
pub mod proto;

use crate::host::tool::{Tool, STATUS_TOOL};
use crate::host::{self, ledger, log, Answer, Ctl, Host};
use manifest::Manifest;
use proto::{Line, Sink, Writer};
use serde_json::{json, Map, Value as Json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// The protocol version served per request, with `_meta`.
pub const MODERN: &str = "2026-07-28";
/// The protocol version served after `initialize`.
pub const LEGACY: &str = "2025-11-25";
/// How long a client may consider the tool list and the discovery result fresh. The
/// list never changes while the server runs.
const TTL_MS: u64 = 60 * 60 * 1000;

pub const VERSION_KEY: &str = "io.modelcontextprotocol/protocolVersion";
const CAPABILITIES_KEY: &str = "io.modelcontextprotocol/clientCapabilities";
const SERVER_INFO_KEY: &str = "io.modelcontextprotocol/serverInfo";

const USAGE: &str = "usage:\n  staalc mcp <manifest.json>\n  staalc mcp status <manifest.json>";

/// `staalc mcp ...`. Returns the exit code.
pub fn main(args: &[String]) -> u8 {
    match args {
        [cmd, path] if cmd == "status" => status_cmd(std::path::Path::new(path)),
        [path] if !path.starts_with('-') => serve(std::path::Path::new(path)),
        _ => {
            eprintln!("staalc: {USAGE}");
            2
        }
    }
}

fn serve(path: &std::path::Path) -> u8 {
    // Over HTTP when the manifest says so (docs/MCP.md section 12); otherwise stdio.
    match manifest::wants_http(path) {
        Ok(true) => return http::serve(path),
        Ok(false) => {}
        Err(e) => {
            log(&e);
            return 2;
        }
    }
    // First, before anything can write to standard output or read standard input.
    let (input, output) = match proto::take_stdio() {
        Ok(io) => io,
        Err(e) => {
            log(&e);
            return 2;
        }
    };
    host::set_log_prefix("staalc mcp");
    let opened = manifest::load(path).and_then(|m| {
        let follow = format!("with {STATUS_TOOL}");
        let h = Host::open(m.settings.clone(), &follow, &m.warnings)?;
        Ok((m, h))
    });
    let (m, server) = match opened {
        Ok(s) => s,
        Err(e) => {
            log(&e);
            return 2;
        }
    };
    log(&format!("serving {} tool(s) from {}", m.tools.len(), path.display()));
    let session = Session::new(m, server.clone());
    let sweeper = Sweeper::start(server.clone());
    let writer = Arc::new(Writer::new(output));
    let out: Arc<dyn Sink> = writer.clone();
    let mut reader = proto::Reader::new(input);
    loop {
        match reader.next_line() {
            Ok(None) => break,
            Ok(Some(Line::TooLong)) => {
                let msg = format!("a message longer than {} bytes was refused unread", proto::MAX_LINE_BYTES);
                out.error(&Json::Null, proto::PARSE_ERROR, &msg, None);
            }
            Ok(Some(Line::Text(bytes))) => session.handle(&bytes, &out),
            Err(e) => {
                log(&format!("cannot read standard input: {e}"));
                break;
            }
        }
        if writer.broken() {
            log("the client is gone (a write to it failed); finishing the runs in progress");
            break;
        }
    }
    session.finish();
    sweeper.stop();
    0
}

/// Deletes files past `server.keep` once a day while the server runs.
pub(crate) struct Sweeper {
    stop: Arc<AtomicBool>,
    thread: std::thread::JoinHandle<()>,
}

impl Sweeper {
    pub(crate) fn start(server: Arc<Host>) -> Sweeper {
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let stop = stop.clone();
            std::thread::spawn(move || {
                let mut last = std::time::Instant::now();
                while !stop.load(Ordering::SeqCst) {
                    std::thread::park_timeout(std::time::Duration::from_secs(1));
                    if last.elapsed() >= host::SWEEP_EVERY {
                        last = std::time::Instant::now();
                        if let Err(e) = server.sweep() {
                            log(&e);
                        }
                    }
                }
            })
        };
        Sweeper { stop, thread }
    }

    pub(crate) fn stop(self) {
        self.stop.store(true, Ordering::SeqCst);
        self.thread.thread().unpark();
        let _ = self.thread.join();
    }
}

/// What both transports share: the tools, the host, and the calls in progress. Each
/// request's messages go to the `Sink` it was given.
pub(crate) struct Session {
    pub(crate) m: Manifest,
    server: Arc<Host>,
    /// Whether a client has sent `initialize`: requests without per-request `_meta`
    /// are then served as 2025-11-25. Never over HTTP, which serves 2026-07-28 only.
    legacy: AtomicBool,
    /// Calls in progress: over stdio by their request id as JSON text (for
    /// `notifications/cancelled`), over HTTP by a key of the server's own.
    calls: Mutex<HashMap<String, Arc<Ctl>>>,
    threads: Mutex<Vec<std::thread::JoinHandle<()>>>,
}

/// How a call can be cancelled: over stdio by `notifications/cancelled` naming its id;
/// over HTTP by its connection closing, which sets the given `Ctl`.
pub(crate) enum Cancel {
    ById,
    Given { ctl: Arc<Ctl>, key: String, caller: Option<Caller> },
}

/// Who an HTTP call is from, by its validated token (docs/MCP.md section 12.6).
pub(crate) struct Caller {
    /// The call's scope: its `once` keys and its runs' status are held within it.
    pub scope: String,
    pub client_id: String,
    /// The token's claims, for app inputs a tool takes from them.
    pub claims: Map<String, Json>,
}

#[derive(Clone, Copy, PartialEq)]
enum Era {
    Legacy,
    Modern,
}

pub(crate) fn valid_id(id: &Json) -> bool {
    id.is_string() || id.is_i64() || id.is_u64()
}

impl Session {
    pub(crate) fn new(m: Manifest, server: Arc<Host>) -> Arc<Session> {
        Arc::new(Session { m, server, legacy: AtomicBool::new(false), calls: Mutex::new(HashMap::new()), threads: Mutex::new(Vec::new()) })
    }

    /// Lets the calls in progress finish (or stop) as they would have, and the notify
    /// commands run; the process may then end.
    pub(crate) fn finish(&self) {
        let threads: Vec<_> = self.threads.lock().map(|mut t| std::mem::take(&mut *t)).unwrap_or_default();
        for t in threads {
            let _ = t.join();
        }
        self.server.wait_for_notifies();
    }

    fn handle(self: &Arc<Self>, bytes: &[u8], out: &Arc<dyn Sink>) {
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return;
        }
        if proto::nesting(bytes, proto::MAX_DEPTH) > proto::MAX_DEPTH {
            let msg = format!("a message nested deeper than {} levels was refused", proto::MAX_DEPTH);
            return out.error(&Json::Null, proto::INVALID_REQUEST, &msg, None);
        }
        let msg: Json = match serde_json::from_slice(bytes) {
            Ok(m) => m,
            Err(e) => return out.error(&Json::Null, proto::PARSE_ERROR, &format!("not JSON: {e}"), None),
        };
        let Some(o) = msg.as_object() else {
            let why = if msg.is_array() { "batches are not part of MCP; send one message per line" } else { "a message must be an object" };
            return out.error(&Json::Null, proto::INVALID_REQUEST, why, None);
        };
        let id = o.get("id");
        if id.is_some_and(|i| !valid_id(i)) {
            return out.error(&Json::Null, proto::INVALID_REQUEST, "a request id must be a string or an integer", None);
        }
        let Some(method) = o.get("method").and_then(Json::as_str) else {
            // A response (the server sends no requests, so it answers nothing) or junk.
            if id.is_some() && (o.contains_key("result") || o.contains_key("error")) {
                return;
            }
            return out.error(id.unwrap_or(&Json::Null), proto::INVALID_REQUEST, "a request needs a \"method\"", None);
        };
        if o.get("jsonrpc").and_then(Json::as_str) != Some("2.0") {
            return out.error(id.unwrap_or(&Json::Null), proto::INVALID_REQUEST, "\"jsonrpc\" must be \"2.0\"", None);
        }
        let empty = Map::new();
        let params = match o.get("params") {
            None => &empty,
            Some(Json::Object(p)) => p,
            Some(_) => {
                if let Some(id) = id {
                    out.error(id, proto::INVALID_PARAMS, "\"params\" must be an object", None);
                }
                return;
            }
        };
        match id {
            None => self.notification(method, params),
            Some(id) => self.request(id, method, params, out, Cancel::ById),
        }
    }

    fn notification(&self, method: &str, params: &Map<String, Json>) {
        if method == "notifications/cancelled"
            && let Some(req) = params.get("requestId")
            && let Ok(calls) = self.calls.lock()
            && let Some(ctl) = calls.get(&req.to_string())
        {
            ctl.cancelled.store(true, Ordering::SeqCst);
        }
        // Every other notification (`notifications/initialized` among them) needs nothing.
    }

    /// One request. Over HTTP the transport has already checked what the specification
    /// asks of the headers and refused every method the server does not serve.
    pub(crate) fn request(self: &Arc<Self>, id: &Json, method: &str, params: &Map<String, Json>, out: &Arc<dyn Sink>, cancel: Cancel) {
        if method == "initialize" && matches!(cancel, Cancel::ById) {
            self.legacy.store(true, Ordering::SeqCst);
            let r = json!({
                "protocolVersion": LEGACY,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": self.server_info(),
            });
            return out.result(id, r);
        }
        let meta = params.get("_meta").and_then(Json::as_object);
        let era = match meta.and_then(|m| m.get(VERSION_KEY)) {
            Some(v) => {
                if v.as_str() != Some(MODERN) {
                    let data = json!({ "supported": [MODERN], "requested": v });
                    return out.error(id, proto::UNSUPPORTED_VERSION, "Unsupported protocol version", Some(data));
                }
                if !meta.and_then(|m| m.get(CAPABILITIES_KEY)).is_some_and(Json::is_object) {
                    let msg = format!("_meta[\"{CAPABILITIES_KEY}\"] is required on every request");
                    return out.error(id, proto::INVALID_PARAMS, &msg, None);
                }
                Era::Modern
            }
            None if self.legacy.load(Ordering::SeqCst) => Era::Legacy,
            None => {
                let msg = format!(
                    "_meta[\"{VERSION_KEY}\"] and _meta[\"{CAPABILITIES_KEY}\"] are required on every request; \
                     this server serves {MODERN} per request, and {LEGACY} after `initialize`"
                );
                return out.error(id, proto::INVALID_PARAMS, &msg, None);
            }
        };
        match (era, method) {
            (Era::Legacy, "ping") => out.result(id, json!({})),
            (Era::Modern, "server/discover") => {
                let r = json!({
                    "supportedVersions": [MODERN],
                    "capabilities": { "tools": { "listChanged": false } },
                    "ttlMs": TTL_MS,
                    "cacheScope": "private",
                });
                out.result(id, self.modern(r));
            }
            (_, "tools/list") => {
                if params.contains_key("cursor") {
                    return out.error(id, proto::INVALID_PARAMS, "this server gives no cursors; the list is never paged", None);
                }
                let mut tools: Vec<Json> = self.m.tools.iter().map(describe).collect();
                tools.push(status_tool());
                let mut r = json!({ "tools": tools });
                if era == Era::Modern {
                    r["ttlMs"] = json!(TTL_MS);
                    r["cacheScope"] = json!("private");
                    r = self.modern(r);
                }
                out.result(id, r);
            }
            (_, "tools/call") => self.call(id, era, params, out, cancel),
            _ => out.error(id, proto::METHOD_NOT_FOUND, &format!("unknown method '{method}'"), None),
        }
    }

    fn server_info(&self) -> Json {
        json!({ "name": self.m.name, "version": env!("CARGO_PKG_VERSION") })
    }

    /// A 2026-07-28 result: `resultType` and the server's identity.
    fn modern(&self, mut r: Json) -> Json {
        r["resultType"] = json!("complete");
        let meta = r.as_object_mut().map(|o| o.entry("_meta").or_insert_with(|| json!({})));
        if let Some(m) = meta.and_then(Json::as_object_mut) {
            m.insert(SERVER_INFO_KEY.into(), self.server_info());
        }
        r
    }

    fn call(self: &Arc<Self>, id: &Json, era: Era, params: &Map<String, Json>, out: &Arc<dyn Sink>, cancel: Cancel) {
        let Some(name) = params.get("name").and_then(Json::as_str) else {
            return out.error(id, proto::INVALID_PARAMS, "tools/call needs the tool's \"name\"", None);
        };
        let args = match params.get("arguments") {
            None => Map::new(),
            Some(Json::Object(a)) => a.clone(),
            Some(_) => return out.error(id, proto::INVALID_PARAMS, "\"arguments\" must be an object", None),
        };
        let tool: Option<usize> = self.m.tools.iter().position(|t| t.name == name);
        if tool.is_none() && name != STATUS_TOOL {
            return out.error(id, proto::INVALID_PARAMS, &format!("unknown tool '{}'", crate::clip::clip(name)), None);
        }
        let token = params
            .get("_meta")
            .and_then(|m| m.get("progressToken"))
            .filter(|t| t.is_string() || t.is_i64() || t.is_u64())
            .cloned();
        let (ctl, key, caller) = match cancel {
            Cancel::ById => (Arc::new(Ctl::default()), id.to_string(), None),
            Cancel::Given { ctl, key, caller } => (ctl, key, caller),
        };
        // App inputs taken from the caller's token, never from the arguments.
        let mut app_inputs: Option<Map<String, Json>> = None;
        if let (Some(i), Some(c)) = (tool, &caller)
            && !self.m.tool_claims[i].is_empty()
        {
            let mut given = Map::new();
            for (input, claim) in &self.m.tool_claims[i] {
                match c.claims.get(claim) {
                    Some(Json::String(v)) => {
                        given.insert(input.clone(), Json::String(v.clone()));
                    }
                    _ => {
                        let a = Answer::error(format!(
                            "the access token has no text claim '{claim}', from which this tool's input '{input}' comes; nothing was done"
                        ));
                        return out.result(id, self.wrap(era, &a));
                    }
                }
            }
            app_inputs = Some(given);
        }
        {
            let Ok(mut calls) = self.calls.lock() else { return };
            // Threads are bounded too: far more calls than runs are answered at once.
            if calls.len() >= 4 * self.m.settings.max_concurrent_runs + 16 {
                drop(calls);
                let a = Answer::error("the server is busy with other calls; nothing was done; try again later");
                return out.result(id, self.wrap(era, &a));
            }
            calls.insert(key.clone(), ctl.clone());
        }
        let (me, id, out) = (self.clone(), id.clone(), out.clone());
        let h = std::thread::spawn(move || {
            let scope = caller.as_ref().map(|c| c.scope.as_str());
            let answer = match tool {
                None => Some(me.status(&args, scope)),
                Some(i) => {
                    let t: &Tool = &me.m.tools[i];
                    let started = |run: &str| {
                        if let Some(token) = &token {
                            out.send(&json!({
                                "jsonrpc": "2.0",
                                "method": "notifications/progress",
                                "params": { "progressToken": token, "progress": 0, "message": format!("run {run} started") },
                            }));
                        }
                    };
                    let a = me.server.call(t, &args, app_inputs.as_ref(), None, scope, &ctl, &started);
                    if let (Some(c), Some(a)) = (&caller, &a) {
                        log(&format!(
                            "tool {} run {}: called by principal {} (client {})",
                            t.name,
                            a.run.as_deref().unwrap_or("none"),
                            c.scope,
                            crate::clip::clip(&c.client_id)
                        ));
                    }
                    a
                }
            };
            if let Ok(mut calls) = me.calls.lock() {
                calls.remove(&key);
            }
            if let Some(a) = answer {
                out.result(&id, me.wrap(era, &a));
            }
        });
        if let Ok(mut t) = self.threads.lock() {
            t.retain(|h| !h.is_finished());
            t.push(h);
        }
    }

    fn wrap(&self, era: Era, a: &Answer) -> Json {
        let r = a.to_json();
        if era == Era::Modern { self.modern(r) } else { r }
    }

    fn status(&self, args: &Map<String, Json>, scope: Option<&str>) -> Answer {
        let run = args.get("run").and_then(Json::as_str);
        let ok = args.len() == 1 && run.is_some_and(|r| r.len() == 32 && r.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
        let Some(run) = run.filter(|_| ok) else {
            return Answer::error(format!("{STATUS_TOOL} takes {{\"run\": \"<the run id a tool's result gave: 32 hexadecimal characters>\"}}"));
        };
        let s = self.server.status(run, scope);
        Answer { status: "done", run: None, text: s.to_string(), structured: Some(s), is_error: false }
    }
}

fn describe(t: &Tool) -> Json {
    let mut d = json!({
        "name": t.name,
        "description": t.description,
        "inputSchema": t.input_schema,
        "outputSchema": t.output_schema,
        "annotations": t.annotations,
    });
    if let Some(title) = &t.title {
        d["title"] = json!(title);
    }
    d
}

fn status_tool() -> Json {
    json!({
        "name": STATUS_TOOL,
        "title": "Status of a run",
        "description": "Reports what happened to a run of this server's tools, by the run id a tool's result gave: \
            running, waiting_for_operator, done (with its result), failed (with why), not_started, expired \
            (its record is past the server's retention) or unknown. It only reads.",
        "inputSchema": {
            "type": "object",
            "properties": { "run": { "type": "string", "pattern": "^[0-9a-f]{32}$" } },
            "required": ["run"],
            "additionalProperties": false
        },
        "outputSchema": {
            "type": "object",
            "properties": {
                "run": { "type": "string" },
                "status": { "enum": ["running", "waiting_for_operator", "done", "failed", "not_started", "expired", "unknown"] },
                "observed_at": { "type": "string" },
                "result": {},
                "failure": { "type": "object" },
                "reason": { "type": "string" }
            },
            "required": ["run", "status", "observed_at"]
        },
        "annotations": { "readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false }
    })
}

/// `staalc mcp status <manifest>`: reads the ledger beside a running server without
/// changing anything, and lists what waits and what each tool's quota holds.
fn status_cmd(path: &std::path::Path) -> u8 {
    let m = match manifest::load(path) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("staalc: {e}");
            return 2;
        }
    };
    let l = match ledger::Ledger::read_only(&m.settings.journals.join("server.ledger")) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("staalc: {e}");
            return 2;
        }
    };
    let now = l.now();
    let mut open = 0;
    for (id, r) in l.runs().filter(|(_, r)| r.end.is_none()) {
        open += 1;
        let waiting = m.settings.journals.join("waiting").join(format!("{id}.json"));
        let what = match std::fs::read_to_string(&waiting).ok().and_then(|t| serde_json::from_str::<Json>(&t).ok()) {
            Some(w) => format!(
                "waiting for an operator ({}): {}\n    resolve with: staalc resolve --journal {} ...",
                w["kind"].as_str().unwrap_or("?"),
                w["in_doubt"].as_str().unwrap_or("?"),
                m.settings.journals.join(&r.tool).join(&id[..2]).join(format!("{id}.journal")).display()
            ),
            None => "in progress, or interrupted (the server resumes it when it next starts)".into(),
        };
        println!("run {id} (tool {}): {what}", r.tool);
    }
    if open == 0 {
        println!("no run is unfinished");
    }
    for t in &m.tools {
        println!("tool {}: {}", t.name, host::describe_quota(&l, t, now));
    }
    0
}
